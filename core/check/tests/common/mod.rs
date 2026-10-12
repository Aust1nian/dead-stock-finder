//! A test harness: the mock Square, a temp data folder, tokens in memory, a
//! fixed clock, and every request recorded.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use dsf_check::{Config, Deps, Outcome, run};
use dsf_compute::LABEL_KEY;
use dsf_mock::{MOCK_APP_ID, MockSquare, Seed};
use dsf_notify::Recorder;
use dsf_schedule::Trigger;
use dsf_square::http::RequestRecord;
use dsf_square::{Environment, MemoryStore, RetryPolicy, TokenStore, Tokens};
use dsf_sync::{Cache, Settings};
use serde_json::{Value, json};

pub struct Harness {
    pub mock: MockSquare,
    pub dir: tempfile::TempDir,
    pub store: MemoryStore,
    pub notifier: Recorder,
    pub now: Arc<Mutex<DateTime<Utc>>>,
    pub requests: Arc<Mutex<Vec<RequestRecord>>>,
}

pub fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Harness {
    pub fn new(seed: Seed, settings: Settings, now: DateTime<Utc>) -> Harness {
        let mock = MockSquare::start(seed, LABEL_KEY);
        mock.state().clock = Some(now);
        let (access, refresh) = mock.state().issue_tokens(&[
            "MERCHANT_PROFILE_READ",
            "ITEMS_READ",
            "ORDERS_READ",
            "INVENTORY_READ",
            "ITEMS_WRITE",
        ]);
        let store = MemoryStore::with(Tokens {
            access_token: access,
            access_expires_at: now + chrono::Duration::days(30),
            refresh_token: refresh,
            refresh_expires_at: None,
            obtained_at: now,
            merchant_id: "M1".into(),
            scopes: vec![
                "MERCHANT_PROFILE_READ".into(),
                "ITEMS_READ".into(),
                "ORDERS_READ".into(),
                "INVENTORY_READ".into(),
                "ITEMS_WRITE".into(),
            ],
            environment: "mock".into(),
        });
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(&dsf_check::paths::cache_file(dir.path())).unwrap();
        cache.save_settings(&Settings { setup_done: true, ..settings }).unwrap();
        Harness {
            mock,
            dir,
            store,
            notifier: Recorder::default(),
            now: Arc::new(Mutex::new(now)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn set_now(&self, t: DateTime<Utc>) {
        *self.now.lock().unwrap() = t;
        self.mock.state().clock = Some(t);
    }

    pub fn advance(&self, d: chrono::Duration) {
        let t = *self.now.lock().unwrap() + d;
        self.set_now(t);
    }

    pub fn config(&self) -> Config {
        Config {
            app_id: MOCK_APP_ID.into(),
            environment: Environment::Mock(self.mock.addr()),
            data_dir: self.dir.path().to_path_buf(),
        }
    }

    pub fn check(&self, trigger: Trigger) -> Outcome {
        let now = self.now.clone();
        let clock = move || *now.lock().unwrap();
        let requests = self.requests.clone();
        let deps = Deps {
            tokens: &self.store,
            notifier: &self.notifier,
            clock: &clock,
            retry: RetryPolicy::fast(),
            network_wait: Duration::from_millis(0),
            observer: Some(Arc::new(move |r: &RequestRecord| requests.lock().unwrap().push(r.clone()))),
            progress: None,
        };
        run(&self.config(), &deps, trigger)
    }

    pub fn cache(&self) -> Cache {
        Cache::open(&dsf_check::paths::cache_file(self.dir.path())).unwrap()
    }

    pub fn label(&self, id: &str) -> Option<String> {
        self.mock.state().label_of(id)
    }

    pub fn writes(&self) -> usize {
        self.mock.state().writes.len()
    }

    pub fn violations(&self) -> Vec<String> {
        self.mock.state().write_violations.clone()
    }

    pub fn tokens(&self) -> Option<Tokens> {
        self.store.load().unwrap()
    }
}

pub fn location(id: &str, tz: &str) -> Value {
    json!({ "id": id, "name": format!("Shop {id}"), "timezone": tz, "status": "ACTIVE", "currency": "USD" })
}

pub fn variation(id: &str, name: &str, price: i64) -> Value {
    json!({
        "type": "ITEM_VARIATION", "id": id,
        "item_variation_data": {
            "name": name, "sku": format!("SKU-{id}"), "pricing_type": "FIXED_PRICING",
            "price_money": { "amount": price, "currency": "USD" }, "track_inventory": true,
            "vendor_information": [{ "unit_cost_money": { "amount": price / 2, "currency": "USD" } }]
        }
    })
}

pub fn item(id: &str, name: &str, variations: Vec<Value>) -> Value {
    json!({
        "type": "ITEM", "id": id,
        "item_data": { "name": name, "product_type": "REGULAR", "variations": variations }
    })
}

pub fn order(id: &str, location: &str, closed: DateTime<Utc>, lines: &[(&str, &str)]) -> Value {
    let line_items: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(i, (var, qty))| json!({ "uid": format!("{id}-{i}"), "catalog_object_id": var, "quantity": qty, "item_type": "ITEM" }))
        .collect();
    json!({
        "id": id, "location_id": location, "state": "COMPLETED",
        "created_at": ts(closed - chrono::Duration::minutes(3)), "closed_at": ts(closed),
        "updated_at": ts(closed), "line_items": line_items
    })
}

pub fn count(var: &str, location: &str, qty: &str, at: DateTime<Utc>) -> Value {
    json!({ "catalog_object_id": var, "catalog_object_type": "ITEM_VARIATION", "state": "IN_STOCK",
            "location_id": location, "quantity": qty, "calculated_at": ts(at) })
}

pub fn merchant() -> Value {
    json!({ "id": "M1", "business_name": "Main St Boutique", "main_location_id": "L1", "currency": "USD" })
}
