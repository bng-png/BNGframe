use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::db::{Database, ItemRow, PriceCache};

#[derive(Debug, Deserialize)]
struct WfmItemsV2 {
    data: Option<Vec<WfmItemV2>>,
}

#[derive(Debug, Deserialize)]
struct WfmItemV2 {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    url_name: String,
    #[serde(default)]
    i18n: HashMap<String, WfmI18n>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WfmI18n {
    #[serde(default)]
    name: String,
    #[serde(default)]
    thumb: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

pub struct PricingService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
}

impl PricingService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("BNGframe/0.1 (+https://github.com/bng/BNGframe)")
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest");
        Self { client, db }
    }

    pub async fn ensure_items_cached(&self) -> Result<usize> {
        {
            let db = self.db.lock().await;
            let n = db.item_count()?;
            if n > 100 {
                return Ok(n);
            }
        }
        info!("Fetching warframe.market item catalog…");
        self.refresh_items().await
    }

    pub async fn refresh_items(&self) -> Result<usize> {
        let items = self.fetch_items_v2().await.context("WFM v2 items")?;
        let db = self.db.lock().await;
        for item in &items {
            db.upsert_item(item)?;
        }
        let n = db.item_count()?;
        info!("Cached {n} market items");
        Ok(n)
    }

    async fn fetch_items_v2(&self) -> Result<Vec<ItemRow>> {
        let resp = self
            .client
            .get("https://api.warframe.market/v2/items")
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await?
            .error_for_status()?;
        let body: WfmItemsV2 = resp.json().await?;
        let raw = body.data.unwrap_or_default();
        Ok(raw
            .into_iter()
            .filter_map(|i| {
                let url_name = if !i.slug.is_empty() {
                    i.slug
                } else {
                    i.url_name
                };
                if url_name.is_empty() {
                    return None;
                }
                let en = i.i18n.get("en");
                let name = en.map(|e| e.name.clone()).unwrap_or_default();
                if name.is_empty() {
                    return None;
                }
                let thumb = en.and_then(|e| e.thumb.clone().or(e.icon.clone()));
                Some(ItemRow {
                    url_name,
                    name,
                    thumb,
                    ducats: None,
                    set_url_name: None,
                    vaulted: i.tags.iter().any(|t| t == "vaulted").then_some(true),
                    mastery: None,
                })
            })
            .collect())
    }

    pub async fn price_for(&self, url_name: &str) -> Result<PriceCache> {
        {
            let db = self.db.lock().await;
            if let Some(p) = db.get_price(url_name)? {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&p.updated_at) {
                    let age = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
                    if age.num_minutes() < 10 {
                        return Ok(p);
                    }
                }
            }
        }

        let url = format!("https://api.warframe.market/v2/orders/item/{url_name}");
        let resp = self
            .client
            .get(&url)
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await?;

        if !resp.status().is_success() {
            warn!("orders fetch failed for {url_name}: {}", resp.status());
            return Ok(PriceCache {
                url_name: url_name.into(),
                platinum: 0.0,
                volume: 0,
                updated_at: Utc::now().to_rfc3339(),
            });
        }

        let body: Value = resp.json().await?;
        let orders = body
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut sells: Vec<f64> = orders
            .into_iter()
            .filter(|o| o.get("type").and_then(|v| v.as_str()) == Some("sell"))
            .filter(|o| o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true))
            .filter_map(|o| o.get("platinum").and_then(|v| v.as_f64()))
            .collect();

        let volume = sells.len() as i64;
        sells.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let platinum = if sells.is_empty() {
            0.0
        } else {
            let take = sells.len().min(5);
            sells[..take].iter().sum::<f64>() / take as f64
        };

        let cache = PriceCache {
            url_name: url_name.into(),
            platinum,
            volume,
            updated_at: Utc::now().to_rfc3339(),
        };
        {
            let db = self.db.lock().await;
            db.upsert_price(&cache)?;
        }
        tokio::time::sleep(Duration::from_millis(220)).await;
        Ok(cache)
    }

    pub async fn name_index(&self) -> Result<HashMap<String, ItemRow>> {
        let db = self.db.lock().await;
        let items = db.all_items()?;
        let mut map = HashMap::new();
        for item in items {
            map.insert(normalize_name(&item.name), item.clone());
            map.insert(item.name.to_lowercase(), item.clone());
            map.insert(item.url_name.clone(), item);
        }
        Ok(map)
    }

    /// Resolve a WFM slug for an inventory display/unique name.
    pub fn resolve_market_item(name: &str, catalog: &[crate::db::ItemRow]) -> Option<crate::db::ItemRow> {
        let index = Self::build_catalog_index(catalog);
        Self::resolve_market_item_indexed(name, &index)
    }

    pub fn build_catalog_index(
        catalog: &[crate::db::ItemRow],
    ) -> std::collections::HashMap<String, crate::db::ItemRow> {
        let mut map = std::collections::HashMap::new();
        for item in catalog {
            map.insert(normalize_name(&item.name), item.clone());
            map.insert(item.url_name.clone(), item.clone());
        }
        map
    }

    pub fn resolve_market_item_indexed(
        name: &str,
        by_norm: &std::collections::HashMap<String, crate::db::ItemRow>,
    ) -> Option<crate::db::ItemRow> {
        let human = humanize_lotus_name(name);
        let norm = normalize_name(&human);
        let slug = lotus_to_slug(name);

        if let Some(item) = by_norm.get(&norm) {
            return Some(item.clone());
        }
        if let Some(item) = by_norm.get(&slug) {
            return Some(item.clone());
        }
        for suf in ["_weapon", "_powersuit", "_blueprint"] {
            if let Some(stripped) = slug.strip_suffix(suf) {
                if let Some(item) = by_norm.get(stripped) {
                    return Some(item.clone());
                }
            }
        }
        None
    }

    /// Fetch & cache WFM prices for inventory rows that resolve to market items.
    pub async fn refresh_inventory_prices(&self, limit: usize) -> Result<PriceRefreshResult> {
        let _ = self.ensure_items_cached().await?;
        let (catalog, inventory) = {
            let db = self.db.lock().await;
            (db.all_items()?, db.list_inventory()?)
        };

        let mut targets: Vec<(String, String)> = Vec::new(); // (unique_name, url_name)
        let mut seen = std::collections::HashSet::new();
        for inv in &inventory {
            if let Some(item) = Self::resolve_market_item(&inv.name, &catalog) {
                if seen.insert(item.url_name.clone()) {
                    // Prefer likely-tradable: mods, blueprints, primes, sets
                    let score = tradable_priority(&inv.name, &inv.item_type, &item.url_name);
                    targets.push((inv.unique_name.clone(), item.url_name));
                    let _ = score;
                }
            }
        }

        // Stable priority: primes/mods first
        targets.sort_by(|a, b| {
            let sa = tradable_priority_slug(&a.1);
            let sb = tradable_priority_slug(&b.1);
            sb.cmp(&sa)
        });
        targets.truncate(limit.max(1));

        let mut priced = 0usize;
        let mut failed = 0usize;
        for (_unique, url) in &targets {
            match self.price_for(url).await {
                Ok(p) if p.platinum > 0.0 || p.volume > 0 => priced += 1,
                Ok(_) => priced += 1,
                Err(_) => failed += 1,
            }
        }

        // Rewrite inventory url_name for matched rows so UI joins work next time
        {
            let db = self.db.lock().await;
            for inv in &inventory {
                if let Some(item) = Self::resolve_market_item(&inv.name, &catalog) {
                    let mut updated = inv.clone();
                    updated.url_name = Some(item.url_name);
                    updated.ducats = item.ducats;
                    let _ = db.upsert_inventory(&updated);
                }
            }
        }

        Ok(PriceRefreshResult {
            matched: targets.len(),
            priced,
            failed,
            catalog_size: catalog.len(),
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriceRefreshResult {
    pub matched: usize,
    pub priced: usize,
    pub failed: usize,
    pub catalog_size: usize,
}

pub fn normalize_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `SarynPrime` / `MK1Bo` → `Saryn Prime` / `MK1 Bo`
pub fn humanize_lotus_name(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.replace('_', " ").chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase()
                    && c.is_uppercase()
                    && next.is_some_and(|n| n.is_lowercase()));
            if boundary && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lotus_to_slug(name: &str) -> String {
    normalize_name(&humanize_lotus_name(name)).replace(' ', "_")
}

fn tradable_priority(name: &str, item_type: &str, slug: &str) -> i32 {
    let n = name.to_lowercase();
    let mut s = 0;
    if n.contains("prime") || slug.contains("prime") {
        s += 50;
    }
    if item_type == "mod" || slug.contains("mod") {
        s += 30;
    }
    if n.contains("blueprint") || slug.contains("blueprint") {
        s += 20;
    }
    if item_type == "misc" {
        s += 5;
    }
    s
}

fn tradable_priority_slug(slug: &str) -> i32 {
    let mut s = 0;
    if slug.contains("prime") {
        s += 50;
    }
    if slug.contains("set") {
        s += 40;
    }
    if slug.contains("blueprint") {
        s += 20;
    }
    s
}
