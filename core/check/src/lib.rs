//! The check. Every trigger runs the same code: the daily time, sign-in,
//! wake, Check now in the window, and `--self-test`.
//!
//! 1. Take the single-instance lock. Another check running: exit. The last
//!    successful check ended under 30 minutes ago: exit (Check now ignores
//!    this).
//! 2. Wait for the network, up to 10 minutes with backoff.
//! 3. Load the token; refresh it if it's over 7 days old. A refused refresh
//!    means disconnected: notify and exit.
//! 4. Sync locations, catalog changes, new orders and in-stock counts.
//! 5. Recompute every label.
//! 6. Write the labels that changed, by the safe-write procedure.
//! 7. Record the time, counts and errors for the status line. Notify only if
//!    the owner must act, or after checks have failed 3 days running.
//! 8. Exit.

pub mod diagnostics;
pub mod log;
pub mod paths;
pub mod selftest;
pub mod status;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use dsf_labels::{LabelError, LabelRun, Writer};
use dsf_notify::Notifier;
use dsf_schedule::Trigger;
use dsf_square::api::Api;
use dsf_square::http::Observer;
use dsf_square::oauth::{self, LABEL_SCOPE};
use dsf_square::{Environment, HttpClient, RetryPolicy, StoreError, TokenStore, Tokens};
use dsf_sync::cache::{parse_ts, ts};
use dsf_sync::{Cache, Progress};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

pub use status::{Status, status};

/// Where and how the check runs.
pub struct Config {
    /// The Square application id (OAuth client id).
    pub app_id: String,
    pub environment: Environment,
    pub data_dir: PathBuf,
}

/// What the check uses from outside, so tests can stand in for each part.
pub struct Deps<'a> {
    pub tokens: &'a dyn TokenStore,
    pub notifier: &'a dyn Notifier,
    pub clock: &'a (dyn Fn() -> DateTime<Utc> + Sync),
    pub retry: RetryPolicy,
    /// How long to wait for the network before giving up (10 minutes).
    pub network_wait: Duration,
    pub observer: Option<Observer>,
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Summary {
    pub business_name: String,
    pub first_sync: bool,
    pub orders_read: u64,
    pub labels_written: u32,
    pub labels_unchanged: u32,
    pub labels_skipped: u32,
    /// Why labels weren't written, when they're on but couldn't be.
    pub label_problem: Option<String>,
    /// Milliseconds per phase.
    pub timings_ms: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    Done(Summary),
    /// Nothing to do this time (lock held, ran recently, not due).
    Skipped(String),
    /// No Square connection on this computer yet.
    NotConnected,
    /// The connection lapsed; the owner was told to reconnect.
    Disconnected,
    /// The check couldn't finish; labels stay as they are.
    Failed(String),
}

impl Outcome {
    pub fn ok(&self) -> bool {
        matches!(self, Outcome::Done(_) | Outcome::Skipped(_))
    }
}

pub const MINIMUM_GAP: chrono::Duration = chrono::Duration::minutes(30);

fn is_background(trigger: Trigger) -> bool {
    matches!(trigger, Trigger::Signin | Trigger::Scheduled | Trigger::Tick)
}

/// Runs one check.
pub fn run(cfg: &Config, deps: &Deps, trigger: Trigger) -> Outcome {
    let log = log::Log::open(&cfg.data_dir);
    let now = (deps.clock)();
    // 1. The lock.
    let lock = match dsf_schedule::try_lock(&cfg.data_dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Outcome::Skipped("another check is running".into()),
        Err(e) => return Outcome::Failed(format!("couldn't take the check lock: {e}")),
    };
    let mut cache = match Cache::open(&paths::cache_file(&cfg.data_dir)) {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("couldn't open the cache: {e}")),
    };
    let settings = match cache.settings() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    if is_background(trigger) && !settings.setup_done {
        return Outcome::Skipped("not set up yet".into());
    }
    let (last_attempt, last_success) = last_times(&cache);
    if trigger == Trigger::Tick && !tick_due(now, settings.check_time, last_attempt, last_success) {
        return Outcome::Skipped("not due".into());
    }
    if is_background(trigger) && last_success.is_some_and(|s| now - s < MINIMUM_GAP) {
        return Outcome::Skipped("checked less than 30 minutes ago".into());
    }
    let check_id = start_record(&cache, now, trigger);
    log.line(&format!("check started ({})", trigger.name()));
    let outcome = run_locked(cfg, deps, &mut cache, &log);
    let end = (deps.clock)();
    finish_record(&cache, check_id, end, &outcome);
    after(&cache, deps, &outcome, end, &log);
    log.line(&format!("check ended: {}", serde_json::to_string(&outcome).unwrap_or_default()));
    drop(lock);
    outcome
}

fn tick_due(
    now: DateTime<Utc>,
    check_time: (u8, u8),
    last_attempt: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
) -> bool {
    dsf_schedule::tick_due_in(&Local, now, check_time, last_attempt, last_success)
}

/// (last attempt start, last successful check end).
pub fn last_times(cache: &Cache) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let conn = cache.conn();
    let attempt: Option<String> = conn
        .query_row("SELECT MAX(started_at) FROM checks WHERE outcome <> 'skipped'", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .flatten();
    let success: Option<String> = conn
        .query_row("SELECT MAX(ended_at) FROM checks WHERE outcome = 'done'", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .flatten();
    (attempt.as_deref().and_then(parse_ts), success.as_deref().and_then(parse_ts))
}

fn start_record(cache: &Cache, now: DateTime<Utc>, trigger: Trigger) -> i64 {
    let _ = cache.conn().execute(
        "INSERT INTO checks (started_at, trigger, outcome) VALUES (?1, ?2, 'running')",
        params![ts(now), trigger.name()],
    );
    cache.conn().last_insert_rowid()
}

fn finish_record(cache: &Cache, id: i64, end: DateTime<Utc>, outcome: &Outcome) {
    let (name, detail, orders, written, skipped) = match outcome {
        Outcome::Done(s) => (
            "done",
            s.label_problem.clone(),
            s.orders_read as i64,
            s.labels_written as i64,
            s.labels_skipped as i64,
        ),
        Outcome::Skipped(r) => ("skipped", Some(r.clone()), 0, 0, 0),
        Outcome::NotConnected => ("not_connected", None, 0, 0, 0),
        Outcome::Disconnected => ("disconnected", None, 0, 0, 0),
        Outcome::Failed(r) => ("failed", Some(r.clone()), 0, 0, 0),
    };
    let _ = cache.conn().execute(
        "UPDATE checks SET ended_at = ?1, outcome = ?2, detail = ?3, orders_read = ?4, labels_written = ?5, labels_skipped = ?6 WHERE id = ?7",
        params![ts(end), name, detail, orders, written, skipped, id],
    );
    // Keep the record small: the last 500 checks.
    let _ = cache.conn().execute("DELETE FROM checks WHERE id <= ?1", [id - 500]);
}

/// Loads the token, waiting for the network and refreshing when due.
fn connect(
    cfg: &Config,
    deps: &Deps,
    cache: &Cache,
    client: &HttpClient,
    log: &log::Log,
    force_refresh: bool,
) -> Result<Tokens, Outcome> {
    let tokens = match deps.tokens.load() {
        Ok(Some(t)) => t,
        Ok(None) => return Err(Outcome::NotConnected),
        Err(StoreError::NeedsInteraction) => {
            return Err(Outcome::Failed("the keychain is locked; the next check will try again".into()));
        }
        Err(e) => return Err(Outcome::Failed(e.to_string())),
    };
    let now = (deps.clock)();
    if !force_refresh && !oauth::needs_refresh(&tokens, now) {
        return Ok(tokens);
    }
    // 2 + 3. Wait for the network while refreshing.
    let deadline = std::time::Instant::now() + deps.network_wait;
    let mut delay = Duration::from_secs(5).min(deps.network_wait.max(Duration::from_millis(1)));
    loop {
        match oauth::refresh(client, &cfg.app_id, &tokens) {
            Ok(mut fresh) => {
                fresh.obtained_at = (deps.clock)();
                // Save before anything else: the old refresh token is spent.
                let mut saved = deps.tokens.save(&fresh);
                if saved.is_err() {
                    std::thread::sleep(Duration::from_millis(200));
                    saved = deps.tokens.save(&fresh);
                }
                if let Err(e) = saved {
                    log.line(&format!("couldn't save the refreshed token: {e}"));
                    return Err(Outcome::Failed(format!("couldn't save the refreshed connection: {e}")));
                }
                log.line("refreshed the Square connection");
                return Ok(fresh);
            }
            Err(e) if oauth::refresh_rejected(&e) => {
                log.line(&format!("Square refused the refresh: {e}"));
                let _ = deps.tokens.delete();
                let _ = cache.set_meta("connection", "disconnected");
                return Err(Outcome::Disconnected);
            }
            Err(e) if e.is_network() && std::time::Instant::now() + delay < deadline => {
                log.line(&format!("waiting for the network: {e}"));
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(60));
            }
            Err(e) => {
                // The access token may still be good for weeks; carry on
                // with it unless it has expired.
                if tokens.access_expires_at > (deps.clock)() + chrono::Duration::hours(1) && !force_refresh {
                    log.line(&format!("refresh failed ({e}); using the current token"));
                    return Ok(tokens);
                }
                return Err(Outcome::Failed(format!("couldn't reach Square: {e}")));
            }
        }
    }
}

fn run_locked(cfg: &Config, deps: &Deps, cache: &mut Cache, log: &log::Log) -> Outcome {
    let http_log = Arc::new(Mutex::new(Vec::new()));
    let client = match HttpClient::new(cfg.environment.clone()) {
        Ok(c) => c.with_retry(deps.retry.clone()).with_log(http_log.clone()),
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let client = match &deps.observer {
        Some(o) => client.with_observer(o.clone()),
        None => client,
    };
    let flush = |log: &log::Log| {
        if let Ok(mut lines) = http_log.lock() {
            for l in lines.drain(..) {
                log.line(&l);
            }
        }
    };
    let mut tokens = match connect(cfg, deps, cache, &client, log, false) {
        Ok(t) => t,
        Err(outcome) => return outcome,
    };
    let _ = cache.set_meta("connection", "connected");
    let settings = match cache.settings() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    // 4. Sync, waiting for the network if it isn't up yet.
    let deadline = std::time::Instant::now() + deps.network_wait;
    let mut delay = Duration::from_secs(5).min(deps.network_wait.max(Duration::from_millis(1)));
    let mut refreshed = false;
    let report = loop {
        let api = Api::new(&client, &tokens.access_token);
        let now = (deps.clock)();
        let opts = dsf_sync::SyncOptions {
            now,
            app_id: &cfg.app_id,
            progress: deps.progress.map(|p| p as &dyn Fn(Progress)),
        };
        match dsf_sync::sync(cache, &api, &opts) {
            Ok(r) => break r,
            Err(dsf_sync::SyncError::Square(e)) if e.is_unauthorized() && !refreshed => {
                refreshed = true;
                match connect(cfg, deps, cache, &client, log, true) {
                    Ok(t) => tokens = t,
                    Err(outcome) => return outcome,
                }
            }
            Err(dsf_sync::SyncError::Square(e))
                if e.is_network() && std::time::Instant::now() + delay < deadline =>
            {
                log.line(&format!("waiting for the network: {e}"));
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(60));
            }
            Err(e) => {
                flush(log);
                return Outcome::Failed(format!("sync: {e}"));
            }
        }
    };
    flush(log);
    let mut summary = Summary {
        business_name: report.business_name.clone(),
        first_sync: report.first_sync,
        orders_read: report.orders_read,
        timings_ms: report.timings_ms.clone(),
        ..Summary::default()
    };
    log.line(&format!(
        "synced: {} catalog objects, {} orders, {} stock counts; {:?} ms",
        report.catalog_objects, report.orders_read, report.counts_read, report.timings_ms
    ));
    if !settings.labels_on {
        return Outcome::Done(summary);
    }
    if let Some(halt) = dsf_labels::halted(cache) {
        summary.label_problem =
            Some(format!("label writes are stopped since {} ({})", halt.at.format("%b %-d"), halt.name));
        return Outcome::Done(summary);
    }
    if !tokens.has_scope(LABEL_SCOPE) {
        summary.label_problem = Some("reconnect Square to allow labels".into());
        return Outcome::Done(summary);
    }
    // 5. Recompute.
    let now = (deps.clock)();
    let planning = std::time::Instant::now();
    let plans = match dsf_sync::label_plan(cache, &settings, now) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    summary.timings_ms.push(("plan".into(), planning.elapsed().as_millis() as u64));
    let writing = std::time::Instant::now();
    // 6. Write.
    let api = Api::new(&client, &tokens.access_token);
    let writer = Writer { api: &api, cache, app_id: &cfg.app_id, now };
    let mut run = LabelRun::default();
    let result = writer.ensure_definition().and_then(|def| writer.write(&plans, &def, &mut run));
    for line in &run.log {
        log.line(line);
    }
    flush(log);
    summary.labels_written = run.written;
    summary.labels_unchanged = run.unchanged;
    summary.labels_skipped = run.skipped.len() as u32;
    summary.timings_ms.push(("labels".into(), writing.elapsed().as_millis() as u64));
    if run.written > 0 {
        // Take in this check's own writes now, so the next check reads only
        // what changed after them. A failure here costs nothing: the next
        // check reads them instead.
        let refreshing = std::time::Instant::now();
        let opts = dsf_sync::SyncOptions { now: (deps.clock)(), app_id: &cfg.app_id, progress: None };
        if let Err(e) = dsf_sync::sync::refresh_catalog(cache, &api, &opts) {
            log.line(&format!("catalog refresh after writing labels: {e}"));
        }
        summary.timings_ms.push(("catalog after labels".into(), refreshing.elapsed().as_millis() as u64));
    }
    log.line(&format!(
        "labels: {} written, {} skipped; {:?} ms",
        run.written,
        run.skipped.len(),
        summary.timings_ms
    ));
    match result {
        Ok(()) => Outcome::Done(summary),
        Err(LabelError::Halted(name)) => {
            summary.label_problem =
                Some(format!("label writes stopped: Square stored more than the label on {name}"));
            Outcome::Done(summary)
        }
        Err(LabelError::NoWriteScope) => {
            summary.label_problem = Some("reconnect Square to allow labels".into());
            Outcome::Done(summary)
        }
        Err(LabelError::DefinitionLimit(reason)) => {
            summary.label_problem = Some(format!("Square won't add the Last sold attribute: {reason}"));
            Outcome::Done(summary)
        }
        Err(LabelError::Square(e)) => Outcome::Failed(format!("writing labels: {e}")),
        Err(e) => Outcome::Failed(format!("writing labels: {e}")),
    }
}

/// 7. Failure streaks and the notifications the owner must see.
fn after(cache: &Cache, deps: &Deps, outcome: &Outcome, now: DateTime<Utc>, log: &log::Log) {
    let notify_once = |key: &str, title: &str, body: &str| {
        if cache.meta(key).ok().flatten().is_none() {
            deps.notifier.notify(title, body);
            let _ = cache.set_meta(key, &ts(now));
            log.line(&format!("notified: {title}"));
        }
    };
    match outcome {
        Outcome::Done(summary) => {
            let _ = cache.clear_meta("failure_streak_start");
            let _ = cache.clear_meta("failure_count");
            let _ = cache.clear_meta("notified_failures");
            let _ = cache.clear_meta("notified_disconnected");
            match summary.label_problem.as_deref() {
                Some(p) if p.starts_with("label writes stopped") => notify_once(
                    "notified_halt",
                    "Dead Stock Finder stopped writing labels",
                    "Square stored more than a label on one item. Open Dead Stock Finder to see what changed.",
                ),
                Some(p) if p.starts_with("reconnect") => notify_once(
                    "notified_scope",
                    "Reconnect Square to keep labels current",
                    "Dead Stock Finder needs permission to write labels in Square.",
                ),
                _ => {
                    let _ = cache.clear_meta("notified_scope");
                }
            }
            if dsf_labels::halted(cache).is_none() {
                let _ = cache.clear_meta("notified_halt");
            }
        }
        Outcome::Disconnected => notify_once(
            "notified_disconnected",
            "Reconnect Square to keep labels current",
            "Square ended Dead Stock Finder's connection. Open the app and choose Connect Square.",
        ),
        Outcome::Failed(reason) => {
            let start = cache.meta_time("failure_streak_start").ok().flatten().unwrap_or(now);
            let count: u32 =
                cache.meta("failure_count").ok().flatten().and_then(|c| c.parse().ok()).unwrap_or(0) + 1;
            let _ = cache.set_meta("failure_streak_start", &ts(start));
            let _ = cache.set_meta("failure_count", &count.to_string());
            // 3 failed checks in a row, spanning 3 different days.
            let days = (now.with_timezone(&Local).date_naive() - start.with_timezone(&Local).date_naive())
                .num_days();
            if count >= 3 && days >= 2 {
                notify_once(
                    "notified_failures",
                    "Checks have failed 3 days in a row",
                    &format!("Labels in Square stay as they are. Last reason: {reason}"),
                );
            }
        }
        Outcome::Skipped(_) | Outcome::NotConnected => {}
    }
}

/// Settings → Remove all labels from Square: syncs, then clears every value
/// this app wrote by the safe-write procedure, then deletes the definition.
/// Returns how many items were cleared, or why it stopped.
pub fn remove_all_labels(cfg: &Config, deps: &Deps) -> Result<u32, String> {
    let log = log::Log::open(&cfg.data_dir);
    let lock = dsf_schedule::try_lock(&cfg.data_dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "a check is running; try again in a minute".to_string())?;
    let mut cache = Cache::open(&paths::cache_file(&cfg.data_dir)).map_err(|e| e.to_string())?;
    let client =
        HttpClient::new(cfg.environment.clone()).map_err(|e| e.to_string())?.with_retry(deps.retry.clone());
    let client = match &deps.observer {
        Some(o) => client.with_observer(o.clone()),
        None => client,
    };
    let tokens = connect(cfg, deps, &cache, &client, &log, false).map_err(|o| format!("{o:?}"))?;
    if !tokens.has_scope(LABEL_SCOPE) {
        return Err("reconnect Square to allow label changes".into());
    }
    let api = Api::new(&client, &tokens.access_token);
    let now = (deps.clock)();
    let opts = dsf_sync::SyncOptions { now, app_id: &cfg.app_id, progress: None };
    dsf_sync::sync(&mut cache, &api, &opts).map_err(|e| e.to_string())?;
    let writer = Writer { api: &api, cache: &cache, app_id: &cfg.app_id, now };
    let mut run = LabelRun::default();
    let result = writer.remove_all(&mut run);
    for line in &run.log {
        log.line(line);
    }
    drop(lock);
    match result {
        Ok(true) => Ok(run.written),
        Ok(false) => Err(format!("{} items couldn't be cleared; see the log", run.skipped.len())),
        Err(e) => Err(e.to_string()),
    }
}
