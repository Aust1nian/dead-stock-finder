//! What the page can ask of the app. The page has no file, network, shell
//! or opener access of its own: everything goes through these commands.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use dsf_check::{Config, Deps, Outcome, Status};
use dsf_notify::SystemNotifier;
use dsf_schedule::{Registration, Trigger};
use dsf_square::oauth::{self, CallbackListener, LABEL_SCOPE, Pkce};
use dsf_square::{Environment, HttpClient, RetryPolicy, TokenStore};
use dsf_sync::{Cache, Progress, Report, ReportQuery, Settings};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::config;

pub struct AppState {
    pub square: config::Square,
    pub data_dir: PathBuf,
    pub cancel_connect: Arc<AtomicBool>,
    pub busy: Arc<Mutex<()>>,
    /// UI tests and local development: a mock Square on loopback.
    pub mock: Option<Arc<dsf_mock::MockSquare>>,
    pub mock_store: Option<Arc<dsf_square::MemoryStore>>,
}

impl AppState {
    fn cache(&self) -> Result<Cache, String> {
        Cache::open(&dsf_check::paths::cache_file(&self.data_dir)).map_err(|e| e.to_string())
    }

    fn store(&self, interactive: bool) -> Box<dyn TokenStore> {
        match &self.mock_store {
            Some(store) => Box::new(SharedStore(store.clone())),
            None => Box::new(config::keychain(&self.square.environment, interactive)),
        }
    }

    fn config(&self) -> Config {
        Config {
            app_id: self.square.app_id.clone(),
            environment: self.square.environment.clone(),
            data_dir: self.data_dir.clone(),
        }
    }
}

struct SharedStore(Arc<dsf_square::MemoryStore>);

impl TokenStore for SharedStore {
    fn load(&self) -> Result<Option<dsf_square::Tokens>, dsf_square::StoreError> {
        self.0.load()
    }
    fn save(&self, t: &dsf_square::Tokens) -> Result<(), dsf_square::StoreError> {
        self.0.save(t)
    }
    fn delete(&self) -> Result<(), dsf_square::StoreError> {
        self.0.delete()
    }
}

#[derive(Serialize)]
pub struct LocationView {
    id: String,
    name: String,
    active: bool,
}

#[derive(Serialize)]
pub struct AppView {
    /// This build knows its Square application id.
    configured: bool,
    connected: bool,
    has_write_scope: bool,
    business_name: Option<String>,
    settings: Settings,
    locations: Vec<LocationView>,
    status: Status,
    next_check: Option<DateTime<Utc>>,
    registration: Registration,
    version: String,
    environment: String,
}

fn next_check(settings: &Settings, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if !settings.background_on || !settings.setup_done {
        return None;
    }
    let last = dsf_schedule::last_occurrence(&Local, now, settings.check_time.0, settings.check_time.1);
    // The next occurrence is a day after the last one (give or take a DST hour).
    let next = dsf_schedule::last_occurrence(
        &Local,
        last + chrono::Duration::hours(25),
        settings.check_time.0,
        settings.check_time.1,
    );
    Some(if next > now { next } else { last + chrono::Duration::days(1) })
}

fn view(state: &AppState) -> Result<AppView, String> {
    let cache = state.cache()?;
    let tokens = state.store(true).load().ok().flatten();
    let settings = cache.settings().map_err(|e| e.to_string())?;
    let locations = cache
        .locations()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| LocationView { active: l.active(), id: l.id, name: l.name })
        .collect();
    let status = dsf_check::status(&cache);
    Ok(AppView {
        configured: !state.square.app_id.is_empty(),
        connected: tokens.is_some(),
        has_write_scope: tokens.as_ref().is_some_and(|t| t.has_scope(LABEL_SCOPE)),
        business_name: cache.meta("business_name").ok().flatten(),
        next_check: next_check(&settings, Utc::now()),
        settings,
        locations,
        status,
        registration: if state.mock.is_some() {
            Registration { method: dsf_schedule::Method::None, detail: "test mode".into() }
        } else {
            dsf_schedule::status()
        },
        version: env!("CARGO_PKG_VERSION").into(),
        environment: state.square.environment.name().into(),
    })
}

#[tauri::command]
pub fn app_view(state: State<'_, AppState>) -> Result<AppView, String> {
    view(&state)
}

/// Opens Square's page in the owner's browser. The browser, not this app,
/// talks to Square there.
fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("/usr/bin/open").arg(url).status().map(|_| ());
    #[cfg(windows)]
    let result = {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (verb, target) = (wide("open"), wide(url));
        let h = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if (h as isize) > 32 { Ok(()) } else { Err(std::io::Error::other("ShellExecute failed")) }
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let result = std::process::Command::new("xdg-open").arg(url).status().map(|_| ());
    result.map_err(|e| format!("couldn't open the browser: {e}"))
}

#[derive(Clone, Serialize)]
struct Connected {
    business_name: String,
}

fn focus(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Connect Square: Square's own page in the browser, PKCE, a loopback
/// listener for the reply, tokens to the keychain before anything else.
#[tauri::command]
pub fn connect(app: AppHandle, state: State<'_, AppState>, labels_on: bool) -> Result<(), String> {
    if state.square.app_id.is_empty() {
        return Err("This build isn't linked to a Square application yet.".into());
    }
    let pkce = Pkce::new();
    let listener = CallbackListener::bind().map_err(|e| e.to_string())?;
    let redirect = listener.redirect_uri();
    let scopes = oauth::scopes(labels_on);
    let url =
        oauth::authorize_url(&state.square.environment, &state.square.app_id, &scopes, &pkce, &redirect);
    state.cancel_connect.store(false, Ordering::Relaxed);
    let cancel = state.cancel_connect.clone();
    let environment = state.square.environment.clone();
    let app_id = state.square.app_id.clone();
    let store = state.store(true);
    let data_dir = state.data_dir.clone();
    let is_mock = state.mock.is_some();
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let code =
                listener.wait(&pkce.state, Duration::from_secs(300), &cancel).map_err(|e| e.to_string())?;
            let client = HttpClient::new(environment.clone()).map_err(|e| e.to_string())?;
            let mut tokens =
                oauth::exchange_code(&client, &app_id, &code, &pkce.verifier, &redirect, &scopes)
                    .map_err(|e| format!("Square didn't finish connecting: {e}"))?;
            tokens.obtained_at = Utc::now();
            store.save(&tokens).map_err(|e| format!("couldn't save the connection in the keychain: {e}"))?;
            let api = dsf_square::api::Api::new(&client, &tokens.access_token);
            let merchant = api.merchant().map_err(|e| e.to_string())?;
            let locations = api.locations().unwrap_or_default();
            let name = merchant
                .business_name
                .or_else(|| locations.first().map(|l| l.name.clone()))
                .unwrap_or_else(|| "your Square account".into());
            if let Ok(cache) = Cache::open(&dsf_check::paths::cache_file(&data_dir)) {
                if cache.meta("merchant_id").ok().flatten().is_some_and(|m| m != merchant.id) {
                    let _ = cache.forget_account();
                }
                let _ = cache.set_meta("business_name", &name);
                let _ = cache.set_meta("connection", "connected");
                let _ = cache.clear_meta("notified_disconnected");
                let _ = cache.clear_meta("notified_scope");
                let tx = cache.conn();
                let _ = tx.execute("DELETE FROM locations", []);
                for (i, l) in locations.iter().enumerate() {
                    let _ = tx.execute(
                        "INSERT INTO locations (id, name, timezone, status, currency, ord) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![l.id, l.name, l.timezone, l.status, l.currency, i as i64],
                    );
                }
            }
            Ok(name)
        })();
        focus(&app);
        match result {
            Ok(business_name) => {
                let _ = app.emit("connected", Connected { business_name });
            }
            Err(message) => {
                let _ = app.emit("connect-failed", message);
            }
        }
    });
    if is_mock {
        // UI tests: play the browser's part against the mock's authorize page.
        let url = url.clone();
        std::thread::spawn(move || mock_browser(&url));
        return Ok(());
    }
    open_browser(&url)
}

/// UI tests only: follows the mock's authorize redirect to the listener.
fn mock_browser(url: &str) {
    use std::io::{Read, Write};
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return,
    };
    let addr = format!("{}:{}", parsed.host_str().unwrap_or("127.0.0.1"), parsed.port().unwrap_or(80));
    let get = |addr: &str, target: &str, host: &str| -> Option<String> {
        let mut s = std::net::TcpStream::connect(addr).ok()?;
        write!(s, "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").ok()?;
        let mut out = String::new();
        s.read_to_string(&mut out).ok()?;
        Some(out)
    };
    let target = format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""));
    let Some(response) = get(&addr, &target, &addr) else { return };
    let Some(location) = response.lines().find_map(|l| l.strip_prefix("Location: ")) else { return };
    let Ok(next) = url::Url::parse(location.trim()) else { return };
    let port = next.port().unwrap_or(80);
    let _ = get(
        &format!("127.0.0.1:{port}"),
        &format!("/?{}", next.query().unwrap_or("")),
        &format!("localhost:{port}"),
    );
}

#[tauri::command]
pub fn cancel_connect(state: State<'_, AppState>) {
    state.cancel_connect.store(true, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct SaveResult {
    view: AppView,
    /// Labels are on but the connection can't write them yet.
    needs_reconnect: bool,
    registration_error: Option<String>,
}

/// Saves the owner's choices. `start` finishes setup. Registers or removes
/// the background check to match.
#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    settings: Settings,
    start: bool,
) -> Result<SaveResult, String> {
    let cache = state.cache()?;
    let before = cache.settings().map_err(|e| e.to_string())?;
    let mut settings = settings;
    settings.window_days = settings.window_days.clamp(1, 3650);
    settings.check_time = (settings.check_time.0.min(23), settings.check_time.1.min(59) / 15 * 15);
    if start {
        settings.setup_done = true;
    } else {
        settings.setup_done = before.setup_done;
    }
    cache.save_settings(&settings).map_err(|e| e.to_string())?;
    let mut registration_error = None;
    if state.mock.is_none() && settings.setup_done {
        let changed = start
            || before.background_on != settings.background_on
            || before.check_time != settings.check_time;
        if changed {
            let result = if settings.background_on {
                dsf_schedule::register(settings.check_time).map(|_| ())
            } else {
                dsf_schedule::unregister()
            };
            if let Err(e) = result {
                registration_error = Some(e.to_string());
            }
        }
    }
    if start {
        std::thread::spawn(|| {
            let _ = dsf_notify::request_permission();
        });
    }
    let tokens = state.store(true).load().ok().flatten();
    let needs_reconnect = settings.labels_on && tokens.as_ref().is_some_and(|t| !t.has_scope(LABEL_SCOPE));
    Ok(SaveResult { view: view(&state)?, needs_reconnect, registration_error })
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProgressView {
    Catalog { objects: u64 },
    Sales { months_done: u32, months_total: u32 },
    Inventory,
}

/// Check now (and the first sync): the same check as the background one,
/// with progress shown.
#[tauri::command]
pub fn check_now(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let busy = state.busy.clone();
    let cfg = state.config();
    let store = state.store(true);
    std::thread::spawn(move || {
        let Ok(_guard) = busy.try_lock() else {
            let _ = app.emit("check-done", Outcome::Skipped("a check is already running".into()));
            return;
        };
        let progress_app = app.clone();
        let progress = move |p: Progress| {
            let view = match p {
                Progress::Catalog { objects } => ProgressView::Catalog { objects },
                Progress::Sales { months_done, months_total } => {
                    ProgressView::Sales { months_done, months_total }
                }
                Progress::Inventory => ProgressView::Inventory,
            };
            let _ = progress_app.emit("progress", view);
        };
        let notifier = SystemNotifier;
        let clock = Utc::now;
        let deps = Deps {
            tokens: store.as_ref(),
            notifier: &notifier,
            clock: &clock,
            retry: RetryPolicy::default(),
            network_wait: Duration::from_secs(60),
            observer: None,
            progress: Some(&progress),
        };
        let outcome = dsf_check::run(&cfg, &deps, Trigger::Manual);
        let _ = app.emit("check-done", outcome);
    });
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct QueryIn {
    window_days: Option<u32>,
    location: Option<String>,
    category: Option<String>,
    slow_only: bool,
}

impl From<QueryIn> for ReportQuery {
    fn from(q: QueryIn) -> Self {
        ReportQuery {
            window_days: q.window_days.filter(|d| *d > 0),
            location: q.location.filter(|l| !l.is_empty()),
            category: q.category.filter(|c| !c.is_empty()),
            slow_only: q.slow_only,
        }
    }
}

#[tauri::command]
pub fn report(state: State<'_, AppState>, query: QueryIn) -> Result<Report, String> {
    let cache = state.cache()?;
    let settings = cache.settings().map_err(|e| e.to_string())?;
    dsf_sync::report(&cache, &settings, Utc::now(), &query.into()).map_err(|e| e.to_string())
}

/// Save: XLSX or CSV through the system dialog. Returns the saved path.
#[tauri::command]
pub async fn export(
    state: State<'_, AppState>,
    query: QueryIn,
    format: String,
) -> Result<Option<String>, String> {
    let cache = state.cache()?;
    let settings = cache.settings().map_err(|e| e.to_string())?;
    let report = dsf_sync::report(&cache, &settings, Utc::now(), &query.into()).map_err(|e| e.to_string())?;
    let csv = format == "csv";
    let name = format!("Idle stock {}.{}", Local::now().format("%Y-%m-%d"), if csv { "csv" } else { "xlsx" });
    let dialog = rfd::AsyncFileDialog::new().set_file_name(&name);
    let dialog =
        if csv { dialog.add_filter("CSV", &["csv"]) } else { dialog.add_filter("Excel workbook", &["xlsx"]) };
    let Some(file) = dialog.save_file().await else { return Ok(None) };
    let path = file.path().to_path_buf();
    write_report(&report, &path, csv)?;
    Ok(Some(path.display().to_string()))
}

fn write_report(report: &Report, path: &std::path::Path, csv: bool) -> Result<(), String> {
    if csv {
        dsf_sync::export::write_csv(report, path).map_err(|e| e.to_string())
    } else {
        dsf_sync::export::write_xlsx(report, path).map_err(|e| e.to_string())
    }
}

/// UI tests: save without the system dialog (WebDriver can't drive it).
#[cfg(feature = "ui-test")]
#[tauri::command]
pub fn export_to(
    state: State<'_, AppState>,
    query: QueryIn,
    format: String,
    path: String,
) -> Result<Option<String>, String> {
    let cache = state.cache()?;
    let settings = cache.settings().map_err(|e| e.to_string())?;
    let report = dsf_sync::report(&cache, &settings, Utc::now(), &query.into()).map_err(|e| e.to_string())?;
    write_report(&report, std::path::Path::new(&path), format == "csv")?;
    Ok(Some(path))
}

#[derive(Clone, Serialize)]
struct RemoveDone {
    ok: bool,
    message: String,
}

#[tauri::command]
pub fn remove_labels(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let busy = state.busy.clone();
    let cfg = state.config();
    let store = state.store(true);
    std::thread::spawn(move || {
        let _guard = busy.lock();
        let notifier = SystemNotifier;
        let clock = Utc::now;
        let deps = Deps {
            tokens: store.as_ref(),
            notifier: &notifier,
            clock: &clock,
            retry: RetryPolicy::default(),
            network_wait: Duration::from_secs(60),
            observer: None,
            progress: None,
        };
        let done = match dsf_check::remove_all_labels(&cfg, &deps) {
            Ok(n) => {
                // Labels stay off until the owner turns them on again.
                if let Ok(cache) = Cache::open(&dsf_check::paths::cache_file(&cfg.data_dir))
                    && let Ok(mut s) = cache.settings()
                {
                    s.labels_on = false;
                    let _ = cache.save_settings(&s);
                }
                RemoveDone {
                    ok: true,
                    message: format!("Removed the Last sold label from {n} items. Labels are now off."),
                }
            }
            Err(e) => RemoveDone { ok: false, message: e },
        };
        let _ = app.emit("remove-done", done);
    });
    Ok(())
}

#[tauri::command]
pub fn halt_details(state: State<'_, AppState>) -> Result<Option<dsf_labels::Halt>, String> {
    Ok(dsf_labels::halted(&state.cache()?))
}

#[tauri::command]
pub fn resume_labels(state: State<'_, AppState>) -> Result<AppView, String> {
    let cache = state.cache()?;
    dsf_labels::resume(&cache).map_err(|e| e.to_string())?;
    let _ = cache.clear_meta("notified_halt");
    view(&state)
}

/// Disconnect: the token leaves this computer. Square's own control to
/// remove the app is in the Dashboard; the page says where.
#[tauri::command]
pub fn disconnect(state: State<'_, AppState>) -> Result<AppView, String> {
    state.store(true).delete().map_err(|e| e.to_string())?;
    if state.mock.is_none() {
        let _ = dsf_schedule::unregister();
    }
    let cache = state.cache()?;
    let mut settings = cache.settings().map_err(|e| e.to_string())?;
    settings.setup_done = false;
    cache.save_settings(&settings).map_err(|e| e.to_string())?;
    let _ = cache.clear_meta("connection");
    view(&state)
}

#[tauri::command]
pub fn self_test(app: AppHandle) {
    std::thread::spawn(move || {
        let report = dsf_check::selftest::run();
        let _ = app.emit("self-test-done", report);
    });
}

#[tauri::command]
pub fn diagnostics(state: State<'_, AppState>) -> String {
    let registration =
        if state.mock.is_some() { "test mode".to_string() } else { format!("{:?}", dsf_schedule::status()) };
    dsf_check::diagnostics::build(&state.data_dir, &registration, state.square.environment.name())
}

#[tauri::command]
pub async fn save_diagnostics(text: String) -> Result<Option<String>, String> {
    let name = format!("Idle Stock Finder diagnostics {}.txt", Local::now().format("%Y-%m-%d"));
    let Some(file) =
        rfd::AsyncFileDialog::new().set_file_name(&name).add_filter("Text", &["txt"]).save_file().await
    else {
        return Ok(None);
    };
    std::fs::write(file.path(), text).map_err(|e| e.to_string())?;
    Ok(Some(file.path().display().to_string()))
}

/// The page reports a script error (logged, never shown raw to the owner).
#[tauri::command]
pub fn page_error(state: State<'_, AppState>, message: String) {
    dsf_check::log::Log::open(&state.data_dir).line(&format!("page error: {message}"));
}

/// The mock environment for UI tests and local runs: `DSF_MOCK=1` in a
/// build with the `ui-test` feature.
pub fn mock_state() -> Option<(Arc<dsf_mock::MockSquare>, Arc<dsf_square::MemoryStore>, Environment)> {
    if !cfg!(feature = "ui-test") || std::env::var("DSF_MOCK").as_deref() != Ok("1") {
        return None;
    }
    let (seed, _now) = crate::demo::seed();
    let mock = Arc::new(dsf_mock::MockSquare::start(seed, dsf_compute::LABEL_KEY));
    let env = Environment::Mock(mock.addr());
    Some((mock, Arc::new(dsf_square::MemoryStore::default()), env))
}
