use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Patterns that indicate the relic reward choice screen.
const REWARD_PATTERNS: &[&str] = &[
    "ProjectionRewardChoice",
    "Relic rewards initialized",
    "Got rewards",
    "Script [Info]: ProjectionRewardChoice.lua",
];

/// Patterns for fissure relic *selection* (recommendation overlay).
const RELIC_SELECT_PATTERNS: &[&str] = &[
    "ProjectionSelection",
    "Select a Relic",
    "VoidFissure",
    "FissureMission",
];

/// Patterns that often coincide with inventory refresh opportunities.
const LOADING_PATTERNS: &[&str] = &[
    "LotusGameRules",
    "Got new inventory",
    "InventoryService",
    "OnSquadMemberJoined",
];

#[derive(Debug, Clone)]
pub enum EeEvent {
    RewardScreen,
    RelicSelectScreen,
    LoadingLikely,
    Line(String),
}

pub struct EeLogWatcher {
    path: PathBuf,
    offset: u64,
}

impl EeLogWatcher {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn seek_end(&mut self) -> Result<()> {
        if !self.path.exists() {
            self.offset = 0;
            return Ok(());
        }
        let meta = std::fs::metadata(&self.path)?;
        self.offset = meta.len();
        Ok(())
    }

    pub fn read_new(&mut self) -> Result<Vec<EeEvent>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let meta = std::fs::metadata(&self.path)?;
        let len = meta.len();
        if len < self.offset {
            // Log rotated / truncated on game restart
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(vec![]);
        }

        let mut file = File::open(&self.path)
            .with_context(|| format!("open EE.log {}", self.path.display()))?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        self.offset = len;

        let mut events = Vec::new();
        for line in buf.lines() {
            if REWARD_PATTERNS.iter().any(|p| line.contains(p)) {
                info!("EE.log reward screen: {}", line.trim());
                events.push(EeEvent::RewardScreen);
            } else if RELIC_SELECT_PATTERNS.iter().any(|p| line.contains(p)) {
                info!("EE.log relic select: {}", line.trim());
                events.push(EeEvent::RelicSelectScreen);
            } else if LOADING_PATTERNS.iter().any(|p| line.contains(p)) {
                events.push(EeEvent::LoadingLikely);
            }
            if line.contains("Script") || line.contains("Reward") || line.contains("Riven") {
                events.push(EeEvent::Line(line.to_string()));
            }
        }
        Ok(events)
    }

    /// Spawn a background task that polls + watches the file.
    pub fn spawn(mut self) -> mpsc::Receiver<EeEvent> {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            if let Err(e) = self.seek_end() {
                warn!("EE.log seek_end: {e}");
            }

            let path = self.path.clone();
            let (n_tx, mut n_rx) = mpsc::channel::<()>(8);
            let watch_tx = n_tx.clone();

            // notify watcher in blocking thread
            std::thread::spawn(move || {
                let (fs_tx, fs_rx) = std::sync::mpsc::channel();
                let mut watcher: RecommendedWatcher = match Watcher::new(
                    fs_tx,
                    notify::Config::default().with_poll_interval(Duration::from_millis(500)),
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        warn!("notify watcher failed: {e}");
                        return;
                    }
                };
                if let Some(parent) = path.parent() {
                    let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
                }
                let _ = watcher.watch(&path, RecursiveMode::NonRecursive);
                while fs_rx.recv().is_ok() {
                    let _ = watch_tx.blocking_send(());
                }
            });

            let mut tick = tokio::time::interval(Duration::from_millis(750));
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    Some(()) = n_rx.recv() => {}
                }
                match self.read_new() {
                    Ok(events) => {
                        for ev in events {
                            if tx.send(ev).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        debug!("EE.log read: {e}");
                    }
                }
            }
        });
        rx
    }
}

/// Extract tradeable item-like names mentioned in chat (best-effort).
pub fn extract_riven_hints(line: &str) -> Option<String> {
    let re = Regex::new(r"(?i)\[([^\]]+Riven[^\]]*)\]").ok()?;
    re.captures(line).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_reward_line() {
        let line = "9999.999 Script [Info]: ProjectionRewardChoice.lua: Relic rewards initialized";
        assert!(REWARD_PATTERNS.iter().any(|p| line.contains(p)));
    }
}
