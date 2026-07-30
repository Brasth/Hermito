//! Versioned app-state records (TOML) for layout, open tab metadata, and per-workspace trust.
//! On restore: referenced paths are validated (stat); missing non-dirty tabs dropped.
//! Dirty acknowledged buffers from journal are preserved even if path missing (journal recovery precedes layout restore).
//! Corrupt state file is backed up and defaults are used.
//! No FS through UI mutations: load only at startup recovery, save only at explicit shutdown flush.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::document::{DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::layout::WorkbenchLayout;
use crate::persistence::{config_dir, durable_atomic_replace, state_path};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppState {
    pub version: u32,
    pub epoch: WorkspaceEpoch,
    pub layout: WorkbenchLayout,
    /// Tab metadata for open editors (paths + per-tab view state). Journal supplies content for dirty on recovery.
    pub tabs: Vec<TabMetadata>,
    pub current_tab: Option<DocumentId>,
    /// Focus landmark serialized as variant name string (e.g. "Editor" | "BottomPane").
    /// Backward-compatible: missing in old TOML -> "Editor" default (pre-persist restore behavior).
    #[serde(default = "default_focus")]
    pub focus: String,
    /// Trust records (per workspace root + authority). Phase-1 uses "local" + inspect_only default.
    pub trust: Vec<TrustRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TabMetadata {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
    pub last_known_revision: DocumentRevision,
    pub cursor_byte: usize,
    pub scroll_top_line: usize,
    pub selection_start_byte: Option<usize>,
    pub selection_end_byte: Option<usize>,
    /// Populated only at startup load/validate for clean (non-journal) tabs by reading validated path.
    /// Never serialized (state stores only metadata); None for dirty/recovered (content from journal).
    #[serde(skip_serializing, default)]
    pub content: Option<String>,
    #[serde(default)]
    pub language: Language,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrustRecord {
    pub workspace_root: PathBuf,
    pub authority: String,
    #[serde(default = "default_authority_kind")]
    pub kind: String,
    pub level: String,
}

impl Default for AppState {
    fn default() -> Self {
        first_run_state()
    }
}
fn default_focus() -> String {
    "Editor".to_string()
}
fn default_authority_kind() -> String {
    "local".to_string()
}

/// Exact first-run layout + trust per phase-01 contract and design-guidelines.
/// left=28 (Project), editor remainder, context+bottom collapsed, Local InspectOnly. No execution surfaces.
pub fn first_run_state() -> AppState {
    let mut layout = WorkbenchLayout::default();
    layout.resize(120, 36);
    layout.primary_visible = true;
    layout.context_visible = false;
    layout.bottom_visible = false;
    layout.left_width = 28;
    layout.context_width = 24;
    layout.bottom_height = 0;
    layout.primary_active_tab = 0;
    layout.context_active_tab = 0;
    AppState {
        version: 1,
        epoch: WorkspaceEpoch(1),
        layout,
        tabs: vec![],
        current_tab: None,
        focus: default_focus(),
        trust: vec![TrustRecord {
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            authority: "local".to_string(),
            kind: "local".to_string(),
            level: "inspect_only".to_string(),
        }],
    }
}

/// Load state. Missing -> first-run. Corrupt -> backup + first-run defaults.
pub fn load_state() -> Result<AppState> {
    let path = state_path();
    if !path.exists() {
        return Ok(first_run_state());
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            backup_corrupt(&path);
            return Err(anyhow!("state read failed: {}", e));
        }
    };
    match toml::from_str::<AppState>(&content) {
        Ok(mut s) if s.version == 1 => {
            if s.tabs.is_empty() {
                s.current_tab = None;
            }
            s.layout.restore_fixed_fields();
            s.layout.recompute();
            Ok(s)
        }
        _ => {
            backup_corrupt(&path);
            Ok(first_run_state())
        }
    }
}

/// Save with full durable sequence. Only from explicit shutdown flush path.
pub fn save_state(state: &AppState) -> Result<()> {
    let dir = config_dir();
    crate::persistence::create_dir_all_owner_only(&dir)?;
    let path = state_path();
    let tmp = path.with_extension("tmp");
    let serialized = toml::to_string_pretty(state)?;
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
    }
    durable_atomic_replace(&tmp, &path)?;
    Ok(())
}

fn backup_corrupt(path: &Path) {
    if path.exists() {
        let bak = path.with_extension("corrupt.bak");
        let _ = std::fs::rename(path, bak);
    }
}

/// Stat-validate paths from state tabs and load their current disk content for clean-tab restore.
/// Call AFTER journal recovery. Missing non-dirty (or unreadable) tabs dropped.
/// Dirty journal buffers kept even if path gone. Content read here (pre-UI) so restore_state receives
/// actual validated file bytes without any FS on the event-loop thread.
pub fn validate_tabs_on_restore(tabs: Vec<TabMetadata>) -> Vec<TabMetadata> {
    tabs.into_iter()
        .filter_map(|mut t| {
            if let Some(p) = &t.path {
                if std::fs::metadata(p).is_ok() {
                    if t.content.is_none() {
                        t.content = std::fs::read_to_string(p).ok();
                    }
                    if t.content.is_some() {
                        Some(t)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                Some(t)
            }
        })
        .collect()
}
