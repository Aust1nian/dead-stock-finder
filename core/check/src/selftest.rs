//! `--self-test` and Help → Check this computer: a full check against
//! bundled mock data, in a temporary folder, talking only to a mock Square
//! on this computer's loopback address. It proves the pieces work here: the
//! cache, the check, the label rules, the safe-write procedure, the host
//! allowlist and the keychain.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use dsf_compute::LABEL_KEY;
use dsf_mock::{MOCK_APP_ID, MockSquare, Seed};
use dsf_notify::Recorder;
use dsf_schedule::Trigger;
use dsf_square::http::RequestRecord;
use dsf_square::{Environment, MemoryStore, OsKeychain, RetryPolicy, TokenStore, Tokens};
use dsf_sync::{Cache, Settings};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Config, Deps, Outcome, paths, run as run_check};

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub passed: bool,
    pub steps: Vec<Step>,
    pub millis: u128,
    pub requests: usize,
    pub version: String,
    pub square_version: String,
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn variation(id: &str, name: &str, price: i64) -> Value {
    json!({ "type": "ITEM_VARIATION", "id": id, "item_variation_data": {
        "name": name, "pricing_type": "FIXED_PRICING", "price_money": { "amount": price, "currency": "USD" }, "track_inventory": true } })
}

fn item(id: &str, name: &str, variations: Vec<Value>) -> Value {
    json!({ "type": "ITEM", "id": id, "item_data": { "name": name, "variations": variations } })
}

fn order(id: &str, location: &str, closed: DateTime<Utc>, variation: &str) -> Value {
    json!({ "id": id, "location_id": location, "state": "COMPLETED", "closed_at": ts(closed), "updated_at": ts(closed),
            "line_items": [{ "uid": "1", "catalog_object_id": variation, "quantity": "1", "item_type": "ITEM" }] })
}

/// Object id and the label it must carry.
type Expected = Vec<(&'static str, Option<&'static str>)>;

/// The bundled data and the labels it must produce on Oct 12, 2026.
fn bundled() -> (Seed, DateTime<Utc>, Expected) {
    let mut seasonal = item("S3", "Scarf", vec![variation("S3V", "Wool", 3000)]);
    seasonal["custom_attribute_values"] =
        json!({ "Square:self": { "name": "Season", "string_value": "Fall" } });
    let seed = Seed {
        app_id: String::new(),
        merchant: json!({ "id": "M-SELF", "business_name": "Self-test Shop", "main_location_id": "L1" }),
        locations: vec![
            json!({ "id": "L1", "name": "Main St", "timezone": "America/New_York", "status": "ACTIVE" }),
            json!({ "id": "L2", "name": "Harbor", "timezone": "America/Los_Angeles", "status": "ACTIVE" }),
        ],
        catalog: vec![
            item("S1", "Mug", vec![variation("S1A", "Blue", 1200), variation("S1B", "Red", 1200)]),
            item("S2", "Candle", vec![variation("S2V", "Vanilla", 1800)]),
            seasonal,
        ],
        orders: vec![
            order("SO1", "L1", utc(2026, 7, 14, 18, 0), "S1A"),
            order("SO2", "L2", utc(2026, 6, 2, 6, 0), "S1B"),
            order("SO3", "L1", utc(2026, 7, 13, 18, 0), "S2V"),
        ],
        counts: vec![],
    };
    let expected = vec![
        ("S1", Some("Last sold Jul 14, 2026")),
        ("S1A", Some("Last sold Jul 14, 2026")),
        ("S1B", Some("SLOW · last sold Jun 1, 2026")),
        ("S2", Some("SLOW · last sold Jul 13, 2026")),
        ("S2V", None),
        ("S3", Some("SLOW · no sales in 12+ months")),
    ];
    (seed, utc(2026, 10, 12, 13, 0), expected)
}

pub fn run() -> Report {
    let started = Instant::now();
    let mut steps = Vec::new();
    let mut step = |name: &str, ok: bool, detail: String| steps.push(Step { name: name.into(), ok, detail });

    let (seed, now, expected) = bundled();
    let mock = MockSquare::start(seed, LABEL_KEY);
    mock.state().clock = Some(now);
    step("mock Square on loopback", mock.addr().ip().is_loopback(), mock.addr().to_string());
    let scopes = ["MERCHANT_PROFILE_READ", "ITEMS_READ", "ORDERS_READ", "INVENTORY_READ", "ITEMS_WRITE"];
    let (access, refresh) = mock.state().issue_tokens(&scopes);
    let store = MemoryStore::with(Tokens {
        access_token: access.clone(),
        access_expires_at: now + chrono::Duration::days(30),
        refresh_token: refresh.clone(),
        refresh_expires_at: None,
        obtained_at: now,
        merchant_id: "M-SELF".into(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        environment: "mock".into(),
    });
    let dir = std::env::temp_dir().join(format!("idle-stock-finder-self-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache_ok = Cache::open(&paths::cache_file(&dir))
        .and_then(|c| c.save_settings(&Settings { setup_done: true, ..Settings::default() }));
    step("cache in a temporary folder", cache_ok.is_ok(), dir.display().to_string());

    let requests: Arc<Mutex<Vec<RequestRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let notifier = Recorder::default();
    let clock = move || now;
    let cfg = Config {
        app_id: MOCK_APP_ID.into(),
        environment: Environment::Mock(mock.addr()),
        data_dir: dir.clone(),
    };
    let deps = Deps {
        tokens: &store,
        notifier: &notifier,
        clock: &clock,
        retry: RetryPolicy::fast(),
        network_wait: Duration::ZERO,
        observer: Some(Arc::new(move |r: &RequestRecord| recorded.lock().unwrap().push(r.clone()))),
        progress: None,
    };

    let first = run_check(&cfg, &deps, Trigger::SelfTest);
    step("first check", matches!(first, Outcome::Done(_)), serde_json::to_string(&first).unwrap_or_default());

    let mut wrong = Vec::new();
    for (id, want) in &expected {
        let got = mock.state().label_of(id);
        if got.as_deref() != *want {
            wrong.push(format!("{id}: expected {want:?}, got {got:?}"));
        }
    }
    let seller_value_kept = mock.state().object("S3").and_then(|o| {
        o.pointer("/custom_attribute_values/Square:self/string_value")
            .and_then(Value::as_str)
            .map(str::to_string)
    }) == Some("Fall".into());
    if !seller_value_kept {
        wrong.push("the seller's own attribute on S3 changed".into());
    }
    step("labels match the known answers", wrong.is_empty(), wrong.join("; "));

    let violations = mock.state().write_violations.clone();
    step("every write changed only the label", violations.is_empty(), violations.join("; "));

    let writes = mock.state().writes.len();
    let second = run_check(&cfg, &deps, Trigger::SelfTest);
    let rewrites = mock.state().writes.len() - writes;
    step(
        "second check writes nothing",
        matches!(second, Outcome::Done(_)) && rewrites == 0,
        format!("{rewrites} writes"),
    );

    let all = requests.lock().unwrap().clone();
    let base = format!("http://{}/", mock.addr());
    let outside: Vec<&RequestRecord> =
        all.iter().filter(|r| !r.allowed || !r.url.starts_with(&base)).collect();
    step(
        "every request stayed on the allowlist",
        outside.is_empty() && !all.is_empty(),
        format!("{} requests, {} outside", all.len(), outside.len()),
    );

    let mut leaked = Vec::new();
    for file in walk(&dir) {
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap_or_default()).to_string();
        if text.contains(&access) || text.contains(&refresh) {
            leaked.push(file.display().to_string());
        }
    }
    step("no token in any file", leaked.is_empty(), leaked.join(", "));

    let report_rows = Cache::open(&paths::cache_file(&dir))
        .ok()
        .and_then(|c| dsf_sync::report(&c, &Settings::default(), now, &Default::default()).ok())
        .map(|r| r.rows.len())
        .unwrap_or(0);
    step("report", report_rows == 4, format!("{report_rows} rows"));

    // The keychain on this computer: save, read back with a second handle,
    // delete. A separate entry, never the real connection.
    let service = format!("{}.selftest", dsf_square::APP_ID);
    let first_handle = OsKeychain::new(&service, "self-test", true);
    let second_handle = OsKeychain::new(&service, "self-test", false);
    let sample = store.load().ok().flatten();
    let keychain = sample
        .as_ref()
        .ok_or_else(|| "no sample".to_string())
        .and_then(|t| first_handle.save(t).map_err(|e| e.to_string()))
        .and_then(|_| second_handle.load().map_err(|e| e.to_string()))
        .map(|loaded| loaded == sample);
    let _ = first_handle.delete();
    match keychain {
        Ok(true) => step("keychain", true, "saved, read back, deleted".into()),
        Ok(false) => step("keychain", false, "read back something else".into()),
        // Linux is development only; containers often have no kernel keyring.
        Err(e) if cfg!(target_os = "linux") => step("keychain", true, format!("skipped on Linux: {e}")),
        Err(e) => step("keychain", false, e),
    }

    let _ = std::fs::remove_dir_all(&dir);
    let passed = steps.iter().all(|s| s.ok);
    Report {
        passed,
        steps,
        millis: started.elapsed().as_millis(),
        requests: all.len(),
        version: env!("CARGO_PKG_VERSION").into(),
        square_version: dsf_square::SQUARE_VERSION.into(),
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn self_test_passes_here() {
        let report = super::run();
        // The keychain step may be unavailable in a sandboxed CI shell;
        // everything else must pass.
        for s in &report.steps {
            if s.name != "keychain" {
                assert!(s.ok, "{}: {}", s.name, s.detail);
            }
        }
    }
}
