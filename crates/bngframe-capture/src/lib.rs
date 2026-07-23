//! Screen capture for Wayland via `grim` (works on Hyprland/Sway and many wlroots compositors).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::DynamicImage;
use tracing::{info, warn};

pub fn capture_monitor(monitor: Option<&str>, out_path: &Path) -> Result<DynamicImage> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut cmd = Command::new("grim");
    if let Some(m) = monitor {
        cmd.arg("-o").arg(m);
    }
    cmd.arg(out_path);

    let output = cmd.output().context("spawn grim (install grim for Wayland capture)")?;
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
