use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use bngframe_capture::capture_monitor;
use bngframe_core::config::Config;
use bngframe_core::db::Database;
use bngframe_core::ocr::{fuzzy_match_item, ocr_reward_screen};
use bngframe_core::pricing::PricingService;
use bngframe_core::state::{AppState, RewardSlot, RewardSnapshot};
use bngframe_overlay::OverlayManager;
use chrono::Utc;
use tracing::info;
use uuid::Uuid;

pub struct RewardPipeline {
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub state: Arc<AppState>,
    pub db: Arc<tokio::sync::Mutex<Database>>,
    pub pricing: Arc<PricingService>,
    pub overlay: Arc<OverlayManager>,
    pub capture_path: PathBuf,
}

impl RewardPipeline {
    pub async fn run(&self, source: &str) -> Result<RewardSnapshot> {
        info!("Running reward pipeline (source={source})");
        let _ = self.pricing.ensure_items_cached().await;
        let (monitor, ocr_lang, cache_dir) = {
            let cfg = self.cfg.read().await;
            (
                cfg.monitor.clone(),
                cfg.ocr_lang.clone(),
                cfg.cache_dir.clone(),
            )
        };

        let img =
            capture_monitor(monitor.as_deref(), &self.capture_path).context("screen capture")?;

        let ocr = ocr_reward_screen(&img, &ocr_lang, &cache_dir)?;
        let catalog = {
            let db = self.db.lock().await;
            db.all_items()?
        };

        let mut slots = Vec::new();
        for (i, text) in ocr.slot_texts.iter().enumerate() {
            let name = if text.is_empty() {
                format!("Unknown {}", i + 1)
            } else {
                text.clone()
            };
            let matched = fuzzy_match_item(&name, &catalog);
            let mut platinum = None;
            let mut volume = None;
            let mut ducats = None;
            let mut url_name = None;
            if let Some(item) = matched {
                url_name = Some(item.url_name.clone());
                ducats = item.ducats;
                if let Ok(p) = self.pricing.price_for(&item.url_name).await {
                    platinum = Some(p.platinum);
                    volume = Some(p.volume);
                }
            }
            slots.push(RewardSlot {
                name,
                matched_url_name: url_name,
                platinum,
                volume,
                ducats,
                owned: None,
                mastered: None,
                rank: None,
            });
        }

        let mut order: Vec<usize> = (0..slots.len()).collect();
        order.sort_by(|&a, &b| {
            let pa = slots[a].platinum.unwrap_or(-1.0);
            let pb = slots[b].platinum.unwrap_or(-1.0);
            pb.partial_cmp(&pa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    slots[b]
                        .ducats
                        .unwrap_or(0)
                        .cmp(&slots[a].ducats.unwrap_or(0))
                })
        });
        for (rank, &idx) in order.iter().enumerate() {
            slots[idx].rank = Some((rank + 1) as u8);
        }
        let best_index = order.first().copied();

        let reward = RewardSnapshot {
            id: Uuid::new_v4().to_string(),
            detected_at: Utc::now(),
            slots,
            best_index,
            source: source.into(),
        };

        self.state.push_reward(reward.clone()).await;
        self.overlay.show_rewards(&reward).await?;
        self.state.emit(bngframe_core::AppEvent::OverlayShown {
            reward_id: reward.id.clone(),
        });
        Ok(reward)
    }
}
