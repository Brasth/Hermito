pub mod action;
pub mod app;
pub mod authority;
pub mod buffer;
pub mod config;
pub mod coordinate;
pub mod lsp;
pub mod document;
pub mod edit;
pub mod event_loop;
pub mod first_run;
pub mod input;
pub mod layout;
pub mod persistence;
pub mod process;
pub mod project;
pub mod pty;
pub mod remote;
pub mod shutdown;
pub mod syntax;
pub mod terminal;
pub mod ui;

use crate::terminal::TerminalGuard;

/// Entry point. Creates runtime, registers platform shutdown, recovers journal
/// BEFORE authority/App use, calls first-run ensure + state load + tab validate,
/// enters terminal guard exactly once, runs event loop (borrowing guard),
/// all exit paths (incl panics) converge on single consuming restore after save/flush.
pub fn run() -> anyhow::Result<()> {
    crate::first_run::ensure_initialized()?;
    // Dedicated journal recovery (synchronous, before any authority or App construction)
    let recovery = crate::persistence::journal::recover_journal()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let (journal, journal_ack_rx) =
            crate::persistence::journal::start_journal_worker(recovery.clone());
        let journal_for_panic = journal.clone();
        let mut guard_opt: Option<TerminalGuard> = None;
        let shutdown_rx = crate::shutdown::register_shutdown(&mut guard_opt, &journal);

        let mut state = crate::persistence::state::load_state()?;
        state.tabs = crate::persistence::state::validate_tabs_on_restore(state.tabs);

        let mut guard = TerminalGuard::enter()?;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            event_loop::run_event_loop(
                &mut guard,
                journal,
                journal_ack_rx,
                recovery,
                shutdown_rx,
                state,
            )
        }));
        // Panic path: flush retained JournalHandle clone (before any restore).
        // This ensures "panic after accepted journal work flushes before terminal restoration".
        // Normal paths (incl signal) flush exactly once via event_loop returns; restore exactly once here.
        if result.is_err() {
            journal_for_panic.flush();
        }
        // restore terminal exactly once here (guard ownership outside catch boundary)
        // panics (including non-render) still reach here for restore
        let _ = guard.restore();

        match result {
            Ok(Ok(_reason)) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(p) => Err(anyhow::anyhow!(format!("{:?}", p))),
        }
    })
}
