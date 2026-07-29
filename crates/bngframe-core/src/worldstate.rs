//! Live world state from warframestat.us (cycles, fissures, Baro).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleInfo {
    pub id: String,
    pub state: String,
    pub time_left: Option<String>,
    pub expiry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FissureInfo {
    pub id: String,
    pub node: String,
    pub mission_type: String,
    pub tier: String,
    pub enemy: Option<String>,
    pub expiry: Option<String>,
    pub eta: Option<String>,
    pub is_hard: bool,
    pub is_storm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoidTraderInfo {
    pub character: String,
    pub location: String,
    pub active: bool,
    pub expiry: Option<String>,
    pub activation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldStateSnapshot {
    pub earth: Option<CycleInfo>,
    pub cetus: Option<CycleInfo>,
    pub vallis: Option<CycleInfo>,
    pub cambion: Option<CycleInfo>,
    pub zariman: Option<CycleInfo>,
    pub void_trader: Option<VoidTraderInfo>,
    pub fissures: Vec<FissureInfo>,
    pub events: Vec<String>,
    pub fetched_at: String,
}

struct Cache {
    at: Instant,
    data: WorldStateSnapshot,
}

pub struct WorldStateService {
    client: reqwest::Client,
    cache: Mutex<Option<Cache>>,
}

impl WorldStateService {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("BNGframe/0.1 (+https://github.com/bng/BNGframe)")
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest");
        Self {
            client,
            cache: Mutex::new(None),
        }
    }

    pub async fn snapshot(&self) -> Result<WorldStateSnapshot> {
        {
            let guard = self.cache.lock().await;
            if let Some(c) = guard.as_ref() {
                if c.at.elapsed() < Duration::from_secs(60) {
                    return Ok(c.data.clone());
                }
            }
        }
        match self.fetch().await {
            Ok(data) => {
                let mut guard = self.cache.lock().await;
                *guard = Some(Cache {
                    at: Instant::now(),
                    data: data.clone(),
                });
                Ok(data)
            }
            Err(e) => {
                warn!("worldstate fetch failed: {e}");
                let guard = self.cache.lock().await;
                if let Some(c) = guard.as_ref() {
                    return Ok(c.data.clone());
                }
                Err(e)
            }
        }
    }

    async fn fetch(&self) -> Result<WorldStateSnapshot> {
        let resp = self
            .client
            .get("https://api.warframestat.us/pc")
            .send()
            .await
            .context("warframestat pc")?
            .error_for_status()?;
        let body: Value = resp.json().await?;
        Ok(parse_worldstate(&body))
    }
}

impl Default for WorldStateService {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_worldstate(body: &Value) -> WorldStateSnapshot {
    let earth = cycle_from(
        "earth",
        body.get("earthCycle"),
        |v| {
            if v.get("isDay").and_then(|x| x.as_bool()).unwrap_or(false) {
                "day"
            } else {
                "night"
            }
        },
    );
    let cetus = cycle_from(
        "cetus",
        body.get("cetusCycle"),
        |v| {
            if v.get("isDay").and_then(|x| x.as_bool()).unwrap_or(false) {
                "day"
            } else {
                "night"
            }
        },
    );
    let vallis = cycle_from("vallis", body.get("vallisCycle"), |v| {
        if v.get("isWarm").and_then(|x| x.as_bool()).unwrap_or(false) {
            "warm"
        } else {
            "cold"
        }
    });
    let cambion = cycle_from("cambion", body.get("cambionCycle"), |v| {
        v.get("active")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
    });
    let zariman = cycle_from("zariman", body.get("zarimanCycle"), |v| {
        v.get("state")
            .or_else(|| v.get("active"))
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
    });

    let void_trader = body.get("voidTrader").map(|vt| VoidTraderInfo {
        character: vt
            .get("character")
            .and_then(|v| v.as_str())
            .unwrap_or("Baro Ki'Teer")
            .into(),
        location: vt
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("—")
            .into(),
        active: vt.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
        expiry: vt
            .get("expiry")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        activation: vt
            .get("activation")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    });

    let mut fissures = Vec::new();
    if let Some(arr) = body.get("fissures").and_then(|v| v.as_array()) {
        for f in arr {
            fissures.push(FissureInfo {
                id: f
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                node: f
                    .get("node")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                mission_type: f
                    .get("missionType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                tier: f
                    .get("tier")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                enemy: f
                    .get("enemy")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                expiry: f
                    .get("expiry")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                eta: f
                    .get("eta")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                is_hard: f.get("isHard").and_then(|v| v.as_bool()).unwrap_or(false),
                is_storm: f.get("isStorm").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
    }
    // Prefer normal (non-steel-path) first, shorter eta
    fissures.sort_by(|a, b| {
        a.is_hard
            .cmp(&b.is_hard)
            .then_with(|| a.tier.cmp(&b.tier))
            .then_with(|| a.node.cmp(&b.node))
    });

    let mut events = Vec::new();
    if let Some(arr) = body.get("events").and_then(|v| v.as_array()) {
        for e in arr.iter().take(8) {
            if let Some(name) = e
                .get("description")
                .or_else(|| e.get("name"))
                .and_then(|v| v.as_str())
            {
                events.push(name.to_string());
            }
        }
    }

    WorldStateSnapshot {
        earth,
        cetus,
        vallis,
        cambion,
        zariman,
        void_trader,
        fissures,
        events,
        fetched_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn cycle_from(
    id: &str,
    v: Option<&Value>,
    state_fn: impl Fn(&Value) -> &str,
) -> Option<CycleInfo> {
    let v = v?;
    Some(CycleInfo {
        id: id.into(),
        state: state_fn(v).to_string(),
        time_left: v
            .get("timeLeft")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        expiry: v
            .get("expiry")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    })
}

/// Shared service handle.
pub type SharedWorldState = Arc<WorldStateService>;
