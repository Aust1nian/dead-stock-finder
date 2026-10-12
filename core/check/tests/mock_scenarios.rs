//! The spec's mock-server scenarios, one test each, against the full check.

mod common;

use chrono::Duration;
use common::*;
use dsf_check::Outcome;
use dsf_mock::{ConflictMode, Seed};
use dsf_schedule::Trigger;
use dsf_sync::Settings;
use serde_json::json;

/// Today: Oct 12, 2026, 09:00 in New York.
fn today() -> chrono::DateTime<chrono::Utc> {
    utc(2026, 10, 12, 13, 0)
}

fn base_seed() -> Seed {
    let mut fifty = Vec::new();
    for i in 0..50 {
        fifty.push(variation(&format!("V6-{i:02}"), &format!("Size {i}"), 1000 + i));
    }
    let mut archived = item("I4", "Old stock", vec![variation("V4", "Regular", 500)]);
    archived["item_data"]["is_archived"] = json!(true);
    let mut elsewhere = item("I5", "Only at the warehouse", vec![variation("V5", "Regular", 700)]);
    elsewhere["present_at_all_locations"] = json!(false);
    elsewhere["present_at_location_ids"] = json!(["L3"]);
    let mut seasonal = item("I8", "Scarf", vec![variation("V8", "Wool", 3000)]);
    seasonal["custom_attribute_values"] =
        json!({ "Square:9f1e": { "name": "Season", "string_value": "Fall" } });
    let mut canceled = order("O-cancel", "L1", utc(2026, 10, 1, 15, 0), &[("V7", "1")]);
    canceled["state"] = json!("CANCELED");
    let custom = json!({
        "id": "O-custom", "location_id": "L1", "state": "COMPLETED",
        "closed_at": ts(utc(2026, 10, 2, 15, 0)), "updated_at": ts(utc(2026, 10, 2, 15, 0)),
        "line_items": [{ "uid": "c1", "quantity": "1", "item_type": "CUSTOM_AMOUNT" }, { "uid": "c2", "catalog_object_id": "V7", "quantity": "0", "item_type": "ITEM" }]
    });
    let returned = json!({
        "id": "O-return", "location_id": "L1", "state": "COMPLETED",
        "closed_at": ts(utc(2026, 10, 3, 15, 0)), "updated_at": ts(utc(2026, 10, 3, 15, 0)), "line_items": [],
        "returns": [{ "source_order_id": "O-v2", "return_line_items": [{ "catalog_object_id": "V2", "quantity": "1" }] }]
    });
    Seed {
        app_id: String::new(),
        merchant: merchant(),
        locations: vec![
            location("L1", "America/New_York"),
            location("L2", "America/Los_Angeles"),
            json!({ "id": "L3", "name": "Warehouse", "timezone": "America/Chicago", "status": "INACTIVE" }),
        ],
        catalog: vec![
            json!({ "type": "CATEGORY", "id": "C1", "category_data": { "name": "Kitchen" } }),
            {
                let mut i =
                    item("I1", "Mug", vec![variation("V1a", "Blue", 1200), variation("V1b", "Red", 1200)]);
                i["item_data"]["reporting_category"] = json!({ "id": "C1" });
                i
            },
            item("I2", "Candle", vec![variation("V2", "Vanilla", 1800)]),
            item("I3", "Never sold", vec![variation("V3", "Regular", 900)]),
            archived,
            elsewhere,
            item("I6", "T-shirt", fifty),
            item("I7", "Odd orders", vec![variation("V7", "Regular", 400)]),
            seasonal,
        ],
        orders: vec![
            order("O-v1a", "L1", utc(2026, 7, 14, 18, 0), &[("V1a", "1")]),
            order("O-v1b", "L1", utc(2026, 6, 1, 18, 0), &[("V1b", "2")]),
            order("O-v2", "L1", utc(2026, 7, 13, 18, 0), &[("V2", "1")]),
            order("O-v4", "L1", utc(2026, 10, 1, 18, 0), &[("V4", "1")]),
            order("O-v6", "L2", utc(2026, 9, 1, 6, 30), &[("V6-07", "0.5")]),
            order("O-v8-l3", "L3", utc(2026, 10, 10, 18, 0), &[("V8", "1")]),
            order("O-old", "L1", utc(2025, 1, 1, 18, 0), &[("V3", "1")]),
            canceled,
            custom,
            returned,
        ],
        counts: vec![count("V1a", "L1", "4", today()), count("V2", "L1", "10", today())],
    }
}

fn expect_base(h: &Harness) {
    assert_eq!(h.label("I1").as_deref(), Some("Last sold Jul 14, 2026"));
    assert_eq!(h.label("V1a").as_deref(), Some("Last sold Jul 14, 2026"));
    assert_eq!(h.label("V1b").as_deref(), Some("SLOW · last sold Jun 1, 2026"));
    assert_eq!(h.label("I2").as_deref(), Some("SLOW · last sold Jul 13, 2026"));
    assert_eq!(h.label("V2"), None, "single variation: item label only");
    assert_eq!(h.label("I3").as_deref(), Some("SLOW · no sales in 12+ months"));
    assert_eq!(h.label("I4"), None, "archived items are left alone");
    assert_eq!(h.label("I5"), None, "not carried at the selected locations");
    // 06:30 UTC Sep 1 is Aug 31 in Los Angeles.
    assert_eq!(h.label("I6").as_deref(), Some("Last sold Aug 31, 2026"));
    assert_eq!(h.label("V6-07").as_deref(), Some("Last sold Aug 31, 2026"));
    assert_eq!(h.label("V6-08").as_deref(), Some("SLOW · no sales in 12+ months"));
    assert_eq!(
        h.label("I7").as_deref(),
        Some("SLOW · no sales in 12+ months"),
        "canceled, custom, zero and returns don't count"
    );
    // The sale at the inactive warehouse doesn't count by default.
    assert_eq!(h.label("I8").as_deref(), Some("SLOW · no sales in 12+ months"));
    assert_eq!(
        h.mock.state().object("I8").unwrap()["custom_attribute_values"]["Square:9f1e"]["string_value"],
        "Fall",
        "the seller's own attribute survives"
    );
    assert!(h.violations().is_empty(), "{:#?}", h.violations());
}

fn done(o: &Outcome) -> &dsf_check::Summary {
    match o {
        Outcome::Done(s) => s,
        other => panic!("expected a finished check, got {other:?}"),
    }
}

#[test]
fn first_check_labels_everything_and_second_writes_nothing() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    let first = h.check(Trigger::Manual);
    let s = done(&first);
    assert!(s.first_sync);
    assert_eq!(s.label_problem, None);
    expect_base(&h);
    let writes = h.writes();
    assert!(writes > 0);
    h.advance(Duration::hours(1));
    let second = h.check(Trigger::Manual);
    assert_eq!(done(&second).labels_written, 0);
    assert_eq!(h.writes(), writes, "a second check with no new orders writes nothing");
    // Every request went to the mock through the allowlist.
    let addr = h.mock.addr().to_string();
    for r in h.requests.lock().unwrap().iter() {
        assert!(r.allowed && r.url.starts_with(&format!("http://{addr}/")), "{}", r.url);
    }
}

#[test]
fn background_checks_wait_30_minutes_and_check_now_does_not() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Signin));
    h.advance(Duration::minutes(10));
    assert!(matches!(h.check(Trigger::Signin), Outcome::Skipped(_)));
    assert!(matches!(h.check(Trigger::Manual), Outcome::Done(_)));
}

#[test]
fn background_checks_need_setup() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.cache().save_settings(&Settings { setup_done: false, ..Settings::default() }).unwrap();
    assert!(matches!(h.check(Trigger::Scheduled), Outcome::Skipped(_)));
    assert_eq!(h.writes(), 0);
}

#[test]
fn paging_across_many_pages() {
    let mut seed = base_seed();
    for i in 0..2500 {
        seed.catalog.push(item(
            &format!("P{i:05}"),
            &format!("Bulk {i}"),
            vec![variation(&format!("PV{i:05}"), "One", 100)],
        ));
        seed.orders.push(order(
            &format!("PO{i:05}"),
            "L1",
            utc(2026, 9, 1, 12, 0) + Duration::minutes(i),
            &[(&format!("PV{i:05}"), "1")],
        ));
    }
    let h = Harness::new(seed, Settings::default(), today());
    done(&h.check(Trigger::Manual));
    expect_base(&h);
    assert_eq!(h.label("P00000").as_deref(), Some("Last sold Sep 1, 2026"));
    // 12:00 UTC Sep 1 + 2,499 minutes = 05:39 UTC Sep 3 = 01:39 Sep 3 in New York.
    assert_eq!(h.label("P02499").as_deref(), Some("Last sold Sep 3, 2026"));
    let state = h.mock.state();
    let searches = state.log.iter().filter(|l| l.path == "/v2/orders/search").count();
    assert!(searches >= 3, "orders came in {searches} pages");
}

#[test]
fn rate_limits_are_retried() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.rate_limit_every = Some(3);
    done(&h.check(Trigger::Manual));
    expect_base(&h);
    assert!(h.mock.state().log.iter().any(|l| l.status == 429));
}

#[test]
fn expired_access_token_is_refreshed_once_and_saved() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    let before = h.tokens().unwrap();
    h.mock.state().expire_access_tokens();
    h.advance(Duration::hours(1));
    done(&h.check(Trigger::Manual));
    let after = h.tokens().unwrap();
    assert_ne!(before.refresh_token, after.refresh_token, "the new refresh token was saved");
    assert_ne!(before.access_token, after.access_token);
}

#[test]
fn token_older_than_7_days_is_refreshed() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    let before = h.tokens().unwrap();
    h.advance(Duration::days(8));
    done(&h.check(Trigger::Manual));
    let after = h.tokens().unwrap();
    assert_ne!(before.refresh_token, after.refresh_token);
    // The old refresh token is spent: Square refuses it now.
    let state = h.mock.state();
    assert!(state.log.iter().filter(|l| l.path == "/oauth2/token").count() == 1);
}

#[test]
fn rejected_refresh_means_disconnected_and_one_notification() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    let labels_before = h.label("I1");
    h.mock.state().faults.reject_refresh = true;
    h.mock.state().expire_access_tokens();
    h.advance(Duration::days(1));
    assert_eq!(h.check(Trigger::Scheduled), Outcome::Disconnected);
    assert!(h.tokens().is_none(), "the dead token is deleted");
    let notes = h.notifier.taken();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "Reconnect Square to keep labels current");
    assert_eq!(h.label("I1"), labels_before, "labels stay as they are");
    h.advance(Duration::days(1));
    assert_eq!(h.check(Trigger::Scheduled), Outcome::NotConnected);
    assert!(h.notifier.taken().is_empty(), "told once");
    let status = dsf_check::status(&h.cache());
    assert!(status.disconnected);
}

#[test]
fn network_drop_mid_sync_fails_safe_and_the_next_check_recovers() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.drop_from = Some(4);
    h.mock.state().faults.drop_count = 1_000;
    assert!(matches!(h.check(Trigger::Manual), Outcome::Failed(_)));
    h.mock.state().faults.drop_from = None;
    done(&h.check(Trigger::Manual));
    expect_base(&h);
}

/// Killing the check at any request and running it again gives the same
/// final labels.
#[test]
fn interrupted_at_every_request_then_rerun_gives_the_same_labels() {
    let clean = Harness::new(base_seed(), Settings::default(), today());
    done(&clean.check(Trigger::Manual));
    let total = clean.mock.state().log.len() as u64;
    for n in 1..=total {
        let h = Harness::new(base_seed(), Settings::default(), today());
        h.mock.state().faults.drop_from = Some(n);
        h.mock.state().faults.drop_count = 1_000;
        let _ = h.check(Trigger::Manual);
        h.mock.state().faults.drop_from = None;
        h.advance(Duration::minutes(1));
        done(&h.check(Trigger::Manual));
        expect_base(&h);
    }
}

#[test]
fn version_conflict_refetches_and_keeps_the_staff_edit() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.staff_edit_after_retrieve.insert("I2".into(), 1);
    done(&h.check(Trigger::Manual));
    expect_base(&h);
    let item = h.mock.state().object("I2").unwrap();
    assert_eq!(item["item_data"]["name"], "Candle (edited)", "the staff edit survives");
}

#[test]
fn conflict_three_times_skips_that_item_only() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.staff_edit_after_retrieve.insert("I2".into(), 50);
    let outcome = h.check(Trigger::Manual);
    let s = done(&outcome);
    assert_eq!(s.labels_skipped, 1);
    assert_eq!(h.label("I2"), None);
    assert_eq!(h.label("I1").as_deref(), Some("Last sold Jul 14, 2026"));
    assert!(h.violations().is_empty());
    let report = dsf_sync::report(&h.cache(), &Settings::default(), today(), &Default::default()).unwrap();
    let row = report.rows.iter().find(|r| r.item_id == "I2").unwrap();
    assert!(row.label_status.starts_with("Skipped"), "{}", row.label_status);
}

#[test]
fn write_that_comes_back_changed_stops_all_writes() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.damage_on_write.insert("I2".into());
    let outcome = h.check(Trigger::Manual);
    let s = done(&outcome);
    assert!(s.label_problem.as_deref().unwrap().starts_with("label writes stopped"));
    let notes = h.notifier.taken();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, "Dead Stock Finder stopped writing labels");
    let halt = dsf_labels::halted(&h.cache()).unwrap();
    assert_eq!(halt.object_id, "I2");
    assert!(halt.paths.contains(&"/item_data/description".to_string()), "{:?}", halt.paths);
    // While stopped, later checks write nothing.
    let writes = h.writes();
    h.mock.state().faults.damage_on_write.clear();
    h.advance(Duration::days(1));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.writes(), writes);
    assert!(h.notifier.taken().is_empty(), "told once");
    // The owner resumes.
    dsf_labels::resume(&h.cache()).unwrap();
    h.advance(Duration::hours(1));
    done(&h.check(Trigger::Manual));
    assert!(h.writes() > writes);
}

#[test]
fn merged_staff_edit_during_a_write_stops_writes_fail_safe() {
    // Square's three-way merge folds a concurrent edit into the label write,
    // so the write's result differs from what was sent: treated as damage.
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.conflict_mode = ConflictMode::Merge;
    h.mock.state().faults.staff_edit_after_retrieve.insert("I2".into(), 1);
    let outcome = h.check(Trigger::Manual);
    assert!(done(&outcome).label_problem.is_some());
    let item = h.mock.state().object("I2").unwrap();
    assert_eq!(item["item_data"]["name"], "Candle (edited)", "nothing of the staff edit was lost");
}

#[test]
fn deleted_and_new_items_between_checks() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    {
        let mut s = h.mock.state();
        s.objects.remove("I3");
        s.put_object(item("I9", "New arrival", vec![variation("V9", "Regular", 2500)]));
    }
    h.advance(Duration::days(2));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I9").as_deref(), Some("No sales since added Oct 14, 2026"));
    h.advance(Duration::days(91));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I9").as_deref(), Some("SLOW · no sales since added Oct 14, 2026"));
    assert!(h.violations().is_empty());
}

#[test]
fn late_and_edited_orders_after_the_checkpoint() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    // An offline POS sale closed 2 days ago reaches Square only now.
    let mut late = order("O-late", "L1", utc(2026, 10, 10, 16, 0), &[("V2", "1")]);
    late["updated_at"] = json!(ts(utc(2026, 10, 12, 14, 0)));
    // An earlier order is edited: its Mug line is now a Candle line.
    let mut edited = order("O-v1b", "L1", utc(2026, 6, 1, 18, 0), &[("V3", "2")]);
    edited["updated_at"] = json!(ts(utc(2026, 10, 12, 14, 30)));
    h.mock.state().add_orders([late, edited]);
    h.advance(Duration::hours(2));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I2").as_deref(), Some("Last sold Oct 10, 2026"));
    assert_eq!(h.label("V1b").as_deref(), Some("SLOW · no sales in 12+ months"), "the edit removed the sale");
    assert_eq!(h.label("I3").as_deref(), Some("SLOW · last sold Jun 1, 2026"));
}

#[test]
fn staff_changing_a_label_by_hand_is_put_back() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    h.mock.state().staff_edit("I1", |o| {
        o["custom_attribute_values"]["dead_stock_last_sold"]["string_value"] = json!("typed by hand");
    });
    h.advance(Duration::hours(1));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I1").as_deref(), Some("Last sold Jul 14, 2026"));
}

#[test]
fn selected_locations_change_what_counts() {
    let settings = Settings { locations: Some(vec!["L1".into(), "L3".into()]), ..Settings::default() };
    let h = Harness::new(base_seed(), settings, today());
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I8").as_deref(), Some("Last sold Oct 10, 2026"), "the warehouse sale counts now");
    assert_eq!(h.label("I5").as_deref(), Some("SLOW · no sales in 12+ months"), "carried at the warehouse");
    assert_eq!(h.label("V6-07").as_deref(), Some("SLOW · no sales in 12+ months"), "LA isn't selected");
}

#[test]
fn labels_off_writes_nothing() {
    let h = Harness::new(base_seed(), Settings { labels_on: false, ..Settings::default() }, today());
    done(&h.check(Trigger::Manual));
    assert_eq!(h.writes(), 0);
    let report = dsf_sync::report(&h.cache(), &Settings::default(), today(), &Default::default()).unwrap();
    assert!(!report.rows.is_empty());
}

#[test]
fn remove_all_labels_restores_the_catalog_exactly() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    let before = h.mock.state().catalog_snapshot();
    done(&h.check(Trigger::Manual));
    assert_ne!(h.mock.state().catalog_snapshot(), before);
    let now = h.now.clone();
    let clock = move || *now.lock().unwrap();
    let deps = dsf_check::Deps {
        tokens: &h.store,
        notifier: &h.notifier,
        clock: &clock,
        retry: dsf_square::RetryPolicy::fast(),
        network_wait: std::time::Duration::ZERO,
        observer: None,
        progress: None,
    };
    let cleared = dsf_check::remove_all_labels(&h.config(), &deps).unwrap();
    assert!(cleared > 0);
    assert_eq!(h.mock.state().catalog_snapshot(), before, "the catalog is back to its starting snapshot");
    assert!(!h.mock.state().objects.values().any(|o| o["type"] == "CUSTOM_ATTRIBUTE_DEFINITION"));
    assert!(h.violations().is_empty());
}

#[test]
fn report_rows_have_stock_and_value() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    let report = dsf_sync::report(&h.cache(), &Settings::default(), today(), &Default::default()).unwrap();
    let mug = report.rows.iter().find(|r| r.variation_id == "V1a").unwrap();
    assert_eq!(mug.on_hand, Some(4.0));
    assert_eq!(mug.value_at_price, Some(4800));
    assert_eq!(mug.value_at_cost, Some(2400));
    assert_eq!(mug.category.as_deref(), Some("Kitchen"));
    assert_eq!(mug.days_since, Some(90));
    assert!(!mug.slow);
    assert_eq!(mug.label_status, "In Square");
    let slow_only = dsf_sync::report(
        &h.cache(),
        &Settings::default(),
        today(),
        &dsf_sync::ReportQuery { slow_only: true, ..Default::default() },
    )
    .unwrap();
    assert!(slow_only.rows.iter().all(|r| r.slow));
    let wider = dsf_sync::report(
        &h.cache(),
        &Settings::default(),
        today(),
        &dsf_sync::ReportQuery { window_days: Some(30), ..Default::default() },
    )
    .unwrap();
    assert!(wider.rows.iter().find(|r| r.variation_id == "V1a").unwrap().slow);
}

#[test]
fn tokens_never_reach_any_file() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    done(&h.check(Trigger::Manual));
    h.mock.state().expire_access_tokens();
    h.advance(Duration::days(8));
    done(&h.check(Trigger::Manual));
    let tokens = h.tokens().unwrap();
    let secrets = [tokens.access_token.clone(), tokens.refresh_token.clone()];
    for entry in walk(h.dir.path()) {
        let bytes = std::fs::read(&entry).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for s in &secrets {
            assert!(!text.contains(s.as_str()), "{} holds a token", entry.display());
        }
        assert!(
            !text.contains("EAAAmock") && !text.contains("EQAAmock"),
            "{} holds a token",
            entry.display()
        );
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn catalog_changes_are_read_without_latest_time_too() {
    let h = Harness::new(base_seed(), Settings::default(), today());
    h.mock.state().faults.omit_latest_time = true;
    done(&h.check(Trigger::Manual));
    expect_base(&h);
    // A staff member restocks the never-sold item with a new variation.
    h.mock.state().staff_edit("I3", |o| {
        o["item_data"]["variations"].as_array_mut().unwrap().push(variation("V3b", "Large", 1200));
    });
    h.mock.state().add_orders([order("O-new", "L1", utc(2026, 10, 12, 18, 0), &[("V3b", "1")])]);
    h.advance(Duration::hours(6));
    done(&h.check(Trigger::Manual));
    assert_eq!(h.label("I3").as_deref(), Some("Last sold Oct 12, 2026"));
    assert_eq!(h.label("V3b").as_deref(), Some("Last sold Oct 12, 2026"));
}
