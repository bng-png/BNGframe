use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::db::{Database, ItemRow};
use crate::pricing::{normalize_name, relic_trade_name};

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

struct RelicCache {
    fetched_at: Instant,
    relics: Vec<RelicInfo>,
}

pub struct RelicService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
    cache: Mutex<Option<RelicCache>>,
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
            cache: Mutex::new(None),
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
        let mut relics = self.cached_relics().await;

        let (inventory, catalog, prices, lotus_relics) = {
            let db = self.db.lock().await;
            let inventory = db.list_inventory().unwrap_or_default();
            let catalog = db.all_items().unwrap_or_default();
            let mut prices = HashMap::new();
            if let Ok(list) = db.list_all_prices() {
                for p in list {
                    prices.insert(p.url_name, p.platinum);
                }
            }
            let mut lotus_relics = HashMap::new();
            if let Ok(m) = db.lotus_name_map("relic") {
                lotus_relics.extend(m);
            }
            // English fallbacks sometimes live under weapon_en / resource
            if let Ok(m) = db.lotus_name_map("weapon_en") {
                for (k, v) in m {
                    if k.contains("VoidProjection") || k.contains("/Relics/") {
                        lotus_relics.entry(k).or_insert(v);
                    }
                }
            }
            (inventory, catalog, prices, lotus_relics)
        };

        let by_norm = build_name_index(&catalog);

        // Pre-index owned relics: "lith g6" → count (from Lotus / display / url).
        let mut owned_relics: HashMap<String, i64> = HashMap::new();
        for i in &inventory {
            for key in inventory_relic_match_keys(i, &lotus_relics) {
                *owned_relics.entry(key).or_insert(0) += i.count;
            }
        }

        let mut out = Vec::new();
        for mut relic in relics.drain(..) {
            let name_l = relic.name.to_lowercase();
            let match_key = planner_relic_key(&relic.name);
            relic.owned = owned_relics.get(&match_key).copied().unwrap_or(0);
            if relic.owned == 0 {
                // Fallback: substring match on enriched names
                relic.owned = inventory
                    .iter()
                    .filter(|i| {
                        let n = i.name.to_lowercase();
                        n.contains(&name_l)
                            || inventory_relic_match_keys(i, &lotus_relics)
                                .iter()
                                .any(|k| k == &match_key)
                    })
                    .map(|i| i.count)
                    .sum();
            }

            for drop in &mut relic.drops {
                if let Some(item) = lookup_drop_item(&drop.item_name, &by_norm) {
                    drop.ducats = item.ducats;
                    drop.platinum = prices.get(&item.url_name).copied();
                }
                let drop_l = drop.item_name.to_lowercase();
                let drop_core = drop_l
                    .replace(" blueprint", "")
                    .replace(" systems", "")
                    .replace(" chassis", "")
                    .replace(" neuroptics", "");
                drop.owned = inventory.iter().any(|i| {
                    i.name.eq_ignore_ascii_case(&drop.item_name)
                        || (i.mastered && i.name.to_lowercase().contains(&drop_core))
                });
                let first = drop_l.split_whitespace().next().unwrap_or("");
                drop.mastered = !first.is_empty()
                    && inventory
                        .iter()
                        .any(|i| i.mastered && i.name.to_lowercase().contains(first));
            }

            relic.score = score_relic(&relic, planner);
            relic.score_label = planner.order_mode.clone();
            relic.favorite = inventory
                .iter()
                .any(|i| i.favorite && i.name.to_lowercase().contains(&name_l));
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

    async fn cached_relics(&self) -> Vec<RelicInfo> {
        {
            let guard = self.cache.lock().await;
            if let Some(c) = guard.as_ref() {
                if c.fetched_at.elapsed() < Duration::from_secs(6 * 3600) {
                    return c.relics.clone();
                }
            }
        }
        let relics = self.fetch_relics().await.unwrap_or_else(|e| {
            warn!("relic fetch failed: {e}");
            demo_relics()
        });
        let mut guard = self.cache.lock().await;
        *guard = Some(RelicCache {
            fetched_at: Instant::now(),
            relics: relics.clone(),
        });
        relics
    }

    async fn fetch_relics(&self) -> Result<Vec<RelicInfo>> {
        // Official drop tables via WFCD (api.warframestat.us/relics was removed → 404).
        let resp = self
            .client
            .get("https://drops.warframestat.us/data/relics.json")
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("drops.warframestat relics {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let arr = body
            .get("relics")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_else(|| body.as_array().cloned().unwrap_or_default());
        info!("Fetched {} relic rows from drops.warframestat", arr.len());

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for r in arr {
            let state = r
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("Intact");
            // One planner card per relic — Intact drop rates.
            if !state.eq_ignore_ascii_case("Intact") {
                continue;
            }
            let tier = r
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let relic_name = r
                .get("relicName")
                .or_else(|| r.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if tier.is_empty() || relic_name.is_empty() {
                continue;
            }
            let name = if relic_name
                .to_ascii_lowercase()
                .starts_with(&tier.to_ascii_lowercase())
            {
                relic_name.clone()
            } else {
                format!("{tier} {relic_name}")
            };
            let key = name.to_ascii_lowercase();
            if !seen.insert(key) {
                continue;
            }

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

fn build_name_index(catalog: &[ItemRow]) -> HashMap<String, &ItemRow> {
    let mut map = HashMap::new();
    for item in catalog {
        for label in [Some(item.name.as_str()), item.name_ru.as_deref()]
            .into_iter()
            .flatten()
        {
            let key = normalize_name(label);
            map.entry(key).or_insert(item);
        }
    }
    map
}

fn planner_relic_key(name: &str) -> String {
    normalize_name(name)
        .trim_end_matches(" relic")
        .trim()
        .to_string()
}

fn inventory_relic_match_keys(
    item: &crate::db::InventoryItem,
    lotus_relics: &HashMap<String, String>,
) -> Vec<String> {
    let mut keys = Vec::new();

    let bare = item
        .unique_name
        .split('#')
        .next()
        .unwrap_or(item.unique_name.as_str());
    // Lotus: T1VoidProjection… → "Lith G6 Intact" → "lith g6"
    if let Some(en) = lotus_relics.get(bare) {
        if let Some(trade) = relic_trade_name(en) {
            keys.push(planner_relic_key(&trade));
        }
        keys.push(planner_relic_key(en));
    }

    if let Some(url) = item.url_name.as_deref() {
        if url.ends_with("_relic") {
            let stem = url
                .trim_end_matches("_relic")
                .replace('_', " ")
                .to_lowercase();
            if !stem.is_empty() && !stem.starts_with('t') {
                keys.push(normalize_name(&stem));
            }
        }
    }
    // «Реликвия Лит G6 · нетронутая» / «Lith G6 Relic»
    let name = item.name.to_lowercase();
    let name = name
        .replace('·', " ")
        .replace("реликвия", " ")
        .replace("relic", " ");
    let mut tokens: Vec<String> = name
        .split_whitespace()
        .filter(|t| {
            !matches!(
                *t,
                "нетронутая"
                    | "необычная"
                    | "безупречная"
                    | "сияющая"
                    | "intact"
                    | "exceptional"
                    | "flawless"
                    | "radiant"
                    | "void"
                    | "projection"
                    | "bronze"
                    | "silver"
                    | "gold"
            )
        })
        .map(|t| match t {
            "лит" => "lith".into(),
            "мезо" => "meso".into(),
            "нео" => "neo".into(),
            "акси" => "axi".into(),
            "реквием" => "requiem".into(),
            "t1" => "lith".into(),
            "t2" => "meso".into(),
            "t3" => "neo".into(),
            "t4" => "axi".into(),
            "t5" => "requiem".into(),
            other => other.to_string(),
        })
        .collect();
    if tokens.len() >= 2 {
        keys.push(normalize_name(&tokens.join(" ")));
    }
    if tokens.len() > 2 {
        tokens.truncate(2);
        keys.push(normalize_name(&tokens.join(" ")));
    }
    keys
}

fn lookup_drop_item<'a>(
    item_name: &str,
    by_norm: &HashMap<String, &'a ItemRow>,
) -> Option<&'a ItemRow> {
    let key = normalize_name(item_name);
    if let Some(item) = by_norm.get(&key) {
        return Some(*item);
    }
    // "X Blueprint" ↔ catalog without / with blueprint
    if let Some(stripped) = key.strip_suffix(" blueprint") {
        if let Some(item) = by_norm.get(stripped) {
            return Some(*item);
        }
        if let Some(item) = by_norm.get(&format!("{stripped} blueprint")) {
            return Some(*item);
        }
    } else if let Some(item) = by_norm.get(&format!("{key} blueprint")) {
        return Some(*item);
    }
    None
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
    vec![RelicInfo {
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
    }]
}
