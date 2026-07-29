//! Overlay presentation for BNGframe.
//!
//! Native wlr-layer-shell painting is compositor-specific; this module:
//! 1. Keeps current overlay payload for the web `/overlay` route (recommended)
//! 2. Sends a desktop notification summary
//! 3. On Hyprland, optionally focuses the overlay browser workspace after OCR

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use bngframe_core::RewardSnapshot;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

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
    focus_overlay_workspace: AtomicBool,
    auto_open_browser: AtomicBool,
    ui_lang: RwLock<String>,
    state: Arc<RwLock<OverlayPayload>>,
}

impl OverlayManager {
    pub fn new(
        cache_dir: PathBuf,
        base_url: String,
        enabled: bool,
        ui_lang: String,
        focus_overlay_workspace: bool,
        auto_open_browser: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            cache_dir,
            base_url,
            enabled,
            focus_overlay_workspace: AtomicBool::new(focus_overlay_workspace),
            auto_open_browser: AtomicBool::new(auto_open_browser),
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

    pub fn set_focus_overlay_workspace(&self, enabled: bool) {
        self.focus_overlay_workspace
            .store(enabled, Ordering::Relaxed);
    }

    pub fn set_auto_open_browser(&self, enabled: bool) {
        self.auto_open_browser.store(enabled, Ordering::Relaxed);
    }

    pub fn state(&self) -> Arc<RwLock<OverlayPayload>> {
        self.state.clone()
    }

    pub async fn show_rewards(&self, reward: &RewardSnapshot) -> Result<()> {
        self.show_rewards_inner(reward, true).await
    }

    /// Update overlay payload without re-opening the browser / re-notifying.
    pub async fn update_rewards(&self, reward: &RewardSnapshot) -> Result<()> {
        self.show_rewards_inner(reward, false).await
    }

    async fn show_rewards_inner(&self, reward: &RewardSnapshot, announce: bool) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let lang = self.ui_lang.read().await.clone();
        std::fs::create_dir_all(&self.cache_dir)?;
        let html_path = self.cache_dir.join("overlay.html");
        let html = render_overlay_html(reward, &self.base_url, &lang);
        let _ = std::fs::write(&html_path, html);

        {
            let mut s = self.state.write().await;
            s.visible = true;
            s.reward = Some(reward.clone());
            s.html_path = Some(html_path.clone());
        }

        if announce {
            notify_rewards(reward, &lang);
            // Browser overlay is opt-in only — default path is notify-send.
            let focus = self.focus_overlay_workspace.load(Ordering::Relaxed);
            let open = self.auto_open_browser.load(Ordering::Relaxed);
            if focus || open {
                let url = format!("{}/overlay", self.base_url);
                tokio::task::spawn_blocking(move || {
                    focus_or_open_overlay_browser(&url, focus, open);
                });
            }
        }

        info!("Reward alert for {} ({} slots)", reward.id, reward.slots.len());
        Ok(())
    }

    /// Live HTML shell (auto-refreshes via /api/overlay + WebSocket).
    pub async fn live_page_html(&self) -> String {
        let lang = self.ui_lang.read().await.clone();
        render_live_overlay_shell(&self.base_url, &lang)
    }

    /// Last rendered HTML on disk (survives daemon restart until next show).
    pub fn cached_html_path(&self) -> PathBuf {
        self.cache_dir.join("overlay.html")
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

fn render_overlay_html(_reward: &RewardSnapshot, base_url: &str, lang: &str) -> String {
    // Disk cache gets the same live shell as /overlay (auto-refreshing).
    render_live_overlay_shell(base_url, lang)
}

/// Live overlay page: polls `/api/overlay` + listens on `/api/ws` so rewards
/// appear without a manual browser refresh.
pub fn render_live_overlay_shell(base_url: &str, lang: &str) -> String {
    let title = if is_ru(lang) {
        "BNGframe — оверлей наград"
    } else {
        "BNGframe Overlay"
    };
    let empty = if is_ru(lang) {
        "Ждём награды…"
    } else {
        "Waiting for rewards…"
    };
    let html_lang = if is_ru(lang) { "ru" } else { "en" };

    format!(
        r#"<!DOCTYPE html>
<html lang="{html_lang}"><head>
<meta charset="utf-8"/>
<title>{title}</title>
<meta http-equiv="Cache-Control" content="no-cache, no-store, must-revalidate"/>
<style>
  html,body {{ margin:0; height:100%; background:transparent; font-family: "IBM Plex Sans", "Segoe UI", sans-serif; color:#f4f0e8; overflow:hidden; }}
  .wrap {{ display:flex; gap:12px; justify-content:center; align-items:flex-end; height:100%; padding:4vh 2vw 8vh; box-sizing:border-box; pointer-events:none; }}
  .card {{ width:22vw; max-width:320px; background:rgba(8,12,18,.78); border:2px solid #fff5; border-radius:10px; padding:14px 16px; backdrop-filter: blur(8px); }}
  .card.best {{ border-color:#3ddc97; }}
  .rank {{ font-size:12px; letter-spacing:.12em; opacity:.7; }}
  .name {{ font-size:18px; font-weight:650; margin:8px 0; line-height:1.25; }}
  .meta {{ display:flex; justify-content:space-between; font-variant-numeric:tabular-nums; opacity:.9; }}
  .hint {{ position:fixed; top:12px; right:16px; font-size:12px; opacity:.55; }}
  .empty {{ opacity:.55; font-size:16px; align-self:center; }}
</style>
</head><body>
<div class="hint">BNGframe · live · <a style="color:#9fd" href="{base_url}">{base_url}</a></div>
<div class="wrap" id="wrap"><div class="empty">{empty}</div></div>
<script>
(function() {{
  const emptyMsg = {empty_js};
  let lastId = null;
  function esc(s) {{
    return String(s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
  }}
  function render(payload) {{
    const wrap = document.getElementById('wrap');
    const reward = payload && payload.reward;
    if (!reward || !reward.slots || !reward.slots.length) {{
      wrap.innerHTML = '<div class="empty">' + emptyMsg + '</div>';
      return;
    }}
    if (reward.id === lastId && wrap.querySelector('.card')) {{
      // Same reward id is reused for early→full OCR updates — refresh if content changed.
      const sig = reward.slots.map(s => (s.name||'') + '|' + (s.platinum||'') + '|' + (s.rank||'')).join(';');
      if (wrap.dataset.sig === sig) return;
      wrap.dataset.sig = sig;
    }} else {{
      lastId = reward.id;
      wrap.dataset.sig = reward.slots.map(s => (s.name||'') + '|' + (s.platinum||'') + '|' + (s.rank||'')).join(';');
    }}
    const best = reward.best_index;
    wrap.innerHTML = reward.slots.map((slot, i) => {{
      const plat = slot.platinum != null ? Math.round(slot.platinum) + 'p' : '—';
      const duc = slot.ducats != null ? slot.ducats + 'd' : '—';
      const rank = slot.rank != null ? slot.rank : (i + 1);
      const cls = best === i ? 'card best' : 'card';
      return '<div class="' + cls + '"><div class="rank">#' + rank + '</div><div class="name">' +
        esc(slot.name) + '</div><div class="meta"><span>' + plat + '</span><span>' + duc + '</span></div></div>';
    }}).join('');
    document.body.style.opacity = '1';
  }}
  async function load() {{
    try {{
      const r = await fetch('/api/overlay', {{ cache: 'no-store' }});
      if (!r.ok) return;
      render(await r.json());
    }} catch (e) {{}}
  }}
  load();
  setInterval(load, 700);
  try {{
    const ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/api/ws');
    ws.onmessage = (ev) => {{
      try {{
        const msg = JSON.parse(ev.data);
        if (msg.type === 'overlay_shown' || msg.type === 'reward_detected') load();
      }} catch (e) {{ load(); }}
    }};
  }} catch (e) {{}}
  setTimeout(() => {{ document.body.style.opacity = '0.35'; }}, 45000);
}})();
</script>
</body></html>"#,
        empty_js = serde_json::to_string(empty).unwrap_or_else(|_| "\"…\"".into()),
    )
}

fn notify_rewards(reward: &RewardSnapshot, lang: &str) {
    let title = if is_ru(lang) {
        "BNGframe — награды"
    } else {
        "BNGframe — rewards"
    };

    let mut lines: Vec<String> = Vec::new();
    for (i, slot) in reward.slots.iter().enumerate() {
        let rank = slot.rank.unwrap_or((i + 1) as u8);
        let plat = slot
            .platinum
            .filter(|p| *p > 0.0)
            .map(|p| format!("{p:.0}p"))
            .unwrap_or_else(|| "—".into());
        let duc = slot
            .ducats
            .map(|d| format!("{d}d"))
            .unwrap_or_else(|| "—".into());
        let marker = if reward.best_index == Some(i) { "★ " } else { "" };
        lines.push(format!("{marker}#{rank} {}  ·  {plat}  ·  {duc}", slot.name));
    }
    if lines.is_empty() {
        lines.push(if is_ru(lang) {
            "награды обнаружены".into()
        } else {
            "rewards detected".into()
        });
    }
    let body = lines.join("\n");

    // -t 20000: keep on screen ~20s so squad timer is still useful
    // Don't wait for the notification daemon — it can stall under load.
    let title = title.to_string();
    let body_notify = body.clone();
    std::thread::spawn(move || {
        let status = Command::new("notify-send")
            .args([
                "-a",
                "BNGframe",
                "-u",
                "critical",
                "-t",
                "20000",
                "--hint=string:x-dunst-stack-tag:bngframe-rewards",
                "--hint=boolean:resident:true",
                &title,
                &body_notify,
            ])
            .status();
        if let Err(e) = status {
            warn!("notify-send failed: {e}");
        }
    });
    info!("Reward notification:\n{body}");
}

fn focus_or_open_overlay_browser(url: &str, focus: bool, open_if_missing: bool) {
    if focus {
        if hypr_focus_overlay_browser() {
            info!("Focused overlay browser workspace");
            return;
        }
        debug!("Overlay browser window not found on Hyprland");
    }
    if open_if_missing {
        info!("Opening overlay in browser: {url}");
        let _ = Command::new("xdg-open").arg(url).status();
        if focus {
            std::thread::sleep(std::time::Duration::from_millis(450));
            if hypr_focus_overlay_browser() {
                info!("Focused newly opened overlay browser");
            }
        }
    }
}

/// Find a browser tab/window showing the BNGframe overlay and focus it (switches workspace).
fn hypr_focus_overlay_browser() -> bool {
    let Ok(out) = Command::new("hyprctl").args(["-j", "clients"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let Ok(clients) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return false;
    };
    let Some(arr) = clients.as_array() else {
        return false;
    };

    let mut best: Option<(i64, String, i64)> = None; // score, address, workspace
    for c in arr {
        let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
        if !is_overlay_browser_title(title) {
            continue;
        }
        let addr = c.get("address").and_then(|v| v.as_str()).unwrap_or("");
        if addr.is_empty() {
            continue;
        }
        let ws = c
            .get("workspace")
            .and_then(|w| w.get("id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut score = 10i64;
        let title_l = title.to_lowercase();
        if title_l.contains("оверлей") || title_l.contains("overlay") {
            score += 50;
        }
        if title_l.contains("/overlay") || title_l.contains("17832/overlay") {
            score += 80;
        }
        if c.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false) {
            score += 20;
        }
        if c.get("floating").and_then(|v| v.as_bool()).unwrap_or(false) {
            score += 5;
        }
        if best.as_ref().map(|(s, ..)| score > *s).unwrap_or(true) {
            best = Some((score, addr.to_string(), ws));
        }
    }

    let Some((_, address, ws)) = best else {
        return false;
    };

    // Switch workspace first (more reliable when window is on another WS), then focus.
    if ws != 0 {
        let ws_status = Command::new("hyprctl")
            .args(["dispatch", "workspace", &ws.to_string()])
            .status();
        if let Err(e) = ws_status {
            warn!("hyprctl workspace {ws}: {e}");
        }
    }
    let focus = Command::new("hyprctl")
        .args(["dispatch", "focuswindow", &format!("address:{address}")])
        .output();
    match focus {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            warn!(
                "hyprctl focuswindow failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            false
        }
        Err(e) => {
            warn!("hyprctl focuswindow: {e}");
            false
        }
    }
}

fn is_overlay_browser_title(title: &str) -> bool {
    let t = title.to_lowercase();
    // Exact companion SPA title is just "bngframe" — skip that.
    if t.trim() == "bngframe" {
        return false;
    }
    if t.contains("17832/overlay") || t.contains("/overlay") {
        return true;
    }
    // HTML <title> from overlay page
    if t.contains("bngframe")
        && (t.contains("оверлей") || t.contains("overlay") || t.contains("наград"))
    {
        return true;
    }
    false
}

pub fn write_layer_shell_note(path: &Path) -> Result<()> {
    let note = r#"# Оверлей BNGframe в Hyprland

Держите страницу http://127.0.0.1:17832/overlay открытой в отдельном окне браузера
(на любом workspace). После успешного OCR демон может переключить вас на этот workspace
(настройка `focus_overlay_workspace` в config.toml / UI).

Пример правил окна:

```
windowrulev2 = float, title:^(BNGframe —)
windowrulev2 = pin, title:^(BNGframe —)
windowrulev2 = opacity 0.92 override 0.92 override, title:^(BNGframe —)
```

Не ставьте `nofocus` на окно оверлея, если хотите автопереключение фокуса.
"#;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, note)?;
    Ok(())
}
