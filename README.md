# BNGframe

AlecaFrame-style Warframe companion for **Linux Wayland**: relic reward overlay (EE.log → capture → OCR → prices), inventory sync via read-only Proton memory → DE mobile API, relic planner, warframe.market, rivens, stats & analytics.

```
Browser SPA (web/)  <── HTTP/WS 127.0.0.1:17832 ──>  bngframe daemon (Rust)
                                                         ├─ EE.log watcher
                                                         ├─ grim + tesseract
                                                         ├─ inventory memory scrape
                                                         └─ overlay HTML + notify-send
```

## Requirements

- Rust toolchain, Node 20+
- Wayland compositor (tested target: **Hyprland** / wlroots)
- `grim`, `tesseract` (+ `tesseract-data-eng`; for Russian game UI also `tesseract-data-rus`)
- Warframe via Steam/Proton
- Optional: `notify-send`, `hyprctl`

OCR for relic rewards defaults to `rus+eng` (Russian client + English fallback). The catalog stores WFM `i18n.ru` names so Cyrillic OCR can match market items. In Settings set OCR lang to `rus`, `eng`, or `rus+eng`, then refresh the market catalog once so `name_ru` is cached.

## Quick start

```bash
# Daemon
cargo run -p bngframe-daemon

# Companion UI (dev)
cd web && npm install && npm run dev
# open http://127.0.0.1:5173 (proxied to daemon)

# Or build SPA and serve from daemon
cd web && npm run build
cargo run -p bngframe-daemon
# open http://127.0.0.1:17832
```

Config is created at `~/.config/bngframe/config.toml`.

Default EE.log path:

`~/.local/share/Steam/steamapps/compatdata/230410/pfx/drive_c/users/steamuser/AppData/Local/Warframe/EE.log`

## Inventory sync (memory)

1. Set `inventory_consent = true` in config (or Settings tab).
2. Launch Warframe, log in.
3. Allow ptrace, e.g.:

```bash
sudo sysctl kernel.yama.ptrace_scope=0
# or: sudo setcap cap_sys_ptrace+ep target/release/bngframe
```

4. Click **Sync from game memory** (or `POST /api/inventory/sync`).

This reads a short-lived session token from the Proton process and calls DE’s mobile inventory endpoint. It is a common community approach but **not** the Overwolf path AlecaFrame uses — understand ToS risk before enabling.

Fallback: place an inventory JSON dump and use **Import dump** / `POST /api/inventory/import`.

## Relic overlay

1. Keep the daemon running while playing.
2. When the reward screen appears, EE.log triggers capture + OCR.
3. Manual fallback: Overview → **Trigger reward OCR**, or `POST /api/rewards/trigger`.
4. Open `/overlay` in a pinned browser window (see `~/.local/share/bngframe/hyprland-overlay.md`).

Hyprland example rules:

```
windowrulev2 = float, title:^(BNGframe Overlay)$
windowrulev2 = pin, title:^(BNGframe Overlay)$
windowrulev2 = nofocus, title:^(BNGframe Overlay)$
```

## API (localhost only)

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/health` | Liveness |
| GET | `/api/status` | Daemon status |
| GET/POST | `/api/config` | Settings |
| GET | `/api/ws` | Events |
| POST | `/api/rewards/trigger` | Manual OCR |
| POST | `/api/inventory/sync` | Memory inventory |
| GET | `/api/relics` | Relic planner |
| POST | `/api/market/signin` | WFM auth |
| POST | `/api/rivens/analyze` | Riven text |
| GET | `/api/analytics` | Market heuristics |

Bound to `127.0.0.1` by default. CORS allows only localhost origins.

## Packaging

- Release binary: `cargo build --release -p bngframe-daemon`
- Optional user unit: [`packaging/bngframe.service`](packaging/bngframe.service)
- Arch notes: [`packaging/ARCH.md`](packaging/ARCH.md)

## Project layout

```
crates/bngframe-core/      # EE.log, DB, pricing, inventory, relics, rivens, …
crates/bngframe-capture/  # grim capture
crates/bngframe-overlay/  # overlay HTML + notifications
crates/bngframe-daemon/   # axum API + orchestration
web/                      # React companion SPA
```

## Локализация

Интерфейс по умолчанию на **русском**. В Настройках можно переключить на English (`ui_lang` в `config.toml`: `ru` | `en`). Оверлей и desktop-уведомления следуют тому же языку.
