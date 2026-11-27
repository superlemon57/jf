//! Configuration file and application folders.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: String,
    pub token: String,
    pub user_id: String,
    pub device_id: String,
    /// Name shown for jf in the iOS Apple TV remote; empty to disable it.
    pub apple_tv: String,
    /// Name shown for jf in the Android TV remote (Google TV app); empty to disable it.
    pub android_tv: String,
    /// Interface language: "en" (default) or "fr".
    pub language: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            token: String::new(),
            user_id: String::new(),
            device_id: String::new(),
            apple_tv: "jf".into(),
            android_tv: "jf".into(),
            language: "en".into(),
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

/// `$VARIABLE/jf` when the variable holds an absolute path, else `fallback/jf`.
fn folder(variable: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(variable).map(PathBuf::from).filter(|path| path.is_absolute()).unwrap_or_else(fallback).join("jf")
}

/// `~/.config/jf` (XDG), `~/Library/Application Support/jf` or `%APPDATA%\jf`.
pub fn config_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        folder("APPDATA", || home().join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        folder("XDG_CONFIG_HOME", || home().join("Library").join("Application Support"))
    } else {
        folder("XDG_CONFIG_HOME", || home().join(".config"))
    }
}

/// `~/.cache/jf` (XDG), `~/Library/Caches/jf` or `%LOCALAPPDATA%\jf`.
pub fn cache_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        folder("LOCALAPPDATA", || home().join("AppData").join("Local")).join("cache")
    } else if cfg!(target_os = "macos") {
        folder("XDG_CACHE_HOME", || home().join("Library").join("Caches"))
    } else {
        folder("XDG_CACHE_HOME", || home().join(".cache"))
    }
}

/// Writes a file readable by the current user only (it may hold a token or a key).
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

impl Config {
    pub fn load() -> Self {
        let mut config: Self = std::fs::read(config_dir().join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if config.device_id.is_empty() {
            config.device_id = rand::random::<[u8; 16]>().iter().map(|byte| format!("{byte:02x}")).collect();
        }
        config
    }

    pub fn logged_in(&self) -> bool {
        !self.server.is_empty() && !self.token.is_empty() && !self.user_id.is_empty()
    }

    /// The file holds the access token: readable by the user only.
    pub fn save(&self) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        write_private(&config_dir().join("config.json"), &json)
    }
}
