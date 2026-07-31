//! Minimal configuration (TOML). First-run only writes theme default.
//! Loaded at startup; no mutation FS paths except init.

pub mod known_hosts;
pub mod trust;

use std::fs::OpenOptions;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::persistence::{config_dir, config_path};
use std::collections::BTreeMap;
use std::path::Path;

use hermito_protocol::request::ExecutionContextV1;

use hex;
use serde_json;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub theme: String,
    // language_servers etc are user-owned only (no repo-derived sources).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_authorities: Vec<SshAuthorityConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_servers: Vec<LanguageServerConfig>,
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
/// User-owned language server configuration (sourced only from user global config.toml).
/// Explicitly rejects any repository or workspace derived language server configuration.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageServerConfig {
    /// User-defined stable identifier for referencing this server (e.g. "rust-analyzer").
    pub id: String,
    pub executable: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_probe_args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<String>,
    /// Associations declare languages, file types or patterns handled by this server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<String>,
    /// Per-context overrides allow variation without changing base (keyed by context name).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_context_overrides: BTreeMap<String, LanguageServerOverride>,
    /// Opaque initialization options for the LSP initialize request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageServerOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
}

/// Effective resolved configuration used for canonical digest computation and LSP spawn.
/// Changing this (executable, args or init options) invalidates prior execution authorization.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLspConfig {
    pub executable: std::path::PathBuf,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_probe_args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
}

/// User configuration selected for a specific document and execution context.
/// The digest is computed after applying the context override, so it remains
/// the sole authority-execution grant key.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLanguageServerConfig {
    pub id: String,
    pub effective: EffectiveLspConfig,
    pub digest: String,
}

/// Stable digest (lowercase hex SHA-256) of the canonical serialization of EffectiveLspConfig.
/// This digest is what persisted LSP grants are bound to.
pub fn lsp_config_digest(effective: &EffectiveLspConfig) -> String {
    let bytes = serde_json::to_vec(effective).expect("EffectiveLspConfig must be serializable");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

/// Produce EffectiveLspConfig from top level spec (simple case; overrides applied by caller if needed).
pub fn effective_lsp_config_from_spec(spec: &LanguageServerConfig) -> EffectiveLspConfig {
    EffectiveLspConfig {
        executable: spec.executable.clone(),
        args: spec.args.clone(),
        version_probe_args: spec.version_probe_args.clone(),
        expected_version: spec.expected_version.clone(),
        expected_digest: spec.expected_digest.clone(),
        initialization_options: spec.initialization_options.clone(),
    }
}

/// Resolve a configured server for a document association and execution
/// context. Configuration order is deliberate: the first matching
/// user-owned association wins.
pub fn resolve_language_server(
    servers: &[LanguageServerConfig],
    language: &str,
    path: Option<&Path>,
    context: &ExecutionContextV1,
) -> Option<ResolvedLanguageServerConfig> {
    servers
        .iter()
        .find(|server| {
            server
                .associations
                .iter()
                .any(|association| association_matches(association, language, path))
        })
        .map(|server| {
            let effective = effective_lsp_config(server, context);
            let digest = lsp_config_digest(&effective);
            ResolvedLanguageServerConfig {
                id: server.id.clone(),
                effective,
                digest,
            }
        })
}

/// Resolve the only stable configuration name for an execution context.
pub fn execution_context_name(context: &ExecutionContextV1) -> String {
    match context {
        ExecutionContextV1::AuthorityRoot => "authority-root".to_owned(),
        ExecutionContextV1::DevContainer { container_id, .. } => {
            format!("dev-container:{container_id}")
        }
    }
}

fn effective_lsp_config(
    server: &LanguageServerConfig,
    context: &ExecutionContextV1,
) -> EffectiveLspConfig {
    let override_config = server
        .per_context_overrides
        .get(&execution_context_name(context));
    EffectiveLspConfig {
        executable: override_config
            .and_then(|config| config.executable.clone())
            .unwrap_or_else(|| server.executable.clone()),
        args: override_config
            .and_then(|config| config.args.clone())
            .unwrap_or_else(|| server.args.clone()),
        version_probe_args: server.version_probe_args.clone(),
        expected_version: server.expected_version.clone(),
        expected_digest: server.expected_digest.clone(),
        initialization_options: override_config
            .and_then(|config| config.initialization_options.clone())
            .or_else(|| server.initialization_options.clone()),
    }
}

fn association_matches(association: &str, language: &str, path: Option<&Path>) -> bool {
    if association.eq_ignore_ascii_case(language) {
        return true;
    }
    let Some(path) = path else {
        return false;
    };
    let extension = path.extension().and_then(|extension| extension.to_str());
    if let Some(extension) = extension {
        if association
            .strip_prefix("*.")
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
            || association
                .strip_prefix('.')
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            return true;
        }
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| association == name)
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
            language_servers: Vec::new(),
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
