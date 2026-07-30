//! Minimal configuration (TOML). First-run only writes theme default.
//! Loaded at startup; no mutation FS paths except init.

use std::fs::OpenOptions;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::persistence::{config_dir, config_path};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AppConfig {
    pub version: u32,
    pub theme: String,
    // future: keybindings, tasks, lsp etc. (config-only per stack)
}
impl AppConfig {
    pub fn default_first_run() -> Self {
        AppConfig {
            version: 1,
            theme: "default".to_string(),
        }
    }
}

pub fn load() -> AppConfig {
    let p = config_path();
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(cfg) = toml::from_str::<AppConfig>(&s) {
            if cfg.version == 1 {
                return cfg;
            }
        }
    }
    AppConfig::default_first_run()
}

pub fn save(cfg: &AppConfig) -> anyhow::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let serialized = toml::to_string_pretty(cfg)?;
    let tmp = config_path().with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, config_path())?;
    crate::persistence::set_owner_only(&config_path())?;
    if let Some(parent) = config_path().parent() {
        if let Ok(d) = std::fs::File::open(parent) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
