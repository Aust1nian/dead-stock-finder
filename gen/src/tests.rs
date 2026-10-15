use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Months, NaiveDate, TimeZone, Timelike, Utc};
use serde_json::{Value, json};

use super::*;
use crate::truth::{World, date_text, parse_time};

const DOT: &str = "\u{b7}";

fn t(s: &str) -> DateTime<Utc> {
    parse_time(&json!(s)).expect("valid time")
}

// ---- (a) determinism ----------------------------------------------------

#[test]
fn same_seed_gives_identical_json() {
    for seed in [0, 1, 6, 13, 42, 139, 2026, u64::MAX] {
        let a = scenario(seed).to_json().to_string();
        let b = scenario(seed).to_json().to_string();
        assert_eq!(a, b, "seed {seed}");
    }
    let size = Size { items: 300, max_variations: 5, orders: 2000, locations: 3 };
    assert_eq!(scenario_sized(9, size).to_json().to_string(), scenario_sized(9, size).to_json().to_string());
    assert_ne!(scenario(1).to_json().to_string(), scenario(2).to_json().to_string());
}

// ---- (b) many seeds -----------------------------------------------------

/// Replays the catalog through the checks: live non-archived items and
/// their variations after each check.
fn expected_keys(s: &Scenario) -> Vec<(BTreeSet<String>, BTreeSet<String>)> {
    let mut items: BTreeMap<String, Value> = s
        .mock
        .catalog
        .iter()
        .filter(|o| o["type"] == "ITEM")
        .map(|o| (o["id"].as_str().unwrap().to_string(), o.clone()))
        .collect();
    let mut out = Vec::new();
    for check in s.checks() {
        for p in &check.catalog_puts {
            items.insert(p["id"].as_str().unwrap().to_string(), p.clone());
        }
        for d in &check.catalog_deletes {
            assert!(items.remove(d).is_some(), "seed {}: deleted {d} was not live", s.seed);
        }
        let mut item_keys = BTreeSet::new();
        let mut var_keys = BTreeSet::new();
        for (id, item) in &items {
            if item.pointer("/item_data/is_archived").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            item_keys.insert(id.clone());
            for v in item.pointer("/item_data/variations").and_then(Value::as_array).unwrap() {
                var_keys.insert(v["id"].as_str().unwrap().to_string());
            }
        }
        out.push((item_keys, var_keys));
    }
    out
}

/// The kind of a label, for coverage.
fn kind(label: &Option<String>) -> String {
    match label {
        None => "none".into(),
        Some(l) if l.starts_with("Last sold ") => "last sold".into(),
        Some(l) if l.starts_with(&format!("SLOW {DOT} last sold ")) => "slow last sold".into(),
        Some(l) if l.starts_with("No sales since added ") => "since added".into(),
        Some(l) if l.starts_with(&format!("SLOW {DOT} no sales since added ")) => "slow since added".into(),
        Some(l) if l.starts_with(&format!("SLOW {DOT} no sales in ")) => {
            l.trim_start_matches(&format!("SLOW {DOT} no sales in ")).to_string()
        }
        Some(l) => panic!("unknown label form {l:?}"),
    }
}

/// The `cases` an edge-case item promises, checked against the truth.
fn check_case(s: &Scenario, id: &str, case: &str) {
    let expect = case.split(" \u{2014} ").next().unwrap();
    let checks: Vec<&Check> = s.checks().collect();
    let at = |n: &str| -> &Check { checks[n.parse::<usize>().unwrap()] };
    let label = |c: &Check| c.truth.items.get(id).cloned().unwrap_or_else(|| panic!("{id} missing")).clone();
    let ctx = format!("seed {} item {id} ({case})", s.seed);
    if let Some(n) = expect.strip_prefix("in@") {
        let l = label(at(n)).expect(&ctx);
        assert!(l.starts_with("Last sold "), "{ctx}: {l}");
    } else if let Some(n) = expect.strip_prefix("out@") {
        let l = label(at(n)).expect(&ctx);
        assert!(l.starts_with(&format!("SLOW {DOT} last sold ")), "{ctx}: {l}");
    } else if let Some(rest) = expect.strip_prefix("date@") {
        let (n, date) = rest.split_once('=').unwrap();
        let l = label(at(n)).expect(&ctx);
        assert!(l.ends_with(&format!("last sold {date}")) || l == format!("Last sold {date}"), "{ctx}: {l}");
    } else if expect == "months" {
        let l = label(checks[0]).expect(&ctx);
        assert_eq!(l, format!("SLOW {DOT} no sales in {}+ months", s.history_months()), "{ctx}");
    } else if expect == "none" {
        for c in &checks {
            assert_eq!(label(c), None, "{ctx}");
        }
    } else if expect == "var1-none" {
        let var = format!("V{}-02", &id[1..]);
        for c in &checks {
            assert_eq!(c.truth.variations.get(&var), Some(&None), "{ctx}");
            assert!(label(c).is_some(), "{ctx}");
        }
    } else if expect == "label" {
        for c in &checks {
            assert!(label(c).is_some(), "{ctx}");
        }
    } else {
        panic!("{ctx}: unknown expectation");
    }
}

fn check_orders(s: &Scenario) {
    let t1 = s.first.at;
    let hs = t1.checked_sub_months(Months::new(s.history_months())).unwrap();
    let mut closed_by: HashMap<String, String> = HashMap::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut check_unique = |o: &Value| {
        if let Some(c) = o["closed_at"].as_str() {
            let id = o["id"].as_str().unwrap().to_string();
            let prior = closed_by.insert(c.to_string(), id.clone());
            assert!(prior.is_none_or(|p| p == id), "seed {}: two orders closed at {c}", s.seed);
        }
    };
    for o in &s.mock.orders {
        check_unique(o);
        assert!(
            seen_ids.insert(o["id"].as_str().unwrap().to_string()),
            "seed {}: duplicate seed order",
            s.seed
        );
        let updated = parse_time(&o["updated_at"]).unwrap();
        assert!(updated < t1, "seed {}: seed order updated after the first check", s.seed);
        if let Some(closed) = parse_time(&o["closed_at"]) {
            assert!(updated >= closed);
            if closed < hs {
                assert_eq!(updated, closed, "seed {}: old order updated later", s.seed);
            }
            if o["state"] == "COMPLETED" && o["line_items"].as_array().is_some_and(|l| !l.is_empty()) {
                assert!(
                    (closed - hs).num_hours().abs() > 48,
                    "seed {}: sale within 2 days of history start",
                    s.seed
                );
            }
        }
    }
    let mut prev = t1;
    for c in &s.later {
        for o in &c.new_orders {
            check_unique(o);
            let updated = parse_time(&o["updated_at"]).unwrap();
            assert!(updated > prev && updated <= c.at, "seed {}: new order updated outside its span", s.seed);
            if let Some(closed) = parse_time(&o["closed_at"]) {
                assert!(closed <= updated);
                assert!((closed - hs).num_hours().abs() > 48);
            }
            let id = o["id"].as_str().unwrap().to_string();
            if !seen_ids.insert(id) {
                assert_eq!(o["state"], "COMPLETED", "seed {}: re-sent orders are completed", s.seed);
            }
        }
        prev = c.at;
    }
}

#[test]
fn two_hundred_seeds_cover_every_live_object_and_case() {
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    let mut features: BTreeSet<&str> = BTreeSet::new();
    let mut cases: BTreeSet<String> = BTreeSet::new();
    for seed in 0..200 {
        let s = scenario(seed);
        let t1 = s.first.at;
        assert!(t1 >= Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
        assert!(t1 < Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap());
        assert!(WINDOWS.contains(&s.window_days));
        assert!(s.later.len() <= 4);
        let zones: Vec<chrono_tz::Tz> =
            s.mock.locations.iter().map(|l| l["timezone"].as_str().unwrap().parse().unwrap()).collect();
        for c in s.checks() {
            for z in &zones {
                let secs = c.at.with_timezone(z).num_seconds_from_midnight();
                assert!((600..=86_400 - 600).contains(&secs), "seed {seed}: check near midnight in {z}");
            }
        }
        let mut prev = t1;
        for c in &s.later {
            let gap = c.at - prev;
            assert!(
                gap >= Duration::hours(1) && gap <= Duration::days(120) + Duration::hours(1),
                "seed {seed}"
            );
            prev = c.at;
        }
        check_orders(&s);

        let keys = expected_keys(&s);
        for (check, (items, vars)) in s.checks().zip(&keys) {
            assert_eq!(&check.truth.items.keys().cloned().collect::<BTreeSet<_>>(), items, "seed {seed}");
            assert_eq!(&check.truth.variations.keys().cloned().collect::<BTreeSet<_>>(), vars, "seed {seed}");
            for (id, _) in &check.label_tampering {
                assert!(items.contains(id) || vars.contains(id), "seed {seed}: tampered {id} not live");
                let touched = check.catalog_puts.iter().any(|p| {
                    p["id"] == id.as_str()
                        || p.pointer("/item_data/variations")
                            .and_then(Value::as_array)
                            .unwrap()
                            .iter()
                            .any(|v| v["id"] == id.as_str())
                });
                assert!(!touched, "seed {seed}: tampered {id} also put");
            }
            for label in check.truth.items.values().chain(check.truth.variations.values()) {
                kinds.insert(kind(label));
            }
            if !check.label_tampering.is_empty() {
                features.insert("tampering");
            }
            if check.label_tampering.iter().any(|(_, v)| v.is_none()) {
                features.insert("tampering removes");
            }
            if !check.catalog_deletes.is_empty() {
                features.insert("deletes");
            }
        }
        for (id, case) in &s.cases {
            check_case(&s, id, case);
            cases.insert(case.split(" \u{2014} ").nth(1).unwrap().split(" at ").next().unwrap().to_string());
        }

        // Features present somewhere in the 200 seeds.
        if s.mock.locations.iter().any(|l| l["status"] == "INACTIVE") {
            features.insert("inactive location");
        }
        if s.selected_locations.is_some() {
            features.insert("selected subset");
        }
        if s.mock.catalog.iter().any(|o| o["type"] == "CUSTOM_ATTRIBUTE_DEFINITION") {
            features.insert("pre-seeded labels");
        }
        if s.mock.catalog.iter().any(|o| {
            o.pointer("/item_data/variations").and_then(Value::as_array).is_some_and(|v| v.len() == 50)
        }) {
            features.insert("50 variations");
        }
        if s.mock.catalog.iter().any(|o| {
            o["custom_attribute_values"]
                .as_object()
                .is_some_and(|m| m.keys().any(|k| k.starts_with("Square:")))
        }) {
            features.insert("seller values");
        }
        if s.mock.orders.iter().any(|o| o.get("returns").is_some()) {
            features.insert("returns");
        }
        let seed_ids: BTreeSet<&str> = s.mock.orders.iter().filter_map(|o| o["id"].as_str()).collect();
        for c in &s.later {
            if c.new_orders
                .iter()
                .any(|o| seed_ids.contains(o["id"].as_str().unwrap()) && o["closed_at"].as_str().is_some())
            {
                features.insert("edited orders");
            }
            if c.catalog_puts.iter().any(|p| p.pointer("/item_data/is_archived") == Some(&json!(true))) {
                features.insert("archived later");
            }
        }
    }
    for k in [
        "none",
        "last sold",
        "slow last sold",
        "since added",
        "slow since added",
        "12+ months",
        "14+ months",
        "15+ months",
    ] {
        assert!(kinds.contains(k), "no seed produced {k:?}: {kinds:?}");
    }
    for f in [
        "tampering",
        "tampering removes",
        "deletes",
        "inactive location",
        "selected subset",
        "pre-seeded labels",
        "50 variations",
        "seller values",
        "returns",
        "edited orders",
        "archived later",
    ] {
        assert!(features.contains(f), "no seed has {f}");
    }
    for c in [
        "sold window days before check 0",
        "late-night sale",
        "daylight-saving change",
        "later sale",
        "never sold",
        "recent sale only",
        "carried only",
        "second variation absent",
    ] {
        assert!(cases.iter().any(|x| x.starts_with(c)), "no seed covers {c:?}: {cases:?}");
    }
}

#[test]
fn odd_sizes_do_not_panic() {
    let sizes = [
        Size { items: 0, max_variations: 1, orders: 0, locations: 0 },
        Size { items: 3, max_variations: 1, orders: 10, locations: 1 },
        Size { items: 39, max_variations: 2, orders: 50, locations: 2 },
        Size { items: 60, max_variations: 50, orders: 300, locations: 12 },
    ];
    for size in sizes {
        for seed in [0, 13, 34, 62, 139] {
            let s = scenario_sized(seed, size);
            let keys = expected_keys(&s);
            for (check, (items, _)) in s.checks().zip(&keys) {
                assert_eq!(&check.truth.items.keys().cloned().collect::<BTreeSet<_>>(), items);
            }
        }
    }
}

// ---- (c) the rules, by hand ---------------------------------------------

fn location(id: &str, zone: &str, active: bool) -> Value {
    json!({ "id": id, "name": id, "timezone": zone, "status": if active { "ACTIVE" } else { "INACTIVE" } })
}

fn world(window: u32, t1: &str, locations: Vec<Value>, main: &str, selected: Option<&[&str]>) -> World {
    World {
        window_days: window,
        t1: t(t1),
        locations,
        main_location_id: main.into(),
        selected: selected.map(|s| s.iter().map(|x| x.to_string()).collect()),
        items: BTreeMap::new(),
        first_seen: BTreeMap::new(),
        orders: BTreeMap::new(),
    }
}

fn item(id: &str, vars: &[&str]) -> Value {
    json!({
        "type": "ITEM",
        "id": id,
        "item_data": {
            "name": id,
            "variations": vars.iter().map(|v| json!({
                "type": "ITEM_VARIATION", "id": v, "present_at_all_locations": true,
                "item_variation_data": { "item_id": id, "name": v },
            })).collect::<Vec<_>>(),
        },
    })
}

fn add(w: &mut World, item: Value) {
    w.items.insert(item["id"].as_str().unwrap().to_string(), item);
}

fn sale(w: &mut World, id: &str, loc: &str, closed: &str, var: &str) {
    order(
        w,
        id,
        loc,
        "COMPLETED",
        closed,
        closed,
        vec![json!({ "catalog_object_id": var, "quantity": "1", "item_type": "ITEM" })],
    );
}

fn order(w: &mut World, id: &str, loc: &str, state: &str, closed: &str, updated: &str, lines: Vec<Value>) {
    w.learn_order(json!({
        "id": id, "location_id": loc, "state": state,
        "closed_at": closed, "updated_at": updated, "line_items": lines,
    }));
}

fn label(truth: &Truth, id: &str) -> Option<String> {
    truth
        .items
        .get(id)
        .or_else(|| truth.variations.get(id))
        .cloned()
        .unwrap_or_else(|| panic!("{id} not in truth"))
}

fn ny() -> Vec<Value> {
    vec![location("L1", "America/New_York", true)]
}

#[test]
fn months_match_a_brute_force_over_start_months() {
    let brute = |window: u32| -> u32 {
        let need = i64::from(window) + 30;
        (12..)
            .find(|m| {
                (1900..2300).all(|y| {
                    (1..=12).all(|mo| {
                        let start = NaiveDate::from_ymd_opt(y, mo, 1).unwrap();
                        (start.checked_add_months(Months::new(*m)).unwrap() - start).num_days() >= need
                    })
                })
            })
            .unwrap()
    };
    for window in [0, 30, 60, 90, 120, 180, 334, 335, 336, 364, 365, 366, 393, 394, 400, 424, 425, 730] {
        assert_eq!(months_for_window(window), brute(window), "window {window}");
    }
    assert_eq!(months_for_window(335), 12);
    assert_eq!(months_for_window(336), 13);
    assert_eq!(months_for_window(365), 14);
    assert_eq!(months_for_window(400), 15);
}

#[test]
fn label_text_format() {
    assert_eq!(date_text(NaiveDate::from_ymd_opt(2026, 7, 4).unwrap()), "Jul 4, 2026");
    assert_eq!(date_text(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()), "Dec 31, 2025");
    assert_eq!(LABEL_KEY, "idle_stock_last_sold");
}

#[test]
fn boundary_day_is_not_slow_the_next_is() {
    // Check at noon EDT on Jul 14; sales at 13:00 EDT (later in the day).
    let mut w = world(90, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    add(&mut w, item("I2", &["V2"]));
    sale(&mut w, "O1", "L1", "2026-04-15T17:00:00.000Z", "V1"); // Apr 15: 90 days
    sale(&mut w, "O2", "L1", "2026-04-14T17:00:00.000Z", "V2"); // Apr 14: 91 days
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Apr 15, 2026");
    assert_eq!(label(&truth, "I2").unwrap(), format!("SLOW {DOT} last sold Apr 14, 2026"));
}

#[test]
fn dates_are_local_not_utc() {
    let locations =
        vec![location("LA", "America/Los_Angeles", true), location("SY", "Australia/Sydney", true)];
    let mut w = world(90, "2026-07-14T19:00:00.000Z", locations, "LA", None);
    add(&mut w, item("I1", &["V1"]));
    add(&mut w, item("I2", &["V2"]));
    // 23:30 PDT on Apr 15 is Apr 16 in UTC; 90 local days, not slow.
    sale(&mut w, "O1", "LA", "2026-04-16T06:30:00.000Z", "V1");
    // 01:00 AEST on Apr 15 is Apr 14 in UTC.
    sale(&mut w, "O2", "SY", "2026-04-14T15:00:00.000Z", "V2");
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Apr 15, 2026");
    // Today in Sydney is Jul 15: 91 days.
    assert_eq!(label(&truth, "I2").unwrap(), format!("SLOW {DOT} last sold Apr 15, 2026"));
}

#[test]
fn daylight_saving_does_not_shift_days() {
    // DST began Mar 8, 2026 in New York. Check at 00:30 EDT on Mar 10.
    let mut w = world(30, "2026-03-10T04:30:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    add(&mut w, item("I2", &["V2"]));
    // 23:45 EST Feb 8: 30 calendar days.
    sale(&mut w, "O1", "L1", "2026-02-09T04:45:00.000Z", "V1");
    // 23:45 EST Feb 7: 31 calendar days, though under 31 x 24 hours ago.
    sale(&mut w, "O2", "L1", "2026-02-08T04:45:00.000Z", "V2");
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Feb 8, 2026");
    assert_eq!(label(&truth, "I2").unwrap(), format!("SLOW {DOT} last sold Feb 7, 2026"));
}

#[test]
fn latest_instant_wins_across_zones() {
    let locations = vec![location("HNL", "Pacific/Honolulu", true), location("SY", "Australia/Sydney", true)];
    let mut w = world(90, "2026-07-20T00:00:00.000Z", locations, "SY", None);
    add(&mut w, item("I1", &["V1"]));
    sale(&mut w, "O1", "SY", "2026-07-14T21:00:00.000Z", "V1"); // Jul 15 07:00 AEST
    sale(&mut w, "O2", "HNL", "2026-07-14T21:30:00.000Z", "V1"); // Jul 14 11:30 HST, later instant
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Jul 14, 2026");
}

#[test]
fn no_sales_reads_m_months() {
    for (window, months) in [(90, 12), (365, 14), (400, 15)] {
        let mut w = world(window, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
        add(&mut w, item("I1", &["V1", "V2"]));
        // Sold only before the history start: not known to the first sync.
        let old = w.history_start() - Duration::days(10);
        let o = json!({
            "id": "O1", "location_id": "L1", "state": "COMPLETED",
            "closed_at": old.to_rfc3339(), "updated_at": old.to_rfc3339(),
            "line_items": [{ "catalog_object_id": "V1", "quantity": "1" }],
        });
        w.learn_first_sync(&[o]);
        assert!(w.orders.is_empty());
        let truth = w.truth(w.t1);
        let expect = format!("SLOW {DOT} no sales in {months}+ months");
        assert_eq!(label(&truth, "I1").unwrap(), expect);
        assert_eq!(label(&truth, "V1").unwrap(), expect);
    }
}

#[test]
fn since_added_uses_home_zone_and_window() {
    // Main location (New York) unselected: home is the first selected
    // location in the merchant's order (Los Angeles), not the first listed.
    let locations = vec![
        location("L1", "America/New_York", true),
        location("L2", "America/Los_Angeles", true),
        location("L3", "Europe/London", true),
    ];
    let mut w = world(90, "2026-07-01T16:00:00.000Z", locations, "L1", Some(&["L3", "L2"]));
    add(&mut w, item("I1", &["V1"]));
    let seen = t("2026-08-03T05:00:00.000Z"); // Aug 2 22:00 PDT; Aug 3 in NY and London
    w.first_seen.insert("I1".into(), seen);
    w.first_seen.insert("V1".into(), seen);
    assert_eq!(label(&w.truth(seen), "I1").unwrap(), "No sales since added Aug 2, 2026");
    // Oct 31 in LA is 90 days after Aug 2; Nov 1 is 91.
    assert_eq!(
        label(&w.truth(t("2026-10-31T19:00:00.000Z")), "I1").unwrap(),
        "No sales since added Aug 2, 2026"
    );
    assert_eq!(
        label(&w.truth(t("2026-11-01T19:00:00.000Z")), "I1").unwrap(),
        format!("SLOW {DOT} no sales since added Aug 2, 2026")
    );
    // A variation added later to an item from the first check: the item
    // keeps "no sales in M+ months", the new variation is "since added".
    add(&mut w, item("I2", &["V2a", "V2b"]));
    w.first_seen.insert("V2b".into(), seen);
    let truth = w.truth(seen);
    assert_eq!(label(&truth, "I2").unwrap(), format!("SLOW {DOT} no sales in 12+ months"));
    assert_eq!(label(&truth, "V2a").unwrap(), format!("SLOW {DOT} no sales in 12+ months"));
    assert_eq!(label(&truth, "V2b").unwrap(), "No sales since added Aug 2, 2026");
    // Seen at the first check counts as existing then.
    w.first_seen.insert("I1".into(), w.t1);
    assert_eq!(label(&w.truth(seen), "I1").unwrap(), format!("SLOW {DOT} no sales in 12+ months"));
}

#[test]
fn single_variation_items_get_no_variation_label() {
    let mut w = world(90, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    add(&mut w, item("I2", &["V2a", "V2b"]));
    sale(&mut w, "O1", "L1", "2026-07-01T16:00:00.000Z", "V1");
    sale(&mut w, "O2", "L1", "2026-07-02T16:00:00.000Z", "V2a");
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Jul 1, 2026");
    assert_eq!(label(&truth, "V1"), None);
    assert_eq!(label(&truth, "I2").unwrap(), "Last sold Jul 2, 2026");
    assert_eq!(label(&truth, "V2a").unwrap(), "Last sold Jul 2, 2026");
    assert_eq!(label(&truth, "V2b").unwrap(), format!("SLOW {DOT} no sales in 12+ months"));
}

#[test]
fn not_carried_at_a_selected_location_gets_none() {
    let locations = vec![location("L1", "America/New_York", true), location("L2", "America/Chicago", true)];
    let mut w = world(90, "2026-07-14T16:00:00.000Z", locations, "L1", Some(&["L1"]));
    let mut only_l2 = item("I1", &["V1"]);
    only_l2["present_at_all_locations"] = json!(false);
    only_l2["present_at_location_ids"] = json!(["L2"]);
    add(&mut w, only_l2);
    let mut absent_l1 = item("I2", &["V2"]);
    absent_l1["absent_at_location_ids"] = json!(["L1"]);
    add(&mut w, absent_l1);
    // A variation absent at the only selected location.
    let mut partial = item("I3", &["V3a", "V3b"]);
    partial["item_data"]["variations"][1]["absent_at_location_ids"] = json!(["L1"]);
    add(&mut w, partial);
    // present_at_all_locations left out means true.
    let mut implicit = item("I4", &["V4"]);
    implicit["item_data"]["variations"][0].as_object_mut().unwrap().shift_remove("present_at_all_locations");
    add(&mut w, implicit);
    sale(&mut w, "O1", "L2", "2026-07-01T16:00:00.000Z", "V1");
    sale(&mut w, "O2", "L1", "2026-07-02T16:00:00.000Z", "V3a");
    sale(&mut w, "O3", "L2", "2026-07-03T16:00:00.000Z", "V3b");
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1"), None);
    assert_eq!(label(&truth, "V1"), None);
    assert_eq!(label(&truth, "I2"), None);
    assert_eq!(label(&truth, "I3").unwrap(), "Last sold Jul 2, 2026");
    assert_eq!(label(&truth, "V3a").unwrap(), "Last sold Jul 2, 2026");
    assert_eq!(label(&truth, "V3b"), None);
    assert_eq!(label(&truth, "I4").unwrap(), format!("SLOW {DOT} no sales in 12+ months"));
}

#[test]
fn unselected_and_inactive_locations_do_not_count() {
    let locations = vec![
        location("L1", "America/New_York", true),
        location("L2", "America/Chicago", false),
        location("L3", "America/Denver", true),
    ];
    let mut w = world(90, "2026-07-14T16:00:00.000Z", locations.clone(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    sale(&mut w, "O1", "L1", "2026-03-01T16:00:00.000Z", "V1");
    sale(&mut w, "O2", "L2", "2026-07-01T16:00:00.000Z", "V1"); // inactive
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), format!("SLOW {DOT} last sold Mar 1, 2026"));

    let mut w = world(90, "2026-07-14T16:00:00.000Z", locations, "L1", Some(&["L1"]));
    add(&mut w, item("I1", &["V1"]));
    sale(&mut w, "O1", "L1", "2026-03-01T16:00:00.000Z", "V1");
    sale(&mut w, "O3", "L3", "2026-07-01T16:00:00.000Z", "V1"); // active, unselected
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), format!("SLOW {DOT} last sold Mar 1, 2026"));
}

#[test]
fn only_completed_positive_catalog_lines_are_sales() {
    let mut w = world(90, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    sale(&mut w, "O1", "L1", "2026-03-01T16:00:00.000Z", "V1");
    let line = |q: &str| json!({ "catalog_object_id": "V1", "quantity": q, "item_type": "ITEM" });
    let at = "2026-07-01T16:00:00.000Z";
    order(&mut w, "O2", "L1", "CANCELED", at, at, vec![line("1")]);
    order(&mut w, "O3", "L1", "OPEN", at, at, vec![line("1")]);
    order(&mut w, "O4", "L1", "COMPLETED", at, at, vec![line("0"), line("-1"), line("")]);
    order(
        &mut w,
        "O5",
        "L1",
        "COMPLETED",
        at,
        at,
        vec![json!({ "catalog_object_id": "V1", "quantity": "1", "item_type": "CUSTOM_AMOUNT" })],
    );
    order(&mut w, "O6", "L1", "COMPLETED", at, at, vec![json!({ "catalog_object_id": "", "quantity": "1" })]);
    w.learn_order(json!({
        "id": "O7", "location_id": "L1", "state": "COMPLETED", "closed_at": at, "updated_at": at,
        "line_items": [],
        "returns": [{ "source_order_id": "O1", "return_line_items": [{ "catalog_object_id": "V1", "quantity": "1" }] }],
    }));
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), format!("SLOW {DOT} last sold Mar 1, 2026"));
    // A fractional quantity counts; a missing item_type counts.
    order(&mut w, "O8", "L1", "COMPLETED", "2026-06-01T16:00:00.000Z", at, vec![line("0.25")]);
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), "Last sold Jun 1, 2026");
    order(
        &mut w,
        "O9",
        "L1",
        "COMPLETED",
        "2026-06-02T16:00:00.000Z",
        at,
        vec![json!({ "catalog_object_id": "V1", "quantity": "2" })],
    );
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), "Last sold Jun 2, 2026");
}

#[test]
fn a_newer_version_of_an_order_replaces_its_lines() {
    let mut w = world(90, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    add(&mut w, item("I2", &["V2"]));
    let closed = "2026-07-01T16:00:00.000Z";
    let both = vec![
        json!({ "catalog_object_id": "V1", "quantity": "1" }),
        json!({ "catalog_object_id": "V2", "quantity": "1" }),
    ];
    order(&mut w, "O1", "L1", "COMPLETED", closed, "2026-07-01T16:05:00.000Z", both.clone());
    sale(&mut w, "O0", "L1", "2026-02-01T16:00:00.000Z", "V1");
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), "Last sold Jul 1, 2026");
    // Edited later: the V1 line removed.
    order(&mut w, "O1", "L1", "COMPLETED", closed, "2026-07-10T12:00:00.000Z", vec![both[1].clone()]);
    // An older copy arriving afterwards changes nothing.
    order(&mut w, "O1", "L1", "COMPLETED", closed, "2026-07-01T16:05:00.000Z", both);
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), format!("SLOW {DOT} last sold Feb 1, 2026"));
    assert_eq!(label(&truth, "I2").unwrap(), "Last sold Jul 1, 2026");
}

#[test]
fn items_use_live_variations_and_archived_items_are_left_out() {
    let mut w = world(90, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1a", "V1b", "V1c"]));
    let mut archived = item("I2", &["V2"]);
    archived["item_data"]["is_archived"] = json!(true);
    add(&mut w, archived);
    sale(&mut w, "O1", "L1", "2026-07-01T16:00:00.000Z", "V1a");
    sale(&mut w, "O2", "L1", "2026-07-10T16:00:00.000Z", "V1b");
    sale(&mut w, "O3", "L1", "2026-07-11T16:00:00.000Z", "V2");
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Jul 10, 2026");
    assert!(!truth.items.contains_key("I2"));
    assert!(!truth.variations.contains_key("V2"));
    // V1b removed: the item falls back to V1a.
    w.items.get_mut("I1").unwrap()["item_data"]["variations"].as_array_mut().unwrap().remove(1);
    let truth = w.truth(w.t1);
    assert_eq!(label(&truth, "I1").unwrap(), "Last sold Jul 1, 2026");
    assert!(!truth.variations.contains_key("V1b"));
}

#[test]
fn time_advancing_crosses_into_slow() {
    let mut w = world(30, "2026-07-14T16:00:00.000Z", ny(), "L1", None);
    add(&mut w, item("I1", &["V1"]));
    sale(&mut w, "O1", "L1", "2026-07-01T16:00:00.000Z", "V1");
    assert_eq!(label(&w.truth(w.t1), "I1").unwrap(), "Last sold Jul 1, 2026");
    assert_eq!(label(&w.truth(t("2026-07-31T23:00:00.000Z")), "I1").unwrap(), "Last sold Jul 1, 2026");
    assert_eq!(
        label(&w.truth(t("2026-08-01T14:00:00.000Z")), "I1").unwrap(),
        format!("SLOW {DOT} last sold Jul 1, 2026")
    );
}

// ---- performance --------------------------------------------------------

/// `cargo test -p dsf-gen --release -- --ignored`
#[test]
#[ignore]
fn sized_scenario_builds_quickly() {
    let size = Size { items: 20_000, max_variations: 5, orders: 100_000, locations: 3 };
    // Seed 1: first check only; seed 29: four later checks.
    for seed in [1, 29] {
        let start = std::time::Instant::now();
        let s = scenario_sized(seed, size);
        let elapsed = start.elapsed();
        eprintln!(
            "seed {seed}: built in {elapsed:?}: {} catalog objects, {} orders, {} later checks ({} new orders)",
            s.mock.catalog.len(),
            s.mock.orders.len(),
            s.later.len(),
            s.later.iter().map(|c| c.new_orders.len()).sum::<usize>(),
        );
        assert!(s.mock.orders.len() >= 100_000);
        assert!(s.first.truth.items.len() >= 19_000);
        assert!(elapsed < std::time::Duration::from_secs(10));
    }
}

#[test]
#[ignore]
fn coverage_report() {
    let mut cases: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for seed in 0..200 {
        let s = scenario(seed);
        for c in s.cases.values() {
            let what = c.split(" \u{2014} ").nth(1).unwrap().split(" at ").next().unwrap().to_string();
            *cases.entry(what).or_default() += 1;
        }
        for check in s.checks() {
            for l in check.truth.items.values() {
                *kinds.entry(kind(l)).or_default() += 1;
            }
        }
    }
    eprintln!("{cases:#?}\n{kinds:#?}");
}
