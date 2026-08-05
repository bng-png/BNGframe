# BNGframe

AlecaFrame-style Warframe companion for **Linux Wayland**: relic reward overlay (EE.log → process memory Lotus paths → prices), inventory sync via read-only Proton memory → DE mobile API, relic planner, warframe.market, rivens, stats & analytics.

```
Browser SPA (web/)  <── HTTP/WS 127.0.0.1:17832 ──>  bngframe daemon (Rust)
                                                         ├─ EE.log watcher (timing)
                                                         ├─ reward memory scrape (squad slots)
                                                         ├─ inventory memory scrape
                                                         └─ overlay HTML + notify-send
```

## Requirements

- Rust toolchain, Node 20+
- Wayland compositor (tested target: **Hyprland** / wlroots)
- Warframe via Steam/Proton
- Optional: `notify-send`, `hyprctl`
- ptrace for inventory **and** reward memory (`ptrace_scope=0` or `cap_sys_ptrace`)

Relic rewards: EE.log gives the local Lotus path + party size; memory may add co-located StoreItems when a tight EE neighborhood exists; **OCR fills remaining squad slots** (other players' paths are rarely present as StoreItems in Proton heaps — catalog floods are ignored).

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

## Relic overlay (memory)

1. Set `reward_memory_consent = true` in config (or Settings).
2. Keep the daemon running while playing (same ptrace requirements as inventory).
3. When the reward screen opens, EE.log triggers a full-region memory poll; Lotus paths are resolved to market items.
4. Manual: Overview → **Scan rewards from memory**, or `POST /api/rewards/trigger`.
5. Debug dump: `POST /api/rewards/memory-scan` → JSON + `~/.cache/bngframe/reward_mem_*.txt`.
6. Open `/overlay` in a pinned browser window (see `~/.local/share/bngframe/hyprland-overlay.md`).

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
| POST | `/api/rewards/trigger` | Manual memory reward scan |
| POST | `/api/rewards/memory-scan` | Debug memory harvest dump |
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
crates/bngframe-core/      # EE.log, DB, pricing, inventory, reward_mem, …
crates/bngframe-capture/  # grim capture (legacy / unused by reward path)
crates/bngframe-overlay/  # overlay HTML + notifications
crates/bngframe-daemon/   # axum API + orchestration
web/                      # React companion SPA
```

## Локализация

Интерфейс по умолчанию на **русском**. В Настройках можно переключить на English (`ui_lang` в `config.toml`: `ru` | `en`). Оверлей и desktop-уведомления следуют тому же языку.
