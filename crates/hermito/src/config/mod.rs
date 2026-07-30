//! Minimal configuration (TOML). First-run only writes theme default.
//! Loaded at startup; no mutation FS paths except init.

pub mod known_hosts;
pub mod trust;

use std::fs::OpenOptions;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::persistence::{config_dir, config_path};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub theme: String,
    // future: keybindings, tasks, lsp etc. (config-only per stack)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_authorities: Vec<SshAuthorityConfig>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SshAuthorityConfig {
    pub label: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    pub root: std::path::PathBuf,
    pub identity: std::path::PathBuf,
    #[serde(default)]
    pub certificate: Option<std::path::PathBuf>,
    #[serde(default)]
    pub passphrase_required: bool,
    pub host_fingerprint: String,
    pub tuf_trusted_root: std::path::PathBuf,
    pub tuf_metadata_url: String,
    pub tuf_targets_url: String,
    pub tuf_datastore: std::path::PathBuf,
    pub tuf_target_cache: std::path::PathBuf,
    pub helper_target: String,
    pub remote_helper_directory: std::path::PathBuf,
}

const fn default_ssh_port() -> u16 {
    22
}

impl AppConfig {
    pub fn default_first_run() -> Self {
        AppConfig {
            version: 1,
            theme: "default".to_string(),
            ssh_authorities: Vec::new(),
        }
    }
}

pub fn load() -> anyhow::Result<AppConfig> {
    let path = config_path();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppConfig::default_first_run());
        }
        Err(error) => return Err(error.into()),
    };
    let config = toml::from_str::<AppConfig>(&contents)?;
    if config.version != 1 {
        anyhow::bail!("unsupported Hermito config version {}", config.version);
    }
    Ok(config)
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
