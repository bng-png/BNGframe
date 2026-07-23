use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardSlot {
    pub name: String,
    pub matched_url_name: Option<String>,
    pub platinum: Option<f64>,
    pub volume: Option<i64>,
    pub ducats: Option<i64>,
    pub owned: Option<i64>,
    pub mastered: Option<bool>,
    pub rank: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardSnapshot {
    pub id: String,
    pub detected_at: DateTime<Utc>,
    pub slots: Vec<RewardSlot>,
    pub best_index: Option<usize>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub watching_eelog: bool,
    pub eelog_path: String,
    pub eelog_exists: bool,
    pub overlay_enabled: bool,
    pub inventory_loaded: bool,
    pub inventory_consent: bool,
    pub items_cached: usize,
    pub last_error: Option<String>,
    pub uptime_secs: u64,
    pub phase: String,
}

impl DaemonStatus {
    pub fn new(eelog_path: String, eelog_exists: bool, overlay_enabled: bool, consent: bool) -> Self {
        Self {
            watching_eelog: false,
            eelog_path,
            eelog_exists,
            overlay_enabled,
            inventory_loaded: false,
            inventory_consent: consent,
            items_cached: 0,
            last_error: None,
            uptime_secs: 0,
            phase: "starting".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    Status { status: DaemonStatus },
    RewardDetected { reward: RewardSnapshot },
    OverlayShown { reward_id: String },
    InventoryUpdated { item_count: usize },
    LogLine { line: String },
    Error { message: String },
}

pub struct AppState {
    pub status: RwLock<DaemonStatus>,
    pub last_rewards: RwLock<VecDeque<RewardSnapshot>>,
    pub started_at: Instant,
    events: broadcast::Sender<AppEvent>,
}

impl AppState {
    pub fn new(status: DaemonStatus) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            status: RwLock::new(status),
            last_rewards: RwLock::new(VecDeque::with_capacity(32)),
            started_at: Instant::now(),
            events,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub fn emit(&self, event: AppEvent) {
        let _ = self.events.send(event);
    }

    pub async fn push_reward(&self, reward: RewardSnapshot) {
        {
            let mut q = self.last_rewards.write().await;
            q.push_front(reward.clone());
            while q.len() > 20 {
                q.pop_back();
            }
        }
        self.emit(AppEvent::RewardDetected { reward });
    }

    pub async fn set_error(&self, message: impl Into<String>) {
        let message = message.into();
        {
            let mut s = self.status.write().await;
            s.last_error = Some(message.clone());
        }
        self.emit(AppEvent::Error { message });
    }

    pub async fn refresh_uptime(&self) {
        let mut s = self.status.write().await;
        s.uptime_secs = self.started_at.elapsed().as_secs();
    }
}
