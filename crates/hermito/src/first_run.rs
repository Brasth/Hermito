//! First-run bootstrap. Called synchronously before event loop / App construction.
//! Initializes:
//! - config.toml with theme=default
//! - state.v1.toml with default layout (Project 28-wide, context+bottom collapsed) + Local InspectOnly trust
//! - empty journal.v1 (or single welcome checkpoint if chosen; empty satisfies "empty or welcome")
//!
//! Never opens terminal, git, env, ports or any execution surface. All authorities start InspectOnly.

use anyhow::Result;

use crate::config::{save as save_config, AppConfig};
use crate::persistence::state::{first_run_state, save_state};
use crate::persistence::{journal_path, set_owner_only, state_path};

/// Returns true if this appears to be a first run (no state file).
pub fn is_first_run() -> bool {
    !state_path().exists()
}

/// Perform first-run initialization if needed. Idempotent.
/// Creates dirs, writes defaults with owner-only perms where applicable.
/// Does not touch runtime state, buffers, or UI.
pub fn ensure_initialized() -> Result<()> {
    let dir = crate::persistence::config_dir();
    crate::persistence::create_dir_all_owner_only(&dir)?;
    let cpath = crate::persistence::config_path();
    if !cpath.exists() {
        let cfg = AppConfig::default_first_run();
        save_config(&cfg)?;
    }

    // State (layout + tabs + trust)
    let spath = state_path();
    if !spath.exists() {
        let st = first_run_state();
        save_state(&st)?;
    }

    // Journal: empty (satisfies contract; welcome content can be a non-dirty editor buffer created later by app)
    let jpath = journal_path();
    if !jpath.exists() {
        // empty file = no recovered dirty buffers
        std::fs::write(&jpath, "")?;
        set_owner_only(&jpath)?;
    }

    // Ensure owner perms on state too if just created
    if spath.exists() {
        let _ = set_owner_only(&spath);
    }

    Ok(())
}

/// Optional helper: write a minimal welcome buffer checkpoint into journal on first run.
/// Not used by default (empty journal + app creates untitled buffer is sufficient and keeps surfaces closed).
#[allow(dead_code)]
pub fn initialize_welcome_journal() -> Result<()> {
    // Would call journal persist, but contract prefers empty on first run to avoid pre-opened exec.
    // Left as no-op to satisfy "empty or welcome" choice without opening surfaces.
    Ok(())
}
