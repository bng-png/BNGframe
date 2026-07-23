use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRow {
    pub url_name: String,
    pub name: String,
    pub thumb: Option<String>,
    pub ducats: Option<i64>,
    pub set_url_name: Option<String>,
    pub vaulted: Option<bool>,
    pub mastery: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItem {
    pub unique_name: String,
    pub name: String,
    pub count: i64,
    pub xp: Option<i64>,
    pub mastered: bool,
    pub item_type: String,
    pub url_name: Option<String>,
    pub platinum: Option<f64>,
    pub ducats: Option<i64>,
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceCache {
    pub url_name: String,
    pub platinum: f64,
    pub volume: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatPoint {
    pub key: String,
    pub value: f64,
    pub recorded_at: String,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS items (
                url_name TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                thumb TEXT,
                ducats INTEGER,
                set_url_name TEXT,
                vaulted INTEGER,
                mastery INTEGER
            );
            CREATE TABLE IF NOT EXISTS prices (
                url_name TEXT PRIMARY KEY,
                platinum REAL NOT NULL,
                volume INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS inventory (
                unique_name TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                count INTEGER NOT NULL,
                xp INTEGER,
                mastered INTEGER NOT NULL,
                item_type TEXT NOT NULL,
                url_name TEXT,
                favorite INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS favorites (
                key TEXT PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL,
                value REAL NOT NULL,
                recorded_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS relic_planner (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                filters_json TEXT NOT NULL,
                order_mode TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_items_name ON items(name);
            CREATE INDEX IF NOT EXISTS idx_stats_key ON stats(key);
            "#,
        )?;
        Ok(())
    }

    pub fn upsert_item(&self, item: &ItemRow) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO items (url_name, name, thumb, ducats, set_url_name, vaulted, mastery)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
               ON CONFLICT(url_name) DO UPDATE SET
                 name=excluded.name, thumb=excluded.thumb, ducats=excluded.ducats,
                 set_url_name=excluded.set_url_name, vaulted=excluded.vaulted, mastery=excluded.mastery"#,
            params![
                item.url_name,
                item.name,
                item.thumb,
                item.ducats,
                item.set_url_name,
                item.vaulted.map(|v| if v { 1 } else { 0 }),
                item.mastery,
            ],
        )?;
        Ok(())
    }

    pub fn item_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn all_items(&self) -> Result<Vec<ItemRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT url_name, name, thumb, ducats, set_url_name, vaulted, mastery FROM items",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ItemRow {
                url_name: r.get(0)?,
                name: r.get(1)?,
                thumb: r.get(2)?,
                ducats: r.get(3)?,
                set_url_name: r.get(4)?,
                vaulted: r.get::<_, Option<i64>>(5)?.map(|v| v != 0),
                mastery: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn find_item_by_name(&self, name: &str) -> Result<Option<ItemRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT url_name, name, thumb, ducats, set_url_name, vaulted, mastery FROM items WHERE lower(name) = lower(?1) LIMIT 1",
        )?;
        let mut rows = stmt.query(params![name])?;
        if let Some(r) = rows.next()? {
            return Ok(Some(ItemRow {
                url_name: r.get(0)?,
                name: r.get(1)?,
                thumb: r.get(2)?,
                ducats: r.get(3)?,
                set_url_name: r.get(4)?,
                vaulted: r.get::<_, Option<i64>>(5)?.map(|v| v != 0),
                mastery: r.get(6)?,
            }));
        }
        Ok(None)
    }

    pub fn upsert_price(&self, p: &PriceCache) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO prices (url_name, platinum, volume, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(url_name) DO UPDATE SET
                 platinum=excluded.platinum, volume=excluded.volume, updated_at=excluded.updated_at"#,
            params![p.url_name, p.platinum, p.volume, p.updated_at],
        )?;
        Ok(())
    }

    pub fn list_all_prices(&self) -> Result<Vec<PriceCache>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url_name, platinum, volume, updated_at FROM prices")?;
        let rows = stmt.query_map([], |r| {
            Ok(PriceCache {
                url_name: r.get(0)?,
                platinum: r.get(1)?,
                volume: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_price(&self, url_name: &str) -> Result<Option<PriceCache>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url_name, platinum, volume, updated_at FROM prices WHERE url_name=?1")?;
        let mut rows = stmt.query(params![url_name])?;
        if let Some(r) = rows.next()? {
            return Ok(Some(PriceCache {
                url_name: r.get(0)?,
                platinum: r.get(1)?,
                volume: r.get(2)?,
                updated_at: r.get(3)?,
            }));
        }
        Ok(None)
    }

    pub fn clear_inventory(&self) -> Result<()> {
        self.conn.execute("DELETE FROM inventory", [])?;
        Ok(())
    }

    pub fn favorite_keys(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT unique_name FROM inventory WHERE favorite=1")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Atomically replace inventory rows while keeping favorite flags.
    pub fn replace_inventory(&self, items: &[InventoryItem]) -> Result<()> {
        let favorites = self.favorite_keys()?;
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM inventory", [])?;
            for item in items {
                let mut row = item.clone();
                row.favorite = favorites.contains(&row.unique_name);
                self.upsert_inventory(&row)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn set_inventory_cache_meta(
        &self,
        synced_at: &str,
        method: &str,
        item_count: usize,
        account_id: Option<&str>,
    ) -> Result<()> {
        self.set_setting("inventory_synced_at", synced_at)?;
        self.set_setting("inventory_method", method)?;
        self.set_setting("inventory_item_count", &item_count.to_string())?;
        if let Some(id) = account_id {
            self.set_setting("inventory_account_id", id)?;
        }
        Ok(())
    }

    pub fn inventory_cache_meta(&self) -> Result<crate::inventory::InventoryCacheMeta> {
        let synced_at = self.get_setting("inventory_synced_at")?;
        let method = self
            .get_setting("inventory_method")?
            .unwrap_or_else(|| "none".into());
        let item_count = self
            .get_setting("inventory_item_count")?
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| self.inventory_count().unwrap_or(0));
        let account_id = self.get_setting("inventory_account_id")?;
        let age_secs = synced_at
            .as_ref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|ts| {
                let now = chrono::Utc::now();
                (now.signed_duration_since(ts.with_timezone(&chrono::Utc)))
                    .num_seconds()
                    .max(0) as u64
            });
        Ok(crate::inventory::InventoryCacheMeta {
            synced_at,
            method,
            item_count,
            account_id,
            age_secs,
            cached: item_count > 0,
        })
    }

    pub fn upsert_inventory(&self, item: &InventoryItem) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO inventory (unique_name, name, count, xp, mastered, item_type, url_name, favorite)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(unique_name) DO UPDATE SET
                 name=excluded.name, count=excluded.count, xp=excluded.xp,
                 mastered=excluded.mastered, item_type=excluded.item_type,
                 url_name=excluded.url_name, favorite=excluded.favorite"#,
            params![
                item.unique_name,
                item.name,
                item.count,
                item.xp,
                if item.mastered { 1 } else { 0 },
                item.item_type,
                item.url_name,
                if item.favorite { 1 } else { 0 },
            ],
        )?;
        Ok(())
    }

    pub fn list_inventory(&self) -> Result<Vec<InventoryItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT unique_name, name, count, xp, mastered, item_type, url_name, favorite FROM inventory ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(InventoryItem {
                unique_name: r.get(0)?,
                name: r.get(1)?,
                count: r.get(2)?,
                xp: r.get(3)?,
                mastered: r.get::<_, i64>(4)? != 0,
                item_type: r.get(5)?,
                url_name: r.get(6)?,
                platinum: None,
                ducats: None,
                favorite: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn inventory_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM inventory", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn set_favorite(&self, unique_name: &str, favorite: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE inventory SET favorite=?1 WHERE unique_name=?2",
            params![if favorite { 1 } else { 0 }, unique_name],
        )?;
        Ok(())
    }

    pub fn insert_stat(&self, key: &str, value: f64, recorded_at: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO stats (key, value, recorded_at) VALUES (?1, ?2, ?3)",
            params![key, value, recorded_at],
        )?;
        Ok(())
    }

    pub fn list_stats(&self, key: Option<&str>, limit: i64) -> Result<Vec<StatPoint>> {
        if let Some(k) = key {
            let mut stmt = self.conn.prepare(
                "SELECT key, value, recorded_at FROM stats WHERE key=?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![k, limit], |r| {
                Ok(StatPoint {
                    key: r.get(0)?,
                    value: r.get(1)?,
                    recorded_at: r.get(2)?,
                })
            })?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT key, value, recorded_at FROM stats ORDER BY id DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit], |r| {
                Ok(StatPoint {
                    key: r.get(0)?,
                    value: r.get(1)?,
                    recorded_at: r.get(2)?,
                })
            })?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        }
    }

    pub fn save_relic_planner(&self, filters_json: &str, order_mode: &str) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO relic_planner (id, filters_json, order_mode) VALUES (1, ?1, ?2)
               ON CONFLICT(id) DO UPDATE SET filters_json=excluded.filters_json, order_mode=excluded.order_mode"#,
            params![filters_json, order_mode],
        )?;
        Ok(())
    }

    pub fn load_relic_planner(&self) -> Result<(String, String)> {
        let mut stmt = self
            .conn
            .prepare("SELECT filters_json, order_mode FROM relic_planner WHERE id=1")?;
        let mut rows = stmt.query([])?;
        if let Some(r) = rows.next()? {
            Ok((r.get(0)?, r.get(1)?))
        } else {
            Ok(("{}".into(), "ducats_profit".into()))
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM settings WHERE key=?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(r) = rows.next()? {
            Ok(Some(r.get(0)?))
        } else {
            Ok(None)
        }
    }
}
