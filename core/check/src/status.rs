//! What the window's status line and Settings show.

use chrono::{DateTime, Utc};
use dsf_sync::Cache;
use dsf_sync::cache::parse_ts;
use rusqlite::OptionalExtension;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct LastCheck {
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub trigger: String,
    pub outcome: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HaltSummary {
    pub name: String,
    pub at: DateTime<Utc>,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub business_name: Option<String>,
    pub synced: bool,
    pub disconnected: bool,
    pub last_check: Option<LastCheck>,
    pub last_success: Option<DateTime<Utc>>,
    /// Items carrying this app's label in Square.
    pub labels_in_square: i64,
    pub halted: Option<HaltSummary>,
    pub failure_count: u32,
}

pub fn status(cache: &Cache) -> Status {
    let conn = cache.conn();
    let last_check = conn
        .query_row(
            "SELECT started_at, ended_at, trigger, outcome, detail FROM checks WHERE outcome <> 'skipped' ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok(LastCheck {
                    started_at: r.get::<_, Option<String>>(0)?.as_deref().and_then(parse_ts),
                    ended_at: r.get::<_, Option<String>>(1)?.as_deref().and_then(parse_ts),
                    trigger: r.get(2)?,
                    outcome: r.get(3)?,
                    detail: r.get(4)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten();
    let labels_in_square = conn
        .query_row("SELECT COUNT(*) FROM items WHERE archived = 0 AND label_value IS NOT NULL", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    Status {
        business_name: cache.meta("business_name").ok().flatten(),
        synced: cache.synced().unwrap_or(false),
        disconnected: cache.meta("connection").ok().flatten().as_deref() == Some("disconnected"),
        last_check,
        last_success: super::last_times(cache).1,
        labels_in_square,
        halted: dsf_labels::halted(cache).map(|h| HaltSummary { name: h.name, at: h.at, paths: h.paths }),
        failure_count: cache.meta("failure_count").ok().flatten().and_then(|c| c.parse().ok()).unwrap_or(0),
    }
}
