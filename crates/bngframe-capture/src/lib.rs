//! Screen capture for Wayland via `grim` (works on Hyprland/Sway and many wlroots compositors).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::DynamicImage;
use tracing::{info, warn};

pub use image::DynamicImage as CapturedImage;

#[derive(Debug, Clone)]
pub struct CaptureRegion {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl CaptureRegion {
    pub fn grim_geometry(&self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.w, self.h)
    }
}

pub fn capture_monitor(monitor: Option<&str>, out_path: &Path) -> Result<DynamicImage> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut cmd = Command::new("grim");
    if let Some(m) = monitor {
        cmd.arg("-o").arg(m);
    }
    cmd.arg(out_path);

    let output = cmd
        .output()
        .context("spawn grim (install grim for Wayland capture)")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        // Fallback: try without -o
        if monitor.is_some() {
            warn!("grim -o failed ({err}), retrying without output name");
            let output2 = Command::new("grim")
                .arg(out_path)
                .output()
                .context("grim fallback")?;
            if !output2.status.success() {
                bail!(
                    "grim failed: {}",
                    String::from_utf8_lossy(&output2.stderr)
                );
            }
        } else {
            bail!("grim failed: {err}");
        }
    }

    info!("Captured screen to {}", out_path.display());
    let img = image::open(out_path).context("open capture")?;
    Ok(img)
}

pub fn capture_region(region: &CaptureRegion, out_path: &Path) -> Result<DynamicImage> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let geom = region.grim_geometry();
    let output = Command::new("grim")
        .args(["-g", &geom])
        .arg(out_path)
        .output()
        .context("spawn grim -g")?;
    if !output.status.success() {
        bail!(
            "grim -g {geom} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    info!(
        "Captured region {geom} to {}",
        out_path.display()
    );
    image::open(out_path).context("open capture")
}

/// Prefer the real Warframe game window on Hyprland; fall back to monitor/full capture.
pub fn capture_for_rewards(monitor: Option<&str>, out_path: &Path) -> Result<DynamicImage> {
    if let Some(region) = find_warframe_region() {
        let on_ws = warframe_is_on_active_workspace();
        if !on_ws {
            // Still try the window region — if the user briefly alt-tabbed, grim may
            // fail or grab the wrong pixels; monitor fallback runs below.
            warn!("Warframe not on active workspace; attempting window capture anyway");
        }
        match capture_region(&region, out_path) {
            Ok(img) => {
                if on_ws || looks_like_reward_ui(&img) {
                    return Ok(img);
                }
                warn!("Off-workspace capture does not look like reward UI; falling back");
            }
            Err(e) => warn!("Warframe window capture failed ({e}), falling back to monitor"),
        }
    } else {
        warn!("Warframe window not found; capturing monitor/full screen");
    }
    capture_monitor(monitor, out_path)
}

fn is_ui_bright(p: [u8; 4]) -> bool {
    let lum = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
    lum > 140.0 || (p[0] > 160 && p[1] > 120 && p[2] < 140)
}

fn is_mission_panel_blue(p: [u8; 4]) -> bool {
    // End-of-mission stats panel is a large dark-blue slab — not relic choice.
    let [r, g, b, _] = p;
    b > r.saturating_add(15) && b > g.saturating_add(10) && (40..180).contains(&b) && r < 120
}

/// Score how much a frame looks like ProjectionRewardChoice (higher = better).
/// Mission-success / loading screens score near zero.
pub fn reward_ui_score(img: &DynamicImage) -> i32 {
    let (w, h) = (img.width(), img.height());
    if w < 800 || h < 600 {
        return 0;
    }
    let rgba = img.to_rgba8();

    // Relic cards: bright label chrome in a mid horizontal band.
    let y0 = (h as f32 * 0.36) as u32;
    let y1 = (h as f32 * 0.50) as u32;
    let x0 = (w as f32 * 0.12) as u32;
    let x1 = (w as f32 * 0.88) as u32;
    const COLS: usize = 32;
    let mut col_bright = [0.0f32; COLS];
    let mut col_n = [0u32; COLS];
    let mut band_bright = 0u64;
    let mut band_n = 0u64;
    for y in (y0..y1).step_by(3) {
        for x in (x0..x1).step_by(3) {
            let p = rgba.get_pixel(x, y).0;
            let ci = ((x - x0) as usize * COLS / (x1 - x0).max(1) as usize).min(COLS - 1);
            col_n[ci] += 1;
            band_n += 1;
            if is_ui_bright(p) {
                col_bright[ci] += 1.0;
                band_bright += 1;
            }
        }
    }
    if band_n == 0 {
        return 0;
    }
    for i in 0..COLS {
        if col_n[i] > 0 {
            col_bright[i] /= col_n[i] as f32;
        }
    }
    let band_frac = band_bright as f32 / band_n as f32;

    // Mission-success lower panel: large blue slab in the *center*, not the
    // whole lower half (relic screens often sit on blue-lit tiles/ships and
    // would otherwise score 0).
    let yb0 = (h as f32 * 0.58) as u32;
    let yb1 = (h as f32 * 0.92) as u32;
    let xb0 = (w as f32 * 0.22) as u32;
    let xb1 = (w as f32 * 0.78) as u32;
    let mut blue = 0u64;
    let mut bn = 0u64;
    for y in (yb0..yb1).step_by(6) {
        for x in (xb0..xb1).step_by(8) {
            let p = rgba.get_pixel(x, y).0;
            if is_mission_panel_blue(p) {
                blue += 1;
            }
            bn += 1;
        }
    }
    let blue_frac = if bn > 0 { blue as f32 / bn as f32 } else { 0.0 };

    // Count separated bright "card" peaks (duo/trio/quad), not a solid grid.
    let mut peaks = 0i32;
    let mut in_peak = false;
    for &v in &col_bright {
        // Thin Cyrillic glyphs are dim after downsample — keep threshold low.
        if v > 0.055 {
            if !in_peak {
                peaks += 1;
                in_peak = true;
            }
        } else if v < 0.025 {
            in_peak = false;
        }
    }

    // Solid mission-success loot panel: very blue *and* no card peaks.
    if blue_frac > 0.88 && peaks <= 1 {
        return 0;
    }
    if blue_frac > 0.75 && peaks > 5 {
        return 0;
    }
    if band_frac < 0.012 {
        return 0;
    }
    // Relic choice has a handful of card columns, not a dense loot grid.
    if peaks == 0 || peaks > 6 {
        return 0;
    }

    let mut score = (band_frac * 400.0) as i32;
    score += match peaks {
        2 => 40,
        3 => 35,
        4 => 45,
        1 => 10,
        _ => 15,
    };
    // Mild blue penalty only — scene lighting must not zero a real screen.
    score -= (blue_frac * 25.0) as i32;
    score.max(1)
}

/// Heuristic: ProjectionRewardChoice UI (not mission-success loot grid).
pub fn looks_like_reward_ui(img: &DynamicImage) -> bool {
    reward_ui_score(img) >= 20
}

/// True when a Warframe client sits on a workspace that is currently visible.
/// Wayland `grim` captures on-screen pixels — a fullscreen window on another
/// workspace still reports geometry, but the capture would be the wrong app.
pub fn warframe_is_on_active_workspace() -> bool {
    let Ok(clients_out) = Command::new("hyprctl").args(["-j", "clients"]).output() else {
        return false;
    };
    if !clients_out.status.success() {
        return false;
    }
    let Ok(monitors_out) = Command::new("hyprctl").args(["-j", "monitors"]).output() else {
        return false;
    };
    if !monitors_out.status.success() {
        return false;
    }

    let Ok(clients) = serde_json::from_slice::<serde_json::Value>(&clients_out.stdout) else {
        return false;
    };
    let Ok(monitors) = serde_json::from_slice::<serde_json::Value>(&monitors_out.stdout) else {
        return false;
    };

    let mut active_ws: Vec<i64> = Vec::new();
    if let Some(arr) = monitors.as_array() {
        for m in arr {
            if let Some(id) = m
                .get("activeWorkspace")
                .and_then(|w| w.get("id"))
                .and_then(|v| v.as_i64())
            {
                active_ws.push(id);
            }
        }
    }
    if active_ws.is_empty() {
        return false;
    }

    let Some(arr) = clients.as_array() else {
        return false;
    };
    for c in arr {
        let class = c.get("class").and_then(|v| v.as_str()).unwrap_or("");
        let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let initial = c
            .get("initialClass")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_wf = class == "steam_app_230410"
            || initial == "steam_app_230410"
            || title.eq_ignore_ascii_case("Warframe");
        if !is_wf {
            continue;
        }
        let title_l = title.to_lowercase();
        if title_l.contains("aleca") || title_l.contains("overwolf") {
            continue;
        }
        let ws = c
            .get("workspace")
            .and_then(|w| w.get("id"))
            .and_then(|v| v.as_i64());
        if let Some(ws) = ws {
            if active_ws.contains(&ws) {
                return true;
            }
        }
    }
    false
}

/// Locate the primary Warframe client window via `hyprctl -j clients`.
pub fn find_warframe_region() -> Option<CaptureRegion> {
    let output = Command::new("hyprctl").args(["-j", "clients"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let clients: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let arr = clients.as_array()?;

    let mut best: Option<(i64, CaptureRegion)> = None;
    for c in arr {
        let class = c.get("class").and_then(|v| v.as_str()).unwrap_or("");
        let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let initial = c
            .get("initialClass")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_wf = class == "steam_app_230410"
            || initial == "steam_app_230410"
            || title.eq_ignore_ascii_case("Warframe");
        if !is_wf {
            continue;
        }
        // Skip Overwolf / AlecaFrame companion windows sharing the same class
        let title_l = title.to_lowercase();
        if title_l.contains("aleca") || title_l.contains("overwolf") {
            continue;
        }

        let at = c.get("at")?.as_array()?;
        let size = c.get("size")?.as_array()?;
        let x = at.first()?.as_i64()? as i32;
        let y = at.get(1)?.as_i64()? as i32;
        let w = size.first()?.as_i64()? as u32;
        let h = size.get(1)?.as_i64()? as u32;
        if w < 800 || h < 600 {
            continue;
        }
        // Off-screen dummy windows
        if x < -5000 || y < -5000 {
            continue;
        }

        let mut score = (w as i64) * (h as i64);
        if title.eq_ignore_ascii_case("Warframe") {
            score += 10_000_000;
        }
        if c.get("fullscreen").and_then(|v| v.as_i64()).unwrap_or(0) > 0 {
            score += 1_000_000;
        }
        let region = CaptureRegion { x, y, w, h };
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, region));
        }
    }
    best.map(|(_, r)| r)
}

pub fn default_capture_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("last_capture.png")
}

/// List outputs via `hyprctl -j monitors` when available.
pub fn list_hypr_monitors() -> Result<Vec<String>> {
    let output = Command::new("hyprctl")
        .args(["-j", "monitors"])
        .output()
        .context("hyprctl")?;
    if !output.status.success() {
        bail!("hyprctl failed");
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let mut names = Vec::new();
    if let Some(arr) = v.as_array() {
        for m in arr {
            if let Some(n) = m.get("name").and_then(|x| x.as_str()) {
                names.push(n.to_string());
            }
        }
    }
    Ok(names)
}
