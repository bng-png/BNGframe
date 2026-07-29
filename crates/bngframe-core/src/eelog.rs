use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Only fire full pipeline when rewards are shown.
const REWARD_TRIGGER: &str = "ProjectionRewardChoice.lua: Got rewards";
/// Fires ~1–2s before Got rewards — prefetch capture (Wine EE.log buffering).
const REWARD_OPEN_TRIGGERS: &[&str] = &[
    "VoidProjections: OpenVoidProjectionRewardScreen",
    "ProjectionRewardChoice.lua: Relic rewards initialized",
    "Created /Lotus/Interface/ProjectionRewardChoice.swf",
];

/// Local (and rarely others') reward paths appear just before Got rewards.
const REWARD_PATH_RE: &str =
    r"VoidProjections:\s+([0-9a-fA-F]+)\s+gets reward\s+(/\S+)";

const RELIC_SELECT_PATTERNS: &[&str] = &[
    "ProjectionSelection.lua",
    "Script [Info]: Select a Relic",
];

const LOADING_PATTERNS: &[&str] = &["Got new inventory", "OnSquadMemberJoined"];

const REWARD_DEBOUNCE: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub enum EeEvent {
    /// Fissure reward choice UI is up. `reward_paths` are Lotus StoreItems paths
    /// parsed from recent `VoidProjections: … gets reward …` lines (often only local).
    RewardScreen { reward_paths: Vec<String> },
    /// Screen is opening — start capture burst before Got rewards (paths may be empty).
    RewardScreenOpening,
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

    pub fn read_new(&mut self, pending_paths: &mut Vec<String>, recent_paths: &mut Vec<String>) -> Result<Vec<EeEvent>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let meta = std::fs::metadata(&self.path)?;
        let len = meta.len();
        if len < self.offset {
            self.offset = 0;
            pending_paths.clear();
            recent_paths.clear();
        }
        if len == self.offset {
            return Ok(vec![]);
        }

        let mut file = File::open(&self.path)
            .with_context(|| format!("open EE.log {}", self.path.display()))?;
        file.seek(SeekFrom::Start(self.offset))?;
        // Wine/Proton EE.log often contains non-UTF8 player-name glyphs. A strict
        // read_to_string error would leave offset stuck and miss Got rewards forever.
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;
        self.offset = len;
        let buf = String::from_utf8_lossy(&raw);

        let path_re = Regex::new(REWARD_PATH_RE).expect("reward path regex");
        let mut events = Vec::new();
        for line in buf.lines() {
            if let Some(c) = path_re.captures(line) {
                let path = c[2].to_string();
                if path.eq_ignore_ascii_case("/null") {
                    debug!("EE.log void reward path ignored: {path}");
                    continue;
                }
                info!("EE.log void reward path: {path}");
                pending_paths.push(path.clone());
                recent_paths.push(path);
                // Keep a short window only
                if pending_paths.len() > 8 {
                    let drain = pending_paths.len() - 8;
                    pending_paths.drain(0..drain);
                }
                if recent_paths.len() > 4 {
                    let drain = recent_paths.len() - 4;
                    recent_paths.drain(0..drain);
                }
            }
            if line.contains(REWARD_TRIGGER) {
                info!("EE.log reward screen: {}", line.trim());
                let mut paths = std::mem::take(pending_paths);
                if paths.is_empty() && !recent_paths.is_empty() {
                    debug!("reward trigger without fresh paths; reusing recent reward paths");
                    paths = recent_paths.clone();
                } else if !paths.is_empty() {
                    *recent_paths = paths.clone();
                }
                // Consume recent paths after firing so a later empty trigger
                // does not replay stale rewards from a previous fissure.
                recent_paths.clear();
                events.push(EeEvent::RewardScreen { reward_paths: paths });
            } else if REWARD_OPEN_TRIGGERS.iter().any(|p| line.contains(p)) {
                info!("EE.log reward opening: {}", line.trim());
                events.push(EeEvent::RewardScreenOpening);
            } else if RELIC_SELECT_PATTERNS.iter().any(|p| line.contains(p)) {
                info!("EE.log relic select: {}", line.trim());
                events.push(EeEvent::RelicSelectScreen);
            } else if LOADING_PATTERNS.iter().any(|p| line.contains(p)) {
                events.push(EeEvent::LoadingLikely);
            }
            if line.contains("Riven") || line.contains("Trade") {
                events.push(EeEvent::Line(line.to_string()));
            }
        }
        Ok(events)
    }

    pub fn spawn(mut self) -> mpsc::Receiver<EeEvent> {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            if let Err(e) = self.seek_end() {
                warn!("EE.log seek_end: {e}");
            }

            let path = self.path.clone();
            let (n_tx, mut n_rx) = mpsc::channel::<()>(8);
            let watch_tx = n_tx.clone();

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

            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let mut last_reward: Option<Instant> = None;
            let mut last_opening: Option<Instant> = None;
            let mut pending_paths: Vec<String> = Vec::new();
            let mut recent_paths: Vec<String> = Vec::new();
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    Some(()) = n_rx.recv() => {}
                }
                match self.read_new(&mut pending_paths, &mut recent_paths) {
                    Ok(events) => {
                        // Wine often flushes Opening + Got rewards in one chunk.
                        // Prefetch then is useless (Got consumes an empty slot) —
                        // skip Opening when RewardScreen is in the same batch.
                        let got_in_batch =
                            events.iter().any(|e| matches!(e, EeEvent::RewardScreen { .. }));
                        for ev in events {
                            let ev = match &ev {
                                EeEvent::RewardScreen { .. } => {
                                    if last_reward
                                        .map(|t| t.elapsed() < REWARD_DEBOUNCE)
                                        .unwrap_or(false)
                                    {
                                        debug!("debounced duplicate RewardScreen");
                                        continue;
                                    }
                                    last_reward = Some(Instant::now());
                                    ev
                                }
                                EeEvent::RewardScreenOpening => {
                                    if got_in_batch {
                                        debug!("skip Opening — Got rewards in same EE flush");
                                        continue;
                                    }
                                    if last_opening
                                        .map(|t| t.elapsed() < Duration::from_secs(3))
                                        .unwrap_or(false)
                                    {
                                        debug!("debounced duplicate RewardScreenOpening");
                                        continue;
                                    }
                                    last_opening = Some(Instant::now());
                                    ev
                                }
                                _ => ev,
                            };
                            if tx.send(ev).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => warn!("EE.log read: {e}"),
                }
            }
        });
        rx
    }
}

pub fn extract_riven_hints(line: &str) -> Option<String> {
    let re = Regex::new(r"(?i)\[([^\]]+Riven[^\]]*)\]").ok()?;
    re.captures(line).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_got_rewards_only() {
        assert!(REWARD_TRIGGER.contains("Got rewards"));
        let preload = "ResourceLoader (/Lotus/Interface/ProjectionRewardChoice.swf) Found";
        assert!(!preload.contains(REWARD_TRIGGER));
        let init = "ProjectionRewardChoice.lua: Relic rewards initialized";
        assert!(!init.contains(REWARD_TRIGGER));
    }

    #[test]
    fn parses_reward_path() {
        let re = Regex::new(REWARD_PATH_RE).unwrap();
        let line = "29961.715 Sys [Info]: VoidProjections: 60369a4dd337f8658a29cf42 gets reward /Lotus/StoreItems/Types/Recipes/Components/FormaBlueprint";
        let c = re.captures(line).unwrap();
        assert_eq!(
            &c[2],
            "/Lotus/StoreItems/Types/Recipes/Components/FormaBlueprint"
        );
    }
}
