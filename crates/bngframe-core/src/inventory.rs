use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::db::{Database, InventoryItem};
use crate::pricing::{
    build_lotus_leaf_index, humanize_lotus_name, normalize_name, resolve_lotus_label_indexed,
    PricingService,
};

const DE_INVENTORY_API: &str = "https://api.warframe.com/api/inventory.php";

/// Patterns commonly found near Warframe session tokens in process memory.
const TOKEN_REGEXES: &[&str] = &[
    // Live game keeps auth in API query strings (most reliable on Proton)
    r#"(?i)accountId=([0-9a-f]{16,40}).{0,80}?nonce=([0-9]{10,24})"#,
    r#"(?i)nonce=([0-9]{10,24}).{0,80}?accountId=([0-9a-f]{16,40})"#,
    // Legacy / helper-style JSON blobs
    r#""AccountId"\s*:\s*"([0-9a-fA-F\-]{16,})""#,
    r#""Nonce"\s*:\s*"([A-Za-z0-9_\-]{10,})""#,
    r#"Nonce=([0-9]{10,24})"#,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySyncResult {
    pub item_count: usize,
    pub credits: Option<f64>,
    pub platinum: Option<f64>,
    pub synced_at: String,
    pub method: String,
    #[serde(default)]
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryCacheMeta {
    pub synced_at: Option<String>,
    pub method: String,
    pub item_count: usize,
    pub account_id: Option<String>,
    pub age_secs: Option<u64>,
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerProfile {
    pub display_name: Option<String>,
    pub mastery_rank: Option<i64>,
    pub account_id: Option<String>,
    /// In-game glyph / profile picture (PublicExport texture URL).
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryListResponse {
    pub items: Vec<InventoryItem>,
    pub cache: InventoryCacheMeta,
}

#[derive(Debug, Clone)]
pub struct SessionCredentials {
    pub account_id: String,
    pub nonce: String,
}

pub struct InventoryService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
    pricing: Arc<PricingService>,
    cache_path: PathBuf,
    data_dir: PathBuf,
}

impl InventoryService {
    pub fn new(
        db: Arc<tokio::sync::Mutex<Database>>,
        pricing: Arc<PricingService>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("BNGframe/0.1")
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .expect("client"),
            db,
            pricing,
            cache_path: data_dir.join("inventory_cache.json"),
            data_dir,
        }
    }

    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    pub async fn cache_meta(&self) -> Result<InventoryCacheMeta> {
        let db = self.db.lock().await;
        db.inventory_cache_meta()
    }

    /// If SQLite inventory is empty but a disk cache exists, restore it.
    pub async fn hydrate_from_disk_if_empty(&self) -> Result<Option<InventorySyncResult>> {
        {
            let db = self.db.lock().await;
            if db.inventory_count()? > 0 {
                return Ok(None);
            }
        }
        if self.cache_path.exists() {
            info!("Hydrating inventory from {}", self.cache_path.display());
            return Ok(Some(
                self.apply_json_file(&self.cache_path, "disk_cache").await?,
            ));
        }
        for cand in default_inventory_dump_candidates() {
            if cand.exists() {
                info!("Hydrating inventory from {}", cand.display());
                return Ok(Some(self.apply_json_file(&cand, "disk_hydrate").await?));
            }
        }
        Ok(None)
    }

    /// Re-apply `inventory_cache.json` so new fields (ranks, etc.) show without a live sync.
    pub async fn refresh_from_disk_cache(&self) -> Result<Option<InventorySyncResult>> {
        if !self.cache_path.exists() {
            return Ok(None);
        }
        info!(
            "Refreshing inventory from disk cache {}",
            self.cache_path.display()
        );
        Ok(Some(
            self.apply_json_file(&self.cache_path, "disk_refresh").await?,
        ))
    }

    pub async fn sync(&self, consent: bool) -> Result<InventorySyncResult> {
        if !consent {
            bail!("Inventory sync requires explicit consent (set inventory_consent = true in config)");
        }

        let creds = find_session_credentials()
            .context("Could not find Warframe session token in process memory. Is Warframe running under Proton? You may need: sudo sysctl kernel.yama.ptrace_scope=0")?;

        info!(
            "Found session credentials for account {} (nonce len {})",
            creds.account_id,
            creds.nonce.len()
        );

        let url = format!(
            "{DE_INVENTORY_API}?accountId={}&nonce={}&ct=STM",
            urlencoding_lite(&creds.account_id),
            urlencoding_lite(&creds.nonce)
        );

        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("DE inventory API returned {}", resp.status());
        }
        let body: Value = resp.json().await.context("parse inventory JSON")?;

        self.write_raw_cache(&body)?;

        let result = self
            .apply_inventory_json(&body, "memory_jwt", Some(&creds.account_id))
            .await?;
        let _ = self.pricing.ensure_items_cached().await;
        Ok(result)
    }

    pub async fn list_enriched(&self) -> Result<InventoryListResponse> {
        // Pull Arcanes/Relics Lotus maps once if missing (otherwise keep warm in background).
        let need_lotus = {
            let db = self.db.lock().await;
            db.lotus_name_count("arcane").unwrap_or(0) < 100
                || db.lotus_name_count("relic").unwrap_or(0) < 500
                || db.lotus_name_count("flavour").unwrap_or(0) < 100
                || !db.lotus_names_look_russian("warframe").unwrap_or(false)
                || !db.lotus_names_look_russian("weapon").unwrap_or(false)
                || !db.lotus_names_look_russian("sentinel").unwrap_or(false)
        };
        if need_lotus {
            let _ = self.pricing.ensure_lotus_mod_names().await;
        } else {
            let pricing = Arc::clone(&self.pricing);
            tokio::spawn(async move {
                let _ = pricing.ensure_items_cached().await;
                let _ = pricing.ensure_lotus_mod_names().await;
            });
        }

        let (mut items, catalog, cache, lotus_mods, prices, bp_product) = {
            let db = self.db.lock().await;
            let items = db.list_inventory()?;
            let catalog = db.all_items()?;
            let cache = db.inventory_cache_meta()?;
            // Prefer DE RU kinds over WFCD EN (weapon_en before weapon/sentinel/…).
            let mut lotus_mods = std::collections::HashMap::new();
            for kind in [
                "mod",
                "arcane",
                "relic",
                "flavour",
                "weapon_en",
                "key",
                "drone",
                "gear",
                "resource",
                "custom",
                "sentinel",
                "warframe",
                "weapon",
            ] {
                if let Ok(m) = db.lotus_name_map(kind) {
                    lotus_mods.extend(m);
                }
            }
            // Last known WFM floors (up to PRICE_DISPLAY_MINS) — UI shows immediately
            let prices = db.list_all_prices()?;
            let bp_product = db
                .get_setting("weapon_blueprint_map")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            (items, catalog, cache, lotus_mods, prices, bp_product)
        };

        let by_norm = PricingService::build_catalog_index(&catalog);
        let lotus_by_leaf = build_lotus_leaf_index(&lotus_mods);
        let price_by_url: std::collections::HashMap<String, f64> = prices
            .into_iter()
            .map(|p| (p.url_name, p.platinum))
            .collect();

        for item in &mut items {
            // Re-derive type from Lotus path (inventory may predate classifier fixes)
            item.item_type = classify_unique(&item.unique_name);
            item.platinum = None;

            let lotus_key = crate::pricing::strip_inventory_unique(&item.unique_name);
            let lotus_label = resolve_lotus_label_indexed(
                &item.unique_name,
                &lotus_mods,
                &bp_product,
                &lotus_by_leaf,
            );
            let wfcd_name = lotus_mods.get(lotus_key).cloned().or_else(|| lotus_label.clone());

            if let Some(row) =
                PricingService::resolve_inventory_market_item(item, &by_norm, &lotus_mods)
            {
                item.url_name = Some(row.url_name.clone());
                item.ducats = row.ducats.or(item.ducats);
                item.thumb = row.thumb.clone().or(item.thumb.clone());
                item.vaulted = row.vaulted.or(item.vaulted);
                item.platinum = price_by_url.get(&row.url_name).copied();
                // Prefer official DE/RU lotus label over market EN when available.
                if let Some(label) = lotus_label.filter(|s| {
                    s.chars()
                        .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
                }) {
                    let label = if item.item_type == "blueprint"
                        && !label.to_lowercase().contains("чертеж")
                    {
                        format!("{label} (Чертеж)")
                    } else {
                        label
                    };
                    item.name = if item.item_type == "relic" {
                        if let Some(ref wfcd) = wfcd_name {
                            crate::pricing::format_relic_display_name(&label, wfcd)
                        } else {
                            label
                        }
                    } else {
                        label
                    };
                } else {
                    let base = row
                        .name_ru
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| row.name.clone());
                    item.name = if item.item_type == "relic" {
                        if let Some(ref wfcd) = wfcd_name {
                            crate::pricing::format_relic_display_name(&base, wfcd)
                        } else {
                            base
                        }
                    } else {
                        base
                    };
                }
            } else {
                // Drop guessed slugs that are not on warframe.market
                if let Some(ref url) = item.url_name {
                    if !by_norm.contains_key(url) {
                        item.url_name = None;
                    }
                }
                if let Some(label) = lotus_label {
                    let label = if item.item_type == "blueprint"
                        && !label.to_lowercase().contains("чертеж")
                        && label
                            .chars()
                            .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
                    {
                        format!("{label} (Чертеж)")
                    } else {
                        label
                    };
                    if item.item_type == "relic" {
                        let trade = crate::pricing::relic_trade_name(&label).unwrap_or(label.clone());
                        let base = PricingService::localized_gear_name(&trade, &by_norm)
                            .unwrap_or(trade);
                        item.name = crate::pricing::format_relic_display_name(&base, &label);
                    } else {
                        item.name = PricingService::localized_gear_name(&label, &by_norm)
                            .filter(|s| {
                                s.chars()
                                    .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
                            })
                            .unwrap_or(label);
                    }
                } else if let Some(en) = wfcd_name {
                    if item.item_type == "relic" {
                        let trade = crate::pricing::relic_trade_name(&en).unwrap_or(en.clone());
                        let base = PricingService::localized_gear_name(&trade, &by_norm)
                            .unwrap_or(trade);
                        item.name = crate::pricing::format_relic_display_name(&base, &en);
                    } else {
                        item.name = PricingService::localized_gear_name(&en, &by_norm)
                            .unwrap_or_else(|| en.clone());
                    }
                } else {
                    item.name = humanize_inventory_label(&item.name);
                }
            }
        }
        Ok(InventoryListResponse { items, cache })
    }

    pub async fn apply_json_file(&self, path: &Path, method: &str) -> Result<InventorySyncResult> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read inventory cache {}", path.display()))?;
        let body: Value = serde_json::from_str(&text).context("parse inventory cache JSON")?;
        if path != self.cache_path {
            let _ = self.write_raw_cache(&body);
        }
        let account_id = {
            let db = self.db.lock().await;
            db.get_setting("inventory_account_id")?
        };
        self.apply_inventory_json(&body, method, account_id.as_deref())
            .await
    }

    async fn apply_inventory_json(
        &self,
        body: &Value,
        method: &str,
        account_id: Option<&str>,
    ) -> Result<InventorySyncResult> {
        let items = parse_inventory_json(body);
        let credits = body
            .get("RegularCredits")
            .and_then(|v| v.as_f64())
            .or_else(|| body.pointer("/Finance/RegularCredits").and_then(|v| v.as_f64()));
        let platinum = body
            .get("PremiumCredits")
            .and_then(|v| v.as_f64())
            .or_else(|| body.pointer("/Finance/PremiumCredits").and_then(|v| v.as_f64()));

        let synced_at = Utc::now().to_rfc3339();
        {
            let db = self.db.lock().await;
            db.replace_inventory(&items)?;
            if let Some(c) = credits {
                db.insert_stat("credits", c, &synced_at)?;
            }
            if let Some(p) = platinum {
                db.insert_stat("platinum", p, &synced_at)?;
            }
            if let Some(lvl) = body.get("PlayerLevel").and_then(|v| v.as_i64()) {
                let _ = db.set_setting("player_mastery_rank", &lvl.to_string());
            }
            if let Some(avatar) = body
                .get("ActiveAvatarImageType")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let _ = db.set_setting("player_avatar_unique", avatar);
            }
            // Helminth subsumed warframes (InfestedFoundry.ConsumedSuits[].s)
            let consumed: Vec<String> = body
                .pointer("/InfestedFoundry/ConsumedSuits")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.get("s").and_then(|x| x.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let _ = db.set_setting(
                "helminth_consumed_suits",
                &serde_json::to_string(&consumed).unwrap_or_else(|_| "[]".into()),
            );
            db.set_inventory_cache_meta(&synced_at, method, items.len(), account_id)?;
        }

        // Resolve in-game name from public profile when we know the account id
        let acc = {
            let db = self.db.lock().await;
            account_id
                .map(|s| s.to_string())
                .or_else(|| db.get_setting("inventory_account_id").ok().flatten())
        };
        if let Some(ref id) = acc {
            let _ = self.refresh_profile_name(id).await;
        }

        // Resolve glyph → PublicExport image URL
        let avatar_unique = {
            let db = self.db.lock().await;
            db.get_setting("player_avatar_unique").ok().flatten()
        };
        if let Some(ref u) = avatar_unique {
            if let Err(e) = self.ensure_avatar_url(u).await {
                warn!("avatar resolve failed: {e:#}");
            }
        }

        Ok(InventorySyncResult {
            item_count: items.len(),
            credits,
            platinum,
            synced_at,
            method: method.into(),
            from_cache: method.contains("cache") || method.contains("hydrate"),
        })
    }

    /// Public DE profile → DisplayName (+ PlayerLevel fallback).
    pub async fn refresh_profile_name(&self, account_id: &str) -> Result<String> {
        let url = format!(
            "https://api.warframe.com/cdn/getProfileViewingData.php?playerId={}",
            urlencoding_lite(account_id)
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("profile fetch")?;
        if !resp.status().is_success() {
            bail!("profile fetch {}", resp.status());
        }
        let body: Value = resp.json().await.context("profile json")?;
        let result = body
            .pointer("/Results/0")
            .or_else(|| body.get("Results").and_then(|v| v.as_array()).and_then(|a| a.first()));
        let name = result
            .and_then(|r| r.get("DisplayName"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .context("profile missing DisplayName")?;
        let lvl = result
            .and_then(|r| r.get("PlayerLevel"))
            .and_then(|v| v.as_i64())
            .or_else(|| body.pointer("/Stats/PlayerLevel").and_then(|v| v.as_i64()));

        {
            let db = self.db.lock().await;
            db.set_setting("player_display_name", &name)?;
            if let Some(l) = lvl {
                db.set_setting("player_mastery_rank", &l.to_string())?;
            }
        }
        info!("Player profile: {name} (MR {})", lvl.map(|n| n.to_string()).unwrap_or_else(|| "?".into()));
        Ok(name)
    }

    pub async fn player_profile(&self) -> Result<PlayerProfile> {
        let (account_id, display_name, mastery_rank, avatar_unique, avatar_url) = {
            let db = self.db.lock().await;
            let account_id = db.get_setting("inventory_account_id")?;
            let display_name = db.get_setting("player_display_name")?;
            let mastery_rank = db
                .get_setting("player_mastery_rank")?
                .and_then(|s| s.parse().ok());
            let avatar_unique = db.get_setting("player_avatar_unique")?;
            let avatar_url = db.get_setting("player_avatar_url")?;
            (account_id, display_name, mastery_rank, avatar_unique, avatar_url)
        };

        // Fill missing name from DE if we have an account id
        let display_name = if display_name.is_none() {
            if let Some(ref id) = account_id {
                self.refresh_profile_name(id).await.ok()
            } else {
                None
            }
        } else {
            display_name
        };

        // PlayerLevel + ActiveAvatarImageType from disk cache if missing
        let mut mastery_rank = mastery_rank;
        let mut avatar_unique = avatar_unique;
        if (mastery_rank.is_none() || avatar_unique.is_none()) && self.cache_path.exists() {
            let text = std::fs::read_to_string(&self.cache_path).unwrap_or_default();
            if let Ok(body) = serde_json::from_str::<Value>(&text) {
                if mastery_rank.is_none() {
                    if let Some(l) = body.get("PlayerLevel").and_then(|v| v.as_i64()) {
                        let db = self.db.lock().await;
                        let _ = db.set_setting("player_mastery_rank", &l.to_string());
                        mastery_rank = Some(l);
                    }
                }
                if avatar_unique.is_none() {
                    if let Some(a) = body
                        .get("ActiveAvatarImageType")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        let db = self.db.lock().await;
                        let _ = db.set_setting("player_avatar_unique", a);
                        avatar_unique = Some(a.to_string());
                    }
                }
            }
        }

        let avatar_url = match (avatar_url, avatar_unique.as_deref()) {
            (Some(url), _) if !url.is_empty() => Some(url),
            (_, Some(unique)) => self.ensure_avatar_url(unique).await.ok(),
            _ => None,
        };

        Ok(PlayerProfile {
            display_name,
            mastery_rank,
            account_id,
            avatar_url,
        })
    }

    /// Resolve Lotus avatar glyph path → content.warframe.com PublicExport texture URL.
    pub async fn ensure_avatar_url(&self, unique_name: &str) -> Result<String> {
        {
            let db = self.db.lock().await;
            if let (Ok(Some(u)), Ok(Some(url))) = (
                db.get_setting("player_avatar_unique"),
                db.get_setting("player_avatar_url"),
            ) {
                if u == unique_name && !url.is_empty() {
                    return Ok(url);
                }
            }
        }

        let texture = self
            .lookup_export_texture(unique_name)
            .await
            .with_context(|| format!("texture for {unique_name}"))?;
        let url = public_export_texture_url(&texture);
        {
            let db = self.db.lock().await;
            db.set_setting("player_avatar_unique", unique_name)?;
            db.set_setting("player_avatar_url", &url)?;
        }
        info!("Avatar glyph resolved → {url}");
        Ok(url)
    }

    async fn lookup_export_texture(&self, unique_name: &str) -> Result<String> {
        let manifest_path = self.data_dir.join("export_manifest.json");
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Some(tex) = texture_from_manifest_json(&text, unique_name) {
                return Ok(tex);
            }
        }

        info!("Fetching Warframe ExportManifest for avatar textures…");
        let index_bytes = self
            .client
            .get("https://content.warframe.com/PublicExport/index_en.txt.lzma")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let index_text = decompress_lzma(&index_bytes).context("decompress PublicExport index")?;
        let manifest_name = index_text
            .lines()
            .find(|l| l.starts_with("ExportManifest.json"))
            .context("ExportManifest missing from PublicExport index")?
            .to_string();
        let manifest_url =
            format!("https://content.warframe.com/PublicExport/Manifest/{manifest_name}");
        let manifest_bytes = self
            .client
            .get(&manifest_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let manifest_text = match decompress_lzma(&manifest_bytes) {
            Ok(s) => s,
            Err(_) => String::from_utf8(manifest_bytes.to_vec())
                .context("ExportManifest not utf-8")?,
        };
        if let Some(parent) = manifest_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&manifest_path, &manifest_text);

        texture_from_manifest_json(&manifest_text, unique_name)
            .with_context(|| format!("{unique_name} not in ExportManifest"))
    }

    fn write_raw_cache(&self, body: &Value) -> Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.cache_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(body)?)?;
        std::fs::rename(&tmp, &self.cache_path)?;
        info!("Wrote inventory cache {}", self.cache_path.display());
        Ok(())
    }
}

fn urlencoding_lite(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decompress_lzma(data: &[u8]) -> Result<String> {
    use std::io::{Cursor, Read};
    // PublicExport index is raw LZMA (0x5d), not .xz — use auto/lzma decoder.
    let stream = xz2::stream::Stream::new_auto_decoder(u64::MAX, 0)
        .or_else(|_| xz2::stream::Stream::new_lzma_decoder(u64::MAX))
        .context("lzma decoder init")?;
    let mut decoder = xz2::read::XzDecoder::new_stream(Cursor::new(data), stream);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .context("lzma decompress")?;
    String::from_utf8(out).context("lzma utf-8")
}

fn texture_from_manifest_json(text: &str, unique_name: &str) -> Option<String> {
    let body: Value = serde_json::from_str(text).ok()?;
    let arr = body
        .get("Manifest")
        .and_then(|v| v.as_array())
        .or_else(|| body.as_array())?;
    for entry in arr {
        if entry.get("uniqueName").and_then(|v| v.as_str()) == Some(unique_name) {
            return entry
                .get("textureLocation")
                .and_then(|v| v.as_str())
                .map(|s| s.replace('\\', "/"));
        }
    }
    None
}

fn public_export_texture_url(texture_location: &str) -> String {
    let path = texture_location.trim_start_matches('/');
    format!("https://content.warframe.com/PublicExport/{path}")
}

pub fn parse_inventory_json(body: &Value) -> Vec<InventoryItem> {
    let mut out = Vec::new();
    let mut by_unique: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // Live equipment bins — actual ownership (XPInfo alone is permanent mastery ledger)
    let owned_counts = count_owned_gear(body);

    // XPInfo — mastery XP (account-permanent; does not mean currently owned)
    if let Some(arr) = body.get("XPInfo").and_then(|v| v.as_array()) {
        for entry in arr {
            let unique = entry
                .get("ItemType")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if unique.is_empty() {
                continue;
            }
            let xp = entry.get("XP").and_then(|v| v.as_i64());
            let count = owned_counts.get(&unique).copied().unwrap_or(0);
            let mastered = xp.unwrap_or(0) >= mastery_xp_threshold(&unique);
            let idx = out.len();
            by_unique.insert(unique.clone(), idx);
            out.push(InventoryItem {
                unique_name: unique.clone(),
                name: display_name_from_unique(&unique),
                count,
                xp,
                mastered,
                item_type: classify_unique(&unique),
                url_name: guess_url_name(&unique),
                platinum: None,
                ducats: None,
                favorite: false,
                thumb: None,
                vaulted: None,
                rank: None,
            });
        }
    }

    // Owned gear with no XPInfo row yet (brand-new / unranked)
    for (unique, count) in &owned_counts {
        if by_unique.contains_key(unique) {
            continue;
        }
        let idx = out.len();
        by_unique.insert(unique.clone(), idx);
        out.push(InventoryItem {
            unique_name: unique.clone(),
            name: display_name_from_unique(unique),
            count: *count,
            xp: None,
            mastered: false,
            item_type: classify_unique(unique),
            url_name: guess_url_name(unique),
            platinum: None,
            ducats: None,
            favorite: false,
            thumb: None,
            vaulted: None,
            rank: None,
        });
    }

    // RawUpgrade / MiscItems / Recipes etc.
    for (key, type_label) in [
        ("RawUpgrades", "mod"),
        ("MiscItems", "misc"),
        ("Recipes", "blueprint"),
        ("Consumables", "consumable"),
        ("FlavourItems", "flavour"),
        ("ShipDecorations", "decoration"),
    ] {
        if let Some(arr) = body.get(key).and_then(|v| v.as_array()) {
            for entry in arr {
                let unique = entry
                    .get("ItemType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if unique.is_empty() {
                    continue;
                }
                let count = entry
                    .get("ItemCount")
                    .or_else(|| entry.get("Count"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1);
                out.push(InventoryItem {
                    unique_name: format!("{unique}#{key}"),
                    name: display_name_from_unique(&unique),
                    count,
                    xp: None,
                    mastered: false,
                    item_type: {
                        let classified = classify_unique(&unique);
                        if classified != "misc" {
                            classified
                        } else {
                            type_label.into()
                        }
                    },
                    url_name: guess_url_name(&unique),
                    platinum: None,
                    ducats: None,
                    favorite: false,
                    thumb: None,
                    vaulted: None,
                    rank: None,
                });
            }
        }
    }

    // Ranked mods / arcanes live in Upgrades with UpgradeFingerprint {"lvl":N}
    merge_upgrade_ranks(body, &mut out);

    out
}

/// Currently owned gear instances from DE inventory bins (not XPInfo).
fn count_owned_gear(body: &Value) -> std::collections::HashMap<String, i64> {
    const BINS: &[&str] = &[
        "Suits",
        "LongGuns",
        "Pistols",
        "Melee",
        "Sentinels",
        "SentinelWeapons",
        "SpaceSuits",
        "SpaceGuns",
        "SpaceMelee",
        "MechSuits",
        "OperatorAmps",
        "Hoverboards",
        "MoaPets",
        "KubrowPets",
        "CatbrowPets",
        "Horses",
        "DataKnives",
    ];
    let mut counts = std::collections::HashMap::new();
    for key in BINS {
        let Some(arr) = body.get(*key).and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in arr {
            let unique = entry
                .get("ItemType")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if unique.is_empty() {
                continue;
            }
            *counts.entry(unique.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Affinity needed for Rank 30 (mastery credit). XPInfo can exceed this while owned.
fn mastery_xp_threshold(unique: &str) -> i64 {
    let u = unique.to_lowercase();
    if u.contains("/powersuits/")
        || u.contains("/sentinels/")
        || u.contains("sentinelpowersuits")
        || u.contains("/pets/")
        || u.contains("kubrow")
        || u.contains("catbrow")
        || u.contains("moapet")
        || u.contains("/mechs/")
        || u.contains("entratimech")
    {
        900_000
    } else {
        450_000
    }
}

/// Apply max `lvl` from `Upgrades` onto matching RawUpgrades rows (or create rows).
fn merge_upgrade_ranks(body: &Value, out: &mut Vec<InventoryItem>) {
    let Some(arr) = body.get("Upgrades").and_then(|v| v.as_array()) else {
        return;
    };

    // ItemType → (max_lvl, instance_count)
    let mut ranks: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();
    for entry in arr {
        let unique = entry
            .get("ItemType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if unique.is_empty() {
            continue;
        }
        let Some(lvl) = fingerprint_lvl(entry.get("UpgradeFingerprint")) else {
            continue;
        };
        let e = ranks.entry(unique.to_string()).or_insert((lvl, 0));
        e.0 = e.0.max(lvl);
        e.1 += 1;
    }

    for (unique, (max_lvl, inst)) in ranks {
        let key_raw = format!("{unique}#RawUpgrades");
        if let Some(item) = out.iter_mut().find(|i| i.unique_name == key_raw) {
            item.rank = Some(match item.rank {
                Some(r) => r.max(max_lvl),
                None => max_lvl,
            });
            continue;
        }
        // Equipped / ranked only — not present in RawUpgrades
        out.push(InventoryItem {
            unique_name: key_raw,
            name: display_name_from_unique(&unique),
            count: inst.max(1),
            xp: None,
            mastered: false,
            item_type: classify_unique(&unique),
            url_name: guess_url_name(&unique),
            platinum: None,
            ducats: None,
            favorite: false,
            thumb: None,
            vaulted: None,
            rank: Some(max_lvl),
        });
    }
}

fn fingerprint_lvl(fp: Option<&Value>) -> Option<i64> {
    let v = fp?;
    if let Some(n) = v.get("lvl").and_then(|x| x.as_i64()) {
        return Some(n);
    }
    let s = v.as_str()?;
    let parsed: Value = serde_json::from_str(s).ok()?;
    parsed.get("lvl").and_then(|x| x.as_i64())
}

fn display_name_from_unique(unique: &str) -> String {
    let leaf = unique.rsplit('/').next().unwrap_or(unique);
    humanize_lotus_name(leaf)
}

fn humanize_inventory_label(name: &str) -> String {
    if name.contains(' ') {
        name.to_string()
    } else {
        humanize_lotus_name(name)
    }
}

fn classify_unique(unique: &str) -> String {
    let u = unique.to_lowercase();
    let bare = u.split('#').next().unwrap_or(&u);

    // Glyphs / titles live under …/AvatarImages/Warframes/… — not actual frames
    if u.contains("#flavouritems")
        || bare.contains("/avatarimages/")
        || bare.contains("/titles/")
        || bare.contains("/photobooth/")
    {
        return "flavour".into();
    }
    // Powersuits only — do not match "/warframes/" (avatar / store folders)
    if bare.contains("/powersuits/") {
        return "warframe".into();
    }
    // Mods / precepts / stances (RawUpgrades inventory + Lotus paths)
    // Arcanes (CosmeticEnhancers) are separate — «мистики»
    if bare.contains("/cosmeticenhancers/") {
        return "arcane".into();
    }
    if u.contains("#rawupgrades")
        || bare.contains("meleetrees")
        || bare.contains("/upgrades/")
        || bare.contains("precept")
        || bare.contains("/sentinelprecepts/")
        || bare.contains("/moaprecepts/")
        || bare.contains("/kubrowpetprecepts/")
        || bare.contains("/catbrowpetprecepts/")
        || bare.contains("creatureprecepts")
        || bare.contains("zanukapetprecepts")
    {
        return "mod".into();
    }
    // Blueprints often live under /Weapons/.../FooBlueprint#Recipes
    if bare.contains("blueprint")
        || u.contains("#recipes")
        || bare.contains("/recipes/")
        || bare.contains("weaponparts")
    {
        return if bare.contains("weaponparts") {
            "part".into()
        } else {
            "blueprint".into()
        };
    }
    if bare.contains("/weapons/")
        || bare.contains("/longguns/")
        || bare.contains("/pistols/")
        || bare.contains("/melee/")
    {
        return classify_weapon_slot(bare).into();
    }
    if bare.contains("/relics/") || bare.contains("voidprojection") {
        return "relic".into();
    }
    "misc".into()
}

fn classify_weapon_slot(u: &str) -> &'static str {
    if u.contains("operatoramplifiers") {
        return "misc";
    }
    // Archwing gear is not primary/secondary/melee inventory tabs
    if u.contains("/archwing/") {
        return "weapon";
    }
    if u.contains("/melee/") || u.contains("meleeweapon") || u.contains("mk1bo") || u.contains("mk1furax")
    {
        return "melee";
    }
    if u.contains("/pistols/")
        || u.contains("/pistol/")
        || u.contains("throwingweapons")
        || u.contains("/akimbo/")
        || u.contains("/secondaries/")
        || u.contains("/secondary/")
        || u.contains("grineerpistol")
        || u.contains("mk1furis")
        || u.contains("mk1kunai")
        || u.contains("thanopistol")
        || u.contains("operator/pistols")
    {
        return "secondary";
    }
    if u.contains("/longguns/")
        || u.contains("/rifle/")
        || u.contains("/bows/")
        || u.contains("/bow/")
        || u.contains("/shotgun")
        || u.contains("/sniper")
        || u.contains("speargun")
        || u.contains("launcher")
        || u.contains("/spears/")
        || u.contains("heavyweapons")
        || u.contains("mk1paris")
        || u.contains("mk1strun")
        || u.contains("thanorifle")
        || u.contains("flamethrower")
        || u.contains("grimoire")
        || u.contains("sentrifle")
    {
        return "primary";
    }
    "weapon"
}

fn guess_url_name(unique: &str) -> Option<String> {
    let leaf = unique.rsplit('/').next()?;
    let slug = normalize_name(&humanize_lotus_name(leaf)).replace(' ', "_");
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Find Warframe PID under Proton and scan readable memory for session credentials.
pub fn find_session_credentials() -> Result<SessionCredentials> {
    let pid = find_warframe_pid().context("Warframe process not found")?;
    info!("Scanning Warframe pid {pid} for session token…");
    scan_process_for_credentials(pid)
}

fn find_warframe_pid() -> Result<u32> {
    // Look for Warframe.x64.exe in process cmdline
    for entry in WalkDir::new("/proc").max_depth(2) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let cmd = cmdline.replace('\0', " ");
        if cmd.contains("Warframe.x64.exe") || cmd.contains("Warframe.exe") {
            return Ok(pid);
        }
    }
    bail!("no Warframe.x64.exe process");
}

fn scan_process_for_credentials(pid: u32) -> Result<SessionCredentials> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let mem_path = format!("/proc/{pid}/mem");
    let mut mem = File::open(&mem_path).with_context(|| {
        format!("open {mem_path} (need ptrace: sysctl kernel.yama.ptrace_scope=0 or setcap cap_sys_ptrace)")
    })?;

    let url_re1 = Regex::new(TOKEN_REGEXES[0]).unwrap();
    let url_re2 = Regex::new(TOKEN_REGEXES[1]).unwrap();
    let account_json = Regex::new(TOKEN_REGEXES[2]).unwrap();
    let nonce_json = Regex::new(TOKEN_REGEXES[3]).unwrap();
    let nonce_eq = Regex::new(TOKEN_REGEXES[4]).unwrap();

    let mut found_account: Option<String> = None;
    let mut found_nonce: Option<String> = None;
    let mut regions_ok = 0u32;
    let mut regions_fail = 0u32;

    for line in maps.lines() {
        if !line.contains("rw-p") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let range: Vec<&str> = parts[0].split('-').collect();
        if range.len() != 2 {
            continue;
        }
        let start = u64::from_str_radix(range[0], 16).unwrap_or(0);
        let end = u64::from_str_radix(range[1], 16).unwrap_or(0);
        if end <= start {
            continue;
        }
        // Warframe under Proton can keep auth URLs deep in large heaps
        let len = (end - start).min(64 * 1024 * 1024);
        if len < 64 {
            continue;
        }
        let mut buf = vec![0u8; len as usize];
        if mem.seek(SeekFrom::Start(start)).is_err() {
            regions_fail += 1;
            continue;
        }
        if mem.read_exact(&mut buf).is_err() {
            regions_fail += 1;
            continue;
        }
        regions_ok += 1;

        let text = String::from_utf8_lossy(&buf);

        if let Some(c) = url_re1.captures(&text) {
            found_account = Some(c[1].to_string());
            found_nonce = Some(c[2].to_string());
            break;
        }
        if let Some(c) = url_re2.captures(&text) {
            found_nonce = Some(c[1].to_string());
            found_account = Some(c[2].to_string());
            break;
        }

        if found_account.is_none() {
            if let Some(c) = account_json.captures(&text) {
                found_account = Some(c[1].to_string());
            }
        }
        if found_nonce.is_none() {
            if let Some(c) = nonce_json.captures(&text) {
                found_nonce = Some(c[1].to_string());
            } else if let Some(c) = nonce_eq.captures(&text) {
                found_nonce = Some(c[1].to_string());
            }
        }
        if found_account.is_some() && found_nonce.is_some() {
            break;
        }
    }

    info!(
        "Memory scan finished (regions_ok={regions_ok}, regions_fail={regions_fail}, account={}, nonce={})",
        found_account.is_some(),
        found_nonce.is_some()
    );

    // Also try warframe-api-helper if present on PATH as fallback
    if found_account.is_none() || found_nonce.is_none() {
        if let Ok(helper) = try_warframe_api_helper() {
            return Ok(helper);
        }
    }

    match (found_account, found_nonce) {
        (Some(account_id), Some(nonce)) => Ok(SessionCredentials { account_id, nonce }),
        _ => bail!(
            "session credentials not found in memory (ptrace ok, but accountId/nonce URL patterns missing). \
             Be logged into Warframe (Orbiter), then retry. Fallback: dump inventory.json with warframe-api-helper and Import."
        ),
    }
}

fn try_warframe_api_helper() -> Result<SessionCredentials> {
    // If user has inventory dump from helper, load it — credentials path varies.
    // Attempt: `warframe-api-helper` printing JSON with AccountId/Nonce (best-effort).
    let output = Command::new("warframe-api-helper")
        .arg("--print-credentials")
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let v: Value = serde_json::from_str(&text)?;
            let account_id = v
                .get("AccountId")
                .or_else(|| v.get("accountId"))
                .and_then(|x| x.as_str())
                .context("AccountId")?
                .to_string();
            let nonce = v
                .get("Nonce")
                .or_else(|| v.get("nonce"))
                .and_then(|x| x.as_str())
                .context("Nonce")?
                .to_string();
            return Ok(SessionCredentials { account_id, nonce });
        }
    }
    bail!("helper unavailable")
}

/// Load inventory from a previously exported JSON file (AlecaFrame / helper dump).
pub async fn import_inventory_file(
    db: &Arc<tokio::sync::Mutex<Database>>,
    path: &Path,
) -> Result<InventorySyncResult> {
    // Prefer InventoryService when available; keep thin helper for daemon import route.
    let text = std::fs::read_to_string(path)?;
    let body: Value = serde_json::from_str(&text)?;
    let items = parse_inventory_json(&body);
    let credits = body.get("RegularCredits").and_then(|v| v.as_f64());
    let platinum = body.get("PremiumCredits").and_then(|v| v.as_f64());
    let synced_at = Utc::now().to_rfc3339();
    {
        let db = db.lock().await;
        db.replace_inventory(&items)?;
        if let Some(c) = credits {
            db.insert_stat("credits", c, &synced_at)?;
        }
        if let Some(p) = platinum {
            db.insert_stat("platinum", p, &synced_at)?;
        }
        db.set_inventory_cache_meta(&synced_at, "file_import", items.len(), None)?;
    }
    // Also copy into standard cache location when possible
    if let Some(home) = dirs::data_dir() {
        let cache = home.join("bngframe/inventory_cache.json");
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(path, &cache);
    }
    warn!("Imported inventory from {}", path.display());
    Ok(InventorySyncResult {
        item_count: items.len(),
        credits,
        platinum,
        synced_at,
        method: "file_import".into(),
        from_cache: false,
    })
}

pub fn default_inventory_dump_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".warframe-helper/inventory.json"));
        v.push(home.join(".local/share/bngframe/inventory.json"));
    }
    v
}
