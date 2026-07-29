use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use crate::config::Config;
use crate::db::Database;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingSuggestion {
    pub inventory_name: String,
    pub url_name: Option<String>,
    pub count: i64,
    pub suggested_plat: Option<f64>,
    pub reason: String,
}

pub struct MarketService {
    client: reqwest::Client,
    db: Arc<tokio::sync::Mutex<Database>>,
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
        }
    }

    pub async fn sign_in(&self, email: &str, password: &str) -> Result<String> {
        let device_id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "email": email,
            "password": password,
            "auth_type": "header",
            "device_id": device_id,
        });
        let resp = self
            .client
            .post("https://api.warframe.market/v1/auth/signin")
            .header("Language", "en")
            .header("Platform", "pc")
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

    pub async fn my_orders(&self, cfg: &Config) -> Result<Vec<MarketOrder>> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        let resp = self
            .client
            .get("https://api.warframe.market/v1/profile/orders")
            .header("Authorization", format!("JWT {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("orders fetch {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let mut out = Vec::new();
        for key in ["sell_orders", "buy_orders"] {
            if let Some(arr) = body.pointer(&format!("/payload/{key}")).and_then(|v| v.as_array()) {
                for o in arr {
                    out.push(MarketOrder {
                        id: o.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                        item_url_name: o
                            .pointer("/item/url_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        order_type: if key.starts_with("sell") {
                            "sell".into()
                        } else {
                            "buy".into()
                        },
                        platinum: o.get("platinum").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        quantity: o.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1),
                        visible: o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
                        user: None,
                        status: None,
                        rank: order_rank(o),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Public orders for an item (no JWT). Prefer online/ingame sellers for WTS.
    pub async fn item_orders(&self, url_name: &str) -> Result<Vec<MarketOrder>> {
        let url = format!("https://api.warframe.market/v2/orders/item/{url_name}");
        let resp = self
            .client
            .get(&url)
            .header("Language", "ru")
            .header("Platform", "pc")
            .send()
            .await
            .context("WFM item orders")?;
        if !resp.status().is_success() {
            bail!("item orders fetch {}", resp.status());
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
                item_url_name: url_name.into(),
                order_type,
                platinum: o.get("platinum").and_then(|v| v.as_f64()).unwrap_or(0.0),
                quantity: o.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1),
                visible: o.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
                user,
                status,
                rank: order_rank(&o),
            });
        }
        Ok(out)
    }

    pub async fn create_order(
        &self,
        cfg: &Config,
        item_url_name: &str,
        order_type: &str,
        platinum: i64,
        quantity: i64,
    ) -> Result<Value> {
        let jwt = self
            .get_jwt(cfg)
            .await?
            .context("Not signed in to warframe.market")?;
        let body = serde_json::json!({
            "item": item_url_name,
            "order_type": order_type,
            "platinum": platinum,
            "quantity": quantity,
            "visible": true,
            "rank": 0,
        });
        let resp = self
            .client
            .post("https://api.warframe.market/v1/profile/orders")
            .header("Authorization", format!("JWT {jwt}"))
            .header("Language", "en")
            .header("Platform", "pc")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("create order {status}: {text}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn suggest_listings(&self) -> Result<Vec<ListingSuggestion>> {
        let inv = {
            let db = self.db.lock().await;
            db.list_inventory()?
        };
        let mut suggestions = Vec::new();
        for item in inv {
            if item.count > 1
                && item.item_type != "warframe"
                && item.item_type != "weapon"
                && item.item_type != "primary"
                && item.item_type != "secondary"
                && item.item_type != "melee"
            {
                let mut suggested = None;
                if let Some(ref url) = item.url_name {
                    if let Ok(Some(p)) = self.db.lock().await.get_price(url) {
                        suggested = Some(p.platinum);
                    }
                }
                suggestions.push(ListingSuggestion {
                    inventory_name: item.name,
                    url_name: item.url_name,
                    count: item.count,
                    suggested_plat: suggested,
                    reason: "duplicate_copy".into(),
                });
            }
        }
        suggestions.sort_by(|a, b| {
            b.suggested_plat
                .partial_cmp(&a.suggested_plat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(suggestions.into_iter().take(50).collect())
    }
}

fn order_rank(o: &Value) -> Option<i64> {
    o.get("rank")
        .or_else(|| o.get("mod_rank"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}
