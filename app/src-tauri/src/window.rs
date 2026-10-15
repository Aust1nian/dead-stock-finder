//! The window. It opens twice in an owner's life: to connect and set up,
//! and later only for the full report or a change. Closing it ends the app:
//! nothing stays running between checks.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{WebviewUrl, WebviewWindowBuilder};

use crate::commands::{self, AppState};
use crate::config;

/// WebView2 runs Edge's background services unless told not to: component
/// updates, SmartScreen, phishing checks, sync, pings, crash upload. All off,
/// so the page's process reaches no host at all; the app's only network
/// traffic is the Rust core's requests to Square.
/// (wry's defaults, msWebOOUI, msPdfOOUI and msSmartScreenProtection, are
/// repeated because setting arguments replaces them.)
const WEBVIEW2_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,msExperimentation,msEdgeUpdate,AutofillServerCommunication,OptimizationHints,Translate,MediaRouter \
--disable-background-networking --disable-component-update --disable-domain-reliability \
--disable-client-side-phishing-detection --disable-sync --disable-default-apps --no-pings --no-first-run \
--disable-breakpad --dns-prefetch-disable";

pub fn run() {
    let mut square = config::square();
    let data_dir = dsf_check::paths::data_dir();
    let (mock, mock_store) = match commands::mock_state() {
        Some((mock, store, env)) => {
            square = config::Square { environment: env, app_id: dsf_mock::MOCK_APP_ID.into() };
            (Some(mock), Some(store))
        }
        None => (None, None),
    };
    let state = AppState {
        square,
        data_dir,
        cancel_connect: Arc::new(AtomicBool::new(false)),
        busy: Arc::new(Mutex::new(())),
        mock,
        mock_store,
    };
    let builder = tauri::Builder::default().manage(state);
    #[cfg(not(feature = "ui-test"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_view,
        commands::connect,
        commands::cancel_connect,
        commands::save_settings,
        commands::check_now,
        commands::report,
        commands::export,
        commands::remove_labels,
        commands::halt_details,
        commands::resume_labels,
        commands::disconnect,
        commands::self_test,
        commands::diagnostics,
        commands::save_diagnostics,
        commands::page_error,
    ]);
    #[cfg(feature = "ui-test")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_view,
        commands::connect,
        commands::cancel_connect,
        commands::save_settings,
        commands::check_now,
        commands::report,
        commands::export,
        commands::export_to,
        commands::remove_labels,
        commands::halt_details,
        commands::resume_labels,
        commands::disconnect,
        commands::self_test,
        commands::diagnostics,
        commands::save_diagnostics,
        commands::page_error,
    ]);
    builder
        .setup(|app| {
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Idle Stock Finder")
                .inner_size(1180.0, 780.0)
                .min_inner_size(900.0, 580.0);
            // Windows UI tests attach Edge WebDriver to a debugging port.
            #[cfg(feature = "ui-test")]
            let window = match std::env::var("DSF_CDP_PORT") {
                Ok(port) => window.additional_browser_args(&format!(
                    "--remote-debugging-port={port} --remote-allow-origins=*"
                )),
                Err(_) => window.additional_browser_args(WEBVIEW2_ARGS),
            };
            #[cfg(not(feature = "ui-test"))]
            let window = window.additional_browser_args(WEBVIEW2_ARGS);
            window.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Idle Stock Finder failed to start");
}
