//! A local mock Square server for tests and `--self-test`.
//!
//! It answers the calls the app makes with Square's response shapes and
//! follows Square's rules as recorded in SQUARE_FACTS.md: full-replacement
//! catalog writes, version checks (reject, or Square's newer three-way merge),
//! single-use PKCE refresh tokens, 10 locations per SearchOrders call, cursor
//! paging, idempotency keys. Faults switch on the edge cases: rate limits,
//! expired tokens, rejected refresh tokens, dropped connections, staff edits
//! between read and write, and writes that come back changed.
//!
//! It also checks the app: every catalog write is compared with the version
//! the app fetched, and anything beyond the app's own label key is recorded
//! in [`State::write_violations`].

pub mod server;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use server::{HttpRequest, HttpResponse, Reply, Server};

/// Square-shaped data to start the mock with. Catalog objects need no
/// versions; the mock stamps them.
#[derive(Clone, Debug, Default)]
pub struct Seed {
    pub app_id: String,
    pub merchant: Value,
    pub locations: Vec<Value>,
    pub catalog: Vec<Value>,
    pub orders: Vec<Value>,
    pub counts: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConflictMode {
    /// A write with an old version is refused (VERSION_MISMATCH).
    #[default]
    Reject,
    /// Square since 2026-07-15: non-overlapping changes merge silently;
    /// VERSION_MISMATCH only when both sides changed the same field.
    Merge,
}

#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Every Nth request answers 429.
    pub rate_limit_every: Option<u64>,
    /// The next N requests answer 429.
    pub rate_limit_next: u32,
    /// The next N requests answer 503.
    pub server_error_next: u32,
    /// From request number N on (counting from 1), drop `drop_count`
    /// connections without answering.
    pub drop_from: Option<u64>,
    pub drop_count: u64,
    /// Refresh tokens are refused (a dead connection).
    pub reject_refresh: bool,
    /// After a batch-retrieve returns one of these ids, a staff member edits
    /// it (the name changes) before the app can write. Counts down per id.
    pub staff_edit_after_retrieve: HashMap<String, u32>,
    pub conflict_mode: ConflictMode,
    /// When a write to one of these ids is applied, Square also changes
    /// another field (the mock's stand-in for a write gone wrong).
    pub damage_on_write: HashSet<String>,
    /// Square's reported bug: writing back seller-defined `Square:` keys
    /// fails with NOT_FOUND.
    pub reject_square_keys: bool,
    /// Creating another seller-visible definition fails (10 already exist).
    pub definition_limit_reached: bool,
    /// A simulated connection: every answer waits this long, plus its size
    /// at this many bytes per second (0 = unlimited).
    pub latency: std::time::Duration,
    /// Search answers carry no `latest_time` (it is a beta field).
    pub omit_latest_time: bool,
    pub bytes_per_second: u64,
}

#[derive(Clone, Debug)]
pub struct Logged {
    pub n: u64,
    pub method: String,
    pub path: String,
    pub square_version: Option<String>,
    pub authorized: bool,
    pub body: Option<Value>,
    pub status: u16,
}

struct Code {
    challenge: String,
    redirect_uri: String,
    scopes: Vec<String>,
    used: bool,
}

#[derive(Clone)]
struct Access {
    scopes: Vec<String>,
    expires_at: DateTime<Utc>,
}

pub struct State {
    pub app_id: String,
    /// The app's own label key, for checking its writes.
    pub label_key: String,
    pub merchant: Value,
    pub locations: Vec<Value>,
    /// Live top-level catalog objects; items hold their variations.
    pub objects: BTreeMap<String, Value>,
    /// Deleted objects as Square returns them with include_deleted_objects.
    pub tombstones: BTreeMap<String, Value>,
    /// Every version of every object (items, variations, definitions).
    history: HashMap<String, HashMap<i64, Value>>,
    parent: HashMap<String, String>,
    pub orders: Vec<Value>,
    pub counts: Vec<Value>,
    pub faults: Faults,
    pub log: Vec<Logged>,
    pub write_violations: Vec<String>,
    /// Writes that reached the catalog (objects written, by id).
    pub writes: Vec<String>,
    pub expect_version: Option<String>,
    /// A fixed clock for the mock's timestamps; real time when `None`.
    pub clock: Option<DateTime<Utc>>,
    last_stamp: DateTime<Utc>,
    version: i64,
    requests: u64,
    codes: HashMap<String, Code>,
    access: HashMap<String, Access>,
    refresh: HashMap<String, (Vec<String>, bool)>,
    idempotency: HashMap<String, (Value, HttpResponseCopy)>,
    next_id: u64,
}

#[derive(Clone)]
struct HttpResponseCopy {
    status: u16,
    body: Value,
}

pub const MOCK_APP_ID: &str = "sq0idp-mock-idle-stock-finder";

impl State {
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.unwrap_or_else(Utc::now)
    }

    /// Strictly increasing timestamps, even within one millisecond.
    fn stamp_time(&mut self) -> DateTime<Utc> {
        let mut t = self.now();
        if t <= self.last_stamp {
            t = self.last_stamp + Duration::milliseconds(1);
        }
        self.last_stamp = t;
        t
    }

    fn next_version(&mut self) -> i64 {
        self.version += 1;
        self.version
    }

    /// Issue tokens directly, as if the seller had connected.
    pub fn issue_tokens(&mut self, scopes: &[&str]) -> (String, String) {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        self.issue(scopes)
    }

    fn issue(&mut self, scopes: Vec<String>) -> (String, String) {
        self.next_id += 1;
        let access = format!("EAAAmock-access-{}-{}", self.next_id, random_hex());
        let refresh = format!("EQAAmock-refresh-{}-{}", self.next_id, random_hex());
        let expires_at = self.now() + Duration::days(30);
        self.access.insert(access.clone(), Access { scopes: scopes.clone(), expires_at });
        self.refresh.insert(refresh.clone(), (scopes, false));
        (access, refresh)
    }

    /// Every access token stops working (as if 30 days passed).
    pub fn expire_access_tokens(&mut self) {
        let past = self.now() - Duration::seconds(1);
        for a in self.access.values_mut() {
            a.expires_at = past;
        }
    }

    pub fn object(&self, id: &str) -> Option<Value> {
        if let Some(o) = self.objects.get(id) {
            return Some(o.clone());
        }
        let item = self.objects.get(self.parent.get(id)?)?;
        variations(item).iter().find(|v| v["id"] == id).cloned()
    }

    /// The app's label on an object, if any.
    pub fn label_of(&self, id: &str) -> Option<String> {
        self.object(id)?
            .get("custom_attribute_values")?
            .get(&self.label_key)?
            .get("string_value")?
            .as_str()
            .map(str::to_string)
    }

    pub fn live_items(&self) -> impl Iterator<Item = &Value> {
        self.objects.values().filter(|o| o["type"] == "ITEM")
    }

    /// A staff member edits an object in the Dashboard.
    pub fn staff_edit(&mut self, id: &str, edit: impl FnOnce(&mut Value)) {
        let Some(top_id) =
            self.objects.contains_key(id).then(|| id.to_string()).or_else(|| self.parent.get(id).cloned())
        else {
            return;
        };
        let mut item = self.objects[&top_id].clone();
        if top_id == id {
            edit(&mut item);
        } else if let Some(vars) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut)
            && let Some(v) = vars.iter_mut().find(|v| v["id"] == id)
        {
            edit(v);
        }
        self.store(item);
    }

    /// Adds or replaces a top-level object (stamping it), outside any request.
    pub fn put_object(&mut self, object: Value) {
        self.store(object);
    }

    /// Deletes a catalog object, as the seller would in the Dashboard.
    pub fn delete_object(&mut self, id: &str) {
        self.delete(id);
    }

    /// Adds orders (e.g. sales made since the last check).
    pub fn add_orders(&mut self, orders: impl IntoIterator<Item = Value>) {
        self.orders.extend(orders);
    }

    /// A snapshot of the catalog: every live object, versions and
    /// timestamps removed, for comparing before and after.
    pub fn catalog_snapshot(&self) -> BTreeMap<String, Value> {
        self.objects
            .iter()
            .filter(|(_, o)| o["type"] != "CUSTOM_ATTRIBUTE_DEFINITION")
            .map(|(id, o)| (id.clone(), unversioned(o)))
            .collect()
    }

    fn store(&mut self, mut object: Value) -> Value {
        let version = self.next_version();
        let at = self.stamp_time().to_rfc3339_opts(SecondsFormat::Millis, true);
        let id = object["id"].as_str().unwrap_or_default().to_string();
        object["version"] = json!(version);
        object["updated_at"] = json!(at);
        object["is_deleted"] = json!(false);
        if object["type"] == "ITEM" {
            let mut kept = HashSet::new();
            if let Some(vars) = object.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
                for v in vars.iter_mut() {
                    v["version"] = json!(version);
                    v["updated_at"] = json!(at);
                    v["is_deleted"] = json!(false);
                    if let Some(data) = v.get_mut("item_variation_data").and_then(Value::as_object_mut) {
                        data.insert("item_id".into(), json!(id));
                    }
                    let vid = v["id"].as_str().unwrap_or_default().to_string();
                    kept.insert(vid.clone());
                    self.parent.insert(vid.clone(), id.clone());
                    self.history.entry(vid).or_default().insert(version, v.clone());
                }
            }
            // Full replacement: variations left out are deleted.
            if let Some(old) = self.objects.get(&id) {
                for v in variations(old) {
                    let vid = v["id"].as_str().unwrap_or_default().to_string();
                    if !kept.contains(&vid) {
                        let mut gone = v.clone();
                        gone["is_deleted"] = json!(true);
                        gone["updated_at"] = json!(at);
                        gone["version"] = json!(version);
                        self.parent.remove(&vid);
                        self.tombstones.insert(vid, gone);
                    }
                }
            }
        }
        self.history.entry(id.clone()).or_default().insert(version, object.clone());
        self.objects.insert(id, object.clone());
        object
    }

    fn delete(&mut self, id: &str) -> Vec<String> {
        let at = self.stamp_time().to_rfc3339_opts(SecondsFormat::Millis, true);
        let version = self.next_version();
        let mut deleted = Vec::new();
        if let Some(obj) = self.objects.remove(id) {
            for v in variations(&obj) {
                let vid = v["id"].as_str().unwrap_or_default().to_string();
                let mut gone = v.clone();
                gone["is_deleted"] = json!(true);
                gone["updated_at"] = json!(at);
                gone["version"] = json!(version);
                self.parent.remove(&vid);
                self.tombstones.insert(vid.clone(), gone);
                deleted.push(vid);
            }
            if obj["type"] == "CUSTOM_ATTRIBUTE_DEFINITION" {
                let key = obj
                    .pointer("/custom_attribute_definition_data/key")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let key = key.to_string();
                self.drop_values(&key);
            }
            let mut gone = obj;
            gone["is_deleted"] = json!(true);
            gone["updated_at"] = json!(at);
            gone["version"] = json!(version);
            self.tombstones.insert(id.to_string(), gone);
            deleted.push(id.to_string());
        } else if let Some(item_id) = self.parent.get(id).cloned() {
            let mut item = self.objects[&item_id].clone();
            if let Some(vars) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
                vars.retain(|v| v["id"] != id);
            }
            self.store(item);
            deleted.push(id.to_string());
        }
        deleted
    }

    /// Deleting a definition removes its values everywhere.
    fn drop_values(&mut self, key: &str) {
        let ids: Vec<String> = self.objects.keys().cloned().collect();
        for id in ids {
            let mut obj = self.objects[&id].clone();
            let mut changed = remove_value(&mut obj, key);
            if let Some(vars) = obj.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
                for v in vars.iter_mut() {
                    changed |= remove_value(v, key);
                }
            }
            if changed {
                self.objects.insert(id, obj);
            }
        }
    }
}

fn remove_value(obj: &mut Value, key: &str) -> bool {
    let Some(map) = obj.get_mut("custom_attribute_values").and_then(Value::as_object_mut) else {
        return false;
    };
    let removed = map.shift_remove(key).is_some();
    if map.is_empty() {
        obj.as_object_mut().unwrap().shift_remove("custom_attribute_values");
    }
    removed
}

fn variations(item: &Value) -> Vec<Value> {
    item.pointer("/item_data/variations").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The object without version, updated_at and is_deleted, at both levels.
pub fn unversioned(o: &Value) -> Value {
    let mut o = o.clone();
    if let Some(m) = o.as_object_mut() {
        for k in ["version", "updated_at", "is_deleted", "created_at"] {
            m.shift_remove(k);
        }
    }
    if let Some(vars) = o.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            if let Some(m) = v.as_object_mut() {
                for k in ["version", "updated_at", "is_deleted", "created_at"] {
                    m.shift_remove(k);
                }
            }
        }
    }
    o
}

fn random_hex() -> String {
    let mut h = Sha256::new();
    h.update(format!("{:?}{:?}", std::time::SystemTime::now(), std::thread::current().id()).as_bytes());
    h.update(std::process::id().to_le_bytes());
    let digest = h.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub struct MockSquare {
    state: Arc<Mutex<State>>,
    server: Server,
}

impl MockSquare {
    pub fn start(seed: Seed, label_key: &str) -> MockSquare {
        let app_id = if seed.app_id.is_empty() { MOCK_APP_ID.to_string() } else { seed.app_id.clone() };
        let mut state = State {
            app_id,
            label_key: label_key.to_string(),
            merchant: seed.merchant,
            locations: seed.locations,
            objects: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            history: HashMap::new(),
            parent: HashMap::new(),
            orders: seed.orders,
            counts: seed.counts,
            faults: Faults::default(),
            log: Vec::new(),
            write_violations: Vec::new(),
            writes: Vec::new(),
            expect_version: None,
            clock: None,
            last_stamp: DateTime::<Utc>::MIN_UTC,
            version: 1_700_000_000_000,
            requests: 0,
            codes: HashMap::new(),
            access: HashMap::new(),
            refresh: HashMap::new(),
            idempotency: HashMap::new(),
            next_id: 0,
        };
        for object in seed.catalog {
            state.store(object);
        }
        let state = Arc::new(Mutex::new(state));
        let handler_state = state.clone();
        let server = Server::start(Arc::new(move |req| handle(&handler_state, req)))
            .expect("mock server binds loopback");
        MockSquare { state, server }
    }

    pub fn addr(&self) -> SocketAddr {
        self.server.addr
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn error(status: u16, category: &str, code: &str, detail: &str) -> HttpResponse {
    HttpResponse::json(
        status,
        &json!({ "errors": [{ "category": category, "code": code, "detail": detail }] }),
    )
}

fn bad_request(code: &str, detail: &str) -> HttpResponse {
    error(400, "INVALID_REQUEST_ERROR", code, detail)
}

fn handle(state: &Arc<Mutex<State>>, req: HttpRequest) -> Reply {
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.requests += 1;
    let n = s.requests;
    let body: Option<Value> = if req.body.is_empty() { None } else { serde_json::from_slice(&req.body).ok() };
    let path = req.path().to_string();
    let logged_version = req.header("Square-Version").map(str::to_string);
    let authorized = req.header("Authorization").is_some();

    // Faults that act before anything else.
    if let Some(from) = s.faults.drop_from
        && n >= from
        && n < from + s.faults.drop_count
    {
        s.log.push(Logged {
            n,
            method: req.method.clone(),
            path,
            square_version: logged_version,
            authorized,
            body,
            status: 0,
        });
        return Reply::Drop;
    }
    let reply = if s.faults.rate_limit_next > 0 {
        s.faults.rate_limit_next -= 1;
        error(429, "RATE_LIMIT_ERROR", "RATE_LIMITED", "too many requests")
    } else if s.faults.rate_limit_every.is_some_and(|k| k > 0 && n.is_multiple_of(k)) {
        error(429, "RATE_LIMIT_ERROR", "RATE_LIMITED", "too many requests")
    } else if s.faults.server_error_next > 0 {
        s.faults.server_error_next -= 1;
        error(503, "API_ERROR", "SERVICE_UNAVAILABLE", "try again")
    } else if s.expect_version.is_some() && path.starts_with("/v2/") && s.expect_version != logged_version {
        bad_request("INVALID_VALUE", "unexpected Square-Version")
    } else {
        route(&mut s, &req, body.as_ref())
    };
    let status = reply.status;
    s.log.push(Logged {
        n,
        method: req.method.clone(),
        path,
        square_version: logged_version,
        authorized,
        body,
        status,
    });
    let mut wait = s.faults.latency;
    if s.faults.bytes_per_second > 0 {
        wait +=
            std::time::Duration::from_secs_f64(reply.body.len() as f64 / s.faults.bytes_per_second as f64);
    }
    drop(s);
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    Reply::Respond(reply)
}

fn route(s: &mut State, req: &HttpRequest, body: Option<&Value>) -> HttpResponse {
    let empty = json!({});
    let body = body.unwrap_or(&empty);
    let path = req.path();
    match (req.method.as_str(), path) {
        ("GET", "/oauth2/authorize") => authorize(s, req),
        ("POST", "/oauth2/token") => token(s, body),
        _ => {
            let scopes = match authenticate(s, req) {
                Ok(scopes) => scopes,
                Err(response) => return response,
            };
            let need = |scope: &str| -> Result<(), HttpResponse> {
                if scopes.iter().any(|s| s == scope) {
                    Ok(())
                } else {
                    Err(error(403, "AUTHENTICATION_ERROR", "INSUFFICIENT_SCOPES", &format!("needs {scope}")))
                }
            };
            let result = match (req.method.as_str(), path) {
                ("GET", "/v2/merchants/me") => need("MERCHANT_PROFILE_READ")
                    .map(|_| HttpResponse::json(200, &json!({ "merchant": s.merchant }))),
                ("GET", "/v2/locations") => need("MERCHANT_PROFILE_READ")
                    .map(|_| HttpResponse::json(200, &json!({ "locations": s.locations }))),
                ("POST", "/v2/catalog/search") => need("ITEMS_READ").map(|_| catalog_search(s, body)),
                ("POST", "/v2/catalog/batch-retrieve") => need("ITEMS_READ").map(|_| batch_retrieve(s, body)),
                ("POST", "/v2/catalog/batch-upsert") => {
                    need("ITEMS_WRITE").map(|_| idempotent(s, body, batch_upsert))
                }
                ("POST", "/v2/catalog/object") => {
                    need("ITEMS_WRITE").map(|_| idempotent(s, body, upsert_one))
                }
                ("DELETE", p) if p.starts_with("/v2/catalog/object/") => need("ITEMS_WRITE").map(|_| {
                    let id = p.trim_start_matches("/v2/catalog/object/").to_string();
                    let deleted = s.delete(&id);
                    let at = s.now().to_rfc3339_opts(SecondsFormat::Millis, true);
                    HttpResponse::json(200, &json!({ "deleted_object_ids": deleted, "deleted_at": at }))
                }),
                ("POST", "/v2/orders/search") => need("ORDERS_READ").map(|_| search_orders(s, body)),
                ("POST", "/v2/inventory/counts/batch-retrieve") => {
                    need("INVENTORY_READ").map(|_| inventory_counts(s, body))
                }
                _ => Ok(error(404, "INVALID_REQUEST_ERROR", "NOT_FOUND", "no such endpoint")),
            };
            result.unwrap_or_else(|e| e)
        }
    }
}

fn authenticate(s: &State, req: &HttpRequest) -> Result<Vec<String>, HttpResponse> {
    let token = req
        .header("Authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| error(401, "AUTHENTICATION_ERROR", "UNAUTHORIZED", "no access token"))?;
    let access = s
        .access
        .get(token)
        .ok_or_else(|| error(401, "AUTHENTICATION_ERROR", "UNAUTHORIZED", "unknown token"))?;
    if access.expires_at <= s.now() {
        return Err(error(
            401,
            "AUTHENTICATION_ERROR",
            "ACCESS_TOKEN_EXPIRED",
            "the access token has expired",
        ));
    }
    Ok(access.scopes.clone())
}

fn authorize(s: &mut State, req: &HttpRequest) -> HttpResponse {
    let q: HashMap<String, String> = req.query().into_iter().collect();
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    if get("client_id") != s.app_id {
        return bad_request("INVALID_VALUE", "unknown client_id");
    }
    let redirect = get("redirect_uri");
    if !redirect.starts_with("http://localhost:") {
        return bad_request("INVALID_VALUE", "redirect_uri not registered");
    }
    if get("code_challenge").is_empty() {
        return bad_request("MISSING_REQUIRED_PARAMETER", "code_challenge");
    }
    s.next_id += 1;
    let code = format!("sq0cgp-mock-{}-{}", s.next_id, random_hex());
    let scopes = get("scope").split([' ', '+']).filter(|x| !x.is_empty()).map(str::to_string).collect();
    s.codes.insert(
        code.clone(),
        Code { challenge: get("code_challenge"), redirect_uri: redirect.clone(), scopes, used: false },
    );
    let mut location = url::Url::parse(&redirect).unwrap();
    location.query_pairs_mut().append_pair("code", &code).append_pair("state", &get("state"));
    HttpResponse { status: 302, headers: vec![("Location".into(), location.to_string())], body: Vec::new() }
}

fn token(s: &mut State, body: &Value) -> HttpResponse {
    let get = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    if get("client_id") != s.app_id {
        return HttpResponse::json(
            401,
            &json!({ "error": "invalid_client", "error_description": "unknown client" }),
        );
    }
    if body.get("client_secret").is_some() {
        return HttpResponse::json(
            400,
            &json!({ "error": "invalid_request", "error_description": "PKCE clients send no secret" }),
        );
    }
    let scopes = match get("grant_type").as_str() {
        "authorization_code" => {
            let Some(code) = s.codes.get_mut(&get("code")) else {
                return HttpResponse::json(
                    401,
                    &json!({ "error": "invalid_grant", "error_description": "unknown code" }),
                );
            };
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(get("code_verifier").as_bytes()));
            if code.used || code.challenge != challenge || code.redirect_uri != get("redirect_uri") {
                return HttpResponse::json(
                    401,
                    &json!({ "error": "invalid_grant", "error_description": "code, verifier or redirect mismatch" }),
                );
            }
            code.used = true;
            code.scopes.clone()
        }
        "refresh_token" => {
            let rejected = s.faults.reject_refresh;
            match s.refresh.get_mut(&get("refresh_token")) {
                Some((scopes, used)) if !*used && !rejected => {
                    *used = true;
                    scopes.clone()
                }
                _ => {
                    return HttpResponse::json(
                        401,
                        &json!({ "error": "invalid_grant", "error_description": "refresh token invalid" }),
                    );
                }
            }
        }
        _ => return HttpResponse::json(400, &json!({ "error": "unsupported_grant_type" })),
    };
    let (access, refresh) = s.issue(scopes);
    let now = s.now();
    HttpResponse::json(
        200,
        &json!({
            "access_token": access,
            "token_type": "bearer",
            "expires_at": (now + Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "merchant_id": s.merchant["id"],
            "refresh_token": refresh,
            "refresh_token_expires_at": (now + Duration::days(90)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "short_lived": false,
        }),
    )
}

fn decode_cursor(body: &Value) -> usize {
    body.get("cursor")
        .and_then(Value::as_str)
        .and_then(|c| URL_SAFE_NO_PAD.decode(c).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn page<T: Clone>(items: &[T], start: usize, limit: usize) -> (Vec<T>, Option<String>) {
    let end = (start + limit).min(items.len());
    let slice = items.get(start..end).map(<[T]>::to_vec).unwrap_or_default();
    let cursor = (end < items.len()).then(|| URL_SAFE_NO_PAD.encode(end.to_string()));
    (slice, cursor)
}

fn parse_time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc))
}

fn catalog_search(s: &State, body: &Value) -> HttpResponse {
    let types: Vec<String> = body
        .get("object_types")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_else(|| vec!["ITEM".into(), "CATEGORY".into()]);
    let include_deleted = body.get("include_deleted_objects").and_then(Value::as_bool).unwrap_or(false);
    let begin = body.get("begin_time").and_then(parse_time);
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 1000) as usize;
    // References, cloned only for the page sent (20,000-item runs).
    let nested: Vec<Value> = if types.iter().any(|t| t == "ITEM_VARIATION") {
        s.objects.values().flat_map(variations).collect()
    } else {
        Vec::new()
    };
    let mut found: Vec<&Value> = s
        .objects
        .values()
        .filter(|obj| types.iter().any(|t| obj["type"] == t.as_str()))
        .chain(nested.iter())
        .collect();
    if include_deleted {
        found.extend(s.tombstones.values().filter(|o| types.iter().any(|t| o["type"] == t.as_str())));
    }
    if let Some(begin) = begin {
        found.retain(|o| parse_time(&o["updated_at"]).is_some_and(|t| t > begin));
    }
    found.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    // Square: "When the associated product catalog was last updated."
    let latest =
        s.objects.values().chain(s.tombstones.values()).filter_map(|o| parse_time(&o["updated_at"])).max();
    let (objects, cursor) = page(&found, decode_cursor(body), limit);
    let mut out = json!({ "objects": objects });
    if let Some(c) = cursor {
        out["cursor"] = json!(c);
    }
    if let Some(t) = latest.filter(|_| !s.faults.omit_latest_time) {
        out["latest_time"] = json!(t.to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    HttpResponse::json(200, &out)
}

fn batch_retrieve(s: &mut State, body: &Value) -> HttpResponse {
    let ids: Vec<String> = body
        .get("object_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let objects: Vec<Value> = ids.iter().filter_map(|id| s.object(id)).collect();
    let response = HttpResponse::json(200, &json!({ "objects": objects }));
    for id in &ids {
        let edit =
            s.faults.staff_edit_after_retrieve.get_mut(id).filter(|n| **n > 0).map(|n| *n -= 1).is_some();
        if edit {
            s.staff_edit(id, |o| {
                if let Some(name) = o.pointer_mut("/item_data/name") {
                    *name = json!(format!("{} (edited)", name.as_str().unwrap_or("")));
                } else if let Some(name) = o.pointer_mut("/item_variation_data/name") {
                    *name = json!(format!("{} (edited)", name.as_str().unwrap_or("")));
                }
            });
        }
    }
    response
}

fn idempotent(s: &mut State, body: &Value, f: fn(&mut State, &Value) -> HttpResponse) -> HttpResponse {
    let key = body.get("idempotency_key").and_then(Value::as_str).unwrap_or("").to_string();
    if key.is_empty() {
        return bad_request("MISSING_REQUIRED_PARAMETER", "idempotency_key");
    }
    if let Some((earlier, response)) = s.idempotency.get(&key) {
        if earlier == body {
            return HttpResponse::json(response.status, &response.body);
        }
        return bad_request("IDEMPOTENCY_KEY_REUSED", "key used with a different request");
    }
    let response = f(s, body);
    let copy = HttpResponseCopy {
        status: response.status,
        body: serde_json::from_slice(&response.body).unwrap_or(Value::Null),
    };
    s.idempotency.insert(key, (body.clone(), copy));
    response
}

fn upsert_one(s: &mut State, body: &Value) -> HttpResponse {
    let Some(object) = body.get("object") else { return bad_request("MISSING_REQUIRED_PARAMETER", "object") };
    match apply_batch(s, std::slice::from_ref(object)) {
        Ok((mut objects, mappings)) => {
            HttpResponse::json(200, &json!({ "catalog_object": objects.pop(), "id_mappings": mappings }))
        }
        Err(e) => e,
    }
}

fn batch_upsert(s: &mut State, body: &Value) -> HttpResponse {
    let batches = body.get("batches").and_then(Value::as_array).cloned().unwrap_or_default();
    let total: usize = batches.iter().map(|b| b["objects"].as_array().map_or(0, Vec::len)).sum();
    if total > 10_000 || batches.iter().any(|b| b["objects"].as_array().map_or(0, Vec::len) > 1000) {
        return bad_request("INVALID_VALUE", "too many objects");
    }
    let mut all = Vec::new();
    let mut mappings = Vec::new();
    for batch in batches {
        let objects = batch["objects"].as_array().cloned().unwrap_or_default();
        // Each batch is all-or-nothing.
        match apply_batch(s, &objects) {
            Ok((objs, maps)) => {
                all.extend(objs);
                mappings.extend(maps);
            }
            Err(e) => return e,
        }
    }
    let at = s.now().to_rfc3339_opts(SecondsFormat::Millis, true);
    HttpResponse::json(200, &json!({ "objects": all, "id_mappings": mappings, "updated_at": at }))
}

/// Checks every object in a batch, then applies them all, or none.
fn apply_batch(s: &mut State, objects: &[Value]) -> Result<(Vec<Value>, Vec<Value>), HttpResponse> {
    let mut prepared = Vec::new();
    for object in objects {
        prepared.push(prepare(s, object)?);
    }
    let mut out = Vec::new();
    let mut mappings = Vec::new();
    for (mut object, temp_id) in prepared {
        let id = object["id"].as_str().unwrap_or_default().to_string();
        if let Some(temp) = temp_id {
            mappings.push(json!({ "client_object_id": temp, "object_id": id }));
        }
        if object["type"] == "ITEM_VARIATION" {
            // A variation written alone: it replaces the nested one and both
            // the variation and its item take the new version.
            let item_id = object
                .pointer("/item_variation_data/item_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut item = s.objects[&item_id].clone();
            if let Some(vars) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut)
                && let Some(slot) = vars.iter_mut().find(|v| v["id"] == id.as_str())
            {
                *slot = object.clone();
            }
            if s.faults.damage_on_write.contains(&id) {
                damage(&mut item);
            }
            let stored = s.store(item);
            s.writes.push(id.clone());
            out.push(variations(&stored).into_iter().find(|v| v["id"] == id.as_str()).unwrap_or(Value::Null));
            continue;
        }
        if s.faults.damage_on_write.contains(&id) {
            damage(&mut object);
        }
        s.writes.push(id.clone());
        out.push(s.store(object));
    }
    Ok((out, mappings))
}

fn damage(object: &mut Value) {
    if let Some(data) = object.get_mut("item_data").and_then(Value::as_object_mut) {
        data.insert("description".into(), json!("changed by the write"));
    }
}

/// Validates one incoming object and resolves version conflicts. Returns
/// the object to store and its client id if it was new.
fn prepare(s: &mut State, incoming: &Value) -> Result<(Value, Option<String>), HttpResponse> {
    let mut object = incoming.clone();
    let id = object["id"].as_str().unwrap_or_default().to_string();
    let kind = object["type"].as_str().unwrap_or_default().to_string();
    if id.is_empty() || kind.is_empty() {
        return Err(bad_request("MISSING_REQUIRED_PARAMETER", "id and type"));
    }
    if let Some(temp) = id.strip_prefix('#') {
        let _ = temp;
        if kind == "CUSTOM_ATTRIBUTE_DEFINITION" {
            check_definition(s, &object)?;
            s.next_id += 1;
            let real = format!("DEF{:08}MOCK", s.next_id);
            object["id"] = json!(real);
            object["custom_attribute_definition_data"]["source_application"] =
                json!({ "application_id": s.app_id, "name": "Idle Stock Finder (mock)" });
            return Ok((object, Some(id)));
        }
        s.next_id += 1;
        object["id"] = json!(format!("NEW{:08}MOCK", s.next_id));
        return Ok((object, Some(id)));
    }
    let current =
        s.object(&id).ok_or_else(|| error(404, "INVALID_REQUEST_ERROR", "NOT_FOUND", "object not found"))?;
    let their_version = current["version"].as_i64();
    let my_version = object["version"].as_i64();
    check_values(s, &object, &current)?;
    if kind == "ITEM" || kind == "ITEM_VARIATION" {
        audit(s, &object);
    }
    if my_version != their_version {
        match s.faults.conflict_mode {
            ConflictMode::Reject => {
                return Err(bad_request(
                    "VERSION_MISMATCH",
                    "Object version does not match latest database version.",
                ));
            }
            ConflictMode::Merge => {
                let base = my_version
                    .and_then(|v| s.history.get(&id).and_then(|h| h.get(&v)).cloned())
                    .ok_or_else(|| bad_request("VERSION_MISMATCH", "unknown base version"))?;
                let merged = merge(&unversioned(&base), &unversioned(&current), &unversioned(&object))
                    .ok_or_else(|| {
                        bad_request(
                            "VERSION_MISMATCH",
                            "Object version does not match latest database version.",
                        )
                    })?;
                object = merged;
            }
        }
    } else if kind == "ITEM" {
        // Nested variations must carry their current versions too.
        let stored: HashMap<String, Option<i64>> = variations(&current)
            .iter()
            .map(|v| (v["id"].as_str().unwrap_or_default().to_string(), v["version"].as_i64()))
            .collect();
        for v in variations(&object) {
            let vid = v["id"].as_str().unwrap_or_default();
            if let Some(expected) = stored.get(vid)
                && v["version"].as_i64() != *expected
            {
                return Err(bad_request(
                    "VERSION_MISMATCH",
                    "Object version does not match latest database version.",
                ));
            }
        }
    }
    Ok((object, None))
}

fn check_definition(s: &State, object: &Value) -> Result<(), HttpResponse> {
    let data = &object["custom_attribute_definition_data"];
    let key = data["key"].as_str().unwrap_or("");
    if key.is_empty()
        || key.len() > 60
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(bad_request("INVALID_VALUE", "custom attribute key"));
    }
    let visible = data["seller_visibility"].as_str().unwrap_or("SELLER_VISIBILITY_READ_WRITE_VALUES");
    let existing_visible = s
        .objects
        .values()
        .filter(|o| o["type"] == "CUSTOM_ATTRIBUTE_DEFINITION")
        .filter(|o| {
            o.pointer("/custom_attribute_definition_data/seller_visibility").and_then(Value::as_str)
                != Some("SELLER_VISIBILITY_HIDDEN")
        })
        .count();
    if visible == "SELLER_VISIBILITY_READ_WRITE_VALUES"
        && (existing_visible >= 10 || s.faults.definition_limit_reached)
    {
        return Err(bad_request(
            "INVALID_VALUE",
            "This seller already has the maximum of 10 seller-visible custom attributes.",
        ));
    }
    if s.objects.values().any(|o| {
        o["type"] == "CUSTOM_ATTRIBUTE_DEFINITION"
            && o.pointer("/custom_attribute_definition_data/key").and_then(Value::as_str) == Some(key)
            && o.pointer("/custom_attribute_definition_data/source_application/application_id")
                .and_then(Value::as_str)
                == Some(s.app_id.as_str())
    }) {
        return Err(bad_request("INVALID_VALUE", "a definition with this key exists"));
    }
    Ok(())
}

/// Custom attribute values must name a definition this app owns, or be
/// another app's or the seller's value sent back as it was.
fn check_values(s: &State, object: &Value, current: &Value) -> Result<(), HttpResponse> {
    let mut targets = vec![(object.clone(), current.clone())];
    let current_vars = variations(current);
    for v in variations(object) {
        let cur = current_vars.iter().find(|c| c["id"] == v["id"]).cloned().unwrap_or(Value::Null);
        targets.push((v, cur));
    }
    let owned: HashSet<String> = s
        .objects
        .values()
        .filter(|o| o["type"] == "CUSTOM_ATTRIBUTE_DEFINITION")
        .filter(|o| {
            o.pointer("/custom_attribute_definition_data/source_application/application_id")
                .and_then(Value::as_str)
                == Some(s.app_id.as_str())
        })
        .filter_map(|o| {
            o.pointer("/custom_attribute_definition_data/key").and_then(Value::as_str).map(str::to_string)
        })
        .collect();
    for (obj, cur) in targets {
        let Some(values) = obj.get("custom_attribute_values").and_then(Value::as_object) else { continue };
        for (key, value) in values {
            if s.faults.reject_square_keys && key.starts_with("Square:") {
                return Err(error(
                    404,
                    "INVALID_REQUEST_ERROR",
                    "NOT_FOUND",
                    &format!("Custom attribute definition with key \"{key}\" not found"),
                ));
            }
            if owned.contains(key) {
                if value.get("string_value").and_then(Value::as_str).is_some_and(|t| t.chars().count() > 255)
                {
                    return Err(bad_request("INVALID_VALUE", "string_value too long"));
                }
                continue;
            }
            if cur
                .pointer(&format!("/custom_attribute_values/{}", key.replace('~', "~0").replace('/', "~1")))
                .is_none()
            {
                return Err(error(
                    404,
                    "INVALID_REQUEST_ERROR",
                    "NOT_FOUND",
                    &format!("Custom attribute definition with key \"{key}\" not found"),
                ));
            }
        }
    }
    Ok(())
}

/// The app's promise: a write differs from the version it fetched only in
/// its own label key. Anything else is recorded as a violation.
fn audit(s: &mut State, incoming: &Value) {
    let id = incoming["id"].as_str().unwrap_or_default().to_string();
    let Some(version) = incoming["version"].as_i64() else {
        s.write_violations.push(format!("{id}: write without a version"));
        return;
    };
    let Some(fetched) = s.history.get(&id).and_then(|h| h.get(&version)).cloned() else {
        s.write_violations.push(format!("{id}: write against unknown version {version}"));
        return;
    };
    let key = s.label_key.clone();
    let mut a = strip_label(incoming, &key);
    let mut b = strip_label(&fetched, &key);
    for v in [&mut a, &mut b] {
        if let Some(m) = v.as_object_mut() {
            m.shift_remove("updated_at");
            m.shift_remove("is_deleted");
        }
    }
    if a != b {
        s.write_violations
            .push(format!("{id}: write changed more than the label\nfetched: {b}\nsent:    {a}"));
    }
}

fn strip_label(object: &Value, key: &str) -> Value {
    let mut o = object.clone();
    remove_value(&mut o, key);
    if let Some(vars) = o.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            remove_value(v, key);
            if let Some(m) = v.as_object_mut() {
                m.shift_remove("updated_at");
                m.shift_remove("is_deleted");
            }
        }
    }
    o
}

/// Three-way merge at field level (objects recurse; arrays are leaves).
/// `None` when both sides changed the same field differently.
fn merge(base: &Value, theirs: &Value, ours: &Value) -> Option<Value> {
    if ours == base {
        return Some(theirs.clone());
    }
    if theirs == base || theirs == ours {
        return Some(ours.clone());
    }
    match (base, theirs, ours) {
        (Value::Object(b), Value::Object(t), Value::Object(o)) => {
            let mut out = Map::new();
            let keys: Vec<&String> = t.keys().chain(o.keys()).chain(b.keys()).collect();
            let mut seen = HashSet::new();
            for k in keys {
                if !seen.insert(k.clone()) {
                    continue;
                }
                let bv = b.get(k).cloned().unwrap_or(Value::Null);
                let tv = t.get(k).cloned().unwrap_or(Value::Null);
                let ov = o.get(k).cloned().unwrap_or(Value::Null);
                let m = merge(&bv, &tv, &ov)?;
                if !m.is_null() {
                    out.insert(k.clone(), m);
                }
            }
            Some(Value::Object(out))
        }
        _ => None,
    }
}

fn search_orders(s: &State, body: &Value) -> HttpResponse {
    let locations: Vec<&str> = body
        .get("location_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if locations.is_empty() || locations.len() > 10 {
        return bad_request("INVALID_VALUE", "location_ids must have 1 to 10 entries");
    }
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(500);
    if limit > 1000 {
        return bad_request("INVALID_VALUE", "limit max is 1000");
    }
    let filter = body.pointer("/query/filter").cloned().unwrap_or(Value::Null);
    let states: Vec<&str> = filter
        .pointer("/state_filter/states")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let dt = filter.get("date_time_filter").and_then(Value::as_object).cloned().unwrap_or_default();
    let (field, range) =
        dt.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap_or(("created_at".into(), Value::Null));
    let sort_field = body.pointer("/query/sort/sort_field").and_then(Value::as_str).unwrap_or("CREATED_AT");
    if !dt.is_empty() && sort_field.to_ascii_lowercase() != field {
        return bad_request("INVALID_VALUE", "sort field must match the date_time_filter field");
    }
    if field == "closed_at" && states.is_empty() {
        return bad_request("INVALID_VALUE", "closed_at needs a state filter");
    }
    let start = range.get("start_at").and_then(parse_time);
    let end = range.get("end_at").and_then(parse_time);
    let found: Vec<&Value> = s
        .orders
        .iter()
        .filter(|o| locations.contains(&o["location_id"].as_str().unwrap_or("")))
        .filter(|o| states.is_empty() || states.contains(&o["state"].as_str().unwrap_or("")))
        .filter(|o| {
            let t = parse_time(&o[field.as_str()]);
            match t {
                Some(t) => start.is_none_or(|s| t >= s) && end.is_none_or(|e| t <= e),
                None => start.is_none() && end.is_none(),
            }
        })
        .collect();
    let asc = body.pointer("/query/sort/sort_order").and_then(Value::as_str) == Some("ASC");
    let sort_key = sort_field.to_ascii_lowercase();
    // Sort keys parsed once, not on every comparison (100,000-order runs).
    type Keyed<'a> = ((Option<DateTime<Utc>>, Option<&'a str>), &'a Value);
    let mut keyed: Vec<Keyed> =
        found.into_iter().map(|o| ((parse_time(&o[sort_key.as_str()]), o["id"].as_str()), o)).collect();
    keyed.sort_by(|a, b| if asc { a.0.cmp(&b.0) } else { b.0.cmp(&a.0) });
    let found: Vec<&Value> = keyed.into_iter().map(|(_, o)| o).collect();
    let (orders, cursor) = page(&found, decode_cursor(body), limit as usize);
    let mut out = json!({ "orders": orders });
    if let Some(c) = cursor {
        out["cursor"] = json!(c);
    }
    HttpResponse::json(200, &out)
}

fn inventory_counts(s: &State, body: &Value) -> HttpResponse {
    let locations: Vec<&str> = body
        .get("location_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let states: Vec<&str> = body
        .get("states")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(1000);
    if !(1..=1000).contains(&limit) {
        return bad_request("INVALID_VALUE", "limit is 1 to 1000");
    }
    let after = body.get("updated_after").and_then(parse_time);
    let found: Vec<&Value> = s
        .counts
        .iter()
        .filter(|c| locations.is_empty() || locations.contains(&c["location_id"].as_str().unwrap_or("")))
        .filter(|c| states.is_empty() || states.contains(&c["state"].as_str().unwrap_or("")))
        .filter(|c| after.is_none_or(|a| parse_time(&c["calculated_at"]).is_some_and(|t| t > a)))
        .collect();
    let (counts, cursor) = page(&found, decode_cursor(body), limit as usize);
    let mut out = json!({ "counts": counts });
    if let Some(c) = cursor {
        out["cursor"] = json!(c);
    }
    HttpResponse::json(200, &out)
}
