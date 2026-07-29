use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::{DynamicImage, GenericImageView, RgbaImage};
use strsim::jaro_winkler;
use tracing::info;

use crate::db::ItemRow;
use crate::pricing::normalize_name;

/// WFInfo-derived layout constants (reference: 1920×1080).
const PIXEL_REWARD_WIDTH: f32 = 968.0;

#[derive(Debug, Clone)]
pub struct OcrResult {
    pub raw_lines: Vec<String>,
    pub slot_texts: Vec<String>,
}

pub fn tesseract_image(path: &Path, lang: &str) -> Result<String> {
    let output = Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(lang)
        .arg("--psm")
        .arg("6")
        .arg("--dpi")
        .arg("300")
        .output()
        .context("spawn tesseract (is it installed?)")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("tesseract failed: {err}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn screen_scaling(img: &DynamicImage) -> f32 {
    let (w, h) = img.dimensions();
    if w * 9 > h * 16 {
        h as f32 / 1080.0
    } else {
        w as f32 / 1920.0
    }
}

/// Isolate bright UI glyphs with a softer threshold than before (Cyrillic is thin).
pub fn preprocess_for_ocr(img: &DynamicImage) -> DynamicImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let lum = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8;
        // Keep light UI text + gold accents; lower threshold preserves thin Cyrillic strokes
        let keep = lum > 130 || (r > 160 && g > 120 && b < 140);
        let v = if keep { 0 } else { 255 };
        out.put_pixel(x, y, image::Rgba([v, v, v, a]));
    }
    DynamicImage::ImageRgba8(out)
}

/// Crop reward *name* bands for one squad-size layout.
fn crop_layout(img: &DynamicImage, n: u32) -> Vec<DynamicImage> {
    crop_layout_band(img, n, 0.365, 0.495)
}

fn crop_layout_band(img: &DynamicImage, n: u32, y0: f32, y1: f32) -> Vec<DynamicImage> {
    let (w, h) = img.dimensions();
    let scale = screen_scaling(img);
    let top = ((h as f32) * y0).max(0.0) as u32;
    let bot = ((h as f32) * y1).min(h as f32) as u32;
    let band_h = bot.saturating_sub(top).max(1);
    // Full-width-ish for 4-card; duo still fairly wide.
    let panel_w = match n {
        2 => ((980.0) * scale).min(w as f32 * 0.68),
        3 => ((PIXEL_REWARD_WIDTH) * scale).min(w as f32 * 0.82),
        _ => ((PIXEL_REWARD_WIDTH + 80.0) * scale).min(w as f32 * 0.96),
    };
    let left = (w as f32 / 2.0 - panel_w / 2.0).max(0.0) as u32;
    let width = (panel_w as u32).min(w.saturating_sub(left)).max(4);
    let slot_w = (width / n).max(1);

    let mut slots = Vec::with_capacity(n as usize);
    for i in 0..n {
        let x = left + i * slot_w;
        let inset = (slot_w as f32 * 0.04) as u32;
        let sx = x.saturating_add(inset);
        let sw = slot_w.saturating_sub(inset * 2).max(1);
        let cropped = img.crop_imm(sx, top, sw, band_h);
        let scaled = cropped.resize(
            sw.saturating_mul(2).max(1),
            band_h.saturating_mul(2).max(1),
            image::imageops::FilterType::CatmullRom,
        );
        slots.push(preprocess_for_ocr(&scaled));
    }
    slots
}

/// True when OCR text has a real item name, not just "Прайм" / "Чертёж" / "Форма".
fn slot_has_item_name(text: &str) -> bool {
    const GENERIC: &[&str] = &[
        "прайм",
        "prime",
        "чертеж",
        "чертёж",
        "черт",
        "blueprint",
        "форма",
        "forma",
        "систем",
        "нейро",
        "каркас",
        "рукоят",
        "клинок",
        "лезви",
        "ствол",
        "приклад",
        "приём",
        "прием",
        "комплект",
        "set",
    ];
    let lo = text.to_lowercase();
    lo.split(|c: char| !c.is_alphabetic()).any(|t| {
        let n = t.chars().count();
        n >= 5
            && !GENERIC.iter().any(|g| {
                t == *g
                    || g.starts_with(t)
                    || t.starts_with(g)
                    || (g.chars().count() >= 4 && t.contains(g))
            })
    })
}

fn slot_is_occupied(text: &str) -> bool {
    // Named item + reward keyword (prime/part/blueprint). Keyword-only
    // fragments like "Прайм" / "Форма" must not count as a filled card.
    if text.trim().is_empty() || looks_like_merged_cards(text) || !slot_has_item_name(text) {
        return false;
    }
    let lo = text.to_lowercase();
    [
        "черт",
        "blueprint",
        "прайм",
        "prime",
        "нейро",
        "каркас",
        "систем",
        "рукоят",
        "клинок",
        "лезви",
        "ствол",
        "приклад",
        "приём",
        "прием",
        "комплект",
        "форма",
        "forma",
    ]
    .iter()
    .any(|kw| lo.contains(kw))
}

fn looks_like_merged_cards(text: &str) -> bool {
    let roles = part_roles(text);
    const WEAPON: &[&str] = &["handle", "blade", "barrel", "stock", "receiver"];
    const FRAME: &[&str] = &["neuroptics", "chassis", "systems"];
    let weapon_n = roles.iter().filter(|r| WEAPON.contains(r)).count();
    let frame_n = roles.iter().filter(|r| FRAME.contains(r)).count();
    if (weapon_n > 0 && frame_n > 0) || weapon_n > 1 || frame_n > 1 {
        return true;
    }
    let lo = text.to_lowercase();
    // Two "Прайм" or two part words in one crop = two cards glued together.
    if lo.matches("прайм").count() + lo.matches("prime").count() >= 2 {
        return true;
    }
    let role_hits: usize = [
        "ствол", "приклад", "приём", "прием", "клинок", "лезви", "рукоят", "каркас", "систем",
        "нейро", "тетив",
    ]
    .iter()
    .map(|kw| lo.matches(kw).count())
    .sum();
    if role_hits >= 2 {
        return true;
    }
    // Two different prime families jammed into one crop (Перигаль + Бронко).
    family_hint_count(text) >= 2
}

/// How many distinct item-family hints appear in OCR text.
fn family_hint_count(text: &str) -> usize {
    let lo = text.to_lowercase();
    const FAMILIES: &[&[&str]] = &[
        &["бронко", "bronco", "акбронко", "akbronco"],
        &["перигаль", "perigale", "ригаль"],
        &["парис", "paris"],
        &["брэйтон", "braton", "братон"],
        &["сарофанг", "sarofang"],
        &["зорен", "zoren"],
        &["клыки", "fang"],
        &["коринф", "corinth"],
        &["лавас", "lavos", "гавос"],
        &["ортос", "orthos"],
        &["калибан", "caliban"],
        &["аши", "ash"],
        &["форма"],
        &["волакс", "volnus"],
        &["дайкю", "daikyu"],
        &["пирана", "pyran"],
        &["компресс", "kompressa"],
    ];
    // Only *known* families count here. OCR noise ("РагкуСпагйе") used to be
    // treated as a second family and made every clean crop look merged.
    FAMILIES
        .iter()
        .filter(|aliases| {
            aliases
                .iter()
                .any(|a| a.chars().count() >= 5 && lo.contains(a))
        })
        .count()
}

fn reward_keyword_score(text: &str) -> i32 {
    let lo = text.to_lowercase();
    let mut score = 0i32;
    for kw in [
        "черт",
        "blueprint",
        "прайм",
        "prime",
        "нейро",
        "каркас",
        "систем",
        "рукоят",
        "клинок",
        "лезви",
        "ствол",
        "приклад",
        "приём",
        "прием",
        "комплект",
    ] {
        if lo.contains(kw) {
            score += 3;
        }
    }
    // Forma alone is a common OCR false positive from empty slots — weak signal.
    if (lo.contains("форма") || lo.contains("forma")) && slot_has_item_name(text) {
        score += 3;
    } else if lo.contains("форма") || lo.contains("forma") {
        score += 1;
    }
    if looks_like_merged_cards(text) {
        score -= 18;
    }
    for bad in ["выбор", "реликви", "процессе", "реагент"] {
        if lo.contains(bad) {
            score -= 2;
        }
    }
    if text.chars().filter(|c| c.is_alphabetic()).count() >= 8 {
        score += 1;
    }
    if slot_has_item_name(text) {
        score += 6;
    }
    if !part_roles(text).is_empty() && text.chars().filter(|c| c.is_alphabetic()).count() >= 10 {
        score += 4;
    }
    score
}

fn rank_layout(n: u32, slot_texts: &[String]) -> i32 {
    let scores: Vec<i32> = slot_texts.iter().map(|t| reward_keyword_score(t)).collect();
    let total: i32 = scores.iter().sum();
    let occupied: Vec<usize> = slot_texts
        .iter()
        .enumerate()
        .filter(|(_, t)| slot_is_occupied(t))
        .map(|(i, _)| i)
        .collect();
    let mut rank = total * 10;
    if let (Some(&a), Some(&b)) = (occupied.first(), occupied.last()) {
        let span = (b - a + 1) as i32;
        let occ = occupied.len() as i32;
        if span == occ {
            rank += 6;
        } else {
            rank -= 12;
        }
    }
    let occ_n = occupied.len() as u32;
    if occ_n == n {
        rank += 15;
    }
    // Duo / insufficient-reactant: two real names in a 2-slot layout.
    if occ_n == 2 && n == 2 {
        rank += 35;
    }
    if occ_n == 2 && n >= 3 {
        rank -= 25;
    }
    // Prefer n=3 when two named primes appear (common trio: Forma + 2 parts).
    if occ_n == 3 && n == 3 {
        rank += 40;
    }
    if occ_n == 3 && n == 2 {
        rank -= 30;
    }
    // n=2 crops often glue adjacent cards — hard-penalize merged text.
    let merged = slot_texts.iter().any(|t| looks_like_merged_cards(t));
    if merged {
        rank -= 50;
    }
    if occ_n == 4 && n == 4 {
        rank += 55;
    }
    if n == 3 && merged {
        rank -= 40;
    }
    // Four distinct families across slots → must be a quad layout.
    let families: usize = slot_texts.iter().map(|t| family_hint_count(t)).sum();
    if families >= 4 && n == 4 {
        rank += 30;
    }
    if families >= 4 && n == 3 {
        rank -= 45;
    }
    if n == 4 && occ_n == 2 && occupied == [1, 2] {
        rank += 4;
    }
    // Penalize layouts that claim more slots than named items.
    if n > occ_n && occ_n > 0 {
        rank -= ((n - occ_n) * 8) as i32;
    }
    rank
}

pub fn ocr_reward_screen(capture: &DynamicImage, lang: &str, cache_dir: &Path) -> Result<OcrResult> {
    ocr_reward_screen_engine(capture, lang, cache_dir, true)
}

/// `prefer_rapid`: try RapidOCR (ONNX) first when models are installed.
pub fn ocr_reward_screen_engine(
    capture: &DynamicImage,
    lang: &str,
    cache_dir: &Path,
    prefer_rapid: bool,
) -> Result<OcrResult> {
    std::fs::create_dir_all(cache_dir)?;

    // Prefer RapidOCR (Cyrillic PP-OCRv5 ONNX) — much cleaner on Warframe RU UI.
    if prefer_rapid {
        if let Some(rapid) = crate::rapidocr::try_ocr_reward_screen(capture, cache_dir) {
            let occupied = rapid
                .slot_texts
                .iter()
                .filter(|t| slot_is_occupied(t) || is_forma_text(t))
                .count();
            if occupied >= 2 {
                info!("OCR using RapidOCR ({} occupied slots)", occupied);
                return Ok(rapid);
            }
            info!("RapidOCR weak ({occupied} occupied) — falling back to Tesseract");
        }
    }

    // Always evaluate 4→3→2. Early-exit only on a clean quad — n=3 often
    // looks "decisive" while gluing a 4-card screen into three merged crops.
    let mut best: Option<(i32, Vec<String>, Vec<String>, Vec<DynamicImage>)> = None;
    for n in [4u32, 3, 2] {
        let slots = crop_layout(capture, n);
        let (raw_lines, slot_texts) = ocr_slots_parallel(&slots, lang, cache_dir, n)?;
        let ranked = rank_layout(n, &slot_texts);
        let decisive = layout_is_decisive(n, ranked, &slot_texts);
        let occupied_n = slot_texts.iter().filter(|t| slot_is_occupied(t)).count();
        let has_merge = slot_texts.iter().any(|t| looks_like_merged_cards(t));
        let effective = ranked + if decisive { 40 } else { 0 };
        info!(
            "OCR layout n={n} rank={ranked} eff={effective} decisive={decisive} texts={:?}",
            slot_texts
                .iter()
                .map(|s| s.chars().take(40).collect::<String>())
                .collect::<Vec<_>>()
        );
        let takes = best
            .as_ref()
            .map(|(s, ..)| effective > *s)
            .unwrap_or(true);
        if takes {
            best = Some((effective, raw_lines, slot_texts, slots));
        }
        if takes && decisive && n == 4 {
            info!("OCR early-exit on n=4 (decisive winning layout)");
            break;
        }
        // Soft early-exit: solid n=4 without waiting for decisive threshold.
        // Each extra layout pass costs a full tesseract round (~1.2s).
        if n == 4 && takes && !has_merge && occupied_n >= 3 && ranked >= 200 {
            info!("OCR early-exit on n=4 (good-enough trio/quad, rank={ranked})");
            break;
        }
    }

    // If the default vertical band was empty, try shifted bands once each (UI scale drift).
    let need_alt = best
        .as_ref()
        .map(|(_, _, texts, _)| texts.iter().all(|t| t.trim().is_empty()))
        .unwrap_or(true);
    if need_alt {
        for (bi, &(y0, y1)) in [(0.340f32, 0.470), (0.390, 0.520)].iter().enumerate() {
            for n in [4u32, 3, 2] {
                let slots = crop_layout_band(capture, n, y0, y1);
                let (raw_lines, slot_texts) = ocr_slots_parallel(&slots, lang, cache_dir, n)?;
                let ranked = rank_layout(n, &slot_texts);
                let decisive = layout_is_decisive(n, ranked, &slot_texts);
                let effective = ranked + if decisive { 40 } else { 0 };
                info!(
                    "OCR alt-band{bi} n={n} rank={ranked} eff={effective} texts={:?}",
                    slot_texts
                        .iter()
                        .map(|s| s.chars().take(40).collect::<String>())
                        .collect::<Vec<_>>()
                );
                let takes = best
                    .as_ref()
                    .map(|(s, ..)| effective > *s)
                    .unwrap_or(true);
                if takes {
                    best = Some((effective, raw_lines, slot_texts, slots));
                }
                if takes && decisive && n == 4 {
                    break;
                }
            }
            if best
                .as_ref()
                .map(|(_, _, texts, _)| texts.iter().any(|t| !t.trim().is_empty()))
                .unwrap_or(false)
            {
                break;
            }
        }
    }

    let (_, raw_lines, slot_texts, slots) = best.unwrap_or_else(|| {
        let slots = crop_layout(capture, 2);
        (0, vec![], vec![String::new(); slots.len()], slots)
    });

    for (i, slot) in slots.iter().enumerate() {
        let path = cache_dir.join(format!("reward_slot_{i}.png"));
        let _ = slot.save(&path);
    }

    info!(
        "OCR slot texts: {:?}",
        slot_texts
            .iter()
            .map(|s| s.chars().take(64).collect::<String>())
            .collect::<Vec<_>>()
    );
    Ok(OcrResult {
        raw_lines,
        slot_texts,
    })
}

/// Fast probe: is the reward *name* band already showing item text?
/// Used to start OCR during Wine EE.log delay without waiting for Got rewards.
pub fn reward_text_likely(capture: &DynamicImage, lang: &str, cache_dir: &Path) -> bool {
    let _ = std::fs::create_dir_all(cache_dir);
    let (w, h) = capture.dimensions();
    let top = ((h as f32) * 0.365) as u32;
    let bot = ((h as f32) * 0.495).min(h as f32) as u32;
    let band_h = bot.saturating_sub(top).max(1);
    let x0 = ((w as f32) * 0.10) as u32;
    let bw = ((w as f32) * 0.80) as u32;
    let cropped = capture.crop_imm(x0, top, bw.min(w.saturating_sub(x0)), band_h);
    let scaled = cropped.resize(
        (bw * 2).max(1),
        (band_h * 2).max(1),
        image::imageops::FilterType::CatmullRom,
    );
    let pre = preprocess_for_ocr(&scaled);
    let path = cache_dir.join("reward_text_probe.png");
    if pre.save(&path).is_err() {
        return false;
    }
    let Ok(text) = tesseract_image(&path, lang) else {
        return false;
    };
    let lo = text.to_lowercase();
    let has_kw = ["прайм", "prime", "черт", "blueprint", "форма", "forma", "ствол", "клинок"]
        .iter()
        .any(|k| lo.contains(k));
    let has_name = slot_has_item_name(&text);
    info!(
        "reward text probe: kw={has_kw} name={has_name} text={:?}",
        text.chars().take(80).collect::<String>()
    );
    has_kw && has_name
}

fn layout_is_decisive(n: u32, rank: i32, slot_texts: &[String]) -> bool {
    // Only a clean 4-slot layout may early-exit. n=2/3 frequently look full
    // while still merging neighbors on a quad reward screen.
    if n != 4 {
        return false;
    }
    if slot_texts.iter().any(|t| looks_like_merged_cards(t)) {
        return false;
    }
    let occupied = slot_texts.iter().filter(|t| slot_is_occupied(t)).count() as u32;
    // 3/4 named cards is enough — waiting for a perfect 4th (often Forma OCR junk)
    // costs ~2s on n=3/n=2 passes the user never needs.
    (occupied >= 3 && rank >= 180) || (occupied == 4 && rank >= 150)
}

fn ocr_slots_parallel(
    slots: &[DynamicImage],
    lang: &str,
    cache_dir: &Path,
    n: u32,
) -> Result<(Vec<String>, Vec<String>)> {
    use std::thread;

    // Save crops on this thread, then OCR in parallel (tesseract is the bottleneck).
    let mut paths = Vec::with_capacity(slots.len());
    for (i, slot) in slots.iter().enumerate() {
        let path = cache_dir.join(format!("reward_slot_try{n}_{i}.png"));
        slot.save(&path).context("save slot png")?;
        paths.push(path);
    }

    let lang = lang.to_string();
    let mut handles = Vec::with_capacity(paths.len());
    for path in paths {
        let lang = lang.clone();
        handles.push(thread::spawn(move || {
            let text = tesseract_image(&path, &lang).unwrap_or_default();
            let cleaned = clean_ocr_text(&text);
            (text, cleaned)
        }));
    }

    let mut raw_lines = Vec::with_capacity(handles.len());
    let mut slot_texts = Vec::with_capacity(handles.len());
    for h in handles {
        match h.join() {
            Ok((text, cleaned)) => {
                raw_lines.push(text);
                slot_texts.push(cleaned);
            }
            Err(_) => {
                raw_lines.push(String::new());
                slot_texts.push(String::new());
            }
        }
    }
    Ok((raw_lines, slot_texts))
}

fn looks_like_ui_chrome(line: &str) -> bool {
    let lower = line.to_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let joined = tokens.join(" ");

    // Relic-tier / selection chrome (whole tokens only for short words)
    const TIER: &[&str] = &["нео", "мезо", "акси", "лит", "neo", "meso", "axi", "lith"];
    if tokens.iter().any(|t| TIER.contains(t)) && !joined.contains("прайм") && !joined.contains("prime")
    {
        return true;
    }
    if joined.contains("реликвия") || joined.contains("relic ") || joined.starts_with("relic") {
        if !joined.contains("прайм") && !joined.contains("prime") {
            return true;
        }
    }
    if joined.contains("выбор") && joined.contains("процесс") {
        return true;
    }
    if joined.contains("последни") && joined.contains("наград") {
        return true;
    }
    false
}

fn looks_like_garbage(line: &str) -> bool {
    if looks_like_ui_chrome(line) {
        return true;
    }
    let lower = line.to_lowercase();
    const BAD: &[&str] = &[
        "http",
        "xdg-open",
        "command not found",
        "/usr/",
        "cache/",
        "firefox",
        "chromium",
        "tesseract",
        "pipeline",
        "cargo ",
        ".rs",
        "github",
    ];
    if BAD.iter().any(|b| lower.contains(b)) {
        return true;
    }
    let alpha = line.chars().filter(|c| c.is_alphabetic()).count();
    let digits_sym = line
        .chars()
        .filter(|c| c.is_ascii_digit() || "{}[]<>|/\\@#$%^*=_".contains(*c))
        .count();
    alpha > 0 && digits_sym as f32 / alpha as f32 > 0.45
}

fn looks_like_nickname(line: &str) -> bool {
    let lower = line.to_lowercase();
    // Real EN item lines always carry these; skip nickname heuristic.
    for kw in [
        "prime",
        "blueprint",
        "systems",
        "chassis",
        "neuro",
        "stock",
        "barrel",
        "receiver",
        "blade",
        "handle",
        "hilt",
        "forma",
        "set",
    ] {
        if lower.contains(kw) {
            return false;
        }
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    // Squad tags under cards are a single Latin camelCase token.
    if tokens.len() != 1 {
        return false;
    }
    let alpha: String = tokens[0].chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.chars().count() < 5 {
        return false;
    }
    let cyr = alpha
        .chars()
        .filter(|c| {
            let c = *c;
            ('а'..='я').contains(&c) || ('А'..='Я').contains(&c) || c == 'ё' || c == 'Ё'
        })
        .count();
    let latin = alpha.chars().filter(|c| c.is_ascii_alphabetic()).count();
    latin >= 5 && cyr == 0
}

fn fix_ocr_typos(s: &str) -> String {
    let mut out = s.to_string();
    for (a, b) in [
        ("Праим", "Прайм"),
        ("праим", "прайм"),
        ("ПРАИМ", "ПРАЙМ"),
        ("Руксят", "Рукоять"),
        ("руксят", "рукоять"),
        ("Чертеж", "Чертёж"),
        ("Корин@", "Коринф"),
        ("корин@", "коринф"),
        ("Гавос", "Лавос"),
        ("гавос", "лавос"),
        ("Гавбс", "Лавос"),
        ("гавбс", "лавос"),
        ("Брзитон", "Брэйтон"),
        ("брзитон", "брэйтон"),
        ("Брёйтон", "Брэйтон"),
        ("брёйтон", "брэйтон"),
        ("Формс", "Форма"),
        ("формс", "форма"),
        ("Клино", "Клинок"),
        ("клино", "клинок"),
        ("1арис", "Парис"),
        ("1Арис", "Парис"),
        ("|арис", "Парис"),
        ("|Арис", "Парис"),
    ] {
        out = out.replace(a, b);
    }
    // Tesseract often drops the leading Ч ("ертёж" ← "чертёж"). Never rewrite
    // when "черт…" is already present — that produced "Ччертёж".
    {
        let lo = out.to_lowercase();
        if !lo.contains("черт") {
            out = out
                .replace("ертёж", "чертёж")
                .replace("ертеж", "чертеж")
                .replace("Ертёж", "Чертёж")
                .replace("Ертеж", "Чертёж");
        }
    }
    // Truncated names — only when the full form is not already present
    if out.contains("Акбронк") && !out.contains("Акбронко") {
        out = out.replace("Акбронк", "Акбронко");
    }
    if out.contains("акбронк") && !out.contains("акбронко") {
        out = out.replace("акбронк", "акбронко");
    }
    out
}

fn is_reward_fragment(line: &str) -> bool {
    let lo = line.to_lowercase();
    if lo.contains("прайм")
        || lo.contains("prime")
        || lo.contains("черт")
        || lo.contains("blueprint")
        || lo.contains("форм")
        || lo.contains("forma")
        || lo.contains("систем")
        || lo.contains("нейро")
        || lo.contains("каркас")
        || lo.contains("рукоят")
        || lo.contains("клинок")
        || lo.contains("лезви")
        || lo.contains("приклад")
        || lo.contains("ствол")
        || lo.contains("приём")
        || lo.contains("прием")
        || lo.contains("комплект")
        || lo.contains("forma")
        || lo.contains("форма")
    {
        return true;
    }
    // Long Cyrillic token ≈ item name (Компресса, Перигаль, …)
    lo.split(|c: char| !c.is_alphabetic())
        .any(|t| t.chars().count() >= 5)
}

pub(crate) fn clean_ocr_text(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().count() >= 3)
        .filter(|l| l.chars().filter(|c| c.is_alphabetic()).count() >= 3)
        .filter(|l| !looks_like_garbage(l))
        .filter(|l| !looks_like_nickname(l))
        .map(|l| fix_ocr_typos(l))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    if lines.len() == 1 {
        return lines[0].clone();
    }
    // Join ALL reward-looking lines. Old logic picked a lone "Прайм" and dropped
    // "ертеж: Компресса" because truncated "ертеж" ≠ "чертеж".
    let reward: Vec<&String> = lines.iter().filter(|l| is_reward_fragment(l)).collect();
    if !reward.is_empty() {
        return reward
            .into_iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    lines.join(" ")
}

/// Coarse part-roles present in OCR / catalog text (multi-label for BP+part).
fn part_roles(s: &str) -> Vec<&'static str> {
    let lo = s.to_lowercase();
    let mut roles = Vec::new();
    if lo.contains("чертеж")
        || lo.contains("чертёж")
        || lo.contains("черт") // truncated OCR: "ертеж" after fix, or mid-word
        || lo.contains("blueprint")
        || lo.contains("_blueprint")
    {
        roles.push("blueprint");
    }
    if lo.contains("рукоят") || lo.contains("handle") || lo.contains("hilt") || lo.contains("_handle")
    {
        roles.push("handle");
    }
    if lo.contains("клинок")
        || lo.contains("клино")
        || lo.contains("blade")
        || lo.contains("лезви")
        || lo.contains("_blade")
    {
        roles.push("blade");
    }
    if lo.contains("ствол") || lo.contains("barrel") || lo.contains("_barrel") {
        roles.push("barrel");
    }
    if lo.contains("приклад") || lo.contains("stock") || lo.contains("_stock") {
        roles.push("stock");
    }
    if lo.contains("приём") || lo.contains("прием") || lo.contains("receiver") || lo.contains("_receiver")
    {
        roles.push("receiver");
    }
    if lo.contains("тетив") || lo.contains("string") || lo.contains("_string") {
        roles.push("string");
    }
    if lo.contains("нейро") || lo.contains("neuro") || lo.contains("_neuroptics") {
        roles.push("neuroptics");
    }
    if lo.contains("каркас") || lo.contains("chassis") || lo.contains("_chassis") {
        roles.push("chassis");
    }
    if lo.contains("систем") || lo.contains("systems") || lo.contains("_systems") {
        roles.push("systems");
    }
    if lo.contains("комплект") || lo.ends_with("_set") || lo.contains(" set") {
        roles.push("set");
    }
    roles
}

fn roles_compatible(query: &str, candidate: &str) -> bool {
    let qr = part_roles(query);
    let cr = part_roles(candidate);
    match (qr.is_empty(), cr.is_empty()) {
        (true, true) => true,
        // Incomplete OCR without a role must not pick a specific part (Клыки Пр → blade).
        (true, false) | (false, true) => false,
        (false, false) => qr.iter().any(|r| cr.contains(r)),
    }
}

/// Normalize for matching: drop punctuation, collapse spaces, strip common wrappers.
pub fn normalize_for_match(s: &str) -> String {
    // Replace punctuation with spaces *before* alphanumeric filtering so
    // "Чертёж:Форма" does not collapse into one token "чертёжформа".
    let mut n = s.to_string();
    for (a, b) in [
        ("(", " "),
        (")", " "),
        (":", " "),
        ("—", " "),
        ("–", " "),
        ("_", " "),
        ("'", " "),
        ("`", " "),
        ("·", " "),
        ("•", " "),
        ("×", " "),
        ("х", " "), // Cyrillic 'x' quantity mark sometimes used as ×
    ] {
        // Only replace standalone quantity "х" between digits/spaces — skip for now;
        // the × and colon replacements matter most.
        if a == "х" {
            continue;
        }
        n = n.replace(a, b);
    }
    let mut n = normalize_name(&n);
    // Tesseract routinely loses the diaeresis ("Бёрстон" → "Берстон").
    n = n.replace("ё", "е");
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words shared by every reward name — never distinctive on their own.
const GENERIC_PART_TOKENS: &[&str] = &[
    "прайм",
    "prime",
    "чертеж",
    "чертёж",
    "blueprint",
    "систем",
    "система",
    "нейро",
    "нейрооптика",
    "каркас",
    "комплект",
    "set",
    "оружие",
    "рукоять",
    "рукоятка",
    "handle",
    "hilt",
    "клинок",
    "blade",
    "лезвие",
    "ствол",
    "barrel",
    "приклад",
    "stock",
    "приёмник",
    "приемник",
    "receiver",
    "тетива",
    "string",
];

fn is_generic_part_token(t: &str) -> bool {
    GENERIC_PART_TOKENS
        .iter()
        .any(|g| t.contains(g) || g.contains(t))
}

/// Fuzzy token equality that tolerates OCR dropping leading characters.
fn token_hits(q: &str, n: &str) -> bool {
    jaro_winkler(q, n) > 0.88
        || n.contains(q)
        || q.contains(n)
        // Truncated leading chars: "нко" ↔ "бронко", "ригаль" ↔ "перигаль"
        || {
            let qn = q.chars().count();
            let nn = n.chars().count();
            qn >= 3 && nn >= qn + 2 && (n.ends_with(q) || q.ends_with(n))
        }
}

fn score_name(query: &str, candidate: &str) -> Option<f64> {
    let name = normalize_for_match(candidate);
    if name.is_empty() {
        return None;
    }
    if !roles_compatible(query, &name) && !roles_compatible(query, candidate) {
        return None;
    }

    let is_generic = is_generic_part_token;

    let q_tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .collect();
    let n_tokens: Vec<&str> = name
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .collect();
    if q_tokens.is_empty() || n_tokens.is_empty() {
        return None;
    }

    // Distinctive tokens (not just "Прайм" / "Рукоять").
    // Allow len≥3 when OCR drops the leading syllable ("›нко" ← Бронко).
    let q_distinct: Vec<&str> = {
        let long: Vec<&str> = q_tokens
            .iter()
            .copied()
            .filter(|t| t.chars().count() >= 4 && !is_generic(t))
            .collect();
        if !long.is_empty() {
            long
        } else {
            q_tokens
                .iter()
                .copied()
                .filter(|t| t.chars().count() >= 3 && !is_generic(t))
                .collect()
        }
    };
    if q_distinct.is_empty() {
        // OCR garbage like "5 Прайм" / ": Рукоять" must not match random parts
        return None;
    }

    if name == query {
        return Some(2.0);
    }
    if name.contains(query) || query.contains(&name) {
        // Avoid "бо" / short fragments matching huge names via contains.
        // Also avoid matching "Бронко Прайм: Ствол" inside a *merged* OCR string
        // that also mentions another prime ("ригаль … Бронко …").
        let qn = query.chars().count();
        let nn = name.chars().count();
        if qn >= 6 && nn >= 6 {
            let len_ratio = nn as f64 / qn.max(1) as f64;
            if len_ratio >= 0.78 {
                return Some(jaro_winkler(query, &name) + 0.15);
            }
        }
    }

    let distinct_hits = q_distinct
        .iter()
        .filter(|t| n_tokens.iter().any(|n| token_hits(t, n)))
        .count();
    if distinct_hits == 0 {
        return None;
    }
    // Long unique names (Сарофанг, Перигаль…) survive even when OCR drops "Прайм"
    // and word order differs from the catalog ("Чертёж: X" vs "X Прайм (Чертеж)").
    let strong_name_hit = q_distinct.iter().any(|t| {
        t.chars().count() >= 6
            && n_tokens.iter().any(|n| {
                jaro_winkler(t, n) > 0.93
                    || *n == *t
                    || (n.starts_with(t) && t.chars().count() >= 6)
                    || token_hits(t, n)
            })
    });
    // A near-exact family token ("Брэйтон") means the crop really is this item,
    // so leftover OCR noise ("ГТБ}ёйм", overlay text) must not veto the match.
    let strong_family_hit = q_distinct.iter().any(|t| {
        t.chars().count() >= 5
            && n_tokens
                .iter()
                .any(|n| *n == *t || jaro_winkler(t, n) > 0.90 || n.ends_with(*t))
    });
    // Unexplained long tokens without a solid family anchor: too risky to match.
    let foreign = q_distinct
        .iter()
        .filter(|t| t.chars().count() >= 5 && !n_tokens.iter().any(|n| token_hits(t, n)))
        .count();
    if foreign > 0 && !strong_family_hit {
        return None;
    }

    let hits = q_tokens
        .iter()
        .filter(|t| n_tokens.iter().any(|n| token_hits(t, n) || jaro_winkler(t, n) > 0.9))
        .count();
    let ratio = hits as f64 / q_tokens.len().max(n_tokens.len()) as f64;
    let base = jaro_winkler(query, &name);
    let mut score = (base + ratio * 0.35 + distinct_hits as f64 * 0.08).min(1.35);
    if strong_name_hit && !part_roles(query).is_empty() {
        score = score.max(0.96);
    }
    // Truncated family + matching part role ("нко Прайм: Ствол") is enough.
    if distinct_hits > 0 && !part_roles(query).is_empty() && roles_compatible(query, &name) {
        score = score.max(0.93);
    }
    if score > 0.88 {
        Some(score)
    } else {
        None
    }
}

/// True when OCR text is (quantity ×) Forma blueprint — not a named prime part.
pub(crate) fn is_forma_text(text: &str) -> bool {
    let q = normalize_for_match(text).to_lowercase();
    if !(q.contains("форма") || q.contains("forma")) {
        return false;
    }
    // Reject named variants like "Обтекаемая Форма".
    let toks: Vec<&str> = q
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .collect();
    let extras = toks
        .iter()
        .filter(|t| {
            let t = **t;
            ![
                "форма",
                "forma",
                "форм",
                "чертеж",
                "чертёж",
                "черт",
                "blueprint",
                "прайм",
                "prime",
            ]
            .contains(&t)
                && !t.chars().all(|c| c.is_ascii_digit())
                && t != "икс" // OCR of "X"
        })
        .count();
    extras == 0
}

fn forma_item() -> &'static ItemRow {
    use std::sync::OnceLock;
    static FORMA: OnceLock<ItemRow> = OnceLock::new();
    FORMA.get_or_init(|| ItemRow {
        url_name: "forma".into(),
        name: "Forma".into(),
        name_ru: Some("Форма".into()),
        thumb: None,
        ducats: None,
        set_url_name: None,
        vaulted: None,
        mastery: None,
    })
}

fn forma_sentinel<'a>(_catalog: &'a [ItemRow]) -> Option<&'a ItemRow> {
    // 'static coerces to 'a
    Some(forma_item())
}

pub fn fuzzy_match_item<'a>(query: &str, catalog: &'a [ItemRow]) -> Option<&'a ItemRow> {
    let q = normalize_for_match(&fix_ocr_typos(query));
    // Forma is a common relic reward but is not listed on warframe.market,
    // so it never appears in the catalog. Still surface it in the overlay.
    if is_forma_text(&q) {
        return forma_sentinel(catalog);
    }
    let alpha = q.chars().filter(|c| c.is_alphabetic()).count();
    if q.is_empty() || alpha < 5 || looks_like_garbage(query) {
        return None;
    }

    let q_roles = part_roles(&q);

    let mut best: Option<(&ItemRow, f64)> = None;
    for item in catalog {
        // Prefer part/blueprint rows over whole sets for reward screens
        for cand in [Some(item.name.as_str()), item.name_ru.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(score) = score_name(&q, cand) {
                let mut score = score;
                if item.url_name.ends_with("_set") {
                    score -= 0.08;
                }
                // Truncated OCR "Чертёж: Сарофанг" should prefer the prime BP.
                if q_roles.iter().any(|r| *r == "blueprint")
                    && item.url_name.contains("_prime_")
                    && item.url_name.ends_with("_blueprint")
                {
                    score += 0.06;
                }
                if score >= 2.0 {
                    return Some(item);
                }
                if best.map(|(_, s)| score > s).unwrap_or(true) {
                    best = Some((item, score));
                }
            }
        }
    }
    let (item, _) = best.filter(|(_, s)| *s >= 0.92)?;
    if names_another_family(&q, item, catalog) {
        return None;
    }
    Some(item)
}

/// True when the crop also names a *different* catalog family than `item`, i.e.
/// two cards were glued into one string ("ригаль … Бронко Прайм: Ствол").
/// Pure OCR noise that matches no catalog family is ignored.
fn names_another_family(query: &str, item: &ItemRow, catalog: &[ItemRow]) -> bool {
    let own: Vec<String> = [Some(item.name.as_str()), item.name_ru.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(|n| {
            normalize_for_match(n)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();

    for tok in query.split_whitespace() {
        if tok.chars().count() < 5 || is_generic_part_token(tok) {
            continue;
        }
        if own.iter().any(|n| token_hits(tok, n)) {
            continue;
        }
        let other_family = catalog.iter().any(|other| {
            other.url_name != item.url_name
                && [Some(other.name.as_str()), other.name_ru.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.split_whitespace().next())
                    .any(|first| {
                        let first = first.to_lowercase();
                        first.chars().count() >= 5 && token_hits(tok, &first)
                    })
        });
        if other_family {
            return true;
        }
    }
    false
}

/// Match one or more catalog rows from a slot. If the crop glued two cards
/// together, try each distinctive family token with the shared part role.
pub fn fuzzy_match_items<'a>(query: &str, catalog: &'a [ItemRow]) -> Vec<&'a ItemRow> {
    if let Some(one) = fuzzy_match_item(query, catalog) {
        return vec![one];
    }
    if !looks_like_merged_cards(query) {
        return vec![];
    }
    let q = normalize_for_match(&fix_ocr_typos(query));
    let roles = part_roles(&q);
    let role_hint = roles.first().copied().unwrap_or("");
    let mut out = Vec::new();
    let lo = q.to_lowercase();
    // Pull long tokens and try "<token> <role words from query>" against catalog.
    for tok in lo.split_whitespace() {
        if tok.chars().count() < 5 {
            continue;
        }
        if ["прайм", "prime", "чертеж", "чертёж", "blueprint", "систем", "ствол"].contains(&tok) {
            continue;
        }
        let probe = if role_hint.is_empty() {
            format!("{tok} прайм")
        } else {
            // Rebuild a minimal query: token + original role keywords.
            let mut parts = vec![tok.to_string()];
            for kw in ["прайм", "чертёж", "чертеж", "ствол", "клинок", "систем", "тетива", "приклад", "приемник", "приёмник", "рукоять", "нейро", "каркас"] {
                if lo.contains(kw) {
                    parts.push(kw.to_string());
                }
            }
            parts.join(" ")
        };
        if let Some(item) = fuzzy_match_item(&probe, catalog) {
            if !out.iter().any(|i: &&ItemRow| i.url_name == item.url_name) {
                out.push(item);
            }
        }
    }
    out
}

pub fn display_name_for_item(item: &ItemRow, prefer_ru: bool) -> String {
    if prefer_ru {
        item.name_ru
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| item.name.clone())
    } else {
        item.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_prefers_two_named_slots_over_four_keyword_noise() {
        // Duo screen OCR: two real cards. A 4-way split often invents
        // "Прайм"/"Форма" fragments in empty space.
        let duo = vec![
            "Чертёж: Парис Прайм".to_string(),
            "Пирана Прайм: Ствол".to_string(),
        ];
        let four_noise = vec![
            "Чертёж: Парис Прайм".to_string(),
            "Прайм".to_string(),
            "Пирана Прайм: Ствол".to_string(),
            "Форма".to_string(),
        ];
        assert!(slot_is_occupied(&duo[0]));
        assert!(slot_is_occupied(&duo[1]));
        assert!(!slot_is_occupied("Прайм"));
        assert!(!slot_is_occupied("Форма"));
        assert!(!slot_is_occupied("Чертёж"));
        let r2 = rank_layout(2, &duo);
        let r4 = rank_layout(4, &four_noise);
        assert!(
            r2 > r4,
            "duo layout should beat noisy 4-split: r2={r2} r4={r4}"
        );
        // n=2 is never decisive — we always evaluate n=3 to avoid merged crops.
        assert!(!layout_is_decisive(2, r2, &duo));
        assert!(!layout_is_decisive(4, r4, &four_noise));
    }

    #[test]
    fn fuzzy_matches_truncated_bronco_barrel() {
        let catalog = vec![
            ItemRow {
                url_name: "bronco_prime_barrel".into(),
                name: "Bronco Prime Barrel".into(),
                name_ru: Some("Бронко Прайм: Ствол".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "perigale_prime_barrel".into(),
                name: "Perigale Prime Barrel".into(),
                name_ru: Some("Перигаль Прайм: Ствол".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        assert_eq!(
            fuzzy_match_item("нко Прайм: Ствол", &catalog).map(|i| i.url_name.as_str()),
            Some("bronco_prime_barrel")
        );
    }

    #[test]
    fn fuzzy_rejects_merged_perigale_bronco_crop() {
        let catalog = vec![
            ItemRow {
                url_name: "perigale_prime_barrel".into(),
                name: "Perigale Prime Barrel".into(),
                name_ru: Some("Перигаль Прайм: Ствол".into()),
                thumb: None,
                ducats: Some(100),
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "bronco_prime_barrel".into(),
                name: "Bronco Prime Barrel".into(),
                name_ru: Some("Бронко Прайм: Ствол".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "perigale_prime_blueprint".into(),
                name: "Perigale Prime Blueprint".into(),
                name_ru: Some("Перигаль Прайм (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        // Real failure: n=2 crop merged Perigale BP + Bronco barrel into one string.
        assert!(fuzzy_match_item("ригаль М Бронко Прайм: Ствол —", &catalog).is_none());
        assert_eq!(
            fuzzy_match_item("Бронко Прайм: Ствол", &catalog).map(|i| i.url_name.as_str()),
            Some("bronco_prime_barrel")
        );
        assert_eq!(
            fuzzy_match_item("Чертёж: Перигаль Прайм", &catalog).map(|i| i.url_name.as_str()),
            Some("perigale_prime_blueprint")
        );
    }

    #[test]
    fn clean_picks_longest() {
        let t = "Forma\nAsh Prime Systems Blueprint\n";
        assert!(clean_ocr_text(t).contains("Ash"));
    }

    #[test]
    fn clean_picks_russian() {
        let t = "Стинакс Прайм\nСистема (Чертеж)\n";
        let c = clean_ocr_text(t);
        assert!(c.contains("Стинакс") || c.contains("Система"));
    }

    #[test]
    fn clean_joins_truncated_blueprint_and_prime() {
        // Real crop: Tesseract drops leading Ч and splits name / Прайм across lines.
        let t = "ертеж: Компресса\nПрайм ;\n4 -\n";
        let c = clean_ocr_text(t);
        assert!(
            c.to_lowercase().contains("компресс") && c.to_lowercase().contains("прайм"),
            "expected Kompressa+Prime join, got {c:?}"
        );
        assert!(
            c.to_lowercase().contains("черт"),
            "expected blueprint marker restored, got {c:?}"
        );
    }

    #[test]
    fn clean_rejects_relic_chrome() {
        let t = "Реликвия Нео\nВыбор В Процессе";
        assert!(clean_ocr_text(t).is_empty());
    }

    #[test]
    fn fuzzy_matches_russian_name() {
        let catalog = vec![ItemRow {
            url_name: "styanax_prime_systems_blueprint".into(),
            name: "Styanax Prime Systems Blueprint".into(),
            name_ru: Some("Стинакс Прайм: Система (Чертеж)".into()),
            thumb: None,
            ducats: None,
            set_url_name: None,
            vaulted: None,
            mastery: None,
        }];
        let m = fuzzy_match_item("Стинакс Прайм Система Чертеж", &catalog);
        assert!(m.is_some());
        assert_eq!(m.unwrap().url_name, "styanax_prime_systems_blueprint");
    }

    #[test]
    fn fuzzy_rejects_prime_only_garbage() {
        let catalog = vec![
            ItemRow {
                url_name: "bo_prime_blueprint".into(),
                name: "Bo Prime Blueprint".into(),
                name_ru: Some("Бо Прайм (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "ash_prime_blueprint".into(),
                name: "Ash Prime Blueprint".into(),
                name_ru: Some("Эш Прайм (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        assert!(fuzzy_match_item("5 Прайм", &catalog).is_none());
        assert!(fuzzy_match_item("Эс Прайм", &catalog).is_none());
        assert!(fuzzy_match_item("Рорма", &catalog).is_none());
    }

    #[test]
    fn fuzzy_matches_noisy_ocr() {
        let catalog = vec![ItemRow {
            url_name: "styanax_prime_systems_blueprint".into(),
            name: "Styanax Prime Systems Blueprint".into(),
            name_ru: Some("Стинакс Прайм: Система (Чертеж)".into()),
            thumb: None,
            ducats: None,
            set_url_name: None,
            vaulted: None,
            mastery: None,
        }];
        let m = fuzzy_match_item("иикю Прайм Стинакс Прайм", &catalog);
        // No part role → must not invent Systems Blueprint
        assert!(m.is_none());
    }

    #[test]
    fn fuzzy_rejects_role_only_and_name_without_role() {
        let catalog = vec![
            ItemRow {
                url_name: "fang_prime_handle".into(),
                name: "Fang Prime Handle".into(),
                name_ru: Some("Клыки Прайм: Рукоять".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "fang_prime_blade".into(),
                name: "Fang Prime Blade".into(),
                name_ru: Some("Клыки Прайм: Клинок".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "sheev_hilt".into(),
                name: "Sheev Hilt".into(),
                name_ru: Some("Шив: Рукоять".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "corinth_prime_blueprint".into(),
                name: "Corinth Prime Blueprint".into(),
                name_ru: Some("Коринф Прайм (Чертёж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        assert!(fuzzy_match_item(": Рукоять", &catalog).is_none());
        assert!(fuzzy_match_item("Клыки Пр", &catalog).is_none());
        let m = fuzzy_match_item("Клыки Прайм: Рукоять", &catalog);
        assert_eq!(m.map(|i| i.url_name.as_str()), Some("fang_prime_handle"));
        let m = fuzzy_match_item("Чертёж: Коринф Прайм", &catalog);
        assert_eq!(m.map(|i| i.url_name.as_str()), Some("corinth_prime_blueprint"));
    }

    #[test]
    fn fuzzy_matches_truncated_blueprint_name() {
        let catalog = vec![
            ItemRow {
                url_name: "sarofang_prime_blueprint".into(),
                name: "Sarofang Prime Blueprint".into(),
                name_ru: Some("Сарофанг Прайм (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "sarofang_blade".into(),
                name: "Sarofang Blade".into(),
                name_ru: Some("Сарофанг: Лезвие".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "braton_prime_blueprint".into(),
                name: "Braton Prime Blueprint".into(),
                name_ru: Some("Брэйтон Прайм (Чертёж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        // OCR often drops trailing "Прайм" on blueprint cards
        let m = fuzzy_match_item("Чертёж: Сарофанг", &catalog);
        assert_eq!(m.map(|i| i.url_name.as_str()), Some("sarofang_prime_blueprint"));
        let m = fuzzy_match_item("Чертёж: Брэйтон Прайм", &catalog);
        assert_eq!(m.map(|i| i.url_name.as_str()), Some("braton_prime_blueprint"));
    }

    #[test]
    fn fuzzy_matches_braton_stock_and_lavos_chassis() {
        let catalog = vec![
            ItemRow {
                url_name: "braton_prime_stock".into(),
                name: "Braton Prime Stock".into(),
                name_ru: Some("Брэйтон Прайм: Приклад".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "lavos_prime_chassis_blueprint".into(),
                name: "Lavos Prime Chassis Blueprint".into(),
                name_ru: Some("Лавос Прайм: Каркас (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        let m = fuzzy_match_item("Брэйтон Прайм: Приклад", &catalog);
        assert_eq!(m.map(|i| i.url_name.as_str()), Some("braton_prime_stock"));
        let m = fuzzy_match_item("Чертёж: Гавос Прайм: Каркас", &catalog);
        assert_eq!(
            m.map(|i| i.url_name.as_str()),
            Some("lavos_prime_chassis_blueprint")
        );
    }
}

#[cfg(test)]
mod live_probe {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn probe_last_capture() {
        let path = PathBuf::from("/home/bng/.cache/bngframe/last_capture.png");
        if !path.exists() {
            return;
        }
        let img = image::open(&path).expect("open capture");
        let cache = PathBuf::from("/tmp/bng_ocr_probe_rs");
        let res = ocr_reward_screen(&img, "rus+eng", &cache).expect("ocr");
        eprintln!("slots={:?}", res.slot_texts);
        let catalog = vec![
            ItemRow {
                url_name: "fang_prime_handle".into(),
                name: "Fang Prime Handle".into(),
                name_ru: Some("Клыки Прайм: Рукоять".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "fang_prime_blade".into(),
                name: "Fang Prime Blade".into(),
                name_ru: Some("Клыки Прайм: Клинок".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "sheev_hilt".into(),
                name: "Sheev Hilt".into(),
                name_ru: Some("Шив: Рукоять".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "corinth_prime_blueprint".into(),
                name: "Corinth Prime Blueprint".into(),
                name_ru: Some("Коринф Прайм (Чертёж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "braton_prime_stock".into(),
                name: "Braton Prime Stock".into(),
                name_ru: Some("Брэйтон Прайм: Приклад".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
            ItemRow {
                url_name: "lavos_prime_chassis_blueprint".into(),
                name: "Lavos Prime Chassis Blueprint".into(),
                name_ru: Some("Лавос Прайм: Каркас (Чертеж)".into()),
                thumb: None,
                ducats: None,
                set_url_name: None,
                vaulted: None,
                mastery: None,
            },
        ];
        for t in &res.slot_texts {
            let m = fuzzy_match_item(t, &catalog);
            eprintln!("match {t:?} → {:?}", m.map(|i| i.url_name.as_str()));
        }
        let joined = res.slot_texts.join(" ").to_lowercase();
        // last_capture.png is overwritten by every capture — skip assert if it's
        // not a reward screen anymore (relic select, orbiter, etc.).
        if !(joined.contains("прайм")
            || joined.contains("приклад")
            || joined.contains("каркас")
            || joined.contains("черт"))
        {
            eprintln!("skip assert: last_capture is not a reward screen");
            return;
        }
        // The fixture catalog only knows a handful of rows, so assert per name that
        // is actually on screen. Rewards outside the fixture stay unmatched here.
        let matched: Vec<_> = res
            .slot_texts
            .iter()
            .filter_map(|t| fuzzy_match_item(t, &catalog).map(|i| i.url_name.as_str()))
            .collect();
        for (hint, expected) in [
            ("брэйтон", "braton_prime_stock"),
            ("братон", "braton_prime_stock"),
            ("лавос", "lavos_prime_chassis_blueprint"),
            ("гавос", "lavos_prime_chassis_blueprint"),
        ] {
            if joined.contains(hint) {
                assert!(
                    matched.contains(&expected),
                    "screen shows {hint:?} but {expected} unmatched: {matched:?} from {:?}",
                    res.slot_texts
                );
            }
        }
    }
}

