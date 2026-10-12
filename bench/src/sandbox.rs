//! The Square Sandbox run (spec § Tests, "Square Sandbox"): against a
//! dedicated Sandbox seller, seeded by this script with items and orders.
//! It snapshots the catalog, runs a check, reads every label back, confirms
//! the diff-only rule held, removes all labels, and confirms the catalog
//! matches its starting snapshot exactly.
//!
//!   SQUARE_SANDBOX_TOKEN=… SQUARE_SANDBOX_APP_ID=… dsf-sandbox [--seed-items 12] [--out sandbox.json]
//!
//! Needs a build with the `sandbox` feature (Square's Sandbox host is off in
//! shipped builds). The token is a Sandbox access token from the Developer
//! Console (SQUARE_APP_SETUP.md); it never leaves this process.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use dsf_square::api::{Api, CatalogQuery};
use dsf_square::http::RequestRecord;
use dsf_square::{Environment, HttpClient, Method, RetryPolicy, TokenStore};
use serde_json::{Value, json};

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn key() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Every live item, versions and timestamps removed, our label stripped.
fn snapshot(api: &Api) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let q = CatalogQuery { object_types: vec!["ITEM"], begin_time: None, include_deleted: false };
    api.search_catalog(&q, |objects| -> Result<(), dsf_square::Error> {
        for o in objects {
            let id = o["id"].as_str().unwrap_or_default().to_string();
            out.insert(id, dsf_labels::diff::without_write_stamps(&o));
        }
        Ok(())
    })
    .expect("read the catalog");
    out
}

pub fn main(args: &[String]) -> i32 {
    let token = std::env::var("SQUARE_SANDBOX_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!("SQUARE_SANDBOX_TOKEN isn't set: nothing to do (see SQUARE_APP_SETUP.md)");
        return 0;
    }
    if std::env::var("SQUARE_SANDBOX_APP_ID").unwrap_or_default().is_empty() {
        eprintln!("SQUARE_SANDBOX_APP_ID isn't set: the app recognises its own label definition by app id");
        return 2;
    }
    if !cfg!(feature = "sandbox") {
        eprintln!("build with --features sandbox");
        return 2;
    }
    let out = arg(args, "--out", "sandbox.json");
    let seed_items: usize = arg(args, "--seed-items", "12").parse().unwrap_or(12);
    let mut steps: Vec<Value> = Vec::new();
    let mut failed = false;
    let mut step = |name: &str, ok: bool, detail: String| {
        println!("[{}] {name}: {detail}", if ok { "pass" } else { "FAIL" });
        failed |= !ok;
        steps.push(json!({ "step": name, "ok": ok, "detail": detail }));
    };
    let client =
        HttpClient::new(Environment::Sandbox).expect("sandbox client").with_retry(RetryPolicy::default());
    let api = Api::new(&client, &token);
    let merchant = api.merchant().expect("Sandbox merchant");
    let locations = api.locations().expect("Sandbox locations");
    let location =
        locations.iter().find(|l| l.status.as_deref() == Some("ACTIVE")).expect("an active location").clone();
    step(
        "sandbox seller",
        true,
        format!("{} · {} locations", merchant.business_name.unwrap_or_default(), locations.len()),
    );

    // 1. Seed: items named "DSF test …" (reused on later runs) and one paid
    //    order for half of them, so labels cover both sold and never sold.
    let before_seed = snapshot(&api);
    let existing: Vec<&Value> = before_seed
        .values()
        .filter(|o| {
            o.pointer("/item_data/name").and_then(Value::as_str).is_some_and(|n| n.starts_with("DSF test"))
        })
        .collect();
    if existing.len() < seed_items {
        let objects: Vec<Value> = (existing.len()..seed_items)
            .map(|i| {
                let mut variations = vec![json!({ "type": "ITEM_VARIATION", "id": format!("#v{i}a"), "item_variation_data": {
                    "name": "Regular", "pricing_type": "FIXED_PRICING", "price_money": { "amount": 500 + i as i64 * 100, "currency": location.currency.clone().unwrap_or("USD".into()) } } })];
                if i % 3 == 0 {
                    variations.push(json!({ "type": "ITEM_VARIATION", "id": format!("#v{i}b"), "item_variation_data": {
                        "name": "Large", "pricing_type": "FIXED_PRICING", "price_money": { "amount": 900 + i as i64 * 100, "currency": location.currency.clone().unwrap_or("USD".into()) } } }));
                }
                let mut item = json!({ "type": "ITEM", "id": format!("#i{i}"), "item_data": { "name": format!("DSF test item {i:02}"), "variations": variations } });
                if i % 4 == 1 {
                    item["item_data"]["description_html"] = json!("<p>Seeded with <b>HTML</b> to prove it survives.</p>");
                }
                item
            })
            .collect();
        match api.batch_upsert(&key(), objects) {
            Ok(r) => step("seed items", true, format!("{} objects created", r.objects.len())),
            Err(e) => step("seed items", false, e.to_string()),
        }
        let created = snapshot(&api);
        let mut sold = 0;
        for (n, item) in created
            .values()
            .filter(|o| {
                o.pointer("/item_data/name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.starts_with("DSF test"))
            })
            .enumerate()
        {
            if n % 2 == 1 {
                continue;
            }
            let Some(var) = item.pointer("/item_data/variations/0/id").and_then(Value::as_str) else {
                continue;
            };
            let order = json!({ "idempotency_key": key(), "order": { "location_id": location.id, "line_items": [{ "catalog_object_id": var, "quantity": "1" }] } });
            let Ok(created) = client.send(Method::Post, "/v2/orders", Some(&token), Some(&order)) else {
                continue;
            };
            let order_id =
                created.body.pointer("/order/id").and_then(Value::as_str).unwrap_or_default().to_string();
            let total = created.body.pointer("/order/total_money").cloned().unwrap_or(Value::Null);
            let payment = json!({ "idempotency_key": key(), "source_id": "cnon:card-nonce-ok", "amount_money": total, "order_id": order_id, "location_id": location.id, "autocomplete": true });
            if client.send(Method::Post, "/v2/payments", Some(&token), Some(&payment)).is_ok() {
                sold += 1;
            }
        }
        step("seed orders", sold > 0, format!("{sold} paid orders"));
        // Square indexes new orders for search after a short delay.
        std::thread::sleep(Duration::from_secs(20));
    } else {
        step("seed", true, format!("reusing {} seeded items", existing.len()));
    }

    // 2. The starting snapshot, then a full check with labels on.
    let start = snapshot(&api);
    let dir = std::env::temp_dir().join(format!("dsf-sandbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = dsf_sync::Cache::open(&dsf_check::paths::cache_file(&dir)).unwrap();
    cache.save_settings(&dsf_sync::Settings { setup_done: true, ..Default::default() }).unwrap();
    drop(cache);
    let store = dsf_square::MemoryStore::default();
    let now = Utc::now();
    store
        .save(&dsf_square::Tokens {
            access_token: token.clone(),
            access_expires_at: now + chrono::Duration::days(30),
            refresh_token: String::new(),
            refresh_expires_at: None,
            obtained_at: now,
            merchant_id: merchant.id.clone(),
            scopes: dsf_square::oauth::scopes(true),
            environment: "sandbox".into(),
        })
        .unwrap();
    let requests: Arc<Mutex<Vec<RequestRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let notifier = dsf_notify::Recorder::default();
    let clock = Utc::now;
    let app_id = std::env::var("SQUARE_SANDBOX_APP_ID").unwrap_or_default();
    let cfg = dsf_check::Config { app_id, environment: Environment::Sandbox, data_dir: dir.clone() };
    let deps = dsf_check::Deps {
        tokens: &store,
        notifier: &notifier,
        clock: &clock,
        retry: RetryPolicy::default(),
        network_wait: Duration::from_secs(60),
        observer: Some(Arc::new(move |r: &RequestRecord| recorded.lock().unwrap().push(r.clone()))),
        progress: None,
    };
    let outcome = dsf_check::run(&cfg, &deps, dsf_schedule::Trigger::Manual);
    step(
        "check",
        matches!(outcome, dsf_check::Outcome::Done(_)),
        serde_json::to_string(&outcome).unwrap_or_default(),
    );

    // 3. Every label read back; nothing but labels changed.
    let after = snapshot(&api);
    let mut labelled = 0;
    let mut other_changes = Vec::new();
    for (id, before) in &start {
        let Some(now_obj) = after.get(id) else { continue };
        if now_obj
            .pointer(&format!("/custom_attribute_values/{}/string_value", dsf_compute::LABEL_KEY))
            .is_some()
        {
            labelled += 1;
        }
        if let Err(paths) = dsf_labels::diff::only_label_changed(before, now_obj) {
            other_changes.push(format!("{id}: {}", paths.join(", ")));
        }
    }
    step("labels written and read back", labelled > 0, format!("{labelled} items carry the label"));
    step("diff-only rule held", other_changes.is_empty(), other_changes.join("; "));
    let outside: usize = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| !r.allowed || !r.url.starts_with("https://connect.squareupsandbox.com/"))
        .count();
    step("every request went to the Sandbox host", outside == 0, format!("{outside} outside"));

    // 4. Remove all labels; the catalog matches the starting snapshot.
    match dsf_check::remove_all_labels(&cfg, &deps) {
        Ok(n) => step("remove all labels", true, format!("{n} items cleared")),
        Err(e) => step("remove all labels", false, e),
    }
    let end = snapshot(&api);
    let strip = |m: &BTreeMap<String, Value>| -> BTreeMap<String, Value> {
        m.iter().map(|(k, v)| (k.clone(), dsf_labels::diff::without_label(v))).collect()
    };
    let same = strip(&start) == strip(&end) && start.keys().eq(end.keys());
    let leftover = end
        .values()
        .filter(|o| o.pointer(&format!("/custom_attribute_values/{}", dsf_compute::LABEL_KEY)).is_some())
        .count();
    step(
        "catalog back to its starting snapshot",
        same && leftover == 0,
        format!("{leftover} labels left; identical apart from labels: {same}"),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&json!({ "at": Utc::now().to_rfc3339(), "steps": steps })).unwrap(),
    )
    .unwrap();
    if failed { 1 } else { 0 }
}
