use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::config::Config;
use crate::db::Database;

const V1_API: &str = "https://api.warframe.market/v1";
const V2_API: &str = "https://api.warframe.market/v2";
const WFM_WS_URL: &str = "wss://ws.warframe.market/socket";
const WFM_WS_PROTOCOL: &str = "wfm";
const MY_ORDERS_TTL: Duration = Duration::from_secs(60);
const MY_ORDERS_DB_KEY: &str = "wfmarket_my_orders_cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketOrder {
    pub id: String,
    pub item_url_name: String,
    pub order_type: String,
    pub platinum: f64,
    pub quantity: i64,
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Mod / arcane rank (0–5 for mystics). Absent for items without ranks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<i64>,
    /// Relic quality etc. (`intact` / `exceptional` / …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_name: Option<String>,
    /// Always English WFM/catalog name (for secondary label under localized name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_name_en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingSuggestion {
    pub inventory_name: String,
    pub url_name: Option<String>,
    pub count: i64,
    pub suggested_plat: Option<f64>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketAuthState {
    pub authenticated: bool,
    /// `online` | `ingame` | `invisible` when authenticated; omitted when logged out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

struct PresenceState {
    status: String,
    stop: Option<oneshot::Sender<()>>,
}

struct MyOrdersCache {
    orders: Vec<MarketOrder>,
    fetched_at: Instant,
}

pub struct MarketService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
    presence: Arc<Mutex<PresenceState>>,
    my_orders_cache: Mutex<Option<MyOrdersCache>>,
}

impl MarketService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("BNGframe/0.1")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("client"),
            db,
            presence: Arc::new(Mutex::new(PresenceState {
                status: "invisible".into(),
                stop: None,
            })),
            my_orders_cache: Mutex::new(None),
        }
    }

    pub async fn sign_in(&self, email: &str, password: &str) -> Result<String> {
        // WFM CSRF: anonymous JWT cookie from the site embeds csrf_token in its payload.
        let (session_jwt, csrf) = self.fetch_wfm_csrf_session().await?;

        let device_id = {
            let db = self.db.lock().await;
            match db.get_setting("wfmarket_device_id")? {
                Some(id) if !id.is_empty() => id,
                _ => {
                    let id = uuid::Uuid::new_v4().to_string();
                    let _ = db.set_setting("wfmarket_device_id", &id);
                    id
                }
            }
        };

        let body = serde_json::json!({
            "email": email,
            "password": password,
            "auth_type": "header",
            "device_id": device_id,
        });
        let resp = self
            .client
            .post(format!("{V1_API}/auth/signin"))
            .header("Language", "en")
            .header("Platform", "pc")
            // Required by WFM: literal "JWT" means "return token in Authorization header".
            .header("Authorization", "JWT")
            .header("Cookie", format!("JWT={session_jwt}"))
            .header("X-CSRFTOKEN", &csrf)
            .header("X-CSRF-Token", &csrf)
            .json(&body)
            .send()
            .await
            .context("WFM signin")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("signin failed {status}: {text}");
        }

        let jwt = resp
            .headers()
            .get("Authorization")
            .or_else(|| resp.headers().get("authorization"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_start_matches("JWT ").trim().to_string())
            .context("missing Authorization JWT header")?;

        {
            let db = self.db.lock().await;
            db.set_setting("wfmarket_jwt", &jwt)?;
        }
        info!("warframe.market sign-in OK");
        Ok(jwt)
    }

    /// Hit warframe.market to obtain an anonymous session JWT that carries `csrf_token`.
    async fn fetch_wfm_csrf_session(&self) -> Result<(String, String)> {
        let resp = self
            .client
            .get("https://warframe.market/")
            .header("Accept", "text/html")
            .send()
            .await
            .context("WFM session bootstrap")?;
        let jwt = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| {
                c.split(';')
                    .next()
                    .and_then(|pair| pair.strip_prefix("JWT="))
                    .map(str::to_string)
            })
            .context("WFM did not set JWT cookie (CSRF session)")?;
        let csrf = csrf_token_from_jwt(&jwt)
            .context("WFM JWT cookie missing csrf_token claim")?;
        Ok((jwt, csrf))
    }

    pub async fn save_jwt(&self, jwt: &str) -> Result<()> {
        let db = self.db.lock().await;
        db.set_setting("wfmarket_jwt", jwt)?;
        Ok(())
    }

    pub async fn get_jwt(&self, cfg: &Config) -> Result<Option<String>> {
        if let Some(ref j) = cfg.wfmarket_jwt {
            if !j.is_empty() {
                return Ok(Some(j.clone()));
            }
        }
        let db = self.db.lock().await;
        Ok(db.get_setting("wfmarket_jwt")?)
    }

    pub async fn auth_state(&self, cfg: &Config) -> Result<MarketAuthState> {
        let jwt = self.get_jwt(cfg).await?;
        let authenticated = jwt.as_ref().is_some_and(|j| !j.is_empty());
        let status = if authenticated {
            Some(self.presence.lock().await.status.clone())
        } else {
            None
        };
        Ok(MarketAuthState {
            authenticated,
            status,
        })
    }

    /// Set WFM visibility via WebSocket (`online` / `ingame` / `invisible`).
    ///
    /// Do **not** send `duration` — that enables WFM's temporary "expires in …"
    /// status-keeper mode. Persistent online/ingame is tied to the open socket.
    pub async fn set_status(&self, cfg: &Config, status: &str) -> Result<MarketAuthState> {
        let status = status.trim().to_ascii_lowercase();
        if !matches!(status.as_str(), "online" | "ingame" | "invisible") {
            bail!("status must be online, ingame, or invisible");
        }
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;

        {
            let mut presence = self.presence.lock().await;
            if let Some(stop) = presence.stop.take() {
                let _ = stop.send(());
            }
            presence.status = status.clone();
        }

        if status == "invisible" {
            wfm_ws_set_status_once(&jwt, "invisible").await?;
        } else {
            let (stop_tx, stop_rx) = oneshot::channel();
            let mut ws = wfm_ws_connect_authed(&jwt).await?;
            wfm_ws_send_status(&mut ws, &status).await?;
            {
                let mut presence = self.presence.lock().await;
                presence.stop = Some(stop_tx);
                presence.status = status.clone();
            }
            let status_owned = status.clone();
            let presence = self.presence.clone();
            tokio::spawn(async move {
                if let Err(e) = wfm_ws_keep_alive(ws, stop_rx).await {
                    warn!("WFM presence ended: {e:#}");
                }
                let mut p = presence.lock().await;
                if p.status == status_owned {
                    p.stop = None;
                    p.status = "invisible".into();
                }
            });
        }

        self.auth_state(cfg).await
    }

    pub async fn my_orders(&self, cfg: &Config) -> Result<Vec<MarketOrder>> {
        self.my_orders_cached(cfg, false).await
    }

    /// Cached my-orders. `force` bypasses TTL (after create/close/delete or UI refresh).
    pub async fn my_orders_cached(&self, cfg: &Config, force: bool) -> Result<Vec<MarketOrder>> {
        if !force {
            let guard = self.my_orders_cache.lock().await;
            if let Some(c) = guard.as_ref() {
                if c.fetched_at.elapsed() < MY_ORDERS_TTL {
                    return Ok(c.orders.clone());
                }
            }
        }

        match self.fetch_my_orders(cfg).await {
            Ok(orders) => {
                self.store_orders_cache(orders.clone()).await;
                Ok(orders)
            }
            Err(e) => {
                if let Some(c) = self.my_orders_cache.lock().await.as_ref() {
                    warn!("my_orders fetch failed, serving memory cache: {e:#}");
                    return Ok(c.orders.clone());
                }
                if let Some(disk) = self.load_orders_disk().await {
                    warn!("my_orders fetch failed, serving disk cache: {e:#}");
                    *self.my_orders_cache.lock().await = Some(MyOrdersCache {
                        orders: disk.clone(),
                        fetched_at: Instant::now(),
                    });
                    return Ok(disk);
                }
                Err(e)
            }
        }
    }

    async fn invalidate_my_orders_cache(&self) {
        *self.my_orders_cache.lock().await = None;
    }

    async fn store_orders_cache(&self, orders: Vec<MarketOrder>) {
        *self.my_orders_cache.lock().await = Some(MyOrdersCache {
            orders: orders.clone(),
            fetched_at: Instant::now(),
        });
        if let Ok(raw) = serde_json::to_string(&orders) {
            let db = self.db.lock().await;
            let _ = db.set_setting(MY_ORDERS_DB_KEY, &raw);
        }
    }

    async fn load_orders_disk(&self) -> Option<Vec<MarketOrder>> {
        let db = self.db.lock().await;
        let raw = db.get_setting(MY_ORDERS_DB_KEY).ok().flatten()?;
        serde_json::from_str(&raw).ok()
    }

    async fn fetch_my_orders(&self, cfg: &Config) -> Result<Vec<MarketOrder>> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        let resp = self
            .client
            .get(format!("{V2_API}/orders/my"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .header("Crossplay", "true")
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("orders fetch {status}: {text}");
        }
        let body: Value = resp.json().await?;
        let arr = body
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let prefer_ru = cfg.prefer_russian_names();
        let mut info_cache: HashMap<String, ItemInfo> = HashMap::new();
        let mut out = Vec::with_capacity(arr.len());
        for o in arr {
            let item_id = o
                .get("itemId")
                .or_else(|| o.get("item_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let info = if item_id.is_empty() {
                ItemInfo::default()
            } else if let Some(cached) = info_cache.get(&item_id) {
                cached.clone()
            } else {
                let resolved = self
                    .resolve_item_info(&item_id, prefer_ru)
                    .await
                    .unwrap_or_else(|_| ItemInfo {
                        slug: item_id.clone(),
                        ..ItemInfo::default()
                    });
                info_cache.insert(item_id.clone(), resolved.clone());
                resolved
            };
            let order_type = o
                .get("type")
                .or_else(|| o.get("order_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("sell")
                .to_string();
            out.push(MarketOrder {
                id: o.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                item_url_name: info.slug.clone(),
                order_type,
                platinum: o
                    .get("platinum")
                    .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
                    .unwrap_or(0.0),
                quantity: o.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1),
                visible: o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
                user: None,
                status: None,
                rank: order_rank(&o),
                subtype: o
                    .get("subtype")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                item_name: info.name.clone(),
                item_name_en: info.name_en.clone(),
                thumb: info.thumb.clone(),
            });
        }
        out.sort_by(|a, b| {
            let ta = if a.order_type == "sell" { 0 } else { 1 };
            let tb = if b.order_type == "sell" { 0 } else { 1 };
            ta.cmp(&tb)
                .then(a.item_url_name.cmp(&b.item_url_name))
                .then(
                    b.platinum
                        .partial_cmp(&a.platinum)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        Ok(out)
    }

    /// Public orders for an item (no JWT). Prefer online/ingame sellers for WTS.
    pub async fn item_orders(&self, url_name: &str) -> Result<Vec<MarketOrder>> {
        let mut candidates = vec![url_name.to_string()];
        let alt = crate::pricing::normalize_market_slug(url_name);
        if alt != url_name {
            candidates.push(alt);
        }
        let mut last_err = None;
        for slug in candidates {
            let url = format!("{V2_API}/orders/item/{slug}");
            let resp = self
                .client
                .get(&url)
                .header("Language", "ru")
                .header("Platform", "pc")
                .header("Crossplay", "true")
                .send()
                .await
                .context("WFM item orders")?;
            if !resp.status().is_success() {
                last_err = Some(format!("item orders fetch {}", resp.status()));
                continue;
            }
            let body: Value = resp.json().await?;
            let arr = body
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut out = Vec::with_capacity(arr.len());
            for o in arr {
                let order_type = o
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sell")
                    .to_string();
                let user = o
                    .pointer("/user/ingameName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let status = o
                    .pointer("/user/status")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                out.push(MarketOrder {
                    id: o.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                    item_url_name: slug.clone(),
                    order_type,
                    platinum: o.get("platinum").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    quantity: o.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1),
                    visible: o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
                    user,
                    status,
                    rank: order_rank(&o),
                    subtype: o
                        .get("subtype")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    item_name: None,
                    item_name_en: None,
                    thumb: None,
                });
            }
            return Ok(out);
        }
        bail!(last_err.unwrap_or_else(|| "item orders fetch failed".into()))
    }

    pub async fn create_order(
        &self,
        cfg: &Config,
        item_url_name: &str,
        order_type: &str,
        platinum: i64,
        quantity: i64,
        rank: Option<i64>,
    ) -> Result<Value> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        let ot = order_type.trim().to_ascii_lowercase();
        if !matches!(ot.as_str(), "sell" | "buy") {
            bail!("order_type must be sell or buy");
        }
        if platinum <= 0 || quantity <= 0 {
            bail!("platinum and quantity must be > 0");
        }

        let meta = self.resolve_item_meta(item_url_name).await?;
        let order_rank = meta.max_rank.map(|max| {
            let r = rank.unwrap_or(0).clamp(0, max);
            r
        });

        let want_slug = crate::pricing::normalize_market_slug(item_url_name);
        // Duplicate listing → bump quantity on the existing matching order.
        if let Ok(existing) = self.my_orders_cached(cfg, true).await {
            if let Some(prev) = existing.iter().find(|o| {
                o.order_type.eq_ignore_ascii_case(&ot)
                    && crate::pricing::normalize_market_slug(&o.item_url_name) == want_slug
                    && o.rank == order_rank
            }) {
                let new_qty = prev.quantity + quantity;
                return self
                    .update_order(
                        cfg,
                        &prev.id,
                        prev.platinum.round() as i64,
                        new_qty,
                        prev.visible,
                        prev.rank,
                    )
                    .await;
            }
        }

        let mut body = serde_json::json!({
            "itemId": meta.id,
            "type": ot,
            "platinum": platinum,
            "quantity": quantity,
            "visible": true,
        });
        // Mods / arcanes: WFM requires `rank` when item has maxRank.
        if let Some(r) = order_rank {
            body["rank"] = serde_json::json!(r);
        }
        let resp = self
            .client
            .post(format!("{V2_API}/order"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .header("Crossplay", "true")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("create order {status}: {text}");
        }
        self.invalidate_my_orders_cache().await;
        let _ = self.my_orders_cached(cfg, true).await;
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    /// Update platinum / quantity / visibility on an existing order.
    pub async fn update_order(
        &self,
        cfg: &Config,
        order_id: &str,
        platinum: i64,
        quantity: i64,
        visible: bool,
        rank: Option<i64>,
    ) -> Result<Value> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        if order_id.trim().is_empty() {
            bail!("missing order id");
        }
        if platinum <= 0 || quantity <= 0 {
            bail!("platinum and quantity must be > 0");
        }
        let mut body = serde_json::json!({
            "platinum": platinum,
            "quantity": quantity,
            "visible": visible,
        });
        if let Some(r) = rank {
            body["rank"] = serde_json::json!(r);
        }
        let resp = self
            .client
            .patch(format!("{V2_API}/order/{order_id}"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .header("Crossplay", "true")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // Some WFM builds accept PUT instead of PATCH.
            let resp2 = self
                .client
                .put(format!("{V2_API}/order/{order_id}"))
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Language", "en")
                .header("Platform", "pc")
                .header("Crossplay", "true")
                .json(&body)
                .send()
                .await?;
            let status2 = resp2.status();
            let text2 = resp2.text().await.unwrap_or_default();
            if !status2.is_success() {
                bail!("update order {status}/{status2}: {text} | {text2}");
            }
            self.invalidate_my_orders_cache().await;
            let _ = self.my_orders_cached(cfg, true).await;
            return Ok(serde_json::from_str(&text2).unwrap_or(Value::String(text2)));
        }
        self.invalidate_my_orders_cache().await;
        let _ = self.my_orders_cached(cfg, true).await;
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    /// Remove an order from the market (no transaction).
    pub async fn delete_order(&self, cfg: &Config, order_id: &str) -> Result<()> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        if order_id.trim().is_empty() {
            bail!("missing order id");
        }
        let resp = self
            .client
            .delete(format!("{V2_API}/order/{order_id}"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .header("Crossplay", "true")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("delete order {status}: {text}");
        }
        self.invalidate_my_orders_cache().await;
        let _ = self.my_orders_cached(cfg, true).await;
        Ok(())
    }

    /// Mark quantity as sold/bought (WFM transaction; removes order if qty depletes).
    pub async fn close_order(
        &self,
        cfg: &Config,
        order_id: &str,
        quantity: i64,
    ) -> Result<Value> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        if order_id.trim().is_empty() {
            bail!("missing order id");
        }
        if quantity <= 0 {
            bail!("quantity must be > 0");
        }
        let resp = self
            .client
            .post(format!("{V2_API}/order/{order_id}/close"))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .header("Crossplay", "true")
            .json(&serde_json::json!({ "quantity": quantity }))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("close order {status}: {text}");
        }
        self.invalidate_my_orders_cache().await;
        let _ = self.my_orders_cached(cfg, true).await;
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn resolve_item_id(&self, slug_or_id: &str) -> Result<String> {
        Ok(self.resolve_item_meta(slug_or_id).await?.id)
    }

    async fn resolve_item_meta(&self, slug_or_id: &str) -> Result<ItemMeta> {
        let resp = self
            .client
            .get(format!("{V2_API}/item/{slug_or_id}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await
            .context("WFM item lookup")?;
        if !resp.status().is_success() {
            bail!("item lookup {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .context("item missing id")?;
        let max_rank = data
            .get("maxRank")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
        Ok(ItemMeta { id, max_rank })
    }

    async fn resolve_item_info(&self, slug_or_id: &str, prefer_ru: bool) -> Result<ItemInfo> {
        // Local catalog hit when slug is already known.
        {
            let db = self.db.lock().await;
            if let Ok(Some(row)) = db.get_item(slug_or_id) {
                return Ok(item_info_from_row(row, prefer_ru, String::new(), None));
            }
        }

        let resp = self
            .client
            .get(format!("{V2_API}/item/{slug_or_id}"))
            .header("Language", if prefer_ru { "ru" } else { "en" })
            .header("Platform", "pc")
            .send()
            .await
            .context("WFM item lookup")?;
        if !resp.status().is_success() {
            bail!("item lookup {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let slug = data
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or(slug_or_id)
            .to_string();

        {
            let db = self.db.lock().await;
            if let Ok(Some(row)) = db.get_item(&slug) {
                return Ok(item_info_from_row(row, prefer_ru, id, thumb_from_v2(&data)));
            }
        }

        let name_en = data
            .pointer("/i18n/en/name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let name_ru = data
            .pointer("/i18n/ru/name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let name = if prefer_ru {
            name_ru.or_else(|| name_en.clone())
        } else {
            name_en.clone()
        };
        if id.is_empty() {
            bail!("item missing id");
        }
        Ok(ItemInfo {
            id,
            slug,
            name,
            name_en,
            thumb: thumb_from_v2(&data),
        })
    }

    pub async fn suggest_listings(&self, prefer_ru: bool) -> Result<Vec<ListingSuggestion>> {
        let (inv, catalog, lotus, bp_product) = {
            let db = self.db.lock().await;
            let inv = db.list_inventory()?;
            let catalog = db.all_items()?;
            let mut lotus = HashMap::new();
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
                    lotus.extend(m);
                }
            }
            let bp_product = db
                .get_setting("weapon_blueprint_map")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            (inv, catalog, lotus, bp_product)
        };
        let by_norm = crate::pricing::PricingService::build_catalog_index(&catalog);
        let lotus_by_leaf = crate::pricing::build_lotus_leaf_index(&lotus);
        let catalog_by_url: HashMap<&str, &crate::db::ItemRow> =
            catalog.iter().map(|i| (i.url_name.as_str(), i)).collect();

        let mut suggestions = Vec::new();
        for item in inv {
            if item.count <= 1
                || matches!(
                    item.item_type.as_str(),
                    "warframe" | "weapon" | "primary" | "secondary" | "melee"
                )
            {
                continue;
            }

            let resolved = crate::pricing::PricingService::resolve_inventory_market_item(
                &item, &by_norm, &lotus,
            );
            let url_name = resolved
                .as_ref()
                .map(|r| r.url_name.clone())
                .or_else(|| {
                    item.url_name
                        .as_deref()
                        .map(crate::pricing::normalize_market_slug)
                });

            let thumb = resolved
                .as_ref()
                .and_then(|r| r.thumb.clone())
                .or_else(|| {
                    url_name
                        .as_deref()
                        .and_then(|u| catalog_by_url.get(u))
                        .and_then(|r| r.thumb.clone())
                });

            let inventory_name = localized_suggestion_name(
                &item,
                resolved.as_ref(),
                prefer_ru,
                &lotus,
                &bp_product,
                &lotus_by_leaf,
            );

            let mut suggested = None;
            if let Some(ref url) = url_name {
                if let Ok(Some(p)) = self.db.lock().await.get_price(url) {
                    suggested = Some(p.platinum);
                }
            }

            suggestions.push(ListingSuggestion {
                inventory_name,
                url_name,
                count: item.count,
                suggested_plat: suggested,
                reason: "duplicate_copy".into(),
                thumb,
                rank: item.rank,
                item_type: Some(item.item_type.clone()),
                unique_name: Some(item.unique_name.clone()),
            });
        }
        suggestions.sort_by(|a, b| {
            b.suggested_plat
                .partial_cmp(&a.suggested_plat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(suggestions.into_iter().take(50).collect())
    }
}

fn localized_suggestion_name(
    item: &crate::db::InventoryItem,
    resolved: Option<&crate::db::ItemRow>,
    prefer_ru: bool,
    lotus: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
    lotus_by_leaf: &HashMap<String, String>,
) -> String {
    let looks_ru = |s: &str| s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));

    if prefer_ru {
        if let Some(row) = resolved {
            if let Some(ru) = row
                .name_ru
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return ru.to_string();
            }
        }
        if let Some(label) =
            crate::pricing::resolve_lotus_label_indexed(&item.unique_name, lotus, bp_product, lotus_by_leaf)
        {
            if looks_ru(&label) {
                if item.item_type == "blueprint" && !label.to_lowercase().contains("чертеж") {
                    return format!("{label} (Чертеж)");
                }
                return label;
            }
        }
        if looks_ru(&item.name) {
            return item.name.clone();
        }
        if let Some(row) = resolved {
            return row.name.clone();
        }
        return item.name.clone();
    }

    // English UI
    if let Some(row) = resolved {
        return row.name.clone();
    }
    item.name.clone()
}

fn order_rank(o: &Value) -> Option<i64> {
    o.get("rank")
        .or_else(|| o.get("mod_rank"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}

#[derive(Debug, Clone)]
struct ItemMeta {
    id: String,
    max_rank: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct ItemInfo {
    #[allow(dead_code)]
    id: String,
    slug: String,
    name: Option<String>,
    name_en: Option<String>,
    thumb: Option<String>,
}

fn item_info_from_row(
    row: crate::db::ItemRow,
    prefer_ru: bool,
    id: String,
    fallback_thumb: Option<String>,
) -> ItemInfo {
    let name_en = Some(row.name.clone()).filter(|s| !s.is_empty());
    let name = if prefer_ru {
        row.name_ru
            .filter(|s| !s.is_empty())
            .or_else(|| name_en.clone())
    } else {
        name_en.clone()
    };
    ItemInfo {
        id,
        slug: row.url_name,
        name,
        name_en,
        thumb: row.thumb.or(fallback_thumb),
    }
}

fn thumb_from_v2(data: &Value) -> Option<String> {
    data.pointer("/i18n/en/thumb")
        .or_else(|| data.pointer("/i18n/ru/thumb"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

type WfmWs = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

fn wfm_ws_msg(route: &str, payload: Value) -> Value {
    let id: String = {
        let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
            .chars()
            .collect();
        uuid::Uuid::new_v4()
            .as_bytes()
            .iter()
            .take(11)
            .map(|b| chars[(*b as usize) % chars.len()])
            .collect()
    };
    serde_json::json!({
        "route": route,
        "payload": payload,
        "id": id,
    })
}

fn ws_text(msg: &Message) -> Option<&str> {
    match msg {
        Message::Text(t) => Some(t.as_str()),
        _ => None,
    }
}

fn route_of(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("route")?.as_str().map(|s| s.to_string()))
}

async fn wfm_ws_connect_authed(jwt: &str) -> Result<WfmWs> {
    let mut request = WFM_WS_URL
        .into_client_request()
        .context("WFM WS URL")?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(WFM_WS_PROTOCOL),
    );
    request.headers_mut().insert(
        "User-Agent",
        HeaderValue::from_static("BNGframe/0.1"),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("WFM WS connect")?;
    let auth = wfm_ws_msg(
        "@wfm|cmd/auth/signIn",
        serde_json::json!({ "token": jwt }),
    );
    ws.send(Message::Text(auth.to_string().into()))
        .await
        .context("WFM WS auth")?;

    // Wait for auth ack before any status commands.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            bail!("WFM WS auth timeout");
        }
        let next = tokio::time::timeout(left, ws.next())
            .await
            .context("WFM WS auth timeout")?
            .context("WFM WS closed during auth")?
            .context("WFM WS read during auth")?;
        match &next {
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p.clone())).await;
            }
            Message::Close(_) => bail!("WFM WS closed during auth"),
            other => {
                if let Some(text) = ws_text(other) {
                    if let Some(route) = route_of(text) {
                        if route.contains("auth/signIn:ok") {
                            return Ok(ws);
                        }
                        if route.contains("auth/signIn:error") {
                            bail!("WFM WS auth failed: {text}");
                        }
                    }
                }
            }
        }
    }
}

async fn wfm_ws_send_status(ws: &mut WfmWs, status: &str) -> Result<()> {
    // No `duration` — that switches WFM into temporary "expires in …" mode.
    let msg = wfm_ws_msg(
        "@wfm|cmd/status/set",
        serde_json::json!({ "status": status }),
    );
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .context("WFM WS set_status")?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            // Status may still apply; don't fail hard if ack is delayed.
            return Ok(());
        }
        let next = match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => bail!("WFM WS read: {e}"),
            Ok(None) => bail!("WFM WS closed after set_status"),
            Err(_) => return Ok(()),
        };
        match &next {
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p.clone())).await;
            }
            Message::Close(_) => bail!("WFM WS closed after set_status"),
            other => {
                if let Some(text) = ws_text(other) {
                    if let Some(route) = route_of(text) {
                        if route.contains("status/set:ok") || route.contains("event/status/set") {
                            return Ok(());
                        }
                        if route.contains("status/set:error") {
                            bail!("WFM set_status failed: {text}");
                        }
                    }
                }
            }
        }
    }
}

async fn wfm_ws_set_status_once(jwt: &str, status: &str) -> Result<()> {
    let mut ws = wfm_ws_connect_authed(jwt).await?;
    wfm_ws_send_status(&mut ws, status).await?;
    let _ = ws.close(None).await;
    Ok(())
}

async fn wfm_ws_keep_alive(mut ws: WfmWs, mut stop: oneshot::Receiver<()>) -> Result<()> {
    loop {
        tokio::select! {
            _ = &mut stop => {
                let _ = wfm_ws_send_status(&mut ws, "invisible").await;
                let _ = ws.close(None).await;
                break;
            }
            next = ws.next() => {
                match next {
                    Some(Ok(Message::Ping(p))) => {
                        let _ = ws.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => bail!("WFM WS read: {e}"),
                }
            }
        }
    }
    Ok(())
}

/// Decode `csrf_token` from an anonymous WFM JWT cookie payload (no signature verify).
fn csrf_token_from_jwt(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload = b64url_decode(payload_b64)?;
    let v: Value = serde_json::from_slice(&payload).ok()?;
    v.get("csrf_token")?
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut s = input.replace('-', "+").replace('_', "/");
    match s.len() % 4 {
        2 => s.push_str("=="),
        3 => s.push('='),
        1 => return None,
        _ => {}
    }
    // Minimal base64 decode (std-free) via a tiny table.
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < bytes.len() || i < bytes.len() {
        if i + 3 >= bytes.len() {
            break;
        }
        let a = val(bytes[i])?;
        let b = val(bytes[i + 1])?;
        let c = val(bytes[i + 2])?;
        let d = val(bytes[i + 3])?;
        out.push((a << 2) | (b >> 4));
        if bytes[i + 2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if bytes[i + 3] != b'=' {
            out.push((c << 6) | d);
        }
        i += 4;
    }
    Some(out)
}
