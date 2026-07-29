use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::db::{Database, ItemRow, PriceCache};

/// Whole gear item required by a blueprint (e.g. Bronco Prime ×2 for Akbronco).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftGearIng {
    pub unique: String,
    pub count: i64,
}

#[derive(Debug, Deserialize)]
struct WfmItemsV2 {
    data: Option<Vec<WfmItemV2>>,
}

#[derive(Debug, Deserialize)]
struct WfmItemV2 {
    #[serde(default)]
    id: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    url_name: String,
    #[serde(default)]
    i18n: HashMap<String, WfmI18n>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    ducats: Option<i64>,
    /// Present on v2 `/items` for vaulted relics (and some other rows).
    #[serde(default)]
    vaulted: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct WfmDucatsPayload {
    payload: WfmDucatsBody,
}

#[derive(Debug, Deserialize)]
struct WfmDucatsBody {
    #[serde(default)]
    previous_hour: Vec<WfmDucatsRow>,
    #[serde(default)]
    previous_day: Vec<WfmDucatsRow>,
}

#[derive(Debug, Deserialize)]
struct WfmDucatsRow {
    /// WFM item id (maps to v2 `/items` `id`).
    item: String,
    #[serde(default)]
    median: Option<f64>,
    #[serde(default)]
    wa_price: Option<f64>,
    #[serde(default)]
    volume: Option<i64>,
    #[serde(default)]
    ducats: Option<i64>,
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
    /// Serialize live order fetches to stay under WFM rate limits.
    order_gate: Arc<tokio::sync::Mutex<()>>,
}

impl PricingService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("BNGframe/0.1 (+https://github.com/bng/BNGframe)")
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest");
        Self {
            client,
            db,
            order_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn ensure_items_cached(&self) -> Result<usize> {
        {
            let db = self.db.lock().await;
            let n = db.item_count()?;
            let ru = db.name_ru_count().unwrap_or(0);
            let ducats = db.ducats_count().unwrap_or(0);
            let vaulted = db.vaulted_count().unwrap_or(0);
            // Re-fetch if catalog incomplete (RU / ducats / vaulted flags missing).
            if n > 100 && ru > (n * 8 / 10) && ducats > 100 && vaulted > 50 {
                return Ok(n);
            }
        }
        info!("Fetching warframe.market item catalog…");
        self.refresh_items().await
    }

    /// Warm ducat values (+ optional plat seed) from WFM Ducats tool.
    /// Platinum from this endpoint is a median — only used as a last-resort seed
    /// when we have no quote yet; live prices always come from order book
    /// (avg of 3 cheapest sells) via [`Self::price_for`].
    pub async fn warm_ducats_prices(&self) -> Result<usize> {
        let _ = self.ensure_items_cached().await?;
        info!("Fetching warframe.market ducats tool prices…");
        let rows = self.fetch_ducats_tool().await.context("WFM ducats tool")?;
        if rows.is_empty() {
            anyhow::bail!("ducats tool returned no rows");
        }
        let id_to_slug = self
            .fetch_item_id_slug_map()
            .await
            .context("WFM v2 id→slug map")?;
        // Seed older than the fresh window so the next price_for hits the order book.
        let seed_at = (Utc::now() - chrono::Duration::minutes(crate::db::PRICE_FRESH_MINS + 1))
            .to_rfc3339();
        let mut prices = Vec::new();
        let mut ducat_updates = Vec::new();
        let mut skipped = 0usize;
        let mut seeded = 0usize;
        {
            let db = self.db.lock().await;
            for row in &rows {
                let Some(slug) = id_to_slug.get(&row.item) else {
                    skipped += 1;
                    continue;
                };
                if let Some(d) = row.ducats.filter(|d| *d > 0) {
                    ducat_updates.push((slug.clone(), d));
                }
                // Don't overwrite a real order-book quote with the ducats median.
                if db.get_price_cached(slug).ok().flatten().is_some() {
                    continue;
                }
                let plat = row
                    .median
                    .filter(|p| *p > 0.0)
                    .or_else(|| row.wa_price.filter(|p| *p > 0.0))
                    .unwrap_or(0.0);
                if plat <= 0.0 {
                    skipped += 1;
                    continue;
                }
                prices.push(PriceCache {
                    url_name: slug.clone(),
                    platinum: plat,
                    volume: row.volume.unwrap_or(0),
                    updated_at: seed_at.clone(),
                });
                seeded += 1;
            }
            db.upsert_prices_batch(&prices)?;
            db.update_item_ducats_batch(&ducat_updates)?;
        }
        info!(
            "Ducats warm: {} ducat rows, seeded {seeded} plat quotes (order-book refresh pending), skipped {skipped}",
            ducat_updates.len()
        );
        Ok(seeded)
    }

    async fn fetch_ducats_tool(&self) -> Result<Vec<WfmDucatsRow>> {
        let resp = self
            .client
            .get("https://api.warframe.market/v1/tools/ducats")
            .header("Language", "ru")
            .header("Platform", "pc")
            .send()
            .await?
            .error_for_status()?;
        let body: WfmDucatsPayload = resp.json().await?;
        // Prefer previous_hour (fresher); fall back to day snapshot.
        let rows = if !body.payload.previous_hour.is_empty() {
            body.payload.previous_hour
        } else {
            body.payload.previous_day
        };
        Ok(rows)
    }

    async fn fetch_item_id_slug_map(&self) -> Result<HashMap<String, String>> {
        let resp = self
            .client
            .get("https://api.warframe.market/v2/items")
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await?
            .error_for_status()?;
        let body: WfmItemsV2 = resp.json().await?;
        let mut map = HashMap::new();
        for i in body.data.unwrap_or_default() {
            if i.id.is_empty() {
                continue;
            }
            let slug = if !i.slug.is_empty() {
                i.slug
            } else {
                i.url_name
            };
            if slug.is_empty() {
                continue;
            }
            map.insert(i.id, slug);
        }
        Ok(map)
    }

    pub async fn refresh_items(&self) -> Result<usize> {
        let mut items = self.fetch_items_v2().await.context("WFM v2 items")?;
        if let Err(e) = self.enrich_vaulted_from_wfcd(&mut items).await {
            warn!("WFCD vaulted enrich: {e:#}");
        }
        propagate_vaulted_to_set_parts(&mut items);
        let db = self.db.lock().await;
        db.upsert_items_batch(&items)?;
        let n = db.item_count()?;
        let ru = db.name_ru_count().unwrap_or(0);
        let vaulted = db.vaulted_count().unwrap_or(0);
        info!("Cached {n} market items ({ru} with name_ru, {vaulted} vaulted)");
        Ok(n)
    }

    /// Cache WFCD Mods/Weapons/Warframes/Arcanes/Relics so inventory Lotus paths map to names.
    pub async fn ensure_lotus_mod_names(&self) -> Result<HashMap<String, String>> {
        {
            let db = self.db.lock().await;
            let mods = db.lotus_name_count("mod").unwrap_or(0);
            let weapons = db.lotus_name_count("weapon").unwrap_or(0);
            let frames = db.lotus_name_count("warframe").unwrap_or(0);
            let arcanes = db.lotus_name_count("arcane").unwrap_or(0);
            let relics = db.lotus_name_count("relic").unwrap_or(0);
            let flavour = db.lotus_name_count("flavour").unwrap_or(0);
            let sentinels = db.lotus_name_count("sentinel").unwrap_or(0);
            // Warframe + weapon names must be Russian DE Export (Cyrillic), not WFCD EN
            let frames_ru = frames > 40 && db.lotus_names_look_russian("warframe").unwrap_or(false);
            let weapons_ru =
                weapons > 200 && db.lotus_names_look_russian("weapon").unwrap_or(false);
            let sentinels_ru =
                sentinels > 20 && db.lotus_names_look_russian("sentinel").unwrap_or(false);
            if mods > 500
                && weapons_ru
                && frames_ru
                && arcanes > 100
                && relics > 500
                && flavour > 100
                && sentinels_ru
            {
                return db.lotus_name_map_all();
            }
        }
        info!("Fetching WFCD/DE item names for Lotus→name mapping…");
        self.refresh_lotus_mod_names().await?;
        self.refresh_lotus_weapon_names().await?;
        self.refresh_lotus_warframe_names().await?;
        self.refresh_lotus_arcane_names().await?;
        self.refresh_lotus_relic_names().await?;
        let _ = self.refresh_lotus_flavour_names().await;
        let _ = self.refresh_lotus_extra_ru_names().await;
        let _ = self.refresh_craft_gear_recipes().await;
        let db = self.db.lock().await;
        db.lotus_name_map_all()
    }

    /// Sentinels, resources, gear, drones, customs — Russian DE labels for mastery/inventory.
    pub async fn refresh_lotus_extra_ru_names(&self) -> Result<usize> {
        let mut total = 0usize;
        for (file, key, kind) in [
            ("ExportSentinels_ru.json", "ExportSentinels", "sentinel"),
            ("ExportResources_ru.json", "ExportResources", "resource"),
            ("ExportGear_ru.json", "ExportGear", "gear"),
            ("ExportDrones_ru.json", "ExportDrones", "drone"),
            ("ExportCustoms_ru.json", "ExportCustoms", "custom"),
            ("ExportKeys_ru.json", "ExportKeys", "key"),
        ] {
            match self.fetch_de_export_name_rows(file, key).await {
                Ok(rows) if !rows.is_empty() => {
                    let mut dedup = HashMap::new();
                    for (u, n) in rows {
                        dedup.insert(u, n);
                    }
                    let rows: Vec<_> = dedup.into_iter().collect();
                    let n = rows.len();
                    let db = self.db.lock().await;
                    db.replace_lotus_names(kind, &rows)?;
                    info!("Cached {n} Lotus {kind} names (DE Export ru)");
                    total += n;
                }
                Ok(_) => warn!("DE {file} empty"),
                Err(e) => warn!("DE {file}: {e:#}"),
            }
        }
        Ok(total)
    }

    pub async fn refresh_lotus_mod_names(&self) -> Result<HashMap<String, String>> {
        let rows = self
            .fetch_wfcd_name_rows(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/Mods.json",
            )
            .await?;
        let db = self.db.lock().await;
        db.replace_lotus_names("mod", &rows)?;
        let map = db.lotus_name_map("mod")?;
        info!("Cached {} Lotus mod names", map.len());
        Ok(map)
    }

    pub async fn refresh_lotus_weapon_names(&self) -> Result<usize> {
        // English slugs for market/wiki set_key (Acceltra → acceltra)
        // + blueprint uniqueName → product uniqueName (BowPrimeBlueprint → Paris Prime)
        let mut en_rows = Vec::new();
        let mut bp_map: HashMap<String, String> = HashMap::new();
        for file in [
            "Primary.json",
            "Secondary.json",
            "Melee.json",
            "Arch-Gun.json",
            "Arch-Melee.json",
            "SentinelWeapons.json",
            "Sentinels.json",
            "Warframes.json",
        ] {
            let url = format!(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/{file}"
            );
            match self.fetch_wfcd_name_rows_and_blueprints(&url).await {
                Ok((mut part, bps)) => {
                    en_rows.append(&mut part);
                    bp_map.extend(bps);
                }
                Err(e) => warn!("WFCD {file}: {e}"),
            }
        }
        {
            let db = self.db.lock().await;
            db.replace_lotus_names("weapon_en", &en_rows)?;
            if !bp_map.is_empty() {
                let raw = serde_json::to_string(&bp_map)?;
                db.set_setting("weapon_blueprint_map", &raw)?;
                info!("Cached {} blueprint→product links", bp_map.len());
            }
        }

        match self
            .fetch_de_export_name_rows("ExportWeapons_ru.json", "ExportWeapons")
            .await
        {
            Ok(rows) if rows.len() > 100 => {
                let mut dedup = std::collections::HashMap::new();
                for (u, n) in rows {
                    dedup.insert(u, n);
                }
                let rows: Vec<(String, String)> = dedup.into_iter().collect();
                let n = rows.len();
                let db = self.db.lock().await;
                db.replace_lotus_names("weapon", &rows)?;
                info!("Cached {n} Lotus weapon names (DE Export ru) + {} EN slugs", en_rows.len());
                return Ok(n);
            }
            Ok(rows) => warn!("DE ExportWeapons_ru too small ({})", rows.len()),
            Err(e) => warn!("DE ExportWeapons_ru: {e:#}"),
        }

        let n = en_rows.len();
        let db = self.db.lock().await;
        db.replace_lotus_names("weapon", &en_rows)?;
        info!("Cached {n} Lotus weapon names (WFCD EN fallback)");
        Ok(n)
    }

    pub async fn weapon_blueprint_map(&self) -> HashMap<String, String> {
        {
            let db = self.db.lock().await;
            if let Ok(Some(raw)) = db.get_setting("weapon_blueprint_map") {
                if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
                    if map.len() > 50 {
                        return map;
                    }
                }
            }
        }
        let _ = self.refresh_lotus_weapon_names().await;
        let db = self.db.lock().await;
        db.get_setting("weapon_blueprint_map")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub async fn refresh_lotus_warframe_names(&self) -> Result<usize> {
        match self
            .fetch_de_export_name_rows("ExportWarframes_ru.json", "ExportWarframes")
            .await
        {
            Ok(rows) if rows.len() > 40 => {
                let n = rows.len();
                let db = self.db.lock().await;
                db.replace_lotus_names("warframe", &rows)?;
                info!("Cached {n} Lotus warframe names (DE Export ru)");
                return Ok(n);
            }
            Ok(rows) => warn!("DE ExportWarframes_ru too small ({})", rows.len()),
            Err(e) => warn!("DE ExportWarframes_ru: {e:#}"),
        }
        let rows = self
            .fetch_wfcd_name_rows(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/Warframes.json",
            )
            .await?;
        let n = rows.len();
        let db = self.db.lock().await;
        db.replace_lotus_names("warframe", &rows)?;
        info!("Cached {n} Lotus warframe names");
        Ok(n)
    }

    pub async fn refresh_lotus_flavour_names(&self) -> Result<usize> {
        let rows = self
            .fetch_de_export_name_rows("ExportFlavour_ru.json", "ExportFlavour")
            .await
            .unwrap_or_default();
        let n = rows.len();
        if n > 0 {
            let db = self.db.lock().await;
            db.replace_lotus_names("flavour", &rows)?;
            info!("Cached {n} Lotus flavour names (DE Export ru)");
        }
        Ok(n)
    }

    /// Cache blueprint → gear ingredients (e.g. Akbronco needs 2× Bronco Prime).
    pub async fn refresh_craft_gear_recipes(&self) -> Result<usize> {
        {
            let db = self.db.lock().await;
            if let Ok(Some(raw)) = db.get_setting("craft_gear_recipes") {
                if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<CraftGearIng>>>(&raw) {
                    if map.len() >= 10 {
                        return Ok(map.len());
                    }
                }
            }
        }
        info!("Fetching DE ExportRecipes for gear craft components…");
        let v = self
            .fetch_de_export_json("ExportRecipes_en.json")
            .await
            .context("ExportRecipes_en")?;
        let arr = v
            .get("ExportRecipes")
            .and_then(|x| x.as_array())
            .context("ExportRecipes array")?;
        let mut map: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for recipe in arr {
            let unique = recipe
                .get("uniqueName")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if unique.is_empty() {
                continue;
            }
            let Some(set_key) = set_key_from_blueprint_unique(unique) else {
                continue;
            };
            let Some(ings) = recipe.get("ingredients").and_then(|x| x.as_array()) else {
                continue;
            };
            for ing in ings {
                let cat = ing
                    .get("ProductCategory")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if !is_gear_product_category(cat) {
                    continue;
                }
                let item = ing
                    .get("ItemType")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if item.is_empty() {
                    continue;
                }
                let n = ing
                    .get("ItemCount")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(1)
                    .max(1);
                let slot = map.entry(set_key.clone()).or_default();
                *slot.entry(item.to_string()).or_insert(0) += n;
            }
        }
        let out: HashMap<String, Vec<CraftGearIng>> = map
            .into_iter()
            .map(|(k, ings)| {
                let mut list: Vec<CraftGearIng> = ings
                    .into_iter()
                    .map(|(unique, count)| CraftGearIng { unique, count })
                    .collect();
                list.sort_by(|a, b| a.unique.cmp(&b.unique));
                (k, list)
            })
            .filter(|(_, list)| !list.is_empty())
            .collect();
        let n = out.len();
        let raw = serde_json::to_string(&out)?;
        {
            let db = self.db.lock().await;
            db.set_setting("craft_gear_recipes", &raw)?;
        }
        info!("Cached craft gear recipes for {n} sets");
        Ok(n)
    }

    /// set_key → whole-item craft ingredients (weapons used as materials).
    pub async fn craft_gear_map(&self) -> HashMap<String, Vec<CraftGearIng>> {
        {
            let db = self.db.lock().await;
            if let Ok(Some(raw)) = db.get_setting("craft_gear_recipes") {
                if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<CraftGearIng>>>(&raw) {
                    if !map.is_empty() {
                        return map;
                    }
                }
            }
        }
        let _ = self.refresh_craft_gear_recipes().await;
        let db = self.db.lock().await;
        db.get_setting("craft_gear_recipes")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    async fn fetch_de_export_json(&self, index_entry_prefix: &str) -> Result<Value> {
        let index_bytes = self
            .client
            .get("https://content.warframe.com/PublicExport/index_en.txt.lzma")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let index_text = decompress_public_export_lzma(&index_bytes)
            .context("decompress PublicExport index_en")?;
        let manifest_line = index_text
            .lines()
            .map(|l| l.trim().trim_end_matches('\r'))
            .find(|l| l.starts_with(index_entry_prefix))
            .with_context(|| format!("{index_entry_prefix} missing from index_en"))?
            .to_string();
        let encoded = manifest_line.replace('!', "%21");
        let url = format!("https://content.warframe.com/PublicExport/Manifest/{encoded}");
        let body_bytes = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let body_text = decompress_public_export_lzma(&body_bytes)
            .or_else(|_| String::from_utf8(body_bytes.to_vec()).context("export not utf-8"))
            .with_context(|| format!("decode {index_entry_prefix}"))?;
        serde_json::from_str(&body_text).with_context(|| format!("parse {index_entry_prefix}"))
    }

    pub async fn refresh_lotus_arcane_names(&self) -> Result<usize> {
        let rows = self
            .fetch_wfcd_name_rows(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/Arcanes.json",
            )
            .await?;
        let n = rows.len();
        let db = self.db.lock().await;
        db.replace_lotus_names("arcane", &rows)?;
        info!("Cached {n} Lotus arcane names");
        Ok(n)
    }

    pub async fn refresh_lotus_relic_names(&self) -> Result<usize> {
        let rows = self
            .fetch_wfcd_name_rows(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/Relics.json",
            )
            .await?;
        let n = rows.len();
        let db = self.db.lock().await;
        db.replace_lotus_names("relic", &rows)?;
        info!("Cached {n} Lotus relic names");
        Ok(n)
    }

    async fn fetch_wfcd_name_rows(&self, url: &str) -> Result<Vec<(String, String)>> {
        let (rows, _) = self.fetch_wfcd_name_rows_and_blueprints(url).await?;
        Ok(rows)
    }

    async fn fetch_wfcd_name_rows_and_blueprints(
        &self,
        url: &str,
    ) -> Result<(Vec<(String, String)>, HashMap<String, String>)> {
        let resp = self.client.get(url).send().await?.error_for_status()?;
        let items: Vec<Value> = resp.json().await.with_context(|| format!("parse {url}"))?;
        let mut rows = Vec::new();
        let mut bps = HashMap::new();
        for item in items {
            let unique = item
                .get("uniqueName")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if unique.is_empty() || name.is_empty() {
                continue;
            }
            rows.push((unique.to_string(), name.to_string()));
            if let Some(comps) = item.get("components").and_then(|v| v.as_array()) {
                for c in comps {
                    let cu = c.get("uniqueName").and_then(|v| v.as_str()).unwrap_or("");
                    let cn = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if cu.is_empty() {
                        continue;
                    }
                    let is_bp = cu.to_ascii_lowercase().contains("blueprint")
                        || cn.eq_ignore_ascii_case("blueprint");
                    if is_bp {
                        bps.insert(cu.to_string(), unique.to_string());
                    }
                }
            }
        }
        Ok((rows, bps))
    }

    /// Official DE PublicExport (`index_ru`) → uniqueName / localized name pairs.
    async fn fetch_de_export_name_rows(
        &self,
        index_entry_prefix: &str,
        json_key: &str,
    ) -> Result<Vec<(String, String)>> {
        let index_bytes = self
            .client
            .get("https://content.warframe.com/PublicExport/index_ru.txt.lzma")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let index_text = decompress_public_export_lzma(&index_bytes)
            .context("decompress PublicExport index_ru")?;
        let manifest_line = index_text
            .lines()
            .map(|l| l.trim().trim_end_matches('\r'))
            .find(|l| l.starts_with(index_entry_prefix))
            .with_context(|| format!("{index_entry_prefix} missing from index_ru"))?
            .to_string();
        // `!` in DE hashes must be encoded — raw `!` can stall some HTTP/2 paths
        let encoded = manifest_line.replace('!', "%21");
        let url = format!("https://content.warframe.com/PublicExport/Manifest/{encoded}");
        info!("Fetching DE PublicExport {index_entry_prefix}…");
        let body_bytes = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("status {url}"))?
            .bytes()
            .await
            .with_context(|| format!("body {url}"))?;
        info!(
            "Downloaded {index_entry_prefix} ({} bytes, head={:02x?})",
            body_bytes.len(),
            body_bytes.iter().take(4).copied().collect::<Vec<_>>()
        );
        let body_text = decompress_public_export_lzma(&body_bytes)
            .or_else(|_| {
                String::from_utf8(body_bytes.to_vec()).context("export not utf-8")
            })
            .with_context(|| format!("decode {index_entry_prefix}"))?;
        info!(
            "Decoded {index_entry_prefix} ({} chars)",
            body_text.len()
        );
        let json_key = json_key.to_string();
        let index_entry_prefix = index_entry_prefix.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            let v: Value = serde_json::from_str(&body_text)
                .with_context(|| format!("parse {index_entry_prefix}"))?;
            let arr = v
                .get(&json_key)
                .and_then(|x| x.as_array())
                .with_context(|| format!("missing array {json_key}"))?;
            let mut rows = Vec::with_capacity(arr.len());
            for item in arr {
                let unique = item
                    .get("uniqueName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
                if unique.is_empty() || name.is_empty() {
                    continue;
                }
                rows.push((unique.to_string(), name.to_string()));
            }
            Ok::<Vec<(String, String)>, anyhow::Error>(rows)
        })
        .await
        .context("join parse")??;
        Ok(rows)
    }

    /// Resolve inventory row → WFM catalog (url_name, EN name, or WFCD Lotus name).
    pub fn resolve_inventory_market_item(
        inv: &crate::db::InventoryItem,
        by_norm: &HashMap<String, ItemRow>,
        lotus_names: &HashMap<String, String>,
    ) -> Option<ItemRow> {
        if let Some(ref url) = inv.url_name {
            if let Some(item) = by_norm.get(url) {
                return Some(item.clone());
            }
        }
        if let Some(item) = Self::resolve_market_item_indexed(&inv.name, by_norm) {
            return Some(item);
        }
        let lotus_key = strip_inventory_unique(&inv.unique_name);
        if let Some(en) = lotus_names.get(lotus_key) {
            if let Some(item) = Self::resolve_market_item_indexed(en, by_norm) {
                return Some(item);
            }
            // Relics: WFCD "Lith A10 Intact" → WFM "Lith A10 Relic"
            if let Some(trade) = relic_trade_name(en) {
                if let Some(item) = Self::resolve_market_item_indexed(&trade, by_norm) {
                    return Some(item);
                }
            }
            // Arcanes sometimes listed without "Arcane " prefix mismatches — try slug
            let slug = normalize_name(en).replace(' ', "_");
            if let Some(item) = by_norm.get(&slug) {
                return Some(item.clone());
            }
        }
        // Relic path without WFCD cache yet: guess lith_*_relic from leaf is hopeless;
        // void projections need the Relics.json map.
        None
    }

    /// Best-effort RU display name for gear using WFM catalog (set listings, etc.).
    pub fn localized_gear_name(
        en_name: &str,
        by_norm: &HashMap<String, ItemRow>,
    ) -> Option<String> {
        let try_one = |label: &str| -> Option<String> {
            let key = normalize_name(label);
            let row = by_norm.get(&key)?;
            row.name_ru
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(row.name.clone()))
                .map(|s| strip_market_set_label(&s))
        };

        if let Some(s) = try_one(en_name) {
            return Some(s);
        }
        if en_name.to_lowercase().contains("prime") {
            if let Some(s) = try_one(&format!("{en_name} Set")) {
                return Some(s);
            }
        } else if let Some(s) = try_one(&format!("{en_name} Set")) {
            return Some(s);
        }
        None
    }

    async fn fetch_items_v2(&self) -> Result<Vec<ItemRow>> {
        let resp = self
            .client
            .get("https://api.warframe.market/v2/items")
            .header("Language", "ru")
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
                let ru = i.i18n.get("ru");
                // Language: ru returns both en + ru in i18n
                let name = en
                    .map(|e| e.name.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| ru.map(|e| e.name.clone()))
                    .unwrap_or_default();
                if name.is_empty() {
                    return None;
                }
                let name_ru = ru
                    .map(|e| e.name.clone())
                    .filter(|s| !s.trim().is_empty());
                let thumb = en
                    .and_then(|e| e.thumb.clone().or(e.icon.clone()))
                    .or_else(|| ru.and_then(|e| e.thumb.clone().or(e.icon.clone())));
                let set_url_name = derive_set_url_name(&url_name, &i.tags);
                let vaulted = i
                    .vaulted
                    .filter(|v| *v)
                    .or_else(|| i.tags.iter().any(|t| t == "vaulted").then_some(true));
                Some(ItemRow {
                    url_name,
                    name,
                    name_ru,
                    thumb,
                    ducats: i.ducats.filter(|d| *d > 0),
                    set_url_name,
                    vaulted,
                    mastery: None,
                })
            })
            .collect())
    }

    /// Mark vaulted primes from WFCD (WFM no longer tags primes as vaulted).
    async fn enrich_vaulted_from_wfcd(&self, items: &mut [ItemRow]) -> Result<()> {
        let mut vaulted_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for file in [
            "Warframes.json",
            "Primary.json",
            "Secondary.json",
            "Melee.json",
            "Sentinels.json",
            "Arch-Gun.json",
            "Arch-Melee.json",
            "Relics.json",
        ] {
            let url = format!(
                "https://raw.githubusercontent.com/WFCD/warframe-items/master/data/json/{file}"
            );
            let resp = match self.client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!("WFCD {file}: {e}");
                    continue;
                }
            };
            if !resp.status().is_success() {
                continue;
            }
            let rows: Vec<Value> = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!("WFCD parse {file}: {e}");
                    continue;
                }
            };
            let is_relics = file == "Relics.json";
            for row in rows {
                if row.get("vaulted").and_then(|v| v.as_bool()) != Some(true) {
                    continue;
                }
                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                if !is_relics && !name.to_ascii_lowercase().contains("prime") {
                    continue;
                }
                // "Ash Prime" → ash_prime ; "Axi A1 Relic" → axi_a1_relic
                let key = normalize_name(name).replace(' ', "_");
                if !key.is_empty() {
                    vaulted_keys.insert(key);
                }
            }
        }
        if vaulted_keys.is_empty() {
            return Ok(());
        }
        let mut n = 0usize;
        for item in items.iter_mut() {
            let slug = item.url_name.as_str();
            let base = slug
                .strip_suffix("_set")
                .or_else(|| {
                    for suf in [
                        "_blueprint",
                        "_neuroptics_blueprint",
                        "_chassis_blueprint",
                        "_systems_blueprint",
                        "_neuroptics",
                        "_chassis",
                        "_systems",
                        "_barrel",
                        "_receiver",
                        "_stock",
                        "_blade",
                        "_handle",
                        "_link",
                        "_grip",
                        "_string",
                        "_gauntlet",
                        "_harness",
                        "_wings",
                        "_carapace",
                        "_cerebrum",
                    ] {
                        if let Some(b) = slug.strip_suffix(suf) {
                            return Some(b);
                        }
                    }
                    None
                })
                .unwrap_or(slug);
            if vaulted_keys.contains(base) {
                item.vaulted = Some(true);
                n += 1;
            }
        }
        info!("Marked {n} WFM rows vaulted from WFCD primes");
        Ok(())
    }

    /// Live WFM quote: average of the `n` cheapest visible sell orders
    /// from ingame/online sellers (falls back to all visible sells if needed).
    pub async fn price_for(&self, url_name: &str) -> Result<PriceCache> {
        self.price_for_inner(url_name, false).await
    }

    /// Instant SQLite quote only — never hits warframe.market (for fast overlay).
    pub async fn price_cached_only(&self, url_name: &str) -> Option<PriceCache> {
        let owned = if url_name.ends_with("_set") && !is_orderable_set_slug(url_name) {
            canonicalize_set_slug(url_name)
        } else {
            None
        };
        let url_name = owned.as_deref().unwrap_or(url_name);
        let db = self.db.lock().await;
        db.get_price(url_name)
            .ok()
            .flatten()
            .or_else(|| db.get_price_cached(url_name).ok().flatten())
    }

    /// Bypass SQLite freshness window and hit warframe.market.
    pub async fn price_for_refresh(&self, url_name: &str) -> Result<PriceCache> {
        self.price_for_inner(url_name, true).await
    }

    async fn price_for_inner(&self, url_name: &str, force: bool) -> Result<PriceCache> {
        // Don't hammer WFM for part pseudo-sets that always 404
        let url_name = if url_name.ends_with("_set") && !is_orderable_set_slug(url_name) {
            match canonicalize_set_slug(url_name) {
                Some(parent) if parent != url_name => {
                    return Box::pin(self.price_for_inner(&parent, force)).await;
                }
                _ => {
                    return Ok(PriceCache {
                        url_name: url_name.into(),
                        platinum: 0.0,
                        volume: 0,
                        updated_at: Utc::now().to_rfc3339(),
                    });
                }
            }
        } else {
            url_name
        };

        if !force {
            let db = self.db.lock().await;
            // Fresh quotes (< PRICE_FRESH_MINS) skip the network
            if let Some(p) = db.get_price(url_name)? {
                return Ok(p);
            }
        }

        let url = format!("https://api.warframe.market/v2/orders/item/{url_name}");
        let _gate = self.order_gate.lock().await;
        let resp = self
            .client
            .get(&url)
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            // Pseudo-sets like atlas_prime_systems_set often 404 — expected, keep quiet
            if code == 404 {
                tracing::debug!("orders 404 for {url_name} (skipping)");
            } else if code == 429 {
                warn!("orders rate-limited for {url_name}; backing off");
                tokio::time::sleep(Duration::from_secs(2)).await;
            } else {
                warn!("orders fetch failed for {url_name}: {}", resp.status());
            }
            // Do NOT cache failures as platinum=0 (would block retries for minutes).
            // Prefer last known display quote so the UI does not blank out on 429.
            {
                let db = self.db.lock().await;
                if let Ok(Some(p)) = db.get_price_cached(url_name) {
                    return Ok(p);
                }
            }
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

        let (platinum, volume) = avg_cheapest_sells(&orders, 3);

        let cache = PriceCache {
            url_name: url_name.into(),
            platinum,
            volume,
            updated_at: Utc::now().to_rfc3339(),
        };
        // Only persist successful live quotes (skip empty book so we retry soon)
        if platinum > 0.0 {
            let db = self.db.lock().await;
            db.upsert_price(&cache)?;
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        Ok(cache)
    }

    pub async fn name_index(&self) -> Result<HashMap<String, ItemRow>> {
        let db = self.db.lock().await;
        let items = db.all_items()?;
        let mut map = HashMap::new();
        for item in items {
            map.insert(normalize_name(&item.name), item.clone());
            map.insert(item.name.to_lowercase(), item.clone());
            if let Some(ref ru) = item.name_ru {
                map.insert(normalize_name(ru), item.clone());
                map.insert(ru.to_lowercase(), item.clone());
            }
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
            if let Some(ref ru) = item.name_ru {
                map.insert(normalize_name(ru), item.clone());
            }
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
        let lotus_mods = self.ensure_lotus_mod_names().await.unwrap_or_default();
        let (catalog, inventory) = {
            let db = self.db.lock().await;
            (db.all_items()?, db.list_inventory()?)
        };
        let by_norm = Self::build_catalog_index(&catalog);

        let mut targets: Vec<(String, String)> = Vec::new(); // (unique_name, url_name)
        let mut seen = std::collections::HashSet::new();
        for inv in &inventory {
            if let Some(item) = Self::resolve_inventory_market_item(inv, &by_norm, &lotus_mods) {
                if seen.insert(item.url_name.clone()) {
                    targets.push((inv.unique_name.clone(), item.url_name.clone()));
                }
                // Also queue the full tradeable set listing (not WFM part-pseudo-sets)
                if let Some(set_url) = item
                    .set_url_name
                    .as_deref()
                    .and_then(canonicalize_set_slug)
                    .or_else(|| canonicalize_set_slug(&item.url_name))
                {
                    if by_norm.contains_key(&set_url) && seen.insert(set_url.clone()) {
                        targets.push((inv.unique_name.clone(), set_url));
                    }
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
            match self.price_for_refresh(url).await {
                Ok(p) if p.platinum > 0.0 || p.volume > 0 => priced += 1,
                Ok(_) => priced += 1,
                Err(_) => failed += 1,
            }
        }

        // Rewrite inventory url_name for matched rows so UI joins work next time
        {
            let db = self.db.lock().await;
            for inv in &inventory {
                if let Some(item) = Self::resolve_inventory_market_item(inv, &by_norm, &lotus_mods)
                {
                    let mut updated = inv.clone();
                    updated.url_name = Some(item.url_name);
                    updated.name = item.name;
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

    /// Re-fetch order-book quotes (avg of 3 cheapest sells) for stale / missing
    /// inventory + set listings. Intended for the hourly background refresh.
    pub async fn refresh_hourly_prices(&self, limit: usize) -> Result<PriceRefreshResult> {
        let result = self.refresh_inventory_prices(limit).await?;
        // Also refresh any other cached slugs that aged out of the fresh window.
        let stale: Vec<String> = {
            let db = self.db.lock().await;
            db.list_prices_raw()?
                .into_iter()
                .filter(|p| {
                    p.platinum > 0.0
                        && p.url_name != "forma"
                        && db.get_price(&p.url_name).ok().flatten().is_none()
                })
                .map(|p| p.url_name)
                .take(limit.saturating_mul(2).max(50))
                .collect()
        };
        let mut priced = result.priced;
        let mut failed = result.failed;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for url in stale {
            if !seen.insert(url.clone()) {
                continue;
            }
            if priced + failed >= limit.saturating_mul(3) {
                break;
            }
            match self.price_for_refresh(&url).await {
                Ok(p) if p.platinum > 0.0 => priced += 1,
                Ok(_) => {}
                Err(_) => failed += 1,
            }
        }
        Ok(PriceRefreshResult {
            matched: result.matched + seen.len(),
            priced,
            failed,
            catalog_size: result.catalog_size,
        })
    }
}

/// Inventory stores `…#RawUpgrades` / `…#MiscItems` suffixes; WFCD uses bare Lotus paths.
pub fn strip_inventory_unique(unique: &str) -> &str {
    unique.split('#').next().unwrap_or(unique)
}

/// DE Export: `"<ARCHWING> Амеша"` → `"Амеша"`.
pub fn strip_export_tags(s: &str) -> String {
    let mut out = s.trim().to_string();
    while out.starts_with('<') {
        if let Some(end) = out.find('>') {
            out = out[end + 1..].trim_start().to_string();
        } else {
            break;
        }
    }
    out
}

/// Internal Lotus leaves with no DE Export row → Russian UI label.
fn hardcoded_lotus_leaf_ru(leaf: &str) -> Option<&'static str> {
    match leaf {
        "OperatorAmpWeapon" => Some("Усилитель"),
        "OperatorTrainingAmpWeapon" => Some("Усилитель: Пылинка"),
        "ZanukaPetAPowerSuit" => Some("Гончая: Дорма"),
        "ZanukaPetBPowerSuit" => Some("Гончая: Бхайра"),
        "ZanukaPetCPowerSuit" => Some("Гончая: Хек"),
        "GenericLensOstron" => Some("Эйдолонская линза"),
        "LocateCreatures" => Some("Обнаружение существ"),
        _ => None,
    }
}

/// Leaf-name index over Lotus paths (prefers Cyrillic labels).
pub fn build_lotus_leaf_index(lotus: &HashMap<String, String>) -> HashMap<String, String> {
    let mut by_leaf: HashMap<String, String> = HashMap::new();
    for (path, name) in lotus {
        let leaf = path.rsplit('/').next().unwrap_or(path.as_str());
        let cleaned = strip_export_tags(name);
        if cleaned.is_empty() {
            continue;
        }
        let ru = cleaned
            .chars()
            .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
        if ru {
            by_leaf.insert(leaf.to_string(), cleaned);
        } else {
            by_leaf.entry(leaf.to_string()).or_insert(cleaned);
        }
    }
    by_leaf
}

/// Rewrite internal blueprint/product paths onto rows that exist in DE Export.
pub fn alias_lotus_product_unique(unique: &str) -> Option<String> {
    let key = strip_inventory_unique(unique);
    let mut leaf = key.rsplit('/').next().unwrap_or(key).to_string();
    if let Some(s) = leaf.strip_suffix("Blueprint") {
        leaf = s.to_string();
    }

    // ZanukaPetCompleteHeadB → …/ZanukaPetParts/ZanukaPetPartHeadB
    if let Some(rest) = leaf.strip_prefix("ZanukaPetCompleteHead") {
        let letter = rest.chars().next().filter(|c| c.is_ascii_alphabetic())?;
        return Some(format!(
            "/Lotus/Types/Friendly/Pets/ZanukaPets/ZanukaPetParts/ZanukaPetPartHead{letter}"
        ));
    }
    // Recipes/ZanukaPetParts/ZanukaPetPartBodyA → Friendly/Pets/…/ZanukaPetPartBodyA
    if leaf.starts_with("ZanukaPetPart") {
        return Some(format!(
            "/Lotus/Types/Friendly/Pets/ZanukaPets/ZanukaPetParts/{leaf}"
        ));
    }

    None
}

fn try_lotus_leaf(by_leaf: &HashMap<String, String>, leaf_name: &str) -> Option<String> {
    by_leaf.get(leaf_name).cloned().or_else(|| {
        for suf in ["Component", "Item", "Weapon", "PowerSuit"] {
            if let Some(n) = by_leaf.get(&format!("{leaf_name}{suf}")) {
                return Some(n.clone());
            }
        }
        None
    })
}

/// Best-effort Russian (or DE) display label for an inventory Lotus path.
pub fn resolve_lotus_label(
    unique: &str,
    lotus: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
) -> Option<String> {
    let by_leaf = build_lotus_leaf_index(lotus);
    resolve_lotus_label_indexed(unique, lotus, bp_product, &by_leaf)
}

pub fn resolve_lotus_label_indexed(
    unique: &str,
    lotus: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
    by_leaf: &HashMap<String, String>,
) -> Option<String> {
    let key = strip_inventory_unique(unique).to_string();
    let leaf = key.rsplit('/').next().unwrap_or(key.as_str()).to_string();

    let clean = |raw: &str| -> Option<String> {
        let s = strip_export_tags(raw);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    let try_path = |path: &str| -> Option<String> { lotus.get(path).and_then(|n| clean(n)) };

    if let Some(n) = try_path(&key) {
        return Some(n);
    }
    if let Some(ru) = hardcoded_lotus_leaf_ru(&leaf) {
        return Some(ru.to_string());
    }
    if let Some(prod) = bp_product.get(&key) {
        if let Some(n) = try_path(prod) {
            return Some(n);
        }
        let prod_leaf = prod.rsplit('/').next().unwrap_or(prod.as_str());
        if let Some(n) = try_lotus_leaf(by_leaf, prod_leaf) {
            return Some(n);
        }
    }
    if let Some(stripped) = key.strip_suffix("Blueprint") {
        if let Some(n) = try_path(stripped) {
            return Some(n);
        }
        for suf in ["Component", "Item"] {
            if let Some(n) = try_path(&format!("{stripped}{suf}")) {
                return Some(n);
            }
        }
        if let Some(prod) = bp_product.get(stripped) {
            if let Some(n) = try_path(prod) {
                return Some(n);
            }
        }
        if let Some(alias) = alias_lotus_product_unique(stripped) {
            if let Some(n) = try_path(&alias) {
                return Some(n);
            }
        }
        let stripped_leaf = stripped.rsplit('/').next().unwrap_or(stripped);
        if let Some(ru) = hardcoded_lotus_leaf_ru(stripped_leaf) {
            return Some(ru.to_string());
        }
        if let Some(n) = try_lotus_leaf(by_leaf, stripped_leaf) {
            return Some(n);
        }
    }
    if let Some(alias) = alias_lotus_product_unique(&key) {
        if let Some(n) = try_path(&alias) {
            return Some(n);
        }
    }
    try_lotus_leaf(by_leaf, &leaf)
}

const RELIC_QUALITIES: &[&str] = &["Intact", "Exceptional", "Flawless", "Radiant"];

/// WFCD "Lith A10 Intact" → WFM listing name "Lith A10 Relic".
pub fn relic_trade_name(wfcd_name: &str) -> Option<String> {
    let mut parts: Vec<&str> = wfcd_name.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    if let Some(last) = parts.last() {
        if RELIC_QUALITIES.iter().any(|q| q.eq_ignore_ascii_case(last)) {
            parts.pop();
        }
    }
    if parts.is_empty() {
        return None;
    }
    // Already "Requiem I Relic" style
    if parts.last().is_some_and(|p| p.eq_ignore_ascii_case("Relic")) {
        return Some(parts.join(" "));
    }
    Some(format!("{} Relic", parts.join(" ")))
}

pub fn relic_quality_en(wfcd_name: &str) -> Option<&'static str> {
    let last = wfcd_name.split_whitespace().last()?;
    RELIC_QUALITIES
        .iter()
        .copied()
        .find(|q| q.eq_ignore_ascii_case(last))
}

pub fn relic_quality_ru(quality_en: &str) -> &'static str {
    match quality_en {
        "Intact" => "нетронутая",
        "Exceptional" => "необычная",
        "Flawless" => "безупречная",
        "Radiant" => "сияющая",
        _ => "?",
    }
}

/// Format relic display: «Реликвия Лит A10 · нетронутая»
pub fn format_relic_display_name(base_ru_or_en: &str, wfcd_name: &str) -> String {
    let base = base_ru_or_en.trim();
    if let Some(q) = relic_quality_en(wfcd_name) {
        format!("{base} · {}", relic_quality_ru(q))
    } else {
        base.to_string()
    }
}

fn strip_market_set_label(name: &str) -> String {
    let mut s = name.trim().to_string();
    // Longer / more specific suffixes first
    for suf in [
        ": Комплект",
        ": комплект",
        ": Set",
        ": Сет",
        ": сет",
        " Комплект",
        " комплект",
        " Set",
        " Сет",
        " сет",
    ] {
        if let Some(stripped) = s.strip_suffix(suf) {
            s = stripped.trim().to_string();
            break;
        }
    }
    s = s.trim_end_matches(':').trim().to_string();
    if let Some((head, tail)) = s.rsplit_once(':') {
        let t = tail.trim().to_lowercase();
        if matches!(t.as_str(), "комплект" | "сет" | "set") {
            s = head.trim().to_string();
        }
    }
    s
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

/// Average platinum of the `n` cheapest visible sell orders.
/// Prefers ingame/online sellers; if fewer than `n`, falls back to all visible sells.
pub fn avg_cheapest_sells(orders: &[serde_json::Value], n: usize) -> (f64, i64) {
    let sell_plat = |prefer_online: bool| -> Vec<f64> {
        let mut sells: Vec<f64> = orders
            .iter()
            .filter(|o| {
                let t = o
                    .get("type")
                    .or_else(|| o.get("order_type"))
                    .and_then(|v| v.as_str());
                t == Some("sell")
            })
            .filter(|o| o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true))
            .filter(|o| {
                if !prefer_online {
                    return true;
                }
                matches!(
                    o.pointer("/user/status").and_then(|v| v.as_str()),
                    Some("ingame") | Some("online")
                )
            })
            .filter_map(|o| o.get("platinum").and_then(|v| v.as_f64()))
            .filter(|p| *p > 0.0)
            .collect();
        sells.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sells
    };

    let mut sells = sell_plat(true);
    if sells.len() < n {
        sells = sell_plat(false);
    }
    let volume = sells.len() as i64;
    if sells.is_empty() || n == 0 {
        return (0.0, volume);
    }
    let take = n.min(sells.len());
    let avg = sells[..take].iter().sum::<f64>() / take as f64;
    // Platinum listings are whole numbers; keep one decimal for 3-way averages.
    let platinum = (avg * 10.0).round() / 10.0;
    (platinum, volume)
}

/// Absolute URL for a WFM relative thumb/icon path.
pub fn wfm_thumb_url(thumb: &str) -> String {
    if thumb.starts_with("http://") || thumb.starts_with("https://") {
        return thumb.to_string();
    }
    format!("https://warframe.market/static/assets/{thumb}")
}

fn derive_set_url_name(url_name: &str, tags: &[String]) -> Option<String> {
    if tags.iter().any(|t| t == "set") || url_name.ends_with("_set") {
        return Some(url_name.to_string());
    }
    for suf in [
        "_blueprint",
        "_neuroptics",
        "_neuroptics_blueprint",
        "_chassis",
        "_chassis_blueprint",
        "_systems",
        "_systems_blueprint",
        "_harness",
        "_harness_blueprint",
        "_wings",
        "_wings_blueprint",
        "_barrel",
        "_receiver",
        "_stock",
        "_blade",
        "_handle",
        "_link",
        "_grip",
        "_string",
        "_lower_limb",
        "_upper_limb",
        "_gauntlet",
        "_pouch",
        "_chain",
        "_hilt",
        "_guard",
        "_ornament",
        "_head",
    ] {
        if let Some(base) = url_name.strip_suffix(suf) {
            if !base.is_empty() {
                return Some(format!("{base}_set"));
            }
        }
    }
    None
}

/// Resolve a VoidProjections StoreItems path to a market catalog row.
/// e.g. `/Lotus/StoreItems/Types/Recipes/Weapons/AkbroncoPrimeBlueprint`
pub fn resolve_lotus_reward_path<'a>(
    path: &str,
    catalog: &'a [crate::db::ItemRow],
) -> Option<&'a crate::db::ItemRow> {
    if !is_plausible_reward_path(path) {
        return None;
    }
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let index = PricingService::build_catalog_index(catalog);

    let try_slug = |slug: &str| -> Option<&'a crate::db::ItemRow> {
        index
            .get(slug)
            .and_then(|item| catalog.iter().find(|i| i.url_name == item.url_name))
    };

    // DE calls neuroptics "Helmet" in recipe paths.
    let aliases = reward_leaf_aliases(leaf);
    for alias in &aliases {
        if let Some(item) = PricingService::resolve_market_item_indexed(alias, &index) {
            return catalog.iter().find(|i| i.url_name == item.url_name);
        }
        let slug = lotus_to_slug(alias);
        if let Some(item) = try_slug(&slug) {
            return Some(item);
        }
        if !slug.ends_with("_blueprint") {
            if let Some(item) = try_slug(&format!("{slug}_blueprint")) {
                return Some(item);
            }
        }
        // LavosPrimeNeuropticsBlueprint → lavos_prime_neuroptics_blueprint via humanize
        let human = humanize_lotus_name(alias);
        if let Some(item) = PricingService::resolve_market_item_indexed(&human, &index) {
            return catalog.iter().find(|i| i.url_name == item.url_name);
        }
        if let Some(item) =
            PricingService::resolve_market_item_indexed(&format!("{human} Blueprint"), &index)
        {
            return catalog.iter().find(|i| i.url_name == item.url_name);
        }
    }

    // PrimeDaikyuUpperLimb → daikyu_prime_upper_limb
    if let Some(rest) = leaf.strip_prefix("Prime") {
        let human = humanize_lotus_name(rest);
        let mut parts: Vec<&str> = human.split_whitespace().collect();
        if !parts.is_empty() {
            let first = parts.remove(0);
            let alt = lotus_to_slug(&format!("{first}Prime{}", parts.join("")));
            let alt = alt.replace("__", "_");
            if let Some(item) = try_slug(&alt) {
                return Some(item);
            }
            if let Some(item) = try_slug(&format!("{alt}_blueprint")) {
                return Some(item);
            }
        }
    }
    None
}

fn is_plausible_reward_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 4 {
        return false;
    }
    let leaf = parts.last().copied().unwrap_or("");
    if matches!(
        leaf,
        "StoreItems" | "Types" | "Recipes" | "Lotus" | "Components" | "Weapons" | "WarframeRecipes"
    ) {
        return false;
    }
    path.contains("/Recipes/")
        || path.contains("/Upgrades/")
        || path.contains("/Types/Items/")
        || path.contains("StoreItems")
}

/// Recipe leaf variants — DE `…HelmetBlueprint` is market `…Neuroptics Blueprint`.
fn reward_leaf_aliases(leaf: &str) -> Vec<String> {
    let mut out = vec![leaf.to_string()];
    let replacements = [
        ("HelmetBlueprint", "NeuropticsBlueprint"),
        ("HelmetComponent", "Neuroptics"),
        ("Helmet", "Neuroptics"),
        // Internal Lotus name vs warframe.market slug
        ("BowPrime", "ParisPrime"),
        ("BowPrimeBlueprint", "ParisPrimeBlueprint"),
        ("PrimePolearmBlade", "OrthosPrimeBlade"),
        ("PrimePolearmHandle", "OrthosPrimeHandle"),
        ("PrimePolearm", "OrthosPrime"),
    ];
    for (from, to) in replacements {
        if leaf.contains(from) {
            out.push(leaf.replace(from, to));
        }
    }
    // LavosPrimeHelmetBlueprint → LavosPrimeNeuropticsBlueprint already covered;
    // also try without trailing Blueprint for set-part rows.
    if let Some(stripped) = leaf.strip_suffix("Blueprint") {
        if !stripped.is_empty() {
            out.push(stripped.to_string());
        }
    }
    out
}

/// Display label for an EE reward path when it is not on warframe.market
/// (Forma Blueprint, Endo, etc.).
pub fn resolve_lotus_reward_label(
    path: &str,
    lotus: &std::collections::HashMap<String, String>,
    prefer_ru: bool,
) -> Option<String> {
    if !is_plausible_reward_path(path) {
        return None;
    }
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if leaf.is_empty() {
        return None;
    }

    // Prefer neuroptics naming when looking up lotus / display
    let display_leaf = leaf
        .replace("HelmetBlueprint", "NeuropticsBlueprint")
        .replace("HelmetComponent", "Neuroptics");

    // /Lotus/StoreItems/X → /Lotus/X (common DE dual path)
    let types_path = path.replacen("/Lotus/StoreItems/", "/Lotus/", 1);
    let candidates = [
        path.to_string(),
        types_path.clone(),
        types_path.replace("HelmetBlueprint", "NeuropticsBlueprint"),
        format!(
            "/Lotus/Types/Items/MiscItems/{}",
            display_leaf.trim_end_matches("Blueprint")
        ),
        format!("/Lotus/Types/Recipes/Components/{display_leaf}"),
        format!("/Lotus/Types/Recipes/WarframeRecipes/{display_leaf}"),
    ];
    for key in &candidates {
        if let Some(name) = lotus.get(key) {
            let cleaned = name.trim();
            if !cleaned.is_empty() {
                if leaf.ends_with("Blueprint")
                    && prefer_ru
                    && !cleaned.to_lowercase().contains("черт")
                {
                    return Some(format!("Чертёж: {cleaned}"));
                }
                return Some(cleaned.to_string());
            }
        }
    }

    // FormaBlueprint → "Forma Blueprint"
    let human = humanize_lotus_name(&display_leaf);
    if human.is_empty() {
        return None;
    }
    if prefer_ru {
        // Best-effort Russian for common untradable rewards
        let lower = human.to_lowercase();
        if lower.starts_with("forma") {
            return Some(if lower.contains("blueprint") {
                "Чертёж Формы".into()
            } else {
                "Форма".into()
            });
        }
        if lower.contains("endo") {
            return Some("Эндо".into());
        }
        // Helmet → нейрооптика in humanized EN
        let human_ru = human
            .replace("Helmet", "Нейрооптика")
            .replace("Neuroptics", "Нейрооптика")
            .replace("Chassis", "Каркас")
            .replace("Systems", "Система")
            .replace("Blueprint", "Чертёж");
        if human_ru != human {
            return Some(human_ru);
        }
    }
    Some(human)
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

fn tradable_priority_slug(slug: &str) -> i32 {
    let mut s = 0;
    if slug.contains("prime") {
        s += 50;
    }
    if is_orderable_set_slug(slug) {
        s += 80;
    } else if slug.ends_with("_set") {
        s -= 40; // part pseudo-sets
    }
    if slug.contains("blueprint") {
        s += 20;
    }
    s
}

/// True for real WFM set listings (`ash_prime_set`), false for part pseudo-sets
/// (`ash_prime_systems_set`) that 404 on the orders endpoint.
pub fn is_orderable_set_slug(url: &str) -> bool {
    let Some(stem) = url.strip_suffix("_set") else {
        return false;
    };
    !PART_SET_SUFFIXES
        .iter()
        .any(|suf| stem.ends_with(suf))
}

const PART_SET_SUFFIXES: &[&str] = &[
    "_neuroptics",
    "_chassis",
    "_systems",
    "_blueprint",
    "_barrel",
    "_receiver",
    "_stock",
    "_link",
    "_blade",
    "_handle",
    "_gauntlet",
    "_grip",
    "_string",
    "_pouch",
    "_chain",
    "_hilt",
    "_guard",
    "_head",
    "_lower_limb",
    "_upper_limb",
    "_carapace",
    "_cerebrum",
    "_harness",
    "_wings",
];

/// Map a part or WFM pseudo-set slug to the parent tradeable set (`atlas_prime_set`).
pub fn canonicalize_set_slug(url: &str) -> Option<String> {
    let mut base = url.strip_suffix("_set").unwrap_or(url).to_string();
    if base.is_empty() {
        return None;
    }
    loop {
        let before = base.clone();
        for suf in PART_SET_SUFFIXES {
            if let Some(stripped) = base.strip_suffix(suf) {
                if !stripped.is_empty() {
                    base = stripped.to_string();
                }
            }
        }
        if base == before {
            break;
        }
    }
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}_set"))
}

/// If a set root is vaulted, mark every part that belongs to that set.
fn propagate_vaulted_to_set_parts(items: &mut [ItemRow]) {
    let mut vaulted_sets: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items.iter() {
        if item.vaulted != Some(true) {
            continue;
        }
        if let Some(set) = item
            .set_url_name
            .as_deref()
            .and_then(canonicalize_set_slug)
            .or_else(|| canonicalize_set_slug(&item.url_name))
        {
            vaulted_sets.insert(set);
        }
        if item.url_name.ends_with("_set") {
            vaulted_sets.insert(item.url_name.clone());
        }
    }
    if vaulted_sets.is_empty() {
        return;
    }
    for item in items.iter_mut() {
        let parent = item
            .set_url_name
            .as_deref()
            .and_then(canonicalize_set_slug)
            .or_else(|| canonicalize_set_slug(&item.url_name));
        if parent.as_ref().is_some_and(|p| vaulted_sets.contains(p)) {
            item.vaulted = Some(true);
        }
    }
}

fn is_gear_product_category(cat: &str) -> bool {
    matches!(
        cat,
        "Pistols"
            | "LongGuns"
            | "Melee"
            | "Suits"
            | "SpaceSuits"
            | "SpaceGuns"
            | "SpaceMelee"
            | "Sentinels"
            | "SentinelWeapons"
            | "MechSuits"
            | "Hoverboards"
            | "OperatorAmps"
            | "MoaPets"
            | "KubrowPets"
            | "CatbrowPets"
    )
}

/// `/Lotus/Types/Recipes/Weapons/AkbroncoPrimeBlueprint` → `akbronco_prime`
fn set_key_from_blueprint_unique(unique: &str) -> Option<String> {
    let leaf = unique.rsplit('/').next()?;
    let base = leaf
        .strip_suffix("Blueprint")
        .or_else(|| leaf.strip_suffix("blueprint"))
        .unwrap_or(leaf);
    if base.is_empty() || base.eq_ignore_ascii_case("blueprint") {
        return None;
    }
    let key = pascal_case_to_set_key(base);
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

fn pascal_case_to_set_key(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '-' || c == ' ' {
            out.push('_');
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

fn decompress_public_export_lzma(data: &[u8]) -> Result<String> {
    use std::io::{Cursor, Read};
    // Most Manifest files are plain JSON; only the index is lzma.
    if let Ok(s) = std::str::from_utf8(data) {
        let t = s.trim_start();
        if t.starts_with('{') || t.starts_with('[') {
            return Ok(s.to_string());
        }
    }
    if data.first().is_some_and(|b| *b == b'{' || *b == b'[') {
        return String::from_utf8(data.to_vec()).context("utf-8 json");
    }
    // Raw LZMA (0x5d) or XZ stream — used for index_*.txt.lzma
    let stream = xz2::stream::Stream::new_lzma_decoder(u64::MAX)
        .or_else(|_| xz2::stream::Stream::new_auto_decoder(u64::MAX, 0))
        .context("lzma decoder")?;
    let mut decoder = xz2::read::XzDecoder::new_stream(Cursor::new(data), stream);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).context("read lzma")?;
    String::from_utf8(out).context("lzma utf-8")
}

#[cfg(test)]
mod set_slug_tests {
    use super::*;

    #[test]
    fn canonicalizes_part_pseudo_sets() {
        assert_eq!(
            canonicalize_set_slug("atlas_prime_systems_set").as_deref(),
            Some("atlas_prime_set")
        );
        assert_eq!(
            canonicalize_set_slug("aksomati_prime_link").as_deref(),
            Some("aksomati_prime_set")
        );
        assert_eq!(
            canonicalize_set_slug("atlas_prime_set").as_deref(),
            Some("atlas_prime_set")
        );
        assert!(is_orderable_set_slug("atlas_prime_set"));
        assert!(!is_orderable_set_slug("atlas_prime_systems_set"));
    }

    #[test]
    fn forma_blueprint_label_from_ee_path() {
        let mut lotus = std::collections::HashMap::new();
        lotus.insert(
            "/Lotus/Types/Items/MiscItems/Forma".into(),
            "Форма".into(),
        );
        let label = resolve_lotus_reward_label(
            "/Lotus/StoreItems/Types/Recipes/Components/FormaBlueprint",
            &lotus,
            true,
        );
        assert_eq!(label.as_deref(), Some("Чертёж: Форма"));
    }

    #[test]
    fn rejects_truncated_storeitems_path() {
        assert!(resolve_lotus_reward_label("/Lotus/StoreItems", &Default::default(), true).is_none());
    }

    #[test]
    fn avg_cheapest_sells_takes_mean_of_three() {
        let orders = serde_json::json!([
            {"type":"sell","visible":true,"platinum":10,"user":{"status":"ingame"}},
            {"type":"sell","visible":true,"platinum":14,"user":{"status":"online"}},
            {"type":"sell","visible":true,"platinum":20,"user":{"status":"ingame"}},
            {"type":"sell","visible":true,"platinum":100,"user":{"status":"ingame"}},
            {"type":"buy","visible":true,"platinum":1,"user":{"status":"ingame"}},
            {"type":"sell","visible":true,"platinum":2,"user":{"status":"offline"}},
        ]);
        let arr = orders.as_array().unwrap();
        let (plat, vol) = avg_cheapest_sells(arr, 3);
        assert_eq!(vol, 4);
        // (10+14+20)/3 = 14.666... → 14.7
        assert!((plat - 14.7).abs() < 0.01, "plat={plat}");
    }


    #[test]
    fn helmet_blueprint_resolves_to_neuroptics_catalog() {
        let catalog = vec![ItemRow {
            url_name: "lavos_prime_neuroptics_blueprint".into(),
            name: "Lavos Prime Neuroptics Blueprint".into(),
            name_ru: Some("Лавос Прайм: Нейрооптика (Чертеж)".into()),
            thumb: None,
            ducats: Some(45),
            set_url_name: Some("lavos_prime_set".into()),
            vaulted: None,
            mastery: None,
        }];
        let hit = resolve_lotus_reward_path(
            "/Lotus/StoreItems/Types/Recipes/WarframeRecipes/LavosPrimeHelmetBlueprint",
            &catalog,
        );
        assert_eq!(
            hit.map(|i| i.url_name.as_str()),
            Some("lavos_prime_neuroptics_blueprint")
        );
    }

    #[test]
    fn bow_prime_blueprint_resolves_to_paris() {
        let catalog = vec![ItemRow {
            url_name: "paris_prime_blueprint".into(),
            name: "Paris Prime Blueprint".into(),
            name_ru: Some("Парис Прайм (Чертеж)".into()),
            thumb: None,
            ducats: Some(15),
            set_url_name: Some("paris_prime_set".into()),
            vaulted: None,
            mastery: None,
        }];
        let hit = resolve_lotus_reward_path(
            "/Lotus/StoreItems/Types/Recipes/Weapons/BowPrimeBlueprint",
            &catalog,
        );
        assert_eq!(
            hit.map(|i| i.url_name.as_str()),
            Some("paris_prime_blueprint")
        );
    }
}
