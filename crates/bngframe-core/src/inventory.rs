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
use crate::pricing::PricingService;

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
        let (mut items, catalog, cache, prices) = {
            let db = self.db.lock().await;
            let items = db.list_inventory()?;
            let catalog = db.all_items()?;
            let cache = db.inventory_cache_meta()?;
            // preload all prices in one pass
            let mut prices = std::collections::HashMap::new();
            for p in db.list_all_prices()? {
                prices.insert(p.url_name, p.platinum);
            }
            (items, catalog, cache, prices)
        };

        let by_norm = PricingService::build_catalog_index(&catalog);

        for item in &mut items {
            if let Some(row) = PricingService::resolve_market_item_indexed(&item.name, &by_norm) {
                item.url_name = Some(row.url_name.clone());
                item.ducats = row.ducats.or(item.ducats);
                if let Some(plat) = prices.get(&row.url_name) {
                    item.platinum = Some(*plat);
                }
            } else if let Some(ref url) = item.url_name {
                if let Some(plat) = prices.get(url) {
                    item.platinum = Some(*plat);
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
        self.apply_inventory_json(&body, method, None).await
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
            db.set_inventory_cache_meta(&synced_at, method, items.len(), account_id)?;
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

pub fn parse_inventory_json(body: &Value) -> Vec<InventoryItem> {
    let mut out = Vec::new();

    // XPInfo — mastered / owned gear
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
            let name = display_name_from_unique(&unique);
            let mastered = xp.unwrap_or(0) > 0;
            out.push(InventoryItem {
                unique_name: unique.clone(),
                name,
                count: 1,
                xp,
                mastered,
                item_type: classify_unique(&unique),
                url_name: guess_url_name(&unique),
                platinum: None,
                ducats: None,
                favorite: false,
            });
        }
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
                    item_type: type_label.into(),
                    url_name: guess_url_name(&unique),
                    platinum: None,
                    ducats: None,
                    favorite: false,
                });
            }
        }
    }

    out
}

fn display_name_from_unique(unique: &str) -> String {
    unique
        .rsplit('/')
        .next()
        .unwrap_or(unique)
        .replace('_', " ")
}

fn classify_unique(unique: &str) -> String {
    let u = unique.to_lowercase();
    if u.contains("/warframes/") {
        "warframe".into()
    } else if u.contains("/weapons/") || u.contains("/longguns/") || u.contains("/pistols/") || u.contains("/melee/") {
        "weapon".into()
    } else if u.contains("/upgrades/") {
        "mod".into()
    } else if u.contains("/relics/") || u.contains("voidprojection") {
        "relic".into()
    } else {
        "misc".into()
    }
}

fn guess_url_name(unique: &str) -> Option<String> {
    let base = unique.rsplit('/').next()?.to_lowercase();
    let slug = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
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
