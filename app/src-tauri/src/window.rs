//! The window: connect, set up, the report, settings. (Phase 5 fills it in.)

use tauri::{WebviewUrl, WebviewWindowBuilder};

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Dead Stock Finder")
                .inner_size(1180.0, 760.0)
                .min_inner_size(880.0, 560.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Dead Stock Finder failed to start");
}
