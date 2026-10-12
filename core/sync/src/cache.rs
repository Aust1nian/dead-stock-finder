//! The local copy: one SQLite file in the per-user app data folder.
//!
//! It holds the catalog snapshot, every sale line, the order checkpoint,
//! inventory counts, settings, the labels last written and the record of
//! each check. Never a token (the property tests scan it).

use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS locations (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, timezone TEXT, status TEXT, currency TEXT, ord INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS categories (id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    version INTEGER,
    name TEXT NOT NULL,
    category_id TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    present_all INTEGER NOT NULL DEFAULT 1,
    present_at TEXT NOT NULL DEFAULT '[]',
    absent_at TEXT NOT NULL DEFAULT '[]',
    first_seen TEXT NOT NULL,
    label_value TEXT);
CREATE TABLE IF NOT EXISTS variations (
    id TEXT PRIMARY KEY,
    item_id TEXT NOT NULL,
    version INTEGER,
    name TEXT NOT NULL,
    sku TEXT,
    ord INTEGER NOT NULL DEFAULT 0,
    price_cents INTEGER,
    cost_cents INTEGER,
    currency TEXT,
    track_inventory INTEGER NOT NULL DEFAULT 0,
    track_overrides TEXT NOT NULL DEFAULT '{}',
    present_all INTEGER NOT NULL DEFAULT 1,
    present_at TEXT NOT NULL DEFAULT '[]',
    absent_at TEXT NOT NULL DEFAULT '[]',
    first_seen TEXT NOT NULL,
    label_value TEXT);
CREATE INDEX IF NOT EXISTS variations_item ON variations(item_id);
CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY, location_id TEXT NOT NULL, closed_at TEXT, updated_at TEXT, version INTEGER);
CREATE TABLE IF NOT EXISTS sale_lines (
    order_id TEXT NOT NULL, variation_id TEXT NOT NULL, location_id TEXT NOT NULL, closed_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS sale_lines_variation ON sale_lines(variation_id, location_id, closed_at);
CREATE INDEX IF NOT EXISTS sale_lines_order ON sale_lines(order_id);
CREATE TABLE IF NOT EXISTS inventory (
    variation_id TEXT NOT NULL, location_id TEXT NOT NULL, quantity TEXT NOT NULL, calculated_at TEXT,
    PRIMARY KEY (variation_id, location_id));
CREATE TABLE IF NOT EXISTS label_skips (object_id TEXT PRIMARY KEY, reason TEXT NOT NULL, at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    trigger TEXT NOT NULL,
    outcome TEXT NOT NULL DEFAULT 'running',
    detail TEXT,
    orders_read INTEGER NOT NULL DEFAULT 0,
    labels_written INTEGER NOT NULL DEFAULT 0,
    labels_skipped INTEGER NOT NULL DEFAULT 0);
"#;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cache data: {0}")]
    Data(String),
}

pub type Result<T> = std::result::Result<T, CacheError>;

/// Timestamps in the cache: UTC, millisecond precision, so text order is
/// time order.
pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// Normalizes a Square timestamp to the cache's format.
pub fn norm_ts(s: &str) -> Option<String> {
    parse_ts(s).map(ts)
}

/// The owner's choices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub window_days: u32,
    /// `None`: every active location.
    pub locations: Option<Vec<String>>,
    pub labels_on: bool,
    /// Local time of the daily check, (hour, minute).
    pub check_time: (u8, u8),
    pub background_on: bool,
    pub setup_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            window_days: 90,
            locations: None,
            labels_on: true,
            check_time: (5, 0),
            background_on: true,
            setup_done: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationRow {
    pub id: String,
    pub name: String,
    pub timezone: Option<String>,
    pub status: Option<String>,
    pub currency: Option<String>,
}

impl LocationRow {
    pub fn active(&self) -> bool {
        self.status.as_deref() != Some("INACTIVE")
    }
}

pub struct Cache {
    pub(crate) conn: Connection,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Cache> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CacheError::Data(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Cache> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Cache> {
        conn.execute_batch(SCHEMA)?;
        let cache = Cache { conn };
        match cache.meta("schema_version")? {
            None => cache.set_meta("schema_version", &SCHEMA_VERSION.to_string())?,
            Some(v) if v == SCHEMA_VERSION.to_string() => {}
            Some(v) => return Err(CacheError::Data(format!("cache schema {v} is newer than this app"))),
        }
        Ok(cache)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn clear_meta(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn meta_time(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self.meta(key)?.as_deref().and_then(parse_ts))
    }

    pub fn settings(&self) -> Result<Settings> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = 'settings'", [], |r| r.get(0))
            .optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let raw = serde_json::to_string(settings).map_err(|e| CacheError::Data(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('settings', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [raw],
        )?;
        Ok(())
    }

    pub fn locations(&self) -> Result<Vec<LocationRow>> {
        let mut stmt =
            self.conn.prepare("SELECT id, name, timezone, status, currency FROM locations ORDER BY ord")?;
        let rows = stmt.query_map([], |r| {
            Ok(LocationRow {
                id: r.get(0)?,
                name: r.get(1)?,
                timezone: r.get(2)?,
                status: r.get(3)?,
                currency: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The selected locations, in Square's order.
    pub fn selected_locations(&self, settings: &Settings) -> Result<Vec<LocationRow>> {
        let all = self.locations()?;
        Ok(match &settings.locations {
            None => all.into_iter().filter(LocationRow::active).collect(),
            Some(ids) => all.into_iter().filter(|l| ids.contains(&l.id)).collect(),
        })
    }

    /// Whether the first sync has finished (the report can be shown).
    pub fn synced(&self) -> Result<bool> {
        Ok(self.meta("first_sync_done")?.as_deref() == Some("1"))
    }

    pub fn item_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM items WHERE archived = 0", [], |r| r.get(0))?)
    }

    /// Forget everything about the Square account (after Disconnect, or when
    /// a different seller connects). Settings stay.
    pub fn forget_account(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM meta WHERE key <> 'schema_version';
             DELETE FROM locations; DELETE FROM categories; DELETE FROM items; DELETE FROM variations;
             DELETE FROM orders; DELETE FROM sale_lines; DELETE FROM inventory; DELETE FROM label_skips;",
        )?;
        Ok(())
    }

    pub fn transaction(&mut self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self.conn.transaction()?)
    }

    /// Raw connection, for reports and tests.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let cache = Cache::open_in_memory().unwrap();
        assert_eq!(cache.settings().unwrap(), Settings::default());
        let s = Settings { window_days: 60, locations: Some(vec!["L1".into()]), ..Settings::default() };
        cache.save_settings(&s).unwrap();
        assert_eq!(cache.settings().unwrap(), s);
    }

    #[test]
    fn reopen_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        {
            let cache = Cache::open(&path).unwrap();
            cache.set_meta("x", "1").unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        assert_eq!(cache.meta("x").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn timestamps_sort_as_text() {
        let a = norm_ts("2026-01-02T03:04:05Z").unwrap();
        let b = norm_ts("2026-01-02T03:04:05.5+00:00").unwrap();
        assert_eq!(a, "2026-01-02T03:04:05.000Z");
        assert_eq!(b, "2026-01-02T03:04:05.500Z");
        assert!(a < b);
    }
}
