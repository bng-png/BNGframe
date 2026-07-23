use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::{Database, StatPoint};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsOverview {
    pub points: Vec<StatPoint>,
    pub latest_credits: Option<f64>,
    pub latest_platinum: Option<f64>,
    pub trade_count: usize,
}

pub struct StatsService {
    db: Arc<tokio::sync::Mutex<Database>>,
}

impl StatsService {
    pub fn new(db: Arc<tokio::sync::Mutex<Database>>) -> Self {
        Self { db }
    }

    pub async fn overview(&self) -> Result<StatsOverview> {
        let db = self.db.lock().await;
        let points = db.list_stats(None, 200)?;
        let credits = db
            .list_stats(Some("credits"), 1)?
            .into_iter()
            .next()
            .map(|p| p.value);
        let platinum = db
            .list_stats(Some("platinum"), 1)?
            .into_iter()
            .next()
            .map(|p| p.value);
        let trade_count = db.list_stats(Some("trade"), 1000)?.len();
        Ok(StatsOverview {
            points,
            latest_credits: credits,
            latest_platinum: platinum,
            trade_count,
        })
    }

    pub async fn record_trade(&self, platinum_delta: f64) -> Result<()> {
        let db = self.db.lock().await;
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_stat("trade", platinum_delta, &now)?;
        Ok(())
    }
}
