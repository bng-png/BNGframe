//! Overlay presentation for BNGframe.
//!
//! Native wlr-layer-shell painting is compositor-specific; this module:
//! 1. Keeps current overlay payload for the web `/overlay` route (recommended)
//! 2. Sends a desktop notification summary
//! 3. On Hyprland, optionally opens the overlay URL via configured browser

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use bngframe_core::RewardSnapshot;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize)]
pub struct OverlayPayload {
    pub visible: bool,
    pub reward: Option<RewardSnapshot>,
    pub html_path: Option<PathBuf>,
}

pub struct OverlayManager {
    cache_dir: PathBuf,
    base_url: String,
    enabled: bool,
    ui_lang: RwLock<String>,
    state: Arc<RwLock<OverlayPayload>>,
}

impl OverlayManager {
    pub fn new(cache_dir: PathBuf, base_url: String, enabled: bool, ui_lang: String) -> Arc<Self> {
        Arc::new(Self {
            cache_dir,
            base_url,
            enabled,
            ui_lang: RwLock::new(ui_lang),
            state: Arc::new(RwLock::new(OverlayPayload {
                visible: false,
                reward: None,
                html_path: None,
            })),
        })
    }

    pub async fn set_ui_lang(&self, lang: &str) {
        *self.ui_lang.write().await = lang.to_string();
    }

    pub fn state(&self) -> Arc<RwLock<OverlayPayload>> {
        self.state.clone()
    }

    pub async fn show_rewards(&self, reward: &RewardSnapshot) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let lang = self.ui_lang.read().await.clone();
        std::fs::create_dir_all(&self.cache_dir)?;
        let html_path = self.cache_dir.join("overlay.html");
        let html = render_overlay_html(reward, &self.base_url, &lang);
        std::fs::write(&html_path, html)?;

        {
            let mut s = self.state.write().await;
            s.visible = true;
            s.reward = Some(reward.clone());
            s.html_path = Some(html_path.clone());
        }

        notify_summary(reward, &lang);
        try_hypr_open_overlay(&format!("{}/overlay", self.base_url));

        info!("Overlay shown for reward {}", reward.id);
        Ok(())
    }

    pub async fn hide(&self) -> Result<()> {
        let mut s = self.state.write().await;
        s.visible = false;
        Ok(())
    }

    pub async fn current(&self) -> OverlayPayload {
        self.state.read().await.clone()
    }
}

fn is_ru(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("ru")
}

fn render_overlay_html(reward: &RewardSnapshot, base_url: &str, lang: &str) -> String {
    let mut cards = String::new();
    for (i, slot) in reward.slots.iter().enumerate() {
        let best = reward.best_index == Some(i);
        let border = if best { "#3ddc97" } else { "#ffffff55" };
        let plat = slot
            .platinum
            .map(|p| format!("{p:.0}p"))
            .unwrap_or_else(|| "—".into());
        let ducats = slot
            .ducats
            .map(|d| format!("{d}d"))
            .unwrap_or_else(|| "—".into());
        cards.push_str(&format!(
            r#"<div class="card" style="border-color:{border}">
              <div class="rank">#{}</div>
              <div class="name">{}</div>
              <div class="meta"><span>{}</span><span>{}</span></div>
            </div>"#,
            slot.rank.unwrap_or((i + 1) as u8),
            html_escape(&slot.name),
            plat,
            ducats
        ));
    }

    let title = if is_ru(lang) {
        "BNGframe — оверлей наград"
    } else {
        "BNGframe Overlay"
    };
    let html_lang = if is_ru(lang) { "ru" } else { "en" };

    format!(
        r#"<!DOCTYPE html>
<html lang="{html_lang}"><head>
<meta charset="utf-8"/>
<title>{title}</title>
<style>
  html,body {{ margin:0; height:100%; background:transparent; font-family: "IBM Plex Sans", "Segoe UI", sans-serif; color:#f4f0e8; overflow:hidden; }}
  .wrap {{ display:flex; gap:12px; justify-content:center; align-items:flex-end; height:100%; padding:4vh 2vw 8vh; box-sizing:border-box; pointer-events:none; }}
  .card {{ width:22vw; max-width:320px; background:rgba(8,12,18,.78); border:2px solid #fff5; border-radius:10px; padding:14px 16px; backdrop-filter: blur(8px); }}
  .rank {{ font-size:12px; letter-spacing:.12em; opacity:.7; }}
  .name {{ font-size:18px; font-weight:650; margin:8px 0; line-height:1.25; }}
  .meta {{ display:flex; justify-content:space-between; font-variant-numeric:tabular-nums; opacity:.9; }}
  .hint {{ position:fixed; top:12px; right:16px; font-size:12px; opacity:.55; }}
</style>
</head><body>
<div class="hint">BNGframe · <a style="color:#9fd" href="{base_url}">{base_url}</a></div>
<div class="wrap">{cards}</div>
<script>
  setTimeout(() => {{ document.body.style.opacity = '0'; }}, 20000);
</script>
</body></html>"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn notify_summary(reward: &RewardSnapshot, lang: &str) {
    let fallback = if is_ru(lang) {
        "награды обнаружены"
    } else {
        "rewards detected"
    };
    let title = if is_ru(lang) {
        "Награды реликвии"
    } else {
        "Relic rewards"
    };
    let best = reward
        .best_index
        .and_then(|i| reward.slots.get(i))
        .map(|s| {
            format!(
                "{} ({})",
                s.name,
                s.platinum
                    .map(|p| format!("{p:.0}p"))
                    .unwrap_or_else(|| "?".into())
            )
        })
        .unwrap_or_else(|| fallback.into());
    let _ = Command::new("notify-send")
        .args(["-a", "BNGframe", title, &best])
        .status();
}

fn try_hypr_open_overlay(url: &str) {
    if Command::new("xdg-open").arg(url).spawn().is_err() {
        warn!("Could not open overlay URL via xdg-open");
    }
}

pub fn write_layer_shell_note(path: &Path) -> Result<()> {
    let note = r#"# Оверлей BNGframe в Hyprland

Добавьте правила окна, чтобы страница оверлея могла лежать поверх Warframe без кражи фокуса:

```
windowrulev2 = float, title:^(BNGframe)
windowrulev2 = pin, title:^(BNGframe)
windowrulev2 = nofocus, title:^(BNGframe)
windowrulev2 = opacity 0.92 override 0.92 override, title:^(BNGframe)
```

После обнаружения награды откройте http://127.0.0.1:17832/overlay в отдельном окне браузера.
"#;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, note)?;
    Ok(())
}
