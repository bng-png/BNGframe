use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path, Query, State,
};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tracing::warn;

use bngframe_core::analytics::AnalyticsService;
use bngframe_core::inventory::default_inventory_dump_candidates;
use bngframe_core::relics::RelicPlannerConfig;
use bngframe_core::rivens::{analyze_riven_text_lang, compare_rivens_lang};

use crate::Services;

pub fn router(services: Arc<Services>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            origin
                .to_str()
                .map(|o| {
                    o.starts_with("http://127.0.0.1")
                        || o.starts_with("http://localhost")
                        || o == "null"
                })
                .unwrap_or(false)
        }))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(tower_http::cors::Any);

    let api = Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/profile", get(player_profile))
        .route("/img", get(cached_image))
        .route("/config", get(get_config).post(update_config))
        .route("/ws", get(ws_handler))
        .route("/rewards", get(list_rewards))
        .route("/rewards/trigger", post(trigger_reward))
        .route("/rewards/memory-scan", post(reward_memory_scan))
        .route("/inventory", get(list_inventory))
        .route("/inventory/cache", get(inventory_cache))
        .route("/inventory/sync", post(sync_inventory))
        .route("/inventory/import", post(import_inventory))
        .route("/inventory/favorite/{id}", post(set_favorite))
        .route("/relics", get(list_relics))
        .route("/relics/planner", get(get_planner).post(set_planner))
        .route("/relics/recommend", get(relic_recommend))
        .route("/market/signin", post(market_signin))
        .route("/market/jwt", post(market_jwt))
        .route("/market/auth", get(market_auth))
        .route("/market/status", get(market_auth).post(market_set_status))
        .route("/market/orders", get(market_orders))
        .route("/market/orders/item/{url_name}", get(market_item_orders))
        .route("/market/orders/create", post(market_create))
        .route("/market/orders/{order_id}", delete(market_delete_order))
        .route("/market/orders/{order_id}/close", post(market_close_order))
        .route("/market/suggestions", get(market_suggestions))
        .route("/rivens/analyze", post(riven_analyze))
        .route("/rivens/compare", post(riven_compare))
        .route("/stats", get(stats))
        .route("/stats/trade", post(record_trade))
        .route("/analytics", get(analytics))
        .route("/languages", get(languages))
        .route("/overlay", get(overlay_state))
        .route("/items/refresh", post(refresh_items))
        .route("/prices/refresh", post(refresh_prices))
        .route("/prices/item/{url_name}", get(price_item))
        .route("/worldstate", get(worldstate))
        .route("/mastery/sets", get(mastery_sets))
        .route("/mastery/recipe/{set_key}", get(mastery_recipe))
        .with_state(services.clone());

    let pages = Router::new()
        .route("/overlay", get(overlay_page))
        .route("/", get(fallback_index))
        .with_state(services);

    let mut app = Router::new().nest("/api", api).merge(pages).layer(cors);

    for dir in [
        PathBuf::from("web/dist"),
        PathBuf::from("../web/dist"),
        PathBuf::from("../../web/dist"),
    ] {
        let assets = dir.join("assets");
        if assets.is_dir() {
            app = app.nest_service("/assets", ServeDir::new(assets));
            break;
        }
    }

    app
}

async fn fallback_index(State(services): State<Arc<Services>>) -> Response {
    for dir in [
        PathBuf::from("web/dist"),
        PathBuf::from("../web/dist"),
        PathBuf::from("../../web/dist"),
    ] {
        let index = dir.join("index.html");
        if let Ok(html) = std::fs::read_to_string(&index) {
            return Html(html).into_response();
        }
    }
    Html(fallback_html(&services)).into_response()
}

fn fallback_html(services: &Services) -> String {
    let _ = services;
    r#"<!DOCTYPE html>
<html lang="ru"><head>
<meta charset="utf-8"/><title>BNGframe</title>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<style>
  :root { --bg:#0c1218; --fg:#e8efe7; --accent:#3ddc97; --muted:#8aa09a; --panel:#152028; }
  * { box-sizing:border-box; }
  body { margin:0; font-family: "IBM Plex Sans", "Segoe UI", sans-serif; background:
    radial-gradient(1200px 600px at 10% -10%, #1a3a32 0%, transparent 55%),
    radial-gradient(900px 500px at 100% 0%, #243048 0%, transparent 50%),
    var(--bg); color:var(--fg); min-height:100vh; }
  header { padding:28px 32px 8px; }
  h1 { margin:0; font-size:2.4rem; letter-spacing:-.03em; }
  h1 span { color:var(--accent); }
  p.lead { color:var(--muted); max-width:42rem; }
  main { padding:8px 32px 48px; display:grid; gap:16px; grid-template-columns:repeat(auto-fit,minmax(280px,1fr)); }
  .card { background:rgba(21,32,40,.85); border:1px solid #ffffff14; border-radius:14px; padding:18px; }
  .card h2 { margin:0 0 8px; font-size:1.05rem; }
  button, a.btn { background:var(--accent); color:#062218; border:0; border-radius:8px; padding:10px 14px; font-weight:650; cursor:pointer; text-decoration:none; display:inline-block; }
  button.secondary { background:#ffffff18; color:var(--fg); }
  pre { white-space:pre-wrap; font-size:12px; color:var(--muted); }
  #status { font-variant-numeric: tabular-nums; }
</style>
</head><body>
<header>
  <h1>BNG<span>frame</span></h1>
  <p class="lead">Компаньон Warframe для Linux Wayland — оверлеи реликвий, инвентарь, маркет, ривены. Соберите веб-UI (<code>cd web && npm run build</code>) для полного компаньона или используйте API ниже.</p>
</header>
<main>
  <div class="card">
    <h2>Статус</h2>
    <pre id="status">загрузка…</pre>
    <button onclick="trigger()">Скан наград (память)</button>
    <button class="secondary" onclick="syncInv()">Синхр. инвентарь</button>
    <a class="btn secondary" href="/overlay" style="margin-left:8px">Оверлей</a>
  </div>
  <div class="card">
    <h2>Последние награды</h2>
    <pre id="rewards">—</pre>
  </div>
  <div class="card">
    <h2>API</h2>
    <pre>/api/health
/api/status
/api/rewards/trigger
/api/inventory/sync
/api/relics
/api/market/*
/api/rivens/*
/api/analytics
/api/ws</pre>
  </div>
</main>
<script>
async function refresh(){
  const s = await fetch('/api/status').then(r=>r.json());
  document.getElementById('status').textContent = JSON.stringify(s,null,2);
  const rewards = await fetch('/api/rewards').then(r=>r.json());
  document.getElementById('rewards').textContent = JSON.stringify(rewards,null,2);
}
async function trigger(){ await fetch('/api/rewards/trigger',{method:'POST'}); refresh(); }
async function syncInv(){ await fetch('/api/inventory/sync',{method:'POST'}); refresh(); }
refresh(); setInterval(refresh, 5000);
const ws = new WebSocket((location.protocol==='https:'?'wss':'ws')+'://'+location.host+'/api/ws');
ws.onmessage = () => refresh();
</script>
</body></html>"#
    .into()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": "bngframe" }))
}

async fn status(State(services): State<Arc<Services>>) -> impl IntoResponse {
    services.state.refresh_uptime().await;
    let mut s = services.state.status.read().await.clone();
    if let Ok(n) = services.db.lock().await.inventory_count() {
        s.inventory_loaded = n > 0;
    }
    if let Ok(n) = services.db.lock().await.item_count() {
        s.items_cached = n;
    }
    Json(s)
}

async fn player_profile(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        services
            .inventory
            .player_profile()
            .await
            .map_err(ApiError::from)?,
    ))
}

#[derive(Deserialize)]
struct ImgQuery {
    /// Remote image URL to cache & serve.
    u: String,
}

async fn cached_image(
    State(services): State<Arc<Services>>,
    Query(q): Query<ImgQuery>,
) -> Result<Response, ApiError> {
    let url = q.u.trim();
    if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ApiError::msg("u must be an http(s) URL"));
    }
    // Basic SSRF guard — only known Warframe / market CDNs
    if !is_allowed_image_host(url) {
        return Err(ApiError::msg("image host not allowed"));
    }
    let img = services
        .imgcache
        .get(url)
        .await
        .map_err(ApiError::from)?;
    Ok((
        [
            (header::CONTENT_TYPE, img.content_type),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
        ],
        img.bytes.as_ref().clone(),
    )
        .into_response())
}

fn is_allowed_image_host(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let host = rest
        .split('/')
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if host.is_empty() {
        return false;
    }
    matches!(
        host.as_str(),
        "warframe.market"
            | "cdn.warframestat.us"
            | "wiki.warframe.com"
            | "warframe.fandom.com"
            | "static.wikia.nocookie.net"
            | "raw.githubusercontent.com"
            | "content.warframe.com"
            | "content-ps4.warframe.com"
            | "content-xb1.warframe.com"
            | "content-swi.warframe.com"
    ) || host.ends_with(".warframe.com")
        || host.ends_with(".warframestat.us")
        || host.ends_with(".nocookie.net")
}

async fn get_config(State(services): State<Arc<Services>>) -> impl IntoResponse {
    let cfg = services.cfg.read().await.clone();
    // redact jwt
    let mut safe = cfg;
    if safe.wfmarket_jwt.is_some() {
        safe.wfmarket_jwt = Some("***".into());
    }
    Json(safe)
}

#[derive(Deserialize)]
struct ConfigPatch {
    inventory_consent: Option<bool>,
    reward_memory_consent: Option<bool>,
    overlay_enabled: Option<bool>,
    ocr_lang: Option<String>,
    eelog_path: Option<String>,
    monitor: Option<String>,
    wfmarket_jwt: Option<String>,
    auto_open_browser: Option<bool>,
    focus_overlay_workspace: Option<bool>,
    ui_lang: Option<String>,
}

async fn update_config(
    State(services): State<Arc<Services>>,
    Json(patch): Json<ConfigPatch>,
) -> Result<impl IntoResponse, ApiError> {
    let mut cfg = services.cfg.write().await;
    if let Some(v) = patch.inventory_consent {
        cfg.inventory_consent = v;
    }
    if let Some(v) = patch.reward_memory_consent {
        cfg.reward_memory_consent = v;
    }
    if let Some(v) = patch.overlay_enabled {
        cfg.overlay_enabled = v;
    }
    if let Some(v) = patch.ocr_lang {
        cfg.ocr_lang = v;
    }
    if let Some(v) = patch.eelog_path {
        cfg.eelog_path = PathBuf::from(v);
    }
    if let Some(v) = patch.monitor {
        cfg.monitor = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = patch.wfmarket_jwt {
        cfg.wfmarket_jwt = Some(v);
    }
    if let Some(v) = patch.auto_open_browser {
        cfg.auto_open_browser = v;
    }
    if let Some(v) = patch.focus_overlay_workspace {
        cfg.focus_overlay_workspace = v;
    }
    let mut ui_lang_to_set: Option<String> = None;
    if let Some(v) = patch.ui_lang {
        if v == "ru" || v == "en" {
            cfg.ui_lang = v.clone();
            ui_lang_to_set = Some(v);
        }
    }
    let focus_ws = cfg.focus_overlay_workspace;
    let auto_open = cfg.auto_open_browser;
    cfg.save().map_err(ApiError::from)?;
    {
        let mut s = services.state.status.write().await;
        s.inventory_consent = cfg.inventory_consent;
        s.overlay_enabled = cfg.overlay_enabled;
        s.eelog_path = cfg.eelog_path.display().to_string();
        s.eelog_exists = cfg.eelog_exists();
    }
    drop(cfg);
    services.overlay.set_focus_overlay_workspace(focus_ws);
    services.overlay.set_auto_open_browser(auto_open);
    if let Some(v) = ui_lang_to_set {
        services.overlay.set_ui_lang(&v).await;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_rewards(State(services): State<Arc<Services>>) -> impl IntoResponse {
    let q = services.state.last_rewards.read().await;
    Json(q.iter().cloned().collect::<Vec<_>>())
}

async fn trigger_reward(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let reward = services
        .pipeline
        .run("manual")
        .await
        .map_err(ApiError::from)?;
    Ok(Json(reward))
}

async fn reward_memory_scan(
    State(services): State<Arc<Services>>,
) -> Result<impl IntoResponse, ApiError> {
    let consent = services.cfg.read().await.reward_memory_consent;
    if !consent {
        return Err(ApiError::msg(
            "reward_memory_consent=false — enable in Settings / config.toml",
        ));
    }
    let cache_dir = services.cfg.read().await.cache_dir.clone();
    let mem = services.pipeline.mem.clone();
    let scan = tokio::task::spawn_blocking(move || mem.debug_scan(&cache_dir))
        .await
        .map_err(|e| ApiError::msg(format!("join: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(scan))
}

async fn list_inventory(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let resp = services
        .inventory
        .list_enriched()
        .await
        .map_err(ApiError::from)?;

    // Warm shared icons + non-relic WFM thumbs (relics use 5 tier wiki icons in UI).
    {
        let img = services.imgcache.clone();
        let mut urls: Vec<String> = vec![
            "https://wiki.warframe.com/images/LithRelicIntact.png".into(),
            "https://wiki.warframe.com/images/MesoRelicIntact.png".into(),
            "https://wiki.warframe.com/images/NeoRelicIntact.png".into(),
            "https://wiki.warframe.com/images/AxiRelicIntact.png".into(),
            "https://wiki.warframe.com/images/RequiemRelicIntact.png".into(),
        ];
        let mut seen: std::collections::HashSet<String> = urls.iter().cloned().collect();
        for item in &resp.items {
            let is_relic = item.item_type == "relic"
                || item
                    .thumb
                    .as_deref()
                    .map(|t| t.contains("_relic."))
                    .unwrap_or(false);
            if is_relic {
                continue;
            }
            if let Some(ref t) = item.thumb {
                if t.contains("unknown.thumb") {
                    continue;
                }
                let u = bngframe_core::pricing::wfm_thumb_url(t);
                if seen.insert(u.clone()) {
                    urls.push(u);
                }
            }
        }
        // Cap per request so inventory load stays snappy
        urls.truncate(125);
        if !urls.is_empty() {
            tokio::spawn(async move {
                img.warm(urls).await;
            });
        }
    }

    Ok(Json(resp))
}

async fn inventory_cache(
    State(services): State<Arc<Services>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        services
            .inventory
            .cache_meta()
            .await
            .map_err(ApiError::from)?,
    ))
}

async fn sync_inventory(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let consent = services.cfg.read().await.inventory_consent;
    let result = services
        .inventory
        .sync(consent)
        .await
        .map_err(ApiError::from)?;
    services
        .state
        .emit(bngframe_core::AppEvent::InventoryUpdated {
            item_count: result.item_count,
        });
    let mut s = services.state.status.write().await;
    s.inventory_loaded = result.item_count > 0;
    Ok(Json(result))
}

#[derive(Deserialize)]
struct ImportBody {
    path: Option<String>,
}

async fn import_inventory(
    State(services): State<Arc<Services>>,
    Json(body): Json<ImportBody>,
) -> Result<impl IntoResponse, ApiError> {
    let path = if let Some(p) = body.path {
        PathBuf::from(p)
    } else {
        default_inventory_dump_candidates()
            .into_iter()
            .find(|p| p.exists())
            .ok_or_else(|| ApiError::msg("No inventory dump found"))?
    };
    let result = services
        .inventory
        .apply_json_file(&path, "file_import")
        .await
        .map_err(ApiError::from)?;
    services
        .state
        .emit(bngframe_core::AppEvent::InventoryUpdated {
            item_count: result.item_count,
        });
    Ok(Json(result))
}

#[derive(Deserialize)]
struct FavoriteBody {
    favorite: bool,
}

async fn set_favorite(
    State(services): State<Arc<Services>>,
    Path(id): Path<String>,
    Json(body): Json<FavoriteBody>,
) -> Result<impl IntoResponse, ApiError> {
    services
        .db
        .lock()
        .await
        .set_favorite(&id, body.favorite)
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_relics(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let planner = services.relics.load_planner().await.unwrap_or_default();
    let relics = services.relics.plan(&planner).await.map_err(ApiError::from)?;
    Ok(Json(relics))
}

async fn relic_recommend(
    State(services): State<Arc<Services>>,
) -> Result<impl IntoResponse, ApiError> {
    let planner = services.relics.load_planner().await.unwrap_or_default();
    let relics = services.relics.plan(&planner).await.map_err(ApiError::from)?;
    let top: Vec<_> = relics.into_iter().filter(|r| r.owned > 0).take(8).collect();
    let summary = top
        .iter()
        .map(|r| format!("{} ({:.1})", r.name, r.score))
        .collect::<Vec<_>>()
        .join(", ");
    let lang = services.cfg.read().await.ui_lang.clone();
    let (title, empty_msg) = if lang.eq_ignore_ascii_case("en") {
        (
            "Relic recommendations",
            "No owned relics scored yet — sync inventory",
        )
    } else {
        (
            "Рекомендации по реликвиям",
            "Нет оценённых реликвий в инвентаре — синхронизируйте инвентарь",
        )
    };
    let _ = std::process::Command::new("notify-send")
        .args([
            "-a",
            "BNGframe",
            title,
            if summary.is_empty() { empty_msg } else { &summary },
        ])
        .status();
    Ok(Json(top))
}

async fn get_planner(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(services.relics.load_planner().await.map_err(ApiError::from)?))
}

async fn set_planner(
    State(services): State<Arc<Services>>,
    Json(cfg): Json<RelicPlannerConfig>,
) -> Result<impl IntoResponse, ApiError> {
    services.relics.save_planner(&cfg).await.map_err(ApiError::from)?;
    Ok(Json(cfg))
}

#[derive(Deserialize)]
struct SignInBody {
    email: String,
    password: String,
}

async fn market_signin(
    State(services): State<Arc<Services>>,
    Json(body): Json<SignInBody>,
) -> Result<impl IntoResponse, ApiError> {
    let jwt = services
        .market
        .sign_in(&body.email, &body.password)
        .await
        .map_err(ApiError::from)?;
    services.cfg.write().await.wfmarket_jwt = Some(jwt.clone());
    let _ = services.cfg.read().await.save();
    Ok(Json(serde_json::json!({ "ok": true, "jwt_len": jwt.len() })))
}

#[derive(Deserialize)]
struct JwtBody {
    jwt: String,
}

async fn market_jwt(
    State(services): State<Arc<Services>>,
    Json(body): Json<JwtBody>,
) -> Result<impl IntoResponse, ApiError> {
    services.market.save_jwt(&body.jwt).await.map_err(ApiError::from)?;
    services.cfg.write().await.wfmarket_jwt = Some(body.jwt);
    let _ = services.cfg.read().await.save();
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn market_auth(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    let state = services
        .market
        .auth_state(&cfg)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(state))
}

#[derive(Deserialize)]
struct MarketStatusBody {
    status: String,
}

async fn market_set_status(
    State(services): State<Arc<Services>>,
    Json(body): Json<MarketStatusBody>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    let state = services
        .market
        .set_status(&cfg, &body.status)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(state))
}

async fn market_orders(
    State(services): State<Arc<Services>>,
    Query(q): Query<MarketOrdersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    let force = q.refresh.unwrap_or(false);
    let orders = services
        .market
        .my_orders_cached(&cfg, force)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(orders))
}

#[derive(Deserialize)]
struct MarketOrdersQuery {
    #[serde(default)]
    refresh: Option<bool>,
}

async fn market_item_orders(
    State(services): State<Arc<Services>>,
    Path(url_name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let orders = services
        .market
        .item_orders(&url_name)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(orders))
}

#[derive(Deserialize)]
struct CreateOrderBody {
    item_url_name: String,
    order_type: String,
    platinum: i64,
    quantity: i64,
    #[serde(default)]
    rank: Option<i64>,
}

async fn market_create(
    State(services): State<Arc<Services>>,
    Json(body): Json<CreateOrderBody>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    let v = services
        .market
        .create_order(
            &cfg,
            &body.item_url_name,
            &body.order_type,
            body.platinum,
            body.quantity,
            body.rank,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Json(v))
}

async fn market_delete_order(
    State(services): State<Arc<Services>>,
    Path(order_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    services
        .market
        .delete_order(&cfg, &order_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CloseOrderBody {
    #[serde(default)]
    quantity: Option<i64>,
}

async fn market_close_order(
    State(services): State<Arc<Services>>,
    Path(order_id): Path<String>,
    Json(body): Json<CloseOrderBody>,
) -> Result<impl IntoResponse, ApiError> {
    let cfg = services.cfg.read().await.clone();
    let orders = services
        .market
        .my_orders(&cfg)
        .await
        .map_err(ApiError::from)?;
    let meta = orders.iter().find(|o| o.id == order_id).cloned();
    let quantity = body
        .quantity
        .filter(|q| *q > 0)
        .or_else(|| meta.as_ref().map(|o| o.quantity))
        .unwrap_or(1);

    let v = services
        .market
        .close_order(&cfg, &order_id, quantity)
        .await
        .map_err(ApiError::from)?;

    if let Some(o) = meta {
        let delta = if o.order_type == "buy" {
            -(o.platinum * quantity as f64)
        } else {
            o.platinum * quantity as f64
        };
        let _ = services.stats.record_trade(delta).await;
    }

    Ok(Json(v))
}

async fn market_suggestions(
    State(services): State<Arc<Services>>,
) -> Result<impl IntoResponse, ApiError> {
    let prefer_ru = services.cfg.read().await.prefer_russian_names();
    Ok(Json(
        services
            .market
            .suggest_listings(prefer_ru)
            .await
            .map_err(ApiError::from)?,
    ))
}

#[derive(Deserialize)]
struct RivenBody {
    text: String,
}

async fn riven_analyze(
    State(services): State<Arc<Services>>,
    Json(body): Json<RivenBody>,
) -> impl IntoResponse {
    let lang = services.cfg.read().await.ui_lang.clone();
    Json(analyze_riven_text_lang(&body.text, &lang))
}

#[derive(Deserialize)]
struct RivenCompareBody {
    old_text: String,
    new_text: String,
}

async fn riven_compare(
    State(services): State<Arc<Services>>,
    Json(body): Json<RivenCompareBody>,
) -> impl IntoResponse {
    let lang = services.cfg.read().await.ui_lang.clone();
    Json(compare_rivens_lang(&body.old_text, &body.new_text, &lang))
}

async fn stats(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(services.stats.overview().await.map_err(ApiError::from)?))
}

#[derive(Deserialize)]
struct TradeBody {
    platinum_delta: f64,
}

async fn record_trade(
    State(services): State<Arc<Services>>,
    Json(body): Json<TradeBody>,
) -> Result<impl IntoResponse, ApiError> {
    services
        .stats
        .record_trade(body.platinum_delta)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn analytics(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(services.analytics.report().await.map_err(ApiError::from)?))
}

async fn languages() -> impl IntoResponse {
    Json(AnalyticsService::language_matrix())
}

async fn worldstate(
    State(services): State<Arc<Services>>,
    Query(q): Query<WorldstateQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let force = q.refresh.unwrap_or(false);
    Ok(Json(
        services
            .worldstate
            .snapshot_cached(force)
            .await
            .map_err(ApiError::from)?,
    ))
}

#[derive(Deserialize)]
struct WorldstateQuery {
    #[serde(default)]
    refresh: Option<bool>,
}

async fn mastery_sets(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    // Fast path: cached catalog + inventory only. Live WFM prices warm on the client.
    Ok(Json(
        services.mastery.list_sets().await.map_err(ApiError::from)?,
    ))
}

async fn mastery_recipe(
    State(services): State<Arc<Services>>,
    Path(set_key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        services
            .mastery
            .recipe_for_set(&set_key)
            .await
            .map_err(ApiError::from)?,
    ))
}

async fn price_item(
    State(services): State<Arc<Services>>,
    Path(url_name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let cache = services
        .pricing
        .price_for(&url_name)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(cache))
}

async fn overlay_state(State(services): State<Arc<Services>>) -> impl IntoResponse {
    Json(services.overlay.current().await)
}

async fn overlay_page(State(services): State<Arc<Services>>) -> Response {
    // Always serve the live shell — it pulls current rewards from /api/overlay.
    let html = services.overlay.live_page_html().await;
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
        ],
        Html(html),
    )
        .into_response()
}

async fn refresh_items(State(services): State<Arc<Services>>) -> Result<impl IntoResponse, ApiError> {
    let n = services
        .pricing
        .refresh_items()
        .await
        .map_err(ApiError::from)?;
    let mut s = services.state.status.write().await;
    s.items_cached = n;
    Ok(Json(serde_json::json!({ "items": n })))
}

#[derive(Deserialize)]
struct PriceRefreshQuery {
    limit: Option<usize>,
}

async fn refresh_prices(
    State(services): State<Arc<Services>>,
    axum::extract::Query(q): axum::extract::Query<PriceRefreshQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(80);
    let result = services
        .pricing
        .refresh_inventory_prices(limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(result))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(services): State<Arc<Services>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, services))
}

async fn ws_loop(socket: WebSocket, services: Arc<Services>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = services.state.subscribe();

    let send_task = tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Ok(text) = serde_json::to_string(&ev) {
                if sender.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
    }
    send_task.abort();
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn msg(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: m.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        // WikiImg cascades intentionally probe many candidate URLs; don't spam WARN.
        if msg.contains("image fetch") || msg.contains("image previously failed") {
            tracing::debug!("api image miss: {msg}");
        } else {
            warn!("api error: {msg}");
        }
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
