//! Local disk + memory cache for remote item / wiki / glyph images.
//! Served via `GET /api/img?u=<url>` so the UI never hits CDNs twice.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

const MAX_CONCURRENT_DOWNLOADS: usize = 8;
const NEGATIVE_TTL: Duration = Duration::from_secs(6 * 3600);
const MEM_MAX_ENTRIES: usize = 512;
const MEM_MAX_BYTES: usize = 96 * 1024 * 1024;

#[derive(Clone)]
pub struct CachedImage {
    pub path: PathBuf,
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
}

struct MemEntry {
    bytes: Arc<Vec<u8>>,
    content_type: &'static str,
    path: PathBuf,
    size: usize,
}

pub struct ImageCache {
    client: reqwest::Client,
    dir: PathBuf,
    /// Cap concurrent remote fetches (different URLs run in parallel).
    download_sem: Arc<Semaphore>,
    /// Coalesce concurrent downloads of the same URL.
    key_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Failed fetches — avoid re-hitting CDN on WikiImg fallback churn.
    negative: Mutex<HashMap<String, Instant>>,
    /// Hot bytes so hits don't re-read disk.
    memory: Mutex<MemoryLru>,
}

struct MemoryLru {
    map: HashMap<String, MemEntry>,
    order: VecDeque<String>,
    bytes: usize,
}

impl MemoryLru {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&mut self, key: &str) -> Option<CachedImage> {
        let entry = self.map.get(key)?;
        // Refresh LRU order
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.to_string());
        }
        Some(CachedImage {
            path: entry.path.clone(),
            bytes: entry.bytes.clone(),
            content_type: entry.content_type,
        })
    }

    fn insert(&mut self, key: String, path: PathBuf, bytes: Arc<Vec<u8>>, content_type: &'static str) {
        let size = bytes.len();
        if size == 0 || size > MEM_MAX_BYTES / 2 {
            return;
        }
        if let Some(old) = self.map.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.size);
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        }
        while self.map.len() >= MEM_MAX_ENTRIES
            || self.bytes.saturating_add(size) > MEM_MAX_BYTES
        {
            let Some(evict) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.map.remove(&evict) {
                self.bytes = self.bytes.saturating_sub(old.size);
            }
        }
        self.bytes = self.bytes.saturating_add(size);
        self.map.insert(
            key.clone(),
            MemEntry {
                bytes,
                content_type,
                path,
                size,
            },
        );
        self.order.push_back(key);
    }
}

impl ImageCache {
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let dir = cache_dir.join("img");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("BNGframe/0.1")
                .timeout(Duration::from_secs(20))
                .pool_max_idle_per_host(4)
                .build()
                .expect("client"),
            dir,
            download_sem: Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            key_locks: Mutex::new(HashMap::new()),
            negative: Mutex::new(HashMap::new()),
            memory: Mutex::new(MemoryLru::new()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn cache_key(url: &str) -> String {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        format!("{:016x}", h.finish())
    }

    /// WFM thumbs embed MD5: `lith_g6_relic.<32hex>.128x128.png`.
    /// Many relics of one era share the same hash → one file on disk.
    fn content_address_key(url: &str) -> Option<String> {
        for part in url.split(|c| c == '/' || c == '?' || c == '#') {
            for token in part.split('.') {
                if token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Some(token.to_ascii_lowercase());
                }
            }
        }
        None
    }

    fn ext_from_url_or_ct(url: &str, content_type: Option<&str>) -> &'static str {
        let path = url.split('?').next().unwrap_or(url);
        let lower = path.to_lowercase();
        if lower.contains(".webp") {
            return "webp";
        }
        if lower.contains(".jpg") || lower.contains(".jpeg") {
            return "jpg";
        }
        if lower.contains(".gif") {
            return "gif";
        }
        if lower.contains(".svg") {
            return "svg";
        }
        if let Some(ct) = content_type {
            let ct = ct.to_lowercase();
            if ct.contains("webp") {
                return "webp";
            }
            if ct.contains("jpeg") || ct.contains("jpg") {
                return "jpg";
            }
            if ct.contains("gif") {
                return "gif";
            }
            if ct.contains("svg") {
                return "svg";
            }
        }
        "png"
    }

    fn find_existing(&self, key: &str) -> Option<PathBuf> {
        for ext in ["png", "webp", "jpg", "jpeg", "gif", "svg"] {
            let p = self.dir.join(format!("{key}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    async fn key_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut map = self.key_locks.lock().await;
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn is_negative(&self, key: &str) -> bool {
        let mut neg = self.negative.lock().await;
        if let Some(at) = neg.get(key).copied() {
            if at.elapsed() < NEGATIVE_TTL {
                return true;
            }
            neg.remove(key);
        }
        false
    }

    async fn mark_negative(&self, key: &str) {
        let mut neg = self.negative.lock().await;
        neg.insert(key.to_string(), Instant::now());
        // Bound map size
        if neg.len() > 4000 {
            let cutoff = Instant::now() - NEGATIVE_TTL;
            neg.retain(|_, t| *t > cutoff);
        }
    }

    async fn load_path(&self, keys: &[&str], path: PathBuf) -> Result<CachedImage> {
        {
            let mut mem = self.memory.lock().await;
            for k in keys {
                if let Some(hit) = mem.get(k) {
                    return Ok(hit);
                }
            }
        }
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let content_type = content_type_for_path(&path);
        let bytes = Arc::new(bytes);
        {
            let mut mem = self.memory.lock().await;
            for k in keys {
                mem.insert(k.to_string(), path.clone(), bytes.clone(), content_type);
            }
        }
        Ok(CachedImage {
            path,
            bytes,
            content_type,
        })
    }

    fn resolve_existing(&self, url_key: &str, content_key: Option<&str>) -> Option<PathBuf> {
        if let Some(p) = self.find_existing(url_key) {
            return Some(p);
        }
        if let Some(ck) = content_key {
            if let Some(p) = self.find_existing(ck) {
                return Some(p);
            }
        }
        None
    }

    /// Ensure `url` is on disk and return path + bytes (memory-cached).
    pub async fn get(&self, url: &str) -> Result<CachedImage> {
        let url = url.trim();
        if url.is_empty() {
            bail!("empty image url");
        }
        if url.starts_with("/api/img") {
            bail!("refusing to cache local proxy url");
        }
        let url_key = Self::cache_key(url);
        let content_key = Self::content_address_key(url);
        let mem_keys: Vec<&str> = match content_key.as_deref() {
            Some(ck) => vec![url_key.as_str(), ck],
            None => vec![url_key.as_str()],
        };
        // Coalesce downloads that share the same WFM content hash.
        let coalesce_key = content_key.clone().unwrap_or_else(|| url_key.clone());

        {
            let mut mem = self.memory.lock().await;
            for k in &mem_keys {
                if let Some(hit) = mem.get(k) {
                    return Ok(hit);
                }
            }
        }

        if let Some(p) = self.resolve_existing(&url_key, content_key.as_deref()) {
            return self.load_path(&mem_keys, p).await;
        }

        if self.is_negative(&url_key).await {
            bail!("image previously failed (cached miss) for {url}");
        }

        let lock = self.key_lock(&coalesce_key).await;
        let _guard = lock.lock().await;

        {
            let mut mem = self.memory.lock().await;
            for k in &mem_keys {
                if let Some(hit) = mem.get(k) {
                    return Ok(hit);
                }
            }
        }
        if let Some(p) = self.resolve_existing(&url_key, content_key.as_deref()) {
            return self.load_path(&mem_keys, p).await;
        }
        if self.is_negative(&url_key).await {
            bail!("image previously failed (cached miss) for {url}");
        }

        let _permit = self
            .download_sem
            .acquire()
            .await
            .expect("download semaphore");

        if let Some(p) = self.resolve_existing(&url_key, content_key.as_deref()) {
            return self.load_path(&mem_keys, p).await;
        }

        debug!("caching image {url}");
        let storage_key = content_key.clone().unwrap_or_else(|| url_key.clone());
        let result = async {
            let resp = self
                .client
                .get(url)
                .header("Accept", "image/*,*/*;q=0.8")
                .send()
                .await
                .with_context(|| format!("fetch image {url}"))?;
            if !resp.status().is_success() {
                bail!("image fetch {} for {url}", resp.status());
            }
            let ct = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let ext = Self::ext_from_url_or_ct(url, ct.as_deref());
            let bytes = resp.bytes().await.context("image body")?;
            if bytes.is_empty() {
                bail!("empty image body for {url}");
            }
            let path = self.dir.join(format!("{storage_key}.{ext}"));
            if !path.is_file() {
                let tmp = path.with_extension(format!("{ext}.tmp"));
                tokio::fs::write(&tmp, &bytes)
                    .await
                    .with_context(|| format!("write {}", tmp.display()))?;
                tokio::fs::rename(&tmp, &path)
                    .await
                    .with_context(|| format!("rename {}", path.display()))?;
            }
            // Alias URL-hash → content-hash so later lookups by either key hit.
            if storage_key != url_key {
                let alias = self.dir.join(format!("{url_key}.{ext}"));
                if !alias.is_file() {
                    let _ = std::fs::hard_link(&path, &alias)
                        .or_else(|_| std::fs::copy(&path, &alias).map(|_| ()));
                }
            }
            let content_type = content_type_for_ext(ext);
            Ok::<_, anyhow::Error>((path, bytes.to_vec(), content_type))
        }
        .await;

        match result {
            Ok((path, raw, content_type)) => {
                let bytes = Arc::new(raw);
                {
                    let mut mem = self.memory.lock().await;
                    for k in &mem_keys {
                        mem.insert(k.to_string(), path.clone(), bytes.clone(), content_type);
                    }
                }
                Ok(CachedImage {
                    path,
                    bytes,
                    content_type,
                })
            }
            Err(e) => {
                self.mark_negative(&url_key).await;
                Err(e)
            }
        }
    }

    /// Ensure `url` is on disk; return absolute path.
    pub async fn ensure(&self, url: &str) -> Result<PathBuf> {
        Ok(self.get(url).await?.path)
    }

    /// Warm a list of remote URLs (best-effort, parallel with concurrency cap).
    pub async fn warm(self: &Arc<Self>, urls: impl IntoIterator<Item = String>) {
        let mut seen = std::collections::HashSet::new();
        let mut handles = Vec::new();
        for url in urls {
            if url.is_empty() || !seen.insert(url.clone()) {
                continue;
            }
            let this = Arc::clone(self);
            handles.push(tokio::spawn(async move {
                if let Err(e) = this.ensure(&url).await {
                    warn!("img warm {url}: {e:#}");
                }
            }));
            // Bound spawn fan-out; semaphore still caps actual downloads.
            if handles.len() >= 64 {
                for h in handles.drain(..) {
                    let _ = h.await;
                }
            }
        }
        for h in handles {
            let _ = h.await;
        }
    }
}

fn content_type_for_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

/// Guess Content-Type from file extension for Serve responses.
pub fn content_type_for_path(path: &Path) -> &'static str {
    content_type_for_ext(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or(""),
    )
}
