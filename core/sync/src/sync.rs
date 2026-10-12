//! Brings the cache up to date with Square, a little at a time.
//!
//! - Locations and the merchant every check.
//! - The catalog in full the first time, then only objects changed since the
//!   last catalog checkpoint (minus a day, to catch Square's indexing lag).
//! - Orders: the first sync reads completed orders closed in the history
//!   window, oldest first, committing page by page so an interrupted first
//!   sync resumes where it stopped. Later checks read orders updated since the
//!   checkpoint minus a day. Duplicates collapse by order id; an older copy of
//!   an order never replaces a newer one.
//! - In-stock counts: in full weekly, otherwise only counts changed since the
//!   last read.
//!
//! Checkpoints use Square's own timestamps, never this computer's clock.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use dsf_compute::LABEL_KEY;
use dsf_square::api::{Api, CatalogQuery, InventoryCount, ORDER_LOCATIONS_PER_CALL, Order, OrderWindow};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::cache::{Cache, CacheError, norm_ts, parse_ts, ts};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Square(#[from] dsf_square::Error),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("Square returned something unexpected: {0}")]
    Data(String),
}

impl From<rusqlite::Error> for SyncError {
    fn from(e: rusqlite::Error) -> Self {
        SyncError::Cache(CacheError::Sql(e))
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Catalog { objects: u64 },
    Sales { months_done: u32, months_total: u32 },
    Inventory,
}

pub struct SyncOptions<'a> {
    pub now: DateTime<Utc>,
    /// This app's Square application id, to recognise its own definition.
    pub app_id: &'a str,
    pub progress: Option<&'a dyn Fn(Progress)>,
}

#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub first_sync: bool,
    pub catalog_objects: u64,
    pub orders_read: u64,
    pub counts_read: u64,
    pub business_name: String,
    /// Milliseconds per phase, for the log and the speed tests.
    pub timings_ms: Vec<(String, u64)>,
}

/// Orders: one day, as the spec says, to catch orders closed or edited late
/// (offline POS sales can reach Square days later; their updated_at is when
/// Square received them, so a day covers indexing and clock differences).
const ORDER_OVERLAP: Duration = Duration::days(1);
/// Catalog: the next read begins exactly at Square's `latest_time` from the
/// last one (the incremental-sync pattern Square designed it for). Without a
/// `latest_time`, it begins two minutes before the newest `updated_at` seen,
/// to cover search-index lag. Stock counts use the same two minutes.
const CATALOG_OVERLAP: Duration = Duration::minutes(2);

pub fn sync(cache: &mut Cache, api: &Api, opts: &SyncOptions) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let mut t = std::time::Instant::now();
    let mut lap = |report: &mut SyncReport, name: &str| {
        report.timings_ms.push((name.to_string(), t.elapsed().as_millis() as u64));
        t = std::time::Instant::now();
    };
    sync_account(cache, api, &mut report)?;
    lap(&mut report, "account");
    report.catalog_objects = sync_catalog(cache, api, opts)?;
    lap(&mut report, "catalog");
    sync_orders(cache, api, opts, &mut report)?;
    lap(&mut report, "orders");
    report.counts_read = sync_inventory(cache, api, opts)?;
    lap(&mut report, "inventory");
    Ok(report)
}

fn sync_account(cache: &mut Cache, api: &Api, report: &mut SyncReport) -> Result<()> {
    let merchant = api.merchant()?;
    let locations = api.locations()?;
    if let Some(known) = cache.meta("merchant_id")?
        && known != merchant.id
    {
        // A different seller: nothing cached belongs to them.
        cache.forget_account()?;
    }
    let name = merchant
        .business_name
        .clone()
        .or_else(|| locations.first().map(|l| l.name.clone()))
        .unwrap_or_else(|| "your Square account".into());
    let tx = cache.transaction()?;
    tx.execute("DELETE FROM locations", [])?;
    for (i, l) in locations.iter().enumerate() {
        tx.execute(
            "INSERT INTO locations (id, name, timezone, status, currency, ord) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![l.id, l.name, l.timezone, l.status, l.currency, i as i64],
        )?;
    }
    for (k, v) in [
        ("merchant_id", Some(merchant.id.clone())),
        ("business_name", Some(name.clone())),
        ("main_location_id", merchant.main_location_id.clone()),
        ("currency", merchant.currency.clone()),
    ] {
        if let Some(v) = v {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )?;
        }
    }
    tx.commit()?;
    report.business_name = name;
    Ok(())
}

/// Reads catalog changes since the checkpoint again: run after writing
/// labels, so the next check doesn't re-read this check's own writes.
pub fn refresh_catalog(cache: &mut Cache, api: &Api, opts: &SyncOptions) -> Result<u64> {
    sync_catalog(cache, api, opts)
}

fn sync_catalog(cache: &mut Cache, api: &Api, opts: &SyncOptions) -> Result<u64> {
    let checkpoint = cache.meta_time("catalog_checkpoint")?;
    let full = checkpoint.is_none();
    let query = match checkpoint {
        None => CatalogQuery {
            object_types: vec!["ITEM", "CATEGORY", "CUSTOM_ATTRIBUTE_DEFINITION"],
            begin_time: None,
            include_deleted: false,
        },
        Some(t) => {
            let exact = cache.meta("catalog_checkpoint_exact")?.as_deref() == Some("1");
            CatalogQuery {
                object_types: vec!["ITEM", "ITEM_VARIATION", "CATEGORY", "CUSTOM_ATTRIBUTE_DEFINITION"],
                begin_time: Some(ts(if exact { t } else { t - CATALOG_OVERLAP })),
                include_deleted: true,
            }
        }
    };
    let now = ts(opts.now);
    let mut seen_items: HashSet<String> = HashSet::new();
    let mut latest: Option<String> = None;
    let mut count = 0u64;
    let latest_time = api.search_catalog(&query, |objects| -> Result<()> {
        let tx = cache.transaction()?;
        for object in &objects {
            if let Some(t) = object.get("updated_at").and_then(Value::as_str).and_then(norm_ts)
                && latest.as_ref().is_none_or(|l| &t > l)
            {
                latest = Some(t);
            }
            if full
                && object["type"] == "ITEM"
                && let Some(id) = object["id"].as_str()
            {
                seen_items.insert(id.to_string());
            }
            ingest_object(&tx, object, &now, opts.app_id)?;
        }
        tx.commit()?;
        count += objects.len() as u64;
        if let Some(p) = opts.progress {
            p(Progress::Catalog { objects: count });
        }
        Ok(())
    })?;
    if full {
        // A full read lists every live item: anything else cached is gone.
        let tx = cache.transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM items")?;
            stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
        };
        for id in ids.iter().filter(|id| !seen_items.contains(*id)) {
            tx.execute("DELETE FROM variations WHERE item_id = ?1", [id])?;
            tx.execute("DELETE FROM items WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        if cache.meta("catalog_first_read_at")?.is_none() {
            cache.set_meta("catalog_first_read_at", &now)?;
        }
    }
    let (next, exact) = match latest_time.as_deref().and_then(norm_ts) {
        Some(t) => (t, true),
        None => (latest.or_else(|| checkpoint.map(ts)).unwrap_or(now), false),
    };
    cache.set_meta("catalog_checkpoint", &next)?;
    cache.set_meta("catalog_checkpoint_exact", if exact { "1" } else { "0" })?;
    Ok(count)
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

fn presence(v: &Value) -> (bool, String, String) {
    let all = v.get("present_at_all_locations").and_then(Value::as_bool).unwrap_or(true);
    let list = |k: &str| {
        v.get(k).cloned().filter(Value::is_array).unwrap_or_else(|| Value::Array(vec![])).to_string()
    };
    (all, list("present_at_location_ids"), list("absent_at_location_ids"))
}

fn label_value(v: &Value) -> Option<String> {
    v.get("custom_attribute_values")?.get(LABEL_KEY)?.get("string_value")?.as_str().map(str::to_string)
}

/// Records one catalog object from Square in the cache.
pub fn ingest_object(tx: &Transaction, object: &Value, now: &str, app_id: &str) -> rusqlite::Result<()> {
    let id = object["id"].as_str().unwrap_or_default();
    let deleted = object.get("is_deleted").and_then(Value::as_bool).unwrap_or(false);
    match object["type"].as_str().unwrap_or_default() {
        "ITEM" => {
            if deleted {
                tx.execute("DELETE FROM variations WHERE item_id = ?1", [id])?;
                tx.execute("DELETE FROM items WHERE id = ?1", [id])?;
                return Ok(());
            }
            let data = &object["item_data"];
            let category = str_at(data, "/reporting_category/id")
                .or_else(|| str_at(data, "/categories/0/id"))
                .or_else(|| str_at(data, "/category_id"));
            let (all, at, absent) = presence(object);
            tx.execute(
                "INSERT INTO items (id, version, name, category_id, archived, present_all, present_at, absent_at, first_seen, label_value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET version = excluded.version, name = excluded.name,
                   category_id = excluded.category_id, archived = excluded.archived, present_all = excluded.present_all,
                   present_at = excluded.present_at, absent_at = excluded.absent_at, label_value = excluded.label_value",
                params![
                    id,
                    object["version"].as_i64(),
                    data["name"].as_str().unwrap_or(""),
                    category,
                    data["is_archived"].as_bool().unwrap_or(false),
                    all,
                    at,
                    absent,
                    now,
                    label_value(object),
                ],
            )?;
            let vars = data["variations"].as_array().cloned().unwrap_or_default();
            let mut keep = Vec::new();
            for (i, v) in vars.iter().enumerate() {
                ingest_variation(tx, v, id, i as i64, now)?;
                keep.push(v["id"].as_str().unwrap_or_default().to_string());
            }
            // Variations no longer in the item are gone.
            let existing: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM variations WHERE item_id = ?1")?;
                stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            for old in existing.iter().filter(|e| !keep.contains(e)) {
                tx.execute("DELETE FROM variations WHERE id = ?1", [old])?;
            }
        }
        "ITEM_VARIATION" => {
            if deleted {
                tx.execute("DELETE FROM variations WHERE id = ?1", [id])?;
                return Ok(());
            }
            let item_id = str_at(object, "/item_variation_data/item_id").unwrap_or_default().to_string();
            let ord: Option<i64> =
                tx.query_row("SELECT ord FROM variations WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            let item_known: bool =
                tx.query_row("SELECT COUNT(*) FROM items WHERE id = ?1", [&item_id], |r| r.get::<_, i64>(0))?
                    > 0;
            if item_known {
                ingest_variation(tx, object, &item_id, ord.unwrap_or(i64::MAX / 2), now)?;
            }
        }
        "CATEGORY" => {
            if deleted {
                tx.execute("DELETE FROM categories WHERE id = ?1", [id])?;
            } else {
                tx.execute(
                    "INSERT INTO categories (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET name = excluded.name",
                    params![id, str_at(object, "/category_data/name").unwrap_or("")],
                )?;
            }
        }
        "CUSTOM_ATTRIBUTE_DEFINITION" => {
            let data = &object["custom_attribute_definition_data"];
            let ours = data["key"].as_str() == Some(LABEL_KEY)
                && str_at(data, "/source_application/application_id") == Some(app_id);
            if ours {
                if deleted {
                    tx.execute("DELETE FROM meta WHERE key = 'definition_id'", [])?;
                } else {
                    tx.execute(
                        "INSERT INTO meta (key, value) VALUES ('definition_id', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        [id],
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn ingest_variation(tx: &Transaction, v: &Value, item_id: &str, ord: i64, now: &str) -> rusqlite::Result<()> {
    let id = v["id"].as_str().unwrap_or_default();
    let data = &v["item_variation_data"];
    let variable = data["pricing_type"].as_str() == Some("VARIABLE_PRICING");
    let price = if variable { None } else { data.pointer("/price_money/amount").and_then(Value::as_i64) };
    let currency = str_at(data, "/price_money/currency")
        .or_else(|| str_at(data, "/vendor_information/0/unit_cost_money/currency"));
    let cost = data.pointer("/vendor_information/0/unit_cost_money/amount").and_then(Value::as_i64);
    let mut overrides = serde_json::Map::new();
    for o in data["location_overrides"].as_array().into_iter().flatten() {
        if let (Some(loc), Some(track)) = (o["location_id"].as_str(), o["track_inventory"].as_bool()) {
            overrides.insert(loc.to_string(), Value::Bool(track));
        }
    }
    let (all, at, absent) = presence(v);
    tx.execute(
        "INSERT INTO variations (id, item_id, version, name, sku, ord, price_cents, cost_cents, currency, track_inventory,
            track_overrides, present_all, present_at, absent_at, first_seen, label_value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(id) DO UPDATE SET item_id = excluded.item_id, version = excluded.version, name = excluded.name,
            sku = excluded.sku, ord = excluded.ord, price_cents = excluded.price_cents, cost_cents = excluded.cost_cents,
            currency = excluded.currency, track_inventory = excluded.track_inventory, track_overrides = excluded.track_overrides,
            present_all = excluded.present_all, present_at = excluded.present_at, absent_at = excluded.absent_at,
            label_value = excluded.label_value",
        params![
            id,
            item_id,
            v["version"].as_i64(),
            data["name"].as_str().unwrap_or(""),
            data["sku"].as_str(),
            ord,
            price,
            cost,
            currency,
            data["track_inventory"].as_bool().unwrap_or(false),
            Value::Object(overrides).to_string(),
            all,
            at,
            absent,
            now,
            label_value(v),
        ],
    )?;
    Ok(())
}

/// The quantity on a line item is a decimal string; a sale needs more than 0.
fn positive(quantity: &str) -> bool {
    quantity.trim().parse::<f64>().is_ok_and(|q| q > 0.0)
}

/// Records a page of orders. Returns the newest `updated_at` seen.
pub fn ingest_orders(tx: &Transaction, orders: &[Order]) -> rusqlite::Result<Option<String>> {
    let mut newest: Option<String> = None;
    for o in orders {
        let updated = o.updated_at.as_deref().and_then(norm_ts);
        if let Some(u) = &updated
            && newest.as_ref().is_none_or(|n| u > n)
        {
            newest = Some(u.clone());
        }
        let existing: Option<(Option<String>, Option<i64>)> = tx
            .query_row("SELECT updated_at, version FROM orders WHERE id = ?1", [&o.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((old_updated, old_version)) = existing {
            let older = match (o.version, old_version) {
                (Some(new), Some(old)) => new < old,
                _ => matches!((&updated, &old_updated), (Some(n), Some(old)) if n < old),
            };
            if older {
                continue;
            }
        }
        tx.execute("DELETE FROM sale_lines WHERE order_id = ?1", [&o.id])?;
        let closed = o.closed_at.as_deref().and_then(norm_ts);
        if o.state != "COMPLETED" || closed.is_none() {
            tx.execute("DELETE FROM orders WHERE id = ?1", [&o.id])?;
            continue;
        }
        let closed = closed.unwrap();
        tx.execute(
            "INSERT INTO orders (id, location_id, closed_at, updated_at, version) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET location_id = excluded.location_id, closed_at = excluded.closed_at,
               updated_at = excluded.updated_at, version = excluded.version",
            params![o.id, o.location_id, closed, updated, o.version],
        )?;
        for line in &o.line_items {
            let Some(variation) = line.catalog_object_id.as_deref().filter(|c| !c.is_empty()) else {
                continue;
            };
            if line.item_type.as_deref() == Some("CUSTOM_AMOUNT") || !positive(&line.quantity) {
                continue;
            }
            tx.execute(
                "INSERT INTO sale_lines (order_id, variation_id, location_id, closed_at) VALUES (?1, ?2, ?3, ?4)",
                params![o.id, variation, o.location_id, closed],
            )?;
        }
    }
    Ok(newest)
}

fn all_location_ids(cache: &Cache) -> Result<Vec<String>> {
    let mut ids: Vec<String> = cache.locations()?.into_iter().map(|l| l.id).collect();
    ids.sort();
    Ok(ids)
}

fn months_between(start: DateTime<Utc>, t: DateTime<Utc>) -> u32 {
    let days = (t - start).num_days().max(0);
    (days as f64 / 30.4375).floor() as u32
}

fn sync_orders(cache: &mut Cache, api: &Api, opts: &SyncOptions, report: &mut SyncReport) -> Result<()> {
    let locations = all_location_ids(cache)?;
    if locations.is_empty() {
        return Ok(());
    }
    let settings = cache.settings()?;
    let needed_months = dsf_compute::history_months(settings.window_days);

    if !cache.synced()? {
        report.first_sync = true;
        let start = match cache.meta_time("history_start")? {
            Some(s) => s,
            None => {
                let s = dsf_compute::history_start(opts.now, needed_months);
                cache.set_meta("history_start", &ts(s))?;
                cache.set_meta("history_months", &needed_months.to_string())?;
                s
            }
        };
        let months: u32 = cache.meta("history_months")?.and_then(|m| m.parse().ok()).unwrap_or(needed_months);
        read_closed(cache, api, opts, report, &locations, "first", start, None, months)?;
        cache.set_meta("first_sync_done", "1")?;
        cache.set_meta("first_sync_at", &ts(opts.now))?;
        return Ok(());
    }

    // The window grew past the history read: read the older months too.
    let months: u32 = cache.meta("history_months")?.and_then(|m| m.parse().ok()).unwrap_or(12);
    if needed_months > months {
        let old_start = cache
            .meta_time("history_start")?
            .ok_or_else(|| SyncError::Data("history start missing".into()))?;
        let first_at = cache.meta_time("first_sync_at")?.unwrap_or(opts.now);
        let new_start = dsf_compute::history_start(first_at, needed_months);
        read_closed(
            cache,
            api,
            opts,
            report,
            &locations,
            "backfill",
            new_start,
            Some(old_start),
            needed_months,
        )?;
        cache.set_meta("history_start", &ts(new_start))?;
        cache.set_meta("history_months", &needed_months.to_string())?;
    }

    let checkpoint = cache
        .meta_time("order_checkpoint")?
        .or(cache.meta_time("history_start")?)
        .ok_or_else(|| SyncError::Data("order checkpoint missing".into()))?;
    let window = OrderWindow::UpdatedSince { start: ts(checkpoint - ORDER_OVERLAP) };
    let mut newest: Option<String> = None;
    api.search_orders(&locations, &window, |orders| -> Result<()> {
        let tx = cache.transaction()?;
        let page_newest = ingest_orders(&tx, &orders)?;
        tx.commit()?;
        report.orders_read += orders.len() as u64;
        if let Some(p) = page_newest
            && newest.as_ref().is_none_or(|n| &p > n)
        {
            newest = Some(p);
        }
        Ok(())
    })?;
    if let Some(n) = newest
        && parse_ts(&n).is_some_and(|n| n > checkpoint)
    {
        cache.set_meta("order_checkpoint", &n)?;
    }
    Ok(())
}

/// Reads completed orders closed in [start, end] for every chunk of 10
/// locations, oldest first, committing page by page with a watermark so an
/// interrupted read resumes. `tag` keeps first-sync and backfill watermarks
/// apart.
#[allow(clippy::too_many_arguments)]
fn read_closed(
    cache: &mut Cache,
    api: &Api,
    opts: &SyncOptions,
    report: &mut SyncReport,
    locations: &[String],
    tag: &str,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    months_total: u32,
) -> Result<()> {
    let span_end = end.unwrap_or(opts.now);
    let total_span = months_between(start, span_end).max(1);
    for (i, chunk) in locations.chunks(ORDER_LOCATIONS_PER_CALL).enumerate() {
        let mark_key = format!("{tag}_watermark_{i}");
        let done_key = format!("{tag}_done_{i}");
        if cache.meta(&done_key)?.is_some() {
            continue;
        }
        let from = cache.meta_time(&mark_key)?.map_or(start, |m| m.max(start));
        let window = OrderWindow::ClosedBetween { start: ts(from), end: end.map(ts) };
        let mut newest_update = cache.meta("order_checkpoint")?;
        api.search_orders(chunk, &window, |orders| -> Result<()> {
            let tx = cache.transaction()?;
            let page_newest = ingest_orders(&tx, &orders)?;
            if let Some(p) = page_newest
                && newest_update.as_ref().is_none_or(|n| &p > n) {
                    newest_update = Some(p);
                }
            let last_closed = orders.iter().filter_map(|o| o.closed_at.as_deref().and_then(norm_ts)).max();
            let put = |k: &str, v: &str| {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![k, v],
                )
            };
            if let Some(c) = &last_closed {
                put(&mark_key, c)?;
            }
            if tag == "first"
                && let Some(n) = &newest_update {
                    put("order_checkpoint", n)?;
                }
            tx.commit()?;
            report.orders_read += orders.len() as u64;
            if let (Some(p), Some(c)) = (opts.progress, last_closed.as_deref().and_then(parse_ts)) {
                let done = months_between(start, c).min(total_span);
                let scaled = ((u64::from(done) * u64::from(months_total)) / u64::from(total_span)) as u32;
                p(Progress::Sales { months_done: scaled.min(months_total), months_total });
            }
            Ok(())
        })?;
        cache.set_meta(&done_key, "1")?;
    }
    if tag == "first" && cache.meta("order_checkpoint")?.is_none() {
        // No orders at all yet: start incremental reads from the history start.
        cache.set_meta("order_checkpoint", &ts(start))?;
    }
    if let Some(p) = opts.progress {
        p(Progress::Sales { months_done: months_total, months_total });
    }
    Ok(())
}

fn sync_inventory(cache: &mut Cache, api: &Api, opts: &SyncOptions) -> Result<u64> {
    let locations = all_location_ids(cache)?;
    if locations.is_empty() {
        return Ok(0);
    }
    if let Some(p) = opts.progress {
        p(Progress::Inventory);
    }
    let last_full = cache.meta_time("inventory_full_at")?;
    let full = last_full.is_none_or(|t| opts.now - t > Duration::days(7));
    let checkpoint = cache.meta_time("inventory_checkpoint")?;
    let after = if full { None } else { checkpoint.map(|c| ts(c - CATALOG_OVERLAP)) };
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut newest: Option<String> = None;
    let mut read = 0u64;
    api.inventory_counts(&locations, after.as_deref(), |counts: Vec<InventoryCount>| -> Result<()> {
        let tx = cache.transaction()?;
        for c in &counts {
            if c.state != "IN_STOCK" {
                continue;
            }
            let calculated = c.calculated_at.as_deref().and_then(norm_ts);
            if let Some(t) = &calculated
                && newest.as_ref().is_none_or(|n| t > n) {
                    newest = Some(t.clone());
                }
            if full {
                seen.insert((c.catalog_object_id.clone(), c.location_id.clone()));
            }
            tx.execute(
                "INSERT INTO inventory (variation_id, location_id, quantity, calculated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(variation_id, location_id) DO UPDATE SET quantity = excluded.quantity, calculated_at = excluded.calculated_at
                 WHERE excluded.calculated_at IS NULL OR inventory.calculated_at IS NULL OR excluded.calculated_at >= inventory.calculated_at",
                params![c.catalog_object_id, c.location_id, c.quantity, calculated],
            )?;
        }
        tx.commit()?;
        read += counts.len() as u64;
        Ok(())
    })?;
    if full {
        let tx = cache.transaction()?;
        let rows: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT variation_id, location_id FROM inventory")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?
        };
        for key in rows.iter().filter(|k| !seen.contains(*k)) {
            tx.execute(
                "DELETE FROM inventory WHERE variation_id = ?1 AND location_id = ?2",
                params![key.0, key.1],
            )?;
        }
        tx.commit()?;
        cache.set_meta("inventory_full_at", &ts(opts.now))?;
    }
    if let Some(n) = newest
        && checkpoint.is_none_or(|c| parse_ts(&n).is_some_and(|n| n > c))
    {
        cache.set_meta("inventory_checkpoint", &n)?;
    }
    Ok(read)
}
