//! Local disk cache for remote item / wiki / glyph images.
//! Served via `GET /api/img?u=<url>` so the UI never hits CDNs twice.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, warn};

pub struct ImageCache {
    client: reqwest::Client,
    dir: PathBuf,
    /// Serialize downloads of the same key.
    gate: Mutex<()>,
}

impl ImageCache {
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let dir = cache_dir.join("img");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("BNGframe/0.1")
                .timeout(Duration::from_secs(45))
                .build()
                .expect("client"),
            dir,
            gate: Mutex::new(()),
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

    /// Ensure `url` is on disk; return absolute path.
    pub async fn ensure(&self, url: &str) -> Result<PathBuf> {
        let url = url.trim();
        if url.is_empty() {
            bail!("empty image url");
        }
        // Already a local cache URL — shouldn't be nested
        if url.starts_with("/api/img") {
            bail!("refusing to cache local proxy url");
        }
        let key = Self::cache_key(url);
        if let Some(p) = self.find_existing(&key) {
            return Ok(p);
        }

        let _gate = self.gate.lock().await;
        if let Some(p) = self.find_existing(&key) {
            return Ok(p);
        }

        debug!("caching image {url}");
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
        let path = self.dir.join(format!("{key}.{ext}"));
        let tmp = path.with_extension(format!("{ext}.tmp"));
        tokio::fs::write(&tmp, &bytes)
            .await
            .with_context(|| format!("write {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("rename {}", path.display()))?;
        Ok(path)
    }

    /// Warm a list of remote URLs (best-effort, sequential to be polite).
    pub async fn warm(self: &Arc<Self>, urls: impl IntoIterator<Item = String>) {
        for url in urls {
            if url.is_empty() {
                continue;
            }
            if let Err(e) = self.ensure(&url).await {
                warn!("img warm {url}: {e:#}");
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

/// Guess Content-Type from file extension for Serve responses.
pub fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}
