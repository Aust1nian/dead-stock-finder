//! The report: one row per variation with its last sale, days since, units
//! on hand and the value tied up in them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use dsf_compute::{LastSold, Verdict};
use serde::Serialize;

use crate::cache::{Cache, Result, Settings};
use crate::plan::{load_items, load_sales, verdicts_for};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportQuery {
    /// A different window just for this view; the labels keep the setting.
    pub window_days: Option<u32>,
    /// One location instead of all selected ones.
    pub location: Option<String>,
    pub category: Option<String>,
    pub slow_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReportRow {
    pub item_id: String,
    pub variation_id: String,
    pub item: String,
    pub variation: String,
    pub sku: Option<String>,
    pub category: Option<String>,
    /// "Jul 14, 2026", or "No sales in 12+ months" / "No sales since added …".
    pub last_sold: String,
    /// ISO date of the last sale, for sorting; none without a sale.
    pub last_sold_date: Option<String>,
    pub days_since: Option<i64>,
    pub slow: bool,
    pub on_hand: Option<f64>,
    pub currency: Option<String>,
    /// On hand × price, in the smallest currency unit.
    pub value_at_price: Option<i64>,
    /// On hand × unit cost, where Square has a cost.
    pub value_at_cost: Option<i64>,
    pub label_status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub rows: Vec<ReportRow>,
    pub window_days: u32,
    pub categories: Vec<String>,
    pub slow_count: usize,
    pub items_shown: usize,
}

fn last_sold_text(v: &Verdict) -> (String, Option<String>) {
    match &v.last {
        LastSold::Sold { date, .. } => (dsf_compute::show_date(*date), Some(date.to_string())),
        LastSold::NoSalesSinceAdded { date, .. } => {
            (format!("No sales since added {}", dsf_compute::show_date(*date)), None)
        }
        LastSold::NoSalesInHistory { months } => (format!("No sales in {months}+ months"), None),
    }
}

fn inventory(cache: &Cache) -> Result<HashMap<(String, String), f64>> {
    let mut stmt = cache.conn().prepare("SELECT variation_id, location_id, quantity FROM inventory")?;
    let rows =
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (v, l, q) = row?;
        if let Ok(q) = q.trim().parse::<f64>() {
            out.insert((v, l), q);
        }
    }
    Ok(out)
}

fn skips(cache: &Cache) -> Result<HashMap<String, String>> {
    let mut stmt = cache.conn().prepare("SELECT object_id, reason FROM label_skips")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn report(cache: &Cache, settings: &Settings, now: DateTime<Utc>, query: &ReportQuery) -> Result<Report> {
    let Some(rules) = crate::plan::rules(cache, settings, now, query.window_days, query.location.as_deref())?
    else {
        return Ok(Report {
            rows: vec![],
            window_days: settings.window_days,
            categories: vec![],
            slow_count: 0,
            items_shown: 0,
        });
    };
    // Label status compares with the labels as computed for Square: the
    // setting's window over all selected locations.
    let label_rules = crate::plan::rules(cache, settings, now, None, None)?;
    let sales = load_sales(cache)?;
    let stock = inventory(cache)?;
    let skipped = skips(cache)?;
    let mut categories: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    let mut items_shown = 0;
    for item in load_items(cache)? {
        if item.archived {
            continue;
        }
        if let Some(c) = &item.category
            && !categories.contains(c)
        {
            categories.push(c.clone());
        }
        if query.category.as_ref().is_some_and(|c| item.category.as_ref() != Some(c)) {
            continue;
        }
        let verdicts = verdicts_for(&item, &sales, &rules);
        let desired =
            label_rules.as_ref().and_then(|r| verdicts_for(&item, &sales, r).item).map(|v| v.label());
        let status = if let Some(reason) = skipped.get(&item.id) {
            format!("Skipped: {reason}")
        } else if !settings.labels_on {
            "Labels off".to_string()
        } else if desired.is_none() {
            "Not carried at selected locations".to_string()
        } else if desired == item.label_value {
            "In Square".to_string()
        } else {
            "Updates at next check".to_string()
        };
        let mut shown = false;
        for (var, verdict) in item.variations.iter().zip(verdicts.variations.iter()) {
            let Some(verdict) = verdict else { continue };
            if query.slow_only && !verdict.slow {
                continue;
            }
            let tracked_locations: Vec<&str> = rules
                .locations
                .iter()
                .filter(|l| var.track_overrides.get(&l.id).copied().unwrap_or(var.track_inventory))
                .map(|l| l.id.as_str())
                .collect();
            let on_hand = (!tracked_locations.is_empty()).then(|| {
                tracked_locations
                    .iter()
                    .map(|l| stock.get(&(var.id.clone(), l.to_string())).copied().unwrap_or(0.0))
                    .sum::<f64>()
            });
            let value = |unit: Option<i64>| match (on_hand, unit) {
                (Some(q), Some(u)) => Some((q * u as f64).round() as i64),
                _ => None,
            };
            let (text, date) = last_sold_text(verdict);
            rows.push(ReportRow {
                item_id: item.id.clone(),
                variation_id: var.id.clone(),
                item: item.name.clone(),
                variation: var.name.clone(),
                sku: var.sku.clone(),
                category: item.category.clone(),
                last_sold: text,
                last_sold_date: date,
                days_since: verdict.days(),
                slow: verdict.slow,
                on_hand,
                currency: var.currency.clone(),
                value_at_price: value(var.price_cents),
                value_at_cost: value(var.cost_cents),
                label_status: status.clone(),
            });
            shown = true;
        }
        if shown {
            items_shown += 1;
        }
    }
    categories.sort_by_key(|c| c.to_lowercase());
    let slow_count = rows.iter().filter(|r| r.slow).count();
    Ok(Report { rows, window_days: rules.window_days, categories, slow_count, items_shown })
}
