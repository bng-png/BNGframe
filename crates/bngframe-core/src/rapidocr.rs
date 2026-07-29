//! RapidOCR (PaddleOCR ONNX) backend for reward-screen text.
//!
//! Uses PP-OCRv5 mobile detector + Cyrillic recognition models under
//! `~/.local/share/bngframe/rapidocr/`. Falls back to Tesseract when models
//! are missing or inference fails.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use image::DynamicImage;
use rapidocr_core::config::{
    ClsConfig, DetConfig, DetInputLimits, InferenceOptions, LimitType, PipelineConfig,
    RapidOcrConfig, RecConfig,
};
use rapidocr_core::{types::OcrLine, RapidOcr};
use tracing::{info, warn};

use crate::ocr::{clean_ocr_text, OcrResult};

static ENGINE: OnceLock<Mutex<Option<RapidOcr>>> = OnceLock::new();

fn engine_slot() -> &'static Mutex<Option<RapidOcr>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

fn model_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bngframe")
        .join("rapidocr")
}

/// True when the Cyrillic RapidOCR model pack is present on disk.
pub fn models_available() -> bool {
    let dir = model_dir();
    [
        "ch_PP-OCRv5_det_mobile.onnx",
        "cyrillic_PP-OCRv5_rec_mobile.onnx",
        "ppocrv5_cyrillic_dict.txt",
    ]
    .iter()
    .all(|f| dir.join(f).is_file())
}

fn build_config(dir: &Path) -> RapidOcrConfig {
    RapidOcrConfig {
        // Det + rec only — orientation classifier not needed for WF UI.
        pipeline: PipelineConfig {
            use_det: true,
            use_cls: false,
            use_rec: true,
        },
        inference: InferenceOptions {
            intra_threads: 4,
            inter_threads: 1,
            ..Default::default()
        },
        text_score: 0.45,
        min_side_len: 30,
        max_side_len: 1920,
        min_height: 30,
        width_height_ratio: 8.0,
        det: Some(DetConfig {
            model_path: dir.join("ch_PP-OCRv5_det_mobile.onnx"),
            limit_side_len: 960,
            limit_type: LimitType::Max,
            input_limits: DetInputLimits::default(),
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
            thresh: 0.3,
            box_thresh: 0.5,
            max_candidates: 1000,
            unclip_ratio: 1.6,
            min_size: 3,
        }),
        cls: Some(ClsConfig {
            model_path: dir.join("ch_ppocr_mobile_v2.0_cls_mobile.onnx"),
            image_shape: [3, 48, 192],
            batch_size: 6,
            thresh: 0.9,
            labels: vec!["0".into(), "180".into()],
        }),
        rec: Some(RecConfig {
            model_path: dir.join("cyrillic_PP-OCRv5_rec_mobile.onnx"),
            dict_path: dir.join("ppocrv5_cyrillic_dict.txt"),
            image_shape: [3, 48, 320],
            batch_size: 6,
        }),
    }
}

fn with_engine<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&mut RapidOcr) -> Result<T>,
{
    let mut guard = engine_slot()
        .lock()
        .map_err(|_| anyhow::anyhow!("rapidocr engine lock poisoned"))?;
    if guard.is_none() {
        if !models_available() {
            bail!("rapidocr models missing under {}", model_dir().display());
        }
        let cfg = build_config(&model_dir());
        let eng = RapidOcr::new(cfg).context("init RapidOCR")?;
        info!(
            "RapidOCR ready (cyrillic PP-OCRv5) from {}",
            model_dir().display()
        );
        *guard = Some(eng);
    }
    f(guard.as_mut().expect("engine just initialized"))
}

fn line_cx(line: &OcrLine) -> f32 {
    let pts = &line.bbox.points;
    (pts[0][0] + pts[1][0] + pts[2][0] + pts[3][0]) * 0.25
}

fn line_cy(line: &OcrLine) -> f32 {
    let pts = &line.bbox.points;
    (pts[0][1] + pts[1][1] + pts[2][1] + pts[3][1]) * 0.25
}

fn looks_rewardish(text: &str) -> bool {
    let lo = text.to_lowercase();
    [
        "черт",
        "прайм",
        "prime",
        "форм",
        "forma",
        "ствол",
        "приклад",
        "приём",
        "прием",
        "клинок",
        "каркас",
        "систем",
        "нейро",
        "рукоят",
        "тетив",
        "blueprint",
        "barrel",
        "stock",
        "receiver",
        "blade",
        "chassis",
    ]
    .iter()
    .any(|kw| lo.contains(kw))
}

/// Map detected lines into up to 4 reward-card slots by horizontal position.
fn lines_to_slots(lines: &[OcrLine], img_w: u32, img_h: u32) -> Vec<String> {
    let h = img_h as f32;
    let w = img_w as f32;
    let y_lo = h * 0.30;
    let y_hi = h * 0.82;

    let mut reward_lines: Vec<&OcrLine> = lines
        .iter()
        .filter(|l| {
            let t = l.text.trim();
            !t.is_empty()
                && l.score >= 0.45
                && looks_rewardish(t)
                && {
                    let cy = line_cy(l);
                    cy >= y_lo && cy <= y_hi
                }
        })
        .collect();

    if reward_lines.is_empty() {
        reward_lines = lines
            .iter()
            .filter(|l| !l.text.trim().is_empty() && l.score >= 0.5 && looks_rewardish(&l.text))
            .collect();
    }

    if reward_lines.is_empty() {
        return vec![String::new(); 4];
    }

    // Split into name anchors vs role-only fragments ("Приёмник", "Каркас").
    // Role-only lines share a card with an anchor above/below them but can sit
    // slightly off-center — forcing a fixed 4-column grid breaks 3-card squads.
    let mut anchors: Vec<&OcrLine> = Vec::new();
    let mut roles: Vec<&OcrLine> = Vec::new();
    for line in reward_lines {
        if is_role_only_line(&line.text) {
            roles.push(line);
        } else {
            anchors.push(line);
        }
    }
    if anchors.is_empty() {
        anchors = roles;
        roles = Vec::new();
    }

    anchors.sort_by(|a, b| {
        line_cx(a)
            .partial_cmp(&line_cx(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Gap-cluster anchors into 2–4 cards.
    let gap = w * 0.10;
    let mut clusters: Vec<Vec<&OcrLine>> = Vec::new();
    for line in anchors {
        if let Some(last) = clusters.last_mut() {
            let prev = line_cx(*last.last().unwrap());
            if (line_cx(line) - prev).abs() < gap {
                last.push(line);
                continue;
            }
        }
        clusters.push(vec![line]);
    }

    // Attach orphan role lines to the nearest card by X.
    for role in roles {
        let rcx = line_cx(role);
        let mut best_i = 0usize;
        let mut best_d = f32::MAX;
        for (i, cluster) in clusters.iter().enumerate() {
            let ccx = cluster.iter().map(|l| line_cx(l)).sum::<f32>() / cluster.len() as f32;
            let d = (rcx - ccx).abs();
            if d < best_d {
                best_d = d;
                best_i = i;
            }
        }
        if best_d < w * 0.20 {
            clusters[best_i].push(role);
        }
    }

    let mut slot_texts: Vec<String> = clusters
        .into_iter()
        .take(4)
        .map(|mut parts| {
            parts.sort_by(|a, b| {
                line_cy(a)
                    .partial_cmp(&line_cy(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let joined = parts
                .iter()
                .map(|l| l.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            // Drop glued player nicknames ("… Дайкю Прайм sigimorPrime").
            strip_trailing_nickname(clean_ocr_text(&joined))
        })
        .filter(|s| !s.trim().is_empty())
        .collect();

    while slot_texts.len() < 4 {
        slot_texts.push(String::new());
    }
    slot_texts.truncate(4);
    slot_texts
}

fn is_role_only_line(text: &str) -> bool {
    let q = text.to_lowercase();
    let has_role = [
        "систем", "нейро", "каркас", "ствол", "приклад", "приём", "прием",
        "клинок", "рукоят", "тетив", "chassis", "systems", "receiver", "barrel",
        "stock", "blade", "handle",
    ]
    .iter()
    .any(|kw| q.contains(kw));
    if !has_role {
        return false;
    }
    // Has a real item family / blueprint word → not role-only.
    let has_name = [
        "черт", "blueprint", "прайм", "prime", "форм", "forma",
    ]
    .iter()
    .any(|kw| q.contains(kw));
    // Long non-role token usually means a family name is present.
    let long_extra = q
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| {
            let n = t.chars().count();
            n >= 5
                && ![
                    "систем", "система", "нейро", "каркас", "ствол", "приклад",
                    "приемник", "приёмник", "клинок", "рукоять", "тетива",
                    "chassis", "systems", "receiver", "barrel", "stock", "blade",
                ]
                .contains(t)
        })
        .count();
    has_role && !has_name && long_extra == 0
}

fn strip_trailing_nickname(text: String) -> String {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 2 {
        return text;
    }
    let last = *parts.last().unwrap();
    let alpha: String = last.chars().filter(|c| c.is_alphabetic()).collect();
    let cyr = alpha
        .chars()
        .filter(|c| {
            let c = *c;
            ('а'..='я').contains(&c) || ('А'..='Я').contains(&c) || c == 'ё' || c == 'Ё'
        })
        .count();
    let latin = alpha.chars().filter(|c| c.is_ascii_alphabetic()).count();
    // Latin camelCase / gamertag glued after a Cyrillic reward name.
    if latin >= 5 && cyr == 0 {
        return parts[..parts.len() - 1].join(" ");
    }
    text
}

/// Run RapidOCR on a reward-screen capture. Prefer this over Tesseract for RU UI.
pub fn ocr_reward_screen_rapid(capture: &DynamicImage, cache_dir: &Path) -> Result<OcrResult> {
    std::fs::create_dir_all(cache_dir)?;
    let path = cache_dir.join("rapid_capture.png");
    // Crop to lower 65% — cuts chrome/FPS overlay noise and speeds det.
    let (w, h) = (capture.width(), capture.height());
    let top = (h as f32 * 0.35) as u32;
    let cropped = capture.crop_imm(0, top, w, h.saturating_sub(top));
    cropped
        .save(&path)
        .with_context(|| format!("save {}", path.display()))?;

    let t0 = Instant::now();
    let output = with_engine(|eng| eng.run_path(&path))?;
    // Remap Y so clustering uses full-frame coordinates.
    let mut lines = output.lines;
    for line in &mut lines {
        for p in &mut line.bbox.points {
            p[1] += top as f32;
        }
    }

    let raw_lines: Vec<String> = lines
        .iter()
        .map(|l| format!("{} ({:.2})", l.text, l.score))
        .collect();
    let slot_texts = lines_to_slots(&lines, w, h);
    info!(
        "RapidOCR done in {:?} — {} lines → slots {:?}",
        t0.elapsed(),
        raw_lines.len(),
        slot_texts
            .iter()
            .map(|s| s.chars().take(48).collect::<String>())
            .collect::<Vec<_>>()
    );
    Ok(OcrResult {
        raw_lines,
        slot_texts,
    })
}

/// Soft availability check used by the pipeline (never panics).
pub fn try_ocr_reward_screen(capture: &DynamicImage, cache_dir: &Path) -> Option<OcrResult> {
    if !models_available() {
        return None;
    }
    match ocr_reward_screen_rapid(capture, cache_dir) {
        Ok(r) if r.slot_texts.iter().any(|t| !t.trim().is_empty()) => Some(r),
        Ok(_) => {
            warn!("RapidOCR returned empty slots");
            None
        }
        Err(e) => {
            warn!("RapidOCR failed: {e:#}");
            None
        }
    }
}
