# Arch Linux packaging notes

## Runtime deps

```bash
sudo pacman -S grim tesseract tesseract-data-eng
# optional
sudo pacman -S libnotify  # notify-send
```

## Build

```bash
cargo build --release -p bngframe-daemon
cd web && npm ci && npm run build
install -Dm755 target/release/bngframe ~/.local/bin/bngframe
```

Serve the SPA by running the daemon from the repo root (so `web/dist` resolves), or set `web_dir` in `~/.config/bngframe/config.toml`.

## ptrace for inventory

```bash
# temporary
sudo sysctl kernel.yama.ptrace_scope=0

# or capability on binary
sudo setcap cap_sys_ptrace+ep ~/.local/bin/bngframe
```

## systemd --user

```bash
cp packaging/bngframe.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now bngframe.service
```

## Flatpak (future)

Portal-based screencopy would replace `grim`; ptrace/inventory is unlikely to work sandboxed without heavy permissions — keep inventory optional or host-native.
