//! The modes that never open a window.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use dsf_check::{Config, Deps, Outcome};
use dsf_notify::SystemNotifier;
use dsf_schedule::Trigger;
use dsf_square::{RetryPolicy, TokenStore};
use dsf_sync::Cache;
use serde_json::json;

use crate::config;

/// Prints and, when asked, writes the JSON result.
pub fn emit(json: &serde_json::Value, out: Option<&Path>) {
    let text = serde_json::to_string_pretty(json).unwrap_or_default();
    println!("{text}");
    if let Some(path) = out {
        let _ = std::fs::write(path, text);
    }
}

pub fn check(trigger: Trigger) -> i32 {
    let square = config::square();
    let data_dir = dsf_check::paths::data_dir();
    let log = dsf_check::log::Log::open(&data_dir);
    let cfg = Config {
        app_id: square.app_id.clone(),
        environment: square.environment.clone(),
        data_dir: data_dir.clone(),
    };
    let tokens = config::keychain(&square.environment, false);
    let notifier = SystemNotifier;
    let clock = Utc::now;
    let deps = Deps {
        tokens: &tokens,
        notifier: &notifier,
        clock: &clock,
        retry: RetryPolicy::default(),
        network_wait: Duration::from_secs(10 * 60),
        observer: None,
        progress: None,
    };
    let outcome = dsf_check::run(&cfg, &deps, trigger);
    // Evidence for the scheduler tests: a background check owns no window.
    log.line(&format!(
        "headless check ({}) finished: {}; windows owned by this process: {}",
        trigger.name(),
        serde_json::to_string(&outcome).unwrap_or_default(),
        own_windows()
    ));
    match outcome {
        Outcome::Failed(_) => 1,
        _ => 0,
    }
}

#[cfg(windows)]
fn own_windows() -> usize {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
    unsafe extern "system" fn each(hwnd: HWND, param: LPARAM) -> i32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == std::process::id() {
            let count = param as *mut usize;
            unsafe { *count += 1 };
        }
        1
    }
    let mut count = 0usize;
    unsafe { EnumWindows(Some(each), &mut count as *mut usize as LPARAM) };
    count
}

#[cfg(not(windows))]
fn own_windows() -> usize {
    // No AppKit or GTK code runs in a headless check.
    0
}

pub fn schedule(action: &str, out: Option<&Path>) -> i32 {
    let data_dir = dsf_check::paths::data_dir();
    let settings =
        Cache::open(&dsf_check::paths::cache_file(&data_dir)).and_then(|c| c.settings()).unwrap_or_default();
    let result = match action {
        "register" => dsf_schedule::register(settings.check_time).map(|r| json!({ "registered": r })),
        "restore" => {
            if settings.setup_done && settings.background_on {
                dsf_schedule::register(settings.check_time).map(|r| json!({ "registered": r }))
            } else {
                Ok(json!({ "registered": null, "reason": "background checks are off or setup isn't done" }))
            }
        }
        "unregister" => dsf_schedule::unregister().map(|_| json!({ "unregistered": true })),
        "status" => Ok(json!({ "status": dsf_schedule::status() })),
        other => Err(dsf_schedule::ScheduleError::Failed(format!("unknown action {other}"))),
    };
    match result {
        Ok(v) => {
            emit(&v, out);
            0
        }
        Err(e) => {
            emit(&json!({ "error": e.to_string() }), out);
            1
        }
    }
}

pub fn uninstall(remove_data: bool) -> i32 {
    let _ = dsf_schedule::unregister();
    if remove_data {
        for env in [dsf_square::Environment::Production, dsf_square::Environment::Sandbox] {
            let _ = config::keychain(&env, true).delete();
        }
        let _ = std::fs::remove_dir_all(dsf_check::paths::data_dir());
    }
    0
}

pub fn hold_lock(seconds: u64) -> i32 {
    let dir = dsf_check::paths::data_dir();
    match dsf_schedule::try_lock(&dir) {
        Ok(Some(lock)) => {
            std::thread::sleep(Duration::from_secs(seconds));
            drop(lock);
            0
        }
        _ => 1,
    }
}

pub fn self_test(out: Option<&Path>) -> i32 {
    let report = dsf_check::selftest::run();
    let passed = report.passed;
    emit(&serde_json::to_value(&report).unwrap_or_default(), out);
    if passed { 0 } else { 1 }
}

pub fn mark_setup() -> i32 {
    let dir = dsf_check::paths::data_dir();
    let done = Cache::open(&dsf_check::paths::cache_file(&dir)).and_then(|c| {
        let mut settings = c.settings()?;
        settings.setup_done = true;
        c.save_settings(&settings)
    });
    if done.is_ok() { 0 } else { 1 }
}
