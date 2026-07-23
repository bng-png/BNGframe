mod api;
mod pipeline;

use std::sync::Arc;

use anyhow::Result;
use bngframe_capture::default_capture_path;
use bngframe_core::config::Config;
use bngframe_core::db::Database;
use bngframe_core::eelog::EeLogWatcher;
use bngframe_core::inventory::InventoryService;
use bngframe_core::market::MarketService;
use bngframe_core::pricing::PricingService;
use bngframe_core::relics::RelicService;
use bngframe_core::state::{AppState, DaemonStatus};
use bngframe_core::stats::StatsService;
use bngframe_core::analytics::AnalyticsService;
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
    );

    let cfg_shared = Arc::new(tokio::sync::RwLock::new(cfg.clone()));

    let pipeline = Arc::new(RewardPipeline {
        cfg: cfg_shared.clone(),
        state: state.clone(),
        db: db.clone(),
        pricing: pricing.clone(),
        overlay: overlay.clone(),
        capture_path: default_capture_path(&cfg.cache_dir),
    });

    // Warm item cache in background
    {
        let pricing = pricing.clone();
        let state = state.clone();
        let inventory = inventory.clone();
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
                    bngframe_core::eelog::EeEvent::RewardScreen => {
                        if let Err(e) = pipeline.run("eelog").await {
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
    });

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
