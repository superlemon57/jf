//! Fichier de configuration et dossiers de l'application.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: String,
    pub token: String,
    pub user_id: String,
    pub device_id: String,
    /// Nom sous lequel jf apparaît comme Apple TV dans la télécommande d'iOS ; vide pour désactiver.
    pub apple_tv: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            token: String::new(),
            user_id: String::new(),
            device_id: String::new(),
            apple_tv: "jf".into(),
        }
    }
}

fn xdg(variable: &str, fallback: &str) -> PathBuf {
    let base = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback));
    base.join("jf")
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Config {
    pub fn load() -> Self {
        let mut config: Self = std::fs::read(config_dir().join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if config.device_id.is_empty() {
            config.device_id = random_id();
        }
        config
    }

    pub fn logged_in(&self) -> bool {
        !self.server.is_empty() && !self.token.is_empty() && !self.user_id.is_empty()
    }

    /// Le fichier contient le jeton d'accès : lisible par l'utilisateur seulement.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(config_dir().join("config.json"))?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }
}
