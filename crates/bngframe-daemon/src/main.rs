mod api;
mod pipeline;

use std::sync::Arc;

use anyhow::Result;
use bngframe_capture::default_capture_path;
use bngframe_core::config::Config;
use bngframe_core::db::Database;
use bngframe_core::eelog::EeLogWatcher;
use bngframe_core::imgcache::ImageCache;
use bngframe_core::inventory::InventoryService;
use bngframe_core::market::MarketService;
use bngframe_core::pricing::PricingService;
use bngframe_core::relics::RelicService;
use bngframe_core::state::{AppState, DaemonStatus};
use bngframe_core::stats::StatsService;
use bngframe_core::analytics::AnalyticsService;
use bngframe_core::mastery::MasteryService;
use bngframe_core::worldstate::WorldStateService;
use bngframe_overlay::{write_layer_shell_note, OverlayManager};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::pipeline::RewardPipeline;

pub struct Services {
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub state: Arc<AppState>,
    pub db: Arc<tokio::sync::Mutex<Database>>,
    pub pricing: Arc<PricingService>,
    pub inventory: Arc<InventoryService>,
    pub market: Arc<MarketService>,
    pub relics: Arc<RelicService>,
    pub stats: Arc<StatsService>,
    pub analytics: Arc<AnalyticsService>,
    pub overlay: Arc<OverlayManager>,
    pub pipeline: Arc<RewardPipeline>,
    pub worldstate: Arc<WorldStateService>,
    pub mastery: Arc<MasteryService>,
    pub imgcache: Arc<ImageCache>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::load()?;
    info!("BNGframe listening on {}", cfg.listen_addr());
    info!("EE.log: {} (exists={})", cfg.eelog_path.display(), cfg.eelog_exists());

    let _ = write_layer_shell_note(&cfg.data_dir.join("hyprland-overlay.md"));

    let db = Arc::new(tokio::sync::Mutex::new(Database::open(&cfg.db_path())?));
    let pricing = Arc::new(PricingService::new(db.clone()));
    let inventory = Arc::new(InventoryService::new(
        db.clone(),
        pricing.clone(),
        cfg.data_dir.clone(),
    ));
    let market = Arc::new(MarketService::new(db.clone()));
    let relics = Arc::new(RelicService::new(db.clone()));
    let stats = Arc::new(StatsService::new(db.clone()));
    let analytics = Arc::new(AnalyticsService::new(db.clone()));
    let worldstate = Arc::new(WorldStateService::new());
    let mastery = Arc::new(MasteryService::new(db.clone(), pricing.clone()));
    let imgcache = Arc::new(ImageCache::new(cfg.cache_dir.clone())?);

    let status = DaemonStatus::new(
        cfg.eelog_path.display().to_string(),
        cfg.eelog_exists(),
        cfg.overlay_enabled,
        cfg.inventory_consent,
    );
    let state = AppState::new(status);

    let overlay = OverlayManager::new(
        cfg.cache_dir.clone(),
        cfg.base_url(),
        cfg.overlay_enabled,
        cfg.ui_lang.clone(),
        cfg.focus_overlay_workspace,
        cfg.auto_open_browser,
    );

    let cfg_shared = Arc::new(tokio::sync::RwLock::new(cfg.clone()));

    let pipeline = Arc::new(RewardPipeline {
        cfg: cfg_shared.clone(),
        state: state.clone(),
        db: db.clone(),
        pricing: pricing.clone(),
        overlay: overlay.clone(),
        capture_path: default_capture_path(&cfg.cache_dir),
        prefetch: Arc::new(tokio::sync::Mutex::new(None)),
        prefetch_ocr_fired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        visual_had_matches: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cancel_prefetch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        run_lock: Arc::new(tokio::sync::Mutex::new(())),
    });

    // Warm item + image caches in background
    {
        let pricing = pricing.clone();
        let state = state.clone();
        let inventory = inventory.clone();
        let imgcache = imgcache.clone();
        tokio::spawn(async move {
            if let Ok(Some(r)) = inventory.hydrate_from_disk_if_empty().await {
                info!(
                    "Restored inventory cache: {} items ({})",
                    r.item_count, r.method
                );
                state.emit(bngframe_core::AppEvent::InventoryUpdated {
                    item_count: r.item_count,
                });
                let mut s = state.status.write().await;
                s.inventory_loaded = r.item_count > 0;
            } else if let Ok(Some(r)) = inventory.refresh_from_disk_cache().await {
                // Re-parse cache so new fields (mod/arcane ranks) land without live sync
                info!(
                    "Refreshed inventory from disk: {} items ({})",
                    r.item_count, r.method
                );
                state.emit(bngframe_core::AppEvent::InventoryUpdated {
                    item_count: r.item_count,
                });
                let mut s = state.status.write().await;
                s.inventory_loaded = r.item_count > 0;
            }
            // Common UI icons (+ shared relic-tier art)
            imgcache
                .warm([
                    "https://wiki.warframe.com/images/PlatinumLarge.png".into(),
                    "https://wiki.warframe.com/images/Platinum.png".into(),
                    "https://wiki.warframe.com/images/DucatsEmoji.png".into(),
                    "https://wiki.warframe.com/images/LithRelicIntact.png".into(),
                    "https://wiki.warframe.com/images/MesoRelicIntact.png".into(),
                    "https://wiki.warframe.com/images/NeoRelicIntact.png".into(),
                    "https://wiki.warframe.com/images/AxiRelicIntact.png".into(),
                    "https://wiki.warframe.com/images/RequiemRelicIntact.png".into(),
                ])
                .await;
            // Profile glyph if known
            if let Ok(p) = inventory.player_profile().await {
                if let Some(url) = p.avatar_url {
                    let _ = imgcache.ensure(&url).await;
                }
            }
            match pricing.ensure_items_cached().await {
                Ok(n) => {
                    let mut s = state.status.write().await;
                    s.items_cached = n;
                    s.phase = "ready".into();
                    state.emit(bngframe_core::AppEvent::Status { status: s.clone() });
                }
                Err(e) => {
                    warn!("item cache: {e}");
                    let _ = state.set_error(format!("item cache: {e}")).await;
                }
            }
            // Bulk ducat values; plat seeds only when missing.
            match pricing.warm_ducats_prices().await {
                Ok(n) => info!("Startup ducats price warm: {n} seeded plat quotes"),
                Err(e) => warn!("ducats price warm: {e:#}"),
            }
            // Order-book quotes for prime parts (avg of 3 cheapest) → OCR cache.
            match pricing.refresh_prime_part_prices().await {
                Ok(r) => info!(
                    "Startup prime-part price refresh: priced={} failed={} matched={}",
                    r.priced, r.failed, r.matched
                ),
                Err(e) => warn!("startup prime price refresh: {e:#}"),
            }
        });
    }

    // Hourly: refresh inventory quotes + any aged cache entries.
    {
        let pricing = pricing.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip immediate tick — startup already refreshed primes
            loop {
                ticker.tick().await;
                match pricing.refresh_prime_part_prices().await {
                    Ok(r) => info!(
                        "Hourly prime-part price refresh: priced={} failed={} matched={}",
                        r.priced, r.failed, r.matched
                    ),
                    Err(e) => warn!("hourly prime price refresh: {e:#}"),
                }
                match pricing.refresh_hourly_prices(120).await {
                    Ok(r) => info!(
                        "Hourly inventory price refresh: priced={} failed={} matched={}",
                        r.priced, r.failed, r.matched
                    ),
                    Err(e) => warn!("hourly inventory price refresh: {e:#}"),
                }
            }
        });
    }

    // EE.log watcher
    {
        let watcher = EeLogWatcher::new(cfg.eelog_path.clone());
        let mut rx = watcher.spawn();
        let pipeline = pipeline.clone();
        let state = state.clone();
        {
            let mut s = state.status.write().await;
            s.watching_eelog = true;
        }
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    bngframe_core::eelog::EeEvent::RewardScreenOpening => {
                        let pipeline = pipeline.clone();
                        tokio::spawn(async move {
                            pipeline.prefetch_capture().await;
                        });
                    }
                    bngframe_core::eelog::EeEvent::RewardScreen { reward_paths } => {
                        if let Err(e) = pipeline.run_with_paths("eelog", &reward_paths).await {
                            warn!("reward pipeline: {e}");
                            state.set_error(format!("reward pipeline: {e}")).await;
                        }
                    }
                    bngframe_core::eelog::EeEvent::RelicSelectScreen => {
                        // Push planner top picks as a lightweight overlay event
                        state.emit(bngframe_core::AppEvent::LogLine {
                            line: "relic_select_detected".into(),
                        });
                    }
                    bngframe_core::eelog::EeEvent::LoadingLikely => {
                        // future: auto inventory refresh
                    }
                    bngframe_core::eelog::EeEvent::Line(line) => {
                        let lower = line.to_lowercase();
                        if lower.contains("trade") || lower.contains("riven") {
                            state.emit(bngframe_core::AppEvent::LogLine { line });
                        }
                    }
                }
            }
        });
    }

    let services = Arc::new(Services {
        cfg: cfg_shared,
        state: state.clone(),
        db,
        pricing,
        inventory,
        market,
        relics,
        stats,
        analytics,
        overlay,
        pipeline,
        worldstate,
        mastery,
        imgcache,
    });

    // Warm mastery set cache in the background so the first UI open is instant.
    {
        let mastery = services.mastery.clone();
        tokio::spawn(async move {
            match mastery.list_sets().await {
                Ok(r) => info!("Mastery cache warmed ({} sets)", r.sets.len()),
                Err(e) => warn!("Mastery warm failed: {e:#}"),
            }
        });
    }

    let app = api::router(services.clone());
    let addr = cfg.listen_addr();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Open companion UI at {}", cfg.base_url());

    if cfg.auto_open_browser {
        let url = cfg.base_url();
        tokio::spawn(async move {
            let _ = std::process::Command::new("xdg-open").arg(url).spawn();
        });
    }

    axum::serve(listener, app).await?;
    Ok(())
}
