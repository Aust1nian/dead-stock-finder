//! From the cache to verdicts: the label every item should carry, and the
//! report's rows. Recomputing everything takes well under a second.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use dsf_compute::{History, Location, Origin, Presence, Rules, Sale, Verdict, verdict};
use rusqlite::params;
use serde::Serialize;

use crate::cache::{Cache, Result, Settings, parse_ts};

/// A location's IANA time zone; UTC if Square sent none we know.
pub fn tz_of(name: Option<&str>) -> Tz {
    name.and_then(|n| n.parse().ok()).unwrap_or(chrono_tz::UTC)
}

/// The rules for this moment, or `None` before the first sync.
pub fn rules(
    cache: &Cache,
    settings: &Settings,
    now: DateTime<Utc>,
    window_days: Option<u32>,
    only_location: Option<&str>,
) -> Result<Option<Rules>> {
    if !cache.synced()? {
        return Ok(None);
    }
    let selected = cache.selected_locations(settings)?;
    let locations: Vec<Location> = selected
        .iter()
        .filter(|l| only_location.is_none_or(|o| o == l.id))
        .map(|l| Location { id: l.id.clone(), tz: tz_of(l.timezone.as_deref()) })
        .collect();
    if locations.is_empty() {
        return Ok(None);
    }
    let main = cache.meta("main_location_id")?;
    let home_tz = locations
        .iter()
        .find(|l| Some(&l.id) == main.as_ref())
        .or_else(|| locations.first())
        .map(|l| l.tz)
        .unwrap_or(chrono_tz::UTC);
    let start = cache.meta_time("history_start")?.unwrap_or(now);
    let months = cache.meta("history_months")?.and_then(|m| m.parse().ok()).unwrap_or(12);
    let first_sync_at = cache.meta_time("catalog_first_read_at")?.unwrap_or(start);
    Ok(Some(Rules {
        window_days: window_days.unwrap_or(settings.window_days),
        locations,
        home_tz,
        now,
        history: History { start, months, first_sync_at },
    }))
}

pub struct VariationRow {
    pub id: String,
    pub item_id: String,
    pub name: String,
    pub sku: Option<String>,
    pub version: Option<i64>,
    pub price_cents: Option<i64>,
    pub cost_cents: Option<i64>,
    pub currency: Option<String>,
    pub track_inventory: bool,
    pub track_overrides: HashMap<String, bool>,
    pub presence: Presence,
    pub first_seen: DateTime<Utc>,
    pub label_value: Option<String>,
}

pub struct ItemRow {
    pub id: String,
    pub version: Option<i64>,
    pub name: String,
    pub category: Option<String>,
    pub archived: bool,
    pub presence: Presence,
    pub first_seen: DateTime<Utc>,
    pub label_value: Option<String>,
    pub variations: Vec<VariationRow>,
}

fn presence(all: bool, at: &str, absent: &str) -> Presence {
    Presence {
        all,
        present_at: serde_json::from_str(at).unwrap_or_default(),
        absent_at: serde_json::from_str(absent).unwrap_or_default(),
    }
}

/// Items with their variations, in name order.
pub fn load_items(cache: &Cache) -> Result<Vec<ItemRow>> {
    let conn = cache.conn();
    let mut vars: HashMap<String, Vec<VariationRow>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, item_id, name, sku, version, price_cents, cost_cents, currency, track_inventory, track_overrides,
                    present_all, present_at, absent_at, first_seen, label_value
             FROM variations ORDER BY item_id, ord, id",
        )?;
        let rows = stmt.query_map([], |r| {
            let overrides: String = r.get(9)?;
            let at: String = r.get(11)?;
            let absent: String = r.get(12)?;
            let first: String = r.get(13)?;
            Ok(VariationRow {
                id: r.get(0)?,
                item_id: r.get(1)?,
                name: r.get(2)?,
                sku: r.get(3)?,
                version: r.get(4)?,
                price_cents: r.get(5)?,
                cost_cents: r.get(6)?,
                currency: r.get(7)?,
                track_inventory: r.get(8)?,
                track_overrides: serde_json::from_str(&overrides).unwrap_or_default(),
                presence: presence(r.get(10)?, &at, &absent),
                first_seen: parse_ts(&first).unwrap_or(DateTime::<Utc>::MIN_UTC),
                label_value: r.get(14)?,
            })
        })?;
        for v in rows {
            let v = v?;
            vars.entry(v.item_id.clone()).or_default().push(v);
        }
    }
    let mut stmt = conn.prepare(
        "SELECT i.id, i.version, i.name, c.name, i.archived, i.present_all, i.present_at, i.absent_at, i.first_seen, i.label_value
         FROM items i LEFT JOIN categories c ON c.id = i.category_id ORDER BY i.name COLLATE NOCASE, i.id",
    )?;
    let rows = stmt.query_map([], |r| {
        let at: String = r.get(6)?;
        let absent: String = r.get(7)?;
        let first: String = r.get(8)?;
        Ok(ItemRow {
            id: r.get(0)?,
            version: r.get(1)?,
            name: r.get(2)?,
            category: r.get(3)?,
            archived: r.get(4)?,
            presence: presence(r.get(5)?, &at, &absent),
            first_seen: parse_ts(&first).unwrap_or(DateTime::<Utc>::MIN_UTC),
            label_value: r.get(9)?,
            variations: Vec::new(),
        })
    })?;
    let mut items = Vec::new();
    for item in rows {
        let mut item = item?;
        item.variations = vars.remove(&item.id).unwrap_or_default();
        items.push(item);
    }
    Ok(items)
}

/// The latest sale per (variation, location).
pub fn load_sales(cache: &Cache) -> Result<HashMap<String, Vec<Sale>>> {
    let mut stmt = cache.conn().prepare(
        "SELECT variation_id, location_id, MAX(closed_at) FROM sale_lines GROUP BY variation_id, location_id",
    )?;
    let mut out: HashMap<String, Vec<Sale>> = HashMap::new();
    let rows = stmt.query_map(params![], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (variation, location, at) = row?;
        if let Some(at) = parse_ts(&at) {
            out.entry(variation).or_default().push(Sale { location_id: location, at });
        }
    }
    Ok(out)
}

/// A variation within its item's presence.
pub fn effective_presence(item: &Presence, variation: &Presence, locations: &[Location]) -> Presence {
    let carried: Vec<String> = locations
        .iter()
        .filter(|l| item.carried_at(&l.id) && variation.carried_at(&l.id))
        .map(|l| l.id.clone())
        .collect();
    Presence { all: false, present_at: carried, absent_at: Vec::new() }
}

pub struct Verdicts {
    pub item: Option<Verdict>,
    pub variations: Vec<Option<Verdict>>,
}

pub fn verdicts_for(item: &ItemRow, sales: &HashMap<String, Vec<Sale>>, rules: &Rules) -> Verdicts {
    let none: Vec<Sale> = Vec::new();
    let item_sales: Vec<&Sale> =
        item.variations.iter().flat_map(|v| sales.get(&v.id).unwrap_or(&none).iter()).collect();
    let origin = Origin { created_at: None, first_seen: item.first_seen };
    let item_verdict = verdict(&item.presence, &origin, item_sales, rules);
    let variations = item
        .variations
        .iter()
        .map(|v| {
            let p = effective_presence(&item.presence, &v.presence, &rules.locations);
            let origin = Origin { created_at: None, first_seen: v.first_seen };
            verdict(&p, &origin, sales.get(&v.id).unwrap_or(&none), rules)
        })
        .collect();
    Verdicts { item: item_verdict, variations }
}

/// What one item's labels should be, and what Square has now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LabelPlan {
    pub item_id: String,
    pub item_name: String,
    pub desired: Option<String>,
    pub current: Option<String>,
    pub variations: Vec<VariationPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VariationPlan {
    pub id: String,
    pub desired: Option<String>,
    pub current: Option<String>,
}

impl LabelPlan {
    pub fn needs_write(&self) -> bool {
        self.desired != self.current || self.variations.iter().any(|v| v.desired != v.current)
    }
}

/// Every item's label: item level always, and variation level when the
/// item has two or more variations (a single variation's label would only
/// repeat the item's). Archived items are left as they are.
pub fn label_plan(cache: &Cache, settings: &Settings, now: DateTime<Utc>) -> Result<Vec<LabelPlan>> {
    let Some(rules) = rules(cache, settings, now, None, None)? else { return Ok(Vec::new()) };
    let sales = load_sales(cache)?;
    let mut plans = Vec::new();
    for item in load_items(cache)? {
        if item.archived {
            continue;
        }
        let v = verdicts_for(&item, &sales, &rules);
        let multi = item.variations.len() > 1;
        plans.push(LabelPlan {
            item_id: item.id.clone(),
            item_name: item.name.clone(),
            desired: v.item.as_ref().map(Verdict::label),
            current: item.label_value.clone(),
            variations: item
                .variations
                .iter()
                .zip(v.variations.iter())
                .map(|(var, verdict)| VariationPlan {
                    id: var.id.clone(),
                    desired: if multi { verdict.as_ref().map(Verdict::label) } else { None },
                    current: var.label_value.clone(),
                })
                .collect(),
        });
    }
    Ok(plans)
}

/// Every label that should be removed: the "Remove all labels" plan.
pub fn removal_plan(cache: &Cache) -> Result<Vec<LabelPlan>> {
    let mut plans = Vec::new();
    for item in load_items(cache)? {
        let plan = LabelPlan {
            item_id: item.id.clone(),
            item_name: item.name.clone(),
            desired: None,
            current: item.label_value.clone(),
            variations: item
                .variations
                .iter()
                .map(|v| VariationPlan { id: v.id.clone(), desired: None, current: v.label_value.clone() })
                .collect(),
        };
        if plan.needs_write() {
            plans.push(plan);
        }
    }
    Ok(plans)
}
