//! The check's log: one line per event, in the app data folder, kept under
//! about 2 MB. It never holds a token (the property tests scan it).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;

const MAX_BYTES: u64 = 1024 * 1024;

pub struct Log {
    path: Option<PathBuf>,
}

impl Log {
    pub fn open(data_dir: &Path) -> Log {
        let dir = super::paths::log_dir(data_dir);
        if std::fs::create_dir_all(&dir).is_err() {
            return Log { path: None };
        }
        let path = dir.join("check.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&path, dir.join("check.log.1"));
        }
        Log { path: Some(path) }
    }

    pub fn line(&self, text: &str) {
        let Some(path) = &self.path else { return };
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ =
                writeln!(f, "{} {}", Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"), text.replace('\n', " | "));
        }
    }
}
