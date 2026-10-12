//! The known answers: an independent implementation of the last-sold rules,
//! computed from the same Square-shaped JSON the mock serves.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::Truth;

const MONTH_NAMES: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Rule 1: the smallest M >= 12 such that every span of M consecutive
/// calendar months (1st of a month to the 1st, M months later) is at least
/// `window_days + 30` days. Checked over a full 400-year Gregorian cycle.
pub fn months_for_window(window_days: u32) -> u32 {
    let need = i64::from(window_days) + 30;
    let mut months = 12;
    while shortest_span_days(months) < need {
        months += 1;
    }
    months
}

fn shortest_span_days(months: u32) -> i64 {
    (0..4800)
        .map(|k: i32| {
            let start = NaiveDate::from_ymd_opt(2000 + k / 12, (k % 12) as u32 + 1, 1).expect("valid date");
            let end = start.checked_add_months(Months::new(months)).expect("in range");
            (end - start).num_days()
        })
        .min()
        .unwrap_or(0)
}

/// `Jul 14, 2026`.
pub fn date_text(d: NaiveDate) -> String {
    format!("{} {}, {}", MONTH_NAMES[d.month0() as usize], d.day(), d.year())
}

pub fn parse_time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc))
}

/// Rule 5 for one object (item or variation) on its own.
pub fn present_at(object: &Value, location: &str) -> bool {
    let listed = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(location)))
    };
    if object.get("present_at_all_locations").and_then(Value::as_bool) == Some(false) {
        listed("present_at_location_ids")
    } else {
        !listed("absent_at_location_ids")
    }
}

pub fn variations_of(item: &Value) -> &[Value] {
    item.pointer("/item_data/variations").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

pub fn is_archived(item: &Value) -> bool {
    item.pointer("/item_data/is_archived").and_then(Value::as_bool) == Some(true)
}

/// What the app knows at a check: the world as Square has it, the orders
/// read so far (latest version of each id), and when each catalog object
/// was first seen.
#[derive(Clone, Debug)]
pub struct World {
    pub window_days: u32,
    /// The first check.
    pub t1: DateTime<Utc>,
    /// The merchant's locations, in Square's order.
    pub locations: Vec<Value>,
    pub main_location_id: String,
    pub selected: Option<Vec<String>>,
    /// Live top-level ITEM objects by id.
    pub items: BTreeMap<String, Value>,
    /// Item and variation ids first seen after `t1` (absent = seen at `t1`).
    pub first_seen: BTreeMap<String, DateTime<Utc>>,
    /// Known orders by id (latest `updated_at` wins).
    pub orders: BTreeMap<String, Value>,
}

impl World {
    pub fn months(&self) -> u32 {
        months_for_window(self.window_days)
    }

    pub fn history_start(&self) -> DateTime<Utc> {
        self.t1.checked_sub_months(Months::new(self.months())).expect("history start in range")
    }

    /// Rule 3: a re-sent order id with a newer `updated_at` replaces the older.
    pub fn learn_order(&mut self, order: Value) {
        let id = order["id"].as_str().unwrap_or_default().to_string();
        let newer = match self.orders.get(&id) {
            None => true,
            Some(old) => parse_time(&order["updated_at"]) >= parse_time(&old["updated_at"]),
        };
        if newer {
            self.orders.insert(id, order);
        }
    }

    /// Rule 2 (first check): completed orders closed at or after the start
    /// of the history read.
    pub fn learn_first_sync(&mut self, orders: &[Value]) {
        let start = self.history_start();
        for o in orders {
            if o["state"] == "COMPLETED" && parse_time(&o["closed_at"]).is_some_and(|t| t >= start) {
                self.learn_order(o.clone());
            }
        }
    }

    /// Rule 4: the selected location ids, in the merchant's order.
    pub fn selected_ids(&self) -> Vec<String> {
        let ids = self.locations.iter().filter_map(|l| l["id"].as_str().map(str::to_string));
        match &self.selected {
            Some(sel) => ids.filter(|id| sel.contains(id)).collect(),
            None => self
                .locations
                .iter()
                .filter(|l| l["status"] == "ACTIVE")
                .filter_map(|l| l["id"].as_str().map(str::to_string))
                .collect(),
        }
    }

    fn zone_of(&self, location: &str) -> Tz {
        self.locations
            .iter()
            .find(|l| l["id"] == location)
            .and_then(|l| l["timezone"].as_str())
            .and_then(|z| z.parse().ok())
            .unwrap_or(Tz::UTC)
    }

    /// The labels Square should hold after a check at `at`.
    pub fn truth(&self, at: DateTime<Utc>) -> Truth {
        let selected = self.selected_ids();
        let selected_set: HashSet<&str> = selected.iter().map(String::as_str).collect();
        let zones: HashMap<&str, Tz> = selected.iter().map(|id| (id.as_str(), self.zone_of(id))).collect();
        let home = if selected.contains(&self.main_location_id) {
            self.main_location_id.clone()
        } else {
            selected.first().cloned().unwrap_or_default()
        };
        let ctx = Ctx {
            window: i64::from(self.window_days),
            months: self.months(),
            t1: self.t1,
            at,
            zones,
            home_zone: self.zone_of(&home),
        };

        // Rules 3 and 4: each variation's latest sale at a selected location.
        let mut latest: HashMap<&str, (DateTime<Utc>, &str)> = HashMap::new();
        for order in self.orders.values() {
            if order["state"] != "COMPLETED" {
                continue;
            }
            let Some(location) = order["location_id"].as_str().filter(|l| selected_set.contains(l)) else {
                continue;
            };
            let Some(closed) = parse_time(&order["closed_at"]) else { continue };
            let lines = order.get("line_items").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
            for line in lines {
                let Some(var) =
                    line.get("catalog_object_id").and_then(Value::as_str).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                if line.get("item_type").and_then(Value::as_str) == Some("CUSTOM_AMOUNT") {
                    continue;
                }
                let qty =
                    line.get("quantity").and_then(Value::as_str).and_then(|q| q.trim().parse::<f64>().ok());
                if !qty.is_some_and(|q| q.is_finite() && q > 0.0) {
                    continue;
                }
                match latest.get(var) {
                    Some((t, _)) if *t >= closed => {}
                    _ => {
                        latest.insert(var, (closed, location));
                    }
                }
            }
        }

        let mut truth = Truth::default();
        for (id, item) in &self.items {
            if is_archived(item) {
                continue;
            }
            let vars = variations_of(item);
            let item_carried = selected.iter().any(|l| present_at(item, l));
            if !item_carried {
                truth.items.insert(id.clone(), None);
                for v in vars {
                    truth.variations.insert(v["id"].as_str().unwrap_or_default().to_string(), None);
                }
                continue;
            }
            let item_latest = vars
                .iter()
                .filter_map(|v| v["id"].as_str().and_then(|vid| latest.get(vid)))
                .copied()
                .max_by_key(|(t, _)| *t);
            truth.items.insert(id.clone(), Some(ctx.label(item_latest, self.first_seen.get(id).copied())));
            for v in vars {
                let vid = v["id"].as_str().unwrap_or_default().to_string();
                let carried = selected.iter().any(|l| present_at(item, l) && present_at(v, l));
                let label = (vars.len() >= 2 && carried).then(|| {
                    ctx.label(latest.get(vid.as_str()).copied(), self.first_seen.get(&vid).copied())
                });
                truth.variations.insert(vid, label);
            }
        }
        truth
    }
}

struct Ctx<'a> {
    window: i64,
    months: u32,
    t1: DateTime<Utc>,
    at: DateTime<Utc>,
    zones: HashMap<&'a str, Tz>,
    home_zone: Tz,
}

impl Ctx<'_> {
    /// Rules 7 and 8.
    fn label(&self, latest: Option<(DateTime<Utc>, &str)>, first_seen: Option<DateTime<Utc>>) -> String {
        match latest {
            Some((closed, location)) => {
                let tz = self.zones.get(location).copied().unwrap_or(Tz::UTC);
                let sold = closed.with_timezone(&tz).date_naive();
                let today = self.at.with_timezone(&tz).date_naive();
                let days = (today - sold).num_days().max(0);
                if days > self.window {
                    format!("SLOW \u{b7} last sold {}", date_text(sold))
                } else {
                    format!("Last sold {}", date_text(sold))
                }
            }
            None => match first_seen.filter(|t| *t > self.t1) {
                Some(seen) => {
                    let added = seen.with_timezone(&self.home_zone).date_naive();
                    let today = self.at.with_timezone(&self.home_zone).date_naive();
                    let days = (today - added).num_days().max(0);
                    if days > self.window {
                        format!("SLOW \u{b7} no sales since added {}", date_text(added))
                    } else {
                        format!("No sales since added {}", date_text(added))
                    }
                }
                None => format!("SLOW \u{b7} no sales in {}+ months", self.months),
            },
        }
    }
}
