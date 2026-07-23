use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::{DynamicImage, GenericImageView, RgbaImage};
use strsim::jaro_winkler;
use tracing::{debug, info};

use crate::db::ItemRow;
use crate::pricing::normalize_name;

#[derive(Debug, Clone)]
pub struct OcrResult {
    pub raw_lines: Vec<String>,
    pub slot_texts: Vec<String>,
}

/// Run tesseract on an image path. Returns raw text.
pub fn tesseract_image(path: &Path, lang: &str) -> Result<String> {
    let output = Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(lang)
        .arg("--psm")
        .arg("6")
        .output()
        .context("spawn tesseract (is it installed?)")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("tesseract failed: {err}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Isolate bright UI text (Warframe reward cards) for better OCR.
pub fn preprocess_for_ocr(img: &DynamicImage) -> DynamicImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let lum = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8;
        // Keep bright/gold-ish UI glyphs, drop dark backgrounds
        let keep = lum > 160 || (r > 180 && g > 140 && b < 120);
        let v = if keep { 0 } else { 255 };
        out.put_pixel(x, y, image::Rgba([v, v, v, a]));
    }
    DynamicImage::ImageRgba8(out)
}

/// Crop four horizontal reward slots from a fullscreen capture.
pub fn crop_reward_slots(img: &DynamicImage) -> Vec<DynamicImage> {
    let (w, h) = img.dimensions();
    // Reward cards sit in the middle band of the screen
    let top = (h as f32 * 0.28) as u32;
    let bottom = (h as f32 * 0.72) as u32;
    let band_h = bottom.saturating_sub(top).max(1);
    let slot_w = w / 4;
    let mut slots = Vec::with_capacity(4);
    for i in 0..4u32 {
        let x = i * slot_w;
        let cropped = img.crop_imm(x, top, slot_w, band_h);
        slots.push(preprocess_for_ocr(&cropped));
    }
    slots
}

pub fn ocr_reward_screen(capture: &DynamicImage, lang: &str, cache_dir: &Path) -> Result<OcrResult> {
    std::fs::create_dir_all(cache_dir)?;
    let slots = crop_reward_slots(capture);
    let mut slot_texts = Vec::new();
    let mut raw_lines = Vec::new();

    for (i, slot) in slots.iter().enumerate() {
        let path = cache_dir.join(format!("reward_slot_{i}.png"));
        slot.save(&path).context("save slot png")?;
        let text = tesseract_image(&path, lang).unwrap_or_default();
        debug!("OCR slot {i}: {text:?}");
        let cleaned = clean_ocr_text(&text);
        raw_lines.push(text);
        slot_texts.push(cleaned);
    }

    // Also try full-band OCR as fallback hints
    info!("OCR produced {} slot texts", slot_texts.len());
    Ok(OcrResult {
        raw_lines,
        slot_texts,
    })
}

fn clean_ocr_text(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.len() >= 3)
        .filter(|l| {
            let alpha = l.chars().filter(|c| c.is_alphabetic()).count();
            alpha >= 3
        })
        .collect();
    // Prefer the longest plausible item line
    lines
        .into_iter()
        .max_by_key(|l| l.len())
        .unwrap_or("")
        .to_string()
}

pub fn fuzzy_match_item<'a>(query: &str, catalog: &'a [ItemRow]) -> Option<&'a ItemRow> {
    let q = normalize_name(query);
    if q.is_empty() {
        return None;
    }

    let mut best: Option<(&ItemRow, f64)> = None;
    for item in catalog {
        let name = normalize_name(&item.name);
        if name == q {
            return Some(item);
        }
        if name.contains(&q) || q.contains(&name) {
            let score = jaro_winkler(&q, &name) + 0.15;
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((item, score));
            }
            continue;
        }
        let score = jaro_winkler(&q, &name);
        if score > 0.88 && best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((item, score));
        }
    }
    best.filter(|(_, s)| *s >= 0.88).map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_picks_longest() {
        let t = "Forma\nAsh Prime Systems Blueprint\n";
        assert!(clean_ocr_text(t).contains("Ash"));
    }
}
