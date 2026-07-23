use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::db::Database;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelicInfo {
    pub name: String,
    pub tier: String,
    pub refinement: String,
    pub owned: i64,
    pub drops: Vec<RelicDrop>,
    pub score: f64,
    pub score_label: String,
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelicDrop {
    pub item_name: String,
    pub rarity: String,
    pub chance: f64,
    pub platinum: Option<f64>,
    pub ducats: Option<i64>,
    pub owned: bool,
    pub mastered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelicPlannerConfig {
    pub order_mode: String,
    pub favorites_first: bool,
    pub min_plat: f64,
    pub ducat_focus: bool,
    pub mr_focus: bool,
}

impl Default for RelicPlannerConfig {
    fn default() -> Self {
        Self {
            order_mode: "ducats_profit".into(),
            favorites_first: false,
            min_plat: 0.0,
            ducat_focus: true,
            mr_focus: false,
        }
    }
}

pub struct RelicService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
}

impl RelicService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("BNGframe/0.1")
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("client"),
            db,
        }
    }

    pub async fn save_planner(&self, cfg: &RelicPlannerConfig) -> Result<()> {
        let db = self.db.lock().await;
        db.save_relic_planner(&serde_json::to_string(cfg)?, &cfg.order_mode)?;
        Ok(())
    }

    pub async fn load_planner(&self) -> Result<RelicPlannerConfig> {
        let db = self.db.lock().await;
        let (json, mode) = db.load_relic_planner()?;
        let mut cfg: RelicPlannerConfig = serde_json::from_str(&json).unwrap_or_default();
        if !mode.is_empty() {
            cfg.order_mode = mode;
        }
        Ok(cfg)
    }

    pub async fn plan(&self, planner: &RelicPlannerConfig) -> Result<Vec<RelicInfo>> {
        let relics = self.fetch_relics().await.unwrap_or_else(|e| {
            warn!("relic fetch failed: {e}");
            demo_relics()
        });

        let inventory = {
            let db = self.db.lock().await;
            db.list_inventory().unwrap_or_default()
        };

        let mut out = Vec::new();
        for mut relic in relics {
            // ownership from inventory names containing relic name
            relic.owned = inventory
                .iter()
                .filter(|i| i.name.to_lowercase().contains(&relic.name.to_lowercase())
                    || i.unique_name.to_lowercase().contains(&relic.name.to_lowercase().replace(' ', "")))
                .map(|i| i.count)
                .sum();

            for drop in &mut relic.drops {
                if let Ok(Some(item)) = self.db.lock().await.find_item_by_name(&drop.item_name) {
                    drop.ducats = item.ducats;
                    if let Ok(Some(p)) = self.db.lock().await.get_price(&item.url_name) {
                        drop.platinum = Some(p.platinum);
                    }
                }
                drop.owned = inventory.iter().any(|i| {
                    i.name.eq_ignore_ascii_case(&drop.item_name) || i.mastered
                        && i.name.to_lowercase().contains(
                            &drop
                                .item_name
                                .to_lowercase()
                                .replace(" blueprint", "")
                                .replace(" systems", "")
                                .replace(" chassis", "")
                                .replace(" neuroptics", ""),
                        )
                });
                drop.mastered = inventory.iter().any(|i| {
                    i.mastered
                        && i.name.to_lowercase().contains(
                            &drop.item_name.to_lowercase().split_whitespace().next().unwrap_or(""),
                        )
                });
            }

            relic.score = score_relic(&relic, planner);
            relic.score_label = planner.order_mode.clone();
            relic.favorite = inventory.iter().any(|i| {
                i.favorite && i.name.to_lowercase().contains(&relic.name.to_lowercase())
            });
            out.push(relic);
        }

        out.sort_by(|a, b| {
            if planner.favorites_first && a.favorite != b.favorite {
                return b.favorite.cmp(&a.favorite);
            }
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    async fn fetch_relics(&self) -> Result<Vec<RelicInfo>> {
        // warframestat relics endpoint
        let resp = self
            .client
            .get("https://api.warframestat.us/relics")
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("warframestat relics {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let arr = body.as_array().cloned().unwrap_or_default();
        info!("Fetched {} relics from warframestat", arr.len());
        let mut out = Vec::new();
        for r in arr.into_iter().take(400) {
            let name = r
                .get("name")
                .or_else(|| r.get("relicName"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let tier = r
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut drops = Vec::new();
            if let Some(rewards) = r.get("rewards").and_then(|v| v.as_array()) {
                for reward in rewards {
                    let item_name = reward
                        .get("itemName")
                        .or_else(|| reward.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if item_name.is_empty() {
                        continue;
                    }
                    drops.push(RelicDrop {
                        item_name,
                        rarity: reward
                            .get("rarity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        chance: reward.get("chance").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        platinum: None,
                        ducats: None,
                        owned: false,
                        mastered: false,
                    });
                }
            }
            out.push(RelicInfo {
                name,
                tier,
                refinement: "Intact".into(),
                owned: 0,
                drops,
                score: 0.0,
                score_label: String::new(),
                favorite: false,
            });
        }
        Ok(out)
    }
}

fn score_relic(relic: &RelicInfo, planner: &RelicPlannerConfig) -> f64 {
    let mut score = 0.0;
    for d in &relic.drops {
        let plat = d.platinum.unwrap_or(0.0);
        let ducats = d.ducats.unwrap_or(0) as f64;
        let weight = match d.rarity.to_lowercase().as_str() {
            "rare" => 0.02,
            "uncommon" => 0.11,
            _ => 0.25,
        };
        match planner.order_mode.as_str() {
            "best_for_mr" => {
                if !d.mastered {
                    score += weight * (10.0 + plat);
                }
            }
            "platinum" => {
                if plat >= planner.min_plat {
                    score += weight * plat;
                }
            }
            _ => {
                // ducats_profit
                score += weight * (ducats + plat * 0.5);
            }
        }
    }
    if relic.owned > 0 {
        score += 1.0;
    }
    score
}

fn demo_relics() -> Vec<RelicInfo> {
    vec![
        RelicInfo {
            name: "Lith A1".into(),
            tier: "Lith".into(),
            refinement: "Intact".into(),
            owned: 0,
            drops: vec![
                RelicDrop {
                    item_name: "Forma Blueprint".into(),
                    rarity: "Uncommon".into(),
                    chance: 11.0,
                    platinum: Some(0.0),
                    ducats: Some(0),
                    owned: false,
                    mastered: false,
                },
                RelicDrop {
                    item_name: "Ash Prime Systems".into(),
                    rarity: "Rare".into(),
                    chance: 2.0,
                    platinum: Some(45.0),
                    ducats: Some(100),
                    owned: false,
                    mastered: false,
                },
            ],
            score: 0.0,
            score_label: String::new(),
            favorite: false,
        },
    ]
}
