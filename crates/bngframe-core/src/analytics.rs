use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::Database;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketCapItem {
    pub url_name: String,
    pub name: String,
    pub platinum: f64,
    pub volume: i64,
    pub market_cap: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsReport {
    pub top_market_cap: Vec<MarketCapItem>,
    pub high_volume: Vec<MarketCapItem>,
    pub generated_note: String,
}

pub struct AnalyticsService {
    db: Arc<tokio::sync::Mutex<Database>>,
}

impl AnalyticsService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        Self { db }
    }

    pub async fn report(&self) -> Result<AnalyticsReport> {
        let db = self.db.lock().await;
        let items = db.all_items()?;
        let mut scored = Vec::new();
        for item in items {
            if let Ok(Some(p)) = db.get_price(&item.url_name) {
                scored.push(MarketCapItem {
                    url_name: item.url_name.clone(),
                    name: item.name.clone(),
                    platinum: p.platinum,
                    volume: p.volume,
                    market_cap: p.platinum * p.volume as f64,
                });
            }
        }

        let mut by_cap = scored.clone();
        by_cap.sort_by(|a, b| {
            b.market_cap
                .partial_cmp(&a.market_cap)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut by_vol = scored;
        by_vol.sort_by(|a, b| b.volume.cmp(&a.volume));

        Ok(AnalyticsReport {
            top_market_cap: by_cap.into_iter().take(25).collect(),
            high_volume: by_vol.into_iter().take(25).collect(),
            generated_note: "Эвристика по кэшу ордеров WFM (среднее из 5 самых дешёвых продаж × видимый объём)."
                .into(),
        })
    }

    /// Simple language support matrix for overlays.
    pub fn language_matrix() -> HashMap<&'static str, bool> {
        HashMap::from([
            ("en", true),
            ("ru", true), // UI + OCR via tesseract rus / rus+eng and WFM name_ru
            ("de", false),
            ("fr", false),
            ("es", false),
            ("zh", false),
            ("ja", false),
            ("ko", false),
            ("pt", false),
            ("uk", false),
        ])
    }
}
