//! The per-user app data folder: the cache, the check lock and the log.
//! No admin rights, nothing outside the user's own profile.

use std::path::{Path, PathBuf};

pub const FOLDER: &str = "com.tightline.idlestockfinder";

/// macOS: ~/Library/Application Support/com.tightline.idlestockfinder
/// Windows: %LOCALAPPDATA%\com.tightline.idlestockfinder
/// Linux (development): $XDG_DATA_HOME or ~/.local/share
/// `DSF_DATA_DIR` overrides it (tests, CI).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DSF_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home)
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
    };
    base.join(FOLDER)
}

pub fn cache_file(dir: &Path) -> PathBuf {
    dir.join("cache.sqlite")
}

pub fn log_dir(dir: &Path) -> PathBuf {
    dir.join("logs")
}
