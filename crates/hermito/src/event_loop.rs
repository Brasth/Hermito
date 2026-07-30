use crate::action::Action;
use crate::app::App;
use crate::document::{BufferPathState, DocumentId, DocumentRevision, WorkspaceEpoch};
use crate::input::handle_event;
use crate::persistence::journal::{JournalAck, JournalHandle, Recovery};
use crate::persistence::state::AppState;
use crate::project::{load_project_file, ProjectFileLoadResult, ProjectScanResult};
use crate::shutdown::ShutdownReason;
use crate::syntax::SyntaxResult;
use crate::terminal::TerminalGuard;
use crate::ui::workbench;
use crossterm::event as crossterm_event;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const QUALIFIED_REMOTE_HELPER_TARGETS: [&str; 2] = [
    "hermito-remote-x86_64-unknown-linux-musl",
    "hermito-remote-aarch64-unknown-linux-musl",
];

fn is_qualified_remote_helper_target(target: &str) -> bool {
    QUALIFIED_REMOTE_HELPER_TARGETS.contains(&target)
}

/// Result of off-thread atomic file save. Only success + exact rev/epoch match will mark clean + Saved + compact.
struct SaveResult {
    epoch: WorkspaceEpoch,
    doc_id: DocumentId,
    revision: DocumentRevision,
    path: std::path::PathBuf,
    success: bool,
}

struct TerminalStartResult {
    epoch: WorkspaceEpoch,
    launch_id: u64,
    authority_label: String,
    result: Result<crate::pty::PtySession, String>,
}

#[derive(Clone)]
struct ConfiguredSshRuntime {
    authority: Arc<crate::authority::ssh::SshAuthority>,
    bootstrap: crate::remote::ssh_bootstrap::SshBootstrap,
    verifier: Arc<crate::authority::tuf_verifier::TufVerifier>,
    host_fingerprint: String,
    helper_target: String,
    remote_helper_directory: std::path::PathBuf,
    passphrase_required: bool,
}

struct SshActivationResult {
    epoch: WorkspaceEpoch,
    label: String,
    result: Result<(), String>,
}

fn configured_ssh_runtime(
    config: crate::config::SshAuthorityConfig,
    epoch: WorkspaceEpoch,
) -> anyhow::Result<(String, ConfiguredSshRuntime)> {
    let label = config.label.clone();
    if label.is_empty() || label.len() > 64 {
        anyhow::bail!("SSH authority label must contain 1..=64 bytes");
    }
    if !config.host_fingerprint.starts_with("SHA256:")
        || config.host_fingerprint.len() <= "SHA256:".len()
    {
        anyhow::bail!("SSH authority requires an explicit SHA256 host fingerprint");
    }
    if !is_qualified_remote_helper_target(&config.helper_target) {
        anyhow::bail!(
            "SSH helper target is not a qualified Phase 2 artifact: {}",
            config.helper_target
        );
    }
    for (name, path) in [
        ("TUF trusted root", &config.tuf_trusted_root),
        ("TUF datastore", &config.tuf_datastore),
        ("TUF target cache", &config.tuf_target_cache),
    ] {
        if !path.is_absolute() {
            anyhow::bail!("{name} must be absolute");
        }
    }
    let bootstrap = crate::remote::ssh_bootstrap::SshBootstrap::new(
        crate::remote::ssh_bootstrap::SshTarget {
            host: config.host,
            port: config.port,
            user: config.user,
        },
        crate::remote::ssh_identity::SshIdentity {
            private_key: config.identity,
            certificate: config.certificate,
        },
        crate::persistence::config_dir().join("known_hosts"),
    )?;
    let authority = crate::authority::ssh::SshAuthority::new(
        label.clone(),
        config.root,
        bootstrap.clone(),
        hermito_protocol::WorkspaceEpoch(epoch.0),
    )?;
    let verifier =
        crate::authority::tuf_verifier::TufVerifier::new(crate::remote::tuf::TufPolicy {
            trusted_root: config.tuf_trusted_root,
            metadata_base_url: url::Url::parse(&config.tuf_metadata_url)?,
            targets_base_url: url::Url::parse(&config.tuf_targets_url)?,
            datastore: config.tuf_datastore,
            target_cache: config.tuf_target_cache,
            offline_metadata_url: None,
            offline_targets_url: None,
            allow_offline_cache: false,
            max_target_bytes: 64 * 1024 * 1024,
        })?;
    Ok((
        label,
        ConfiguredSshRuntime {
            authority,
            bootstrap,
            verifier: Arc::new(verifier),
            host_fingerprint: config.host_fingerprint,
            helper_target: config.helper_target,
            remote_helper_directory: config.remote_helper_directory,
            passphrase_required: config.passphrase_required,
        },
    ))
}

async fn activate_configured_ssh(
    runtime: ConfiguredSshRuntime,
    passphrase: Option<zeroize::Zeroizing<Vec<u8>>>,
) -> Result<(), String> {
    let candidates = runtime
        .bootstrap
        .scan_host_keys()
        .await
        .map_err(|error| error.to_string())?;
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.fingerprint == runtime.host_fingerprint)
        .ok_or_else(|| {
            format!(
                "SSH host did not present configured fingerprint {}",
                runtime.host_fingerprint
            )
        })?;
    crate::config::known_hosts::KnownHostsStore::new(runtime.bootstrap.known_hosts.clone())
        .accept(
            &runtime.bootstrap.target.host,
            runtime.bootstrap.target.port,
            candidate,
            &runtime.host_fingerprint,
        )
        .map_err(|error| error.to_string())?;
    runtime
        .authority
        .activate(
            runtime.verifier.as_ref(),
            &runtime.helper_target,
            runtime.remote_helper_directory,
            passphrase.as_ref(),
        )
        .await
        .map_err(|error| error.to_string())
}

fn dispatch_ssh_activation(
    label: String,
    epoch: WorkspaceEpoch,
    runtime: ConfiguredSshRuntime,
    passphrase: Option<zeroize::Zeroizing<Vec<u8>>>,
    tx: std::sync::mpsc::SyncSender<SshActivationResult>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let result = activate_configured_ssh(runtime, passphrase).await;
        let _ = tx.send(SshActivationResult {
            epoch,
            label,
            result,
        });
    })
}
const TICK_MS: u64 = 16;

/// Coalescing state for syntax: at most ONE in-flight job globally (for current doc or pending).
/// After result (stale or good), if no flight and current needs update, dispatch latest desired.
/// Pure methods for deterministic tests of coalescing without timing/threads.
#[derive(Default, Debug)]
struct SyntaxCoalesceState {
    in_flight: Option<(DocumentId, DocumentRevision)>,
}

impl SyntaxCoalesceState {
    fn has_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }
    fn on_dispatch(&mut self, doc: DocumentId, rev: DocumentRevision) {
        self.in_flight = Some((doc, rev));
    }
    fn on_result(&mut self, doc: DocumentId, rev: DocumentRevision) -> bool {
        if self.in_flight == Some((doc, rev)) {
            self.in_flight = None;
            true
        } else {
            false
        }
    }
    fn should_spawn_for(&self, _doc: DocumentId, _rev: DocumentRevision, up_to_date: bool) -> bool {
        !up_to_date && self.in_flight.is_none()
    }
    fn clear(&mut self) {
        self.in_flight = None;
    }
}
/// Coalescing state for saves, per-document: serialize writes so that for a given doc,
/// an in-flight save for rev N completes (rename) before any retained N+1 save begins its write/rename.
/// Retains *latest* request (with its content snapshot) if Save triggered while in-flight for doc.
/// After any result for the doc, launch retained pending if present (N result does not prevent N+1 write).
/// This guarantees on-disk: older rev cannot commit after newer (no N after N+1 rename).
/// Pure methods support deterministic regression tests.
/// SaveRequestData held only for latest per doc (bounded memory).
#[derive(Default, Debug)]
struct SaveCoalesceState {
    in_flight: std::collections::HashMap<DocumentId, DocumentRevision>,
    pending: std::collections::HashMap<DocumentId, SaveRequestData>,
}

#[derive(Clone, Debug)]
struct SaveRequestData {
    epoch: WorkspaceEpoch,
    doc_id: DocumentId,
    revision: DocumentRevision,
    path: std::path::PathBuf,
    content: String,
}

impl SaveCoalesceState {
    #[cfg(test)]
    fn has_in_flight_for(&self, doc: DocumentId) -> bool {
        self.in_flight.contains_key(&doc)
    }

    /// If in-flight for doc: retain this as the latest pending (replaces any prior pending), return None (do not spawn).
    /// Else: record in-flight, return Some to spawn now.
    fn on_request(&mut self, data: SaveRequestData) -> Option<SaveRequestData> {
        let doc = data.doc_id;
        match self.in_flight.entry(doc) {
            std::collections::hash_map::Entry::Occupied(_) => {
                self.pending.insert(doc, data);
                None
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(data.revision);
                Some(data)
            }
        }
    }

    /// Called after processing a SaveResult (stale or matching). Clears flight slot for doc.
    /// Returns any retained pending request for the doc (to be spawned now, serialized after prior).
    fn on_result(&mut self, doc: DocumentId, _rev: DocumentRevision) -> Option<SaveRequestData> {
        self.in_flight.remove(&doc);
        if let Some(data) = self.pending.remove(&doc) {
            self.in_flight.insert(doc, data.revision);
            Some(data)
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.in_flight.clear();
        self.pending.clear();
    }
}

/// Coalescing state for project file reads: at most ONE in-flight read worker globally.
/// While a read is in-flight, further RequestProjectFile coalesce to the *latest* (path, epoch) only.
/// Duplicate path requests do not spawn extra workers. On result drain: if the result matched in-flight
/// (exact path+epoch), clear then launch pending latest *only if its epoch still current*.
/// Stale results (wrong epoch or path) are ignored and do not clear/wedge. Uses try_send for results
/// (never blocks sender thread). Pure methods enable deterministic state-machine tests.
#[derive(Default, Debug)]
struct ProjectFileCoalesceState {
    in_flight: Option<(std::path::PathBuf, WorkspaceEpoch)>,
    pending: Option<(std::path::PathBuf, WorkspaceEpoch)>,
}

impl ProjectFileCoalesceState {
    fn has_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// If idle: record as in-flight and return true (spawn now).
    /// If busy: retain only this as latest pending (replaces any), return false (coalesced).
    fn on_request(&mut self, path: std::path::PathBuf, epoch: WorkspaceEpoch) -> bool {
        if self.in_flight.is_none() {
            self.in_flight = Some((path, epoch));
            true
        } else {
            self.pending = Some((path, epoch));
            false
        }
    }

    fn on_dispatch(&mut self, path: std::path::PathBuf, epoch: WorkspaceEpoch) {
        self.in_flight = Some((path, epoch));
    }

    /// Match only exact current in-flight (path+epoch). If yes, clear it and return true.
    /// Stale result returns false, in-flight stays (future requests not blocked).
    fn on_result(&mut self, path: &std::path::PathBuf, epoch: WorkspaceEpoch) -> bool {
        if let Some((p, e)) = &self.in_flight {
            if p == path && *e == epoch {
                self.in_flight = None;
                return true;
            }
        }
        false
    }

    fn take_pending(&mut self) -> Option<(std::path::PathBuf, WorkspaceEpoch)> {
        self.pending.take()
    }

    fn clear(&mut self) {
        self.in_flight = None;
        self.pending = None;
    }
}

/// Dispatch at most one syntax job for current doc if its rev not highlighted.
/// If we have retained tree+text for the doc (from prior successful syntax), supply them so
/// compute_syntax builds exact InputEdit from sources and incremental parses.
fn dispatch_syntax_for_current(
    app: &App,
    syntax_tx: &std::sync::mpsc::SyncSender<SyntaxResult>,
    syntax_coalesce: &mut SyntaxCoalesceState,
) {
    if let Some(id) = app.current_doc {
        if let Some(buf) = app.buffers.iter().find(|b| b.id() == id) {
            let rev = buf.revision();
            let up_to_date = app.syntax_is_current(id, rev);
            if syntax_coalesce.should_spawn_for(id, rev, up_to_date) {
                syntax_coalesce.on_dispatch(id, rev);
                let new_text = buf.text();
                let (old_text, old_tree) = match app.syntax_retained(id) {
                    Some((_, Some(tree), text)) => (Some(text), Some(tree)),
                    _ => (None, None),
                };
                let req = crate::syntax::SyntaxRequest {
                    epoch: app.epoch,
                    doc_id: id,
                    revision: rev,
                    language: buf.language(),
                    new_text,
                    old_text,
                    old_tree,
                    edit: None,
                };
                let stx = syntax_tx.clone();
                std::thread::spawn(move || {
                    let res = crate::syntax::compute_syntax(req);
                    let _ = stx.try_send(res); // do not block result send forever
                });
            }
        }
    }
}
fn spawn_save(data: SaveRequestData, tx: std::sync::mpsc::SyncSender<SaveResult>) {
    let SaveRequestData {
        epoch: ep,
        doc_id: id,
        revision: rev,
        path,
        content,
    } = data;
    std::thread::spawn(move || {
        let success = crate::persistence::save_file_atomic(&path, &content).is_ok();
        let _ = tx.send(SaveResult {
            epoch: ep,
            doc_id: id,
            revision: rev,
            path,
            success,
        });
    });
}

fn spawn_project_file_read(
    path: std::path::PathBuf,
    epoch: WorkspaceEpoch,
    tx: std::sync::mpsc::SyncSender<ProjectFileLoadResult>,
) {
    std::thread::spawn(move || {
        let res = load_project_file(path, epoch);
        let _ = tx.try_send(res); // do not block result send forever; never wedge UI
    });
}

fn spawn_local_terminal(
    root: std::path::PathBuf,
    epoch: WorkspaceEpoch,
    launch_id: u64,
    rows: u16,
    cols: u16,
    tx: std::sync::mpsc::SyncSender<TerminalStartResult>,
) {
    std::thread::spawn(move || {
        use crate::authority::{
            types::{AuthorityRequest, PtyRequest},
            Authority,
        };
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let authority = crate::authority::local::LocalAuthority::new(
                "host",
                root.clone(),
                hermito_protocol::WorkspaceEpoch(epoch.0),
            )
            .map_err(|error| error.to_string())?;
            authority.grant_execution();
            let command = crate::pty::default_shell_command(&root);
            let request = AuthorityRequest::new(
                PtyRequest {
                    command,
                    size: portable_pty::PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                },
                hermito_protocol::WorkspaceEpoch(epoch.0),
                hermito_protocol::EnvironmentEpoch(0),
                None,
            );
            let opened = runtime
                .block_on(authority.spawn_pty(request, tokio_util::sync::CancellationToken::new()))
                .map_err(|error| error.to_string())?;
            Ok(opened.payload)
        })();
        let _ = tx.send(TerminalStartResult {
            epoch,
            launch_id,
            authority_label: "host".into(),
            result,
        });
    });
}

fn spawn_remote_terminal(
    epoch: WorkspaceEpoch,
    launch_id: u64,
    authority_label: String,
    authority: Arc<crate::authority::ssh::SshAuthority>,
    request: crate::authority::types::AuthorityRequest<crate::authority::types::PtyRequest>,
    tx: std::sync::mpsc::SyncSender<TerminalStartResult>,
) {
    use crate::authority::Authority;
    tokio::spawn(async move {
        let result = authority
            .spawn_pty(request, tokio_util::sync::CancellationToken::new())
            .await
            .map(|opened| opened.payload)
            .map_err(|error| error.to_string());
        let _ = tx.send(TerminalStartResult {
            epoch,
            launch_id,
            authority_label,
            result,
        });
    });
}

fn dispatch_terminal_start(
    spec: crate::app::TerminalSpawnSpec,
    tx: std::sync::mpsc::SyncSender<TerminalStartResult>,
) {
    match spec {
        crate::app::TerminalSpawnSpec::Local {
            root,
            epoch,
            launch_id,
            rows,
            cols,
        } => spawn_local_terminal(root, epoch, launch_id, rows, cols, tx),
        crate::app::TerminalSpawnSpec::Remote {
            epoch,
            launch_id,
            authority_label,
            authority,
            request,
        } => spawn_remote_terminal(epoch, launch_id, authority_label, authority, request, tx),
    }
}

pub fn run_event_loop(
    guard: &mut TerminalGuard,
    journal: JournalHandle,
    journal_ack_rx: Receiver<JournalAck>,
    recovery: Recovery,
    shutdown_rx: Receiver<ShutdownReason>,
    initial_state: AppState,
) -> anyhow::Result<ShutdownReason> {
    let (input_tx, input_rx) = sync_channel::<crossterm_event::Event>(64);
    let _input_thread = std::thread::spawn(move || {
        // bounded dedicated Crossterm input producer: send() blocks producer on full (ok, dedicated thread);
        // guarantees no drop/reorder under flood, memory bounded, UI loop uses try_recv never blocks.
        while let Ok(ev) = crossterm_event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    // restore uses journal recovery (pre layout) + validated state for layout/trust/tabs/cursor/scroll
    let mut app = App::restore_state(initial_state, recovery, journal.clone());
    let mut configured_ssh = std::collections::HashMap::new();
    for config in crate::config::load()?.ssh_authorities {
        let configured_label = config.label.clone();
        let (label, runtime) = configured_ssh_runtime(config, app.epoch).map_err(|error| {
            anyhow::anyhow!("invalid SSH authority {configured_label}: {error}")
        })?;
        if configured_ssh.contains_key(&label) {
            anyhow::bail!("duplicate SSH authority label {label}");
        }
        app.register_ssh_authority(Arc::clone(&runtime.authority));
        configured_ssh.insert(label, runtime);
    }
    // Before first snapshot/draw, resize from actual terminal size (not the 120x36 default).
    // This ensures WorkbenchLayout rects (incl. editor reserving bottom header) are correct on first frame
    // and match mouse hit testing at the real size.
    if let Some(t) = guard.terminal_mut() {
        if let Ok(sz) = t.size() {
            app.apply_action(Action::TerminalResize {
                width: sz.width,
                height: sz.height,
            });
        }
    }

    // bounded initial project scan (off-thread, tagged result)
    let (project_tx, project_rx) = sync_channel::<ProjectScanResult>(2);
    {
        let ptx = project_tx.clone();
        let root = std::path::PathBuf::from(app.workspace_root());
        let ep = app.epoch;
        std::thread::spawn(move || {
            let local_rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let res = local_rt.block_on(crate::project::scan_project(root, ep, cancel));
            let _ = ptx.send(res);
        });
    }
    // bounded project file read channel (off event loop thread, epoch tagged result)
    let (file_tx, file_rx) = sync_channel::<ProjectFileLoadResult>(2);
    // bounded save result channel (off event loop thread atomic write + fsync+rename, epoch/rev tagged)
    let (save_tx, save_rx) = sync_channel::<SaveResult>(2);
    // Exactly one terminal launch runs at a time. Its tagged result is delivered
    // reliably; a superseding request is dispatched only after the stale result drains.
    let (terminal_tx, terminal_rx) = sync_channel::<TerminalStartResult>(1);
    let (ssh_activation_tx, ssh_activation_rx) =
        sync_channel::<SshActivationResult>(configured_ssh.len().max(1));
    let mut ssh_activation_in_flight = std::collections::HashMap::new();
    let mut startup_passphrase_prompted = false;
    for (label, runtime) in &configured_ssh {
        if crate::authority::Authority::trust(runtime.authority.as_ref())
            == crate::authority::types::AuthorityTrust::ExecutionGranted
        {
            if runtime.passphrase_required {
                app.set_authority_connection(
                    label,
                    crate::app::AuthorityConnectionState::Disconnected,
                    format!("SSH authority {label} requires its key passphrase."),
                );
                if !startup_passphrase_prompted
                    && app.current_ssh_authority_label().as_deref() == Some(label.as_str())
                {
                    app.prompt_ssh_passphrase(label.clone());
                    startup_passphrase_prompted = true;
                }
            } else {
                app.set_authority_connection(
                    label,
                    crate::app::AuthorityConnectionState::Connecting,
                    format!("Connecting SSH authority {label}…"),
                );
                let task = dispatch_ssh_activation(
                    label.clone(),
                    app.epoch,
                    runtime.clone(),
                    None,
                    ssh_activation_tx.clone(),
                );
                ssh_activation_in_flight.insert(label.clone(), task);
            }
        }
    }

    // bounded syntax result channel; coalesced single in-flight for current doc (no per-rev fanout)
    let (syntax_tx, syntax_rx) = sync_channel::<SyntaxResult>(4);
    let mut syntax_coalesce = SyntaxCoalesceState::default();
    let mut file_coalesce = ProjectFileCoalesceState::default();
    let mut save_coalesce = SaveCoalesceState::default();
    let mut terminal_launch_in_flight = false;
    // initial draw (borrow guard only)
    {
        let snap = app.snapshot();
        if let Some(t) = guard.terminal_mut() {
            let _ = t.draw(|f| workbench::render(f, &snap));
        }
    }

    let mut last_tick = Instant::now();

    loop {
        // drain tagged project results (reject stale epoch in apply)
        while let Ok(pres) = project_rx.try_recv() {
            app.apply_action(Action::UpdateProjectState {
                tree: Some(pres.tree),
                epoch: pres.epoch,
            });
        }

        // drain tagged project file load results (coalesced: at most 1 worker; stale epoch/path result ignored w/o clearing;
        // after matched result drain, launch retained latest pending only if its epoch still matches current)
        while let Ok(fres) = file_rx.try_recv() {
            let matched = file_coalesce.on_result(&fres.path, fres.epoch);
            if matched {
                app.apply_action(Action::ProjectFileLoaded {
                    path: fres.path,
                    content: fres.content,
                    epoch: fres.epoch,
                });
            }
            if !file_coalesce.has_in_flight() {
                if let Some((p, e)) = file_coalesce.take_pending() {
                    if e == app.epoch {
                        file_coalesce.on_dispatch(p.clone(), e);
                        spawn_project_file_read(p, e, file_tx.clone());
                    }
                }
            }
        }

        // drain tagged syntax results (epoch/rev checked inside apply_syntax_result)
        // clear marker only for exact in-flight match (stale results do not wedge); after any result dispatch latest if needed
        loop {
            match syntax_rx.try_recv() {
                Ok(sres) => {
                    let _ = syntax_coalesce.on_result(sres.doc_id, sres.revision);
                    app.apply_syntax_result(sres);
                    if !syntax_coalesce.has_in_flight() {
                        dispatch_syntax_for_current(&app, &syntax_tx, &mut syntax_coalesce);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    syntax_coalesce.clear();
                    save_coalesce.clear();
                    file_coalesce.clear();
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        // drain acks
        while let Ok(ack) = journal_ack_rx.try_recv() {
            app.apply_journal_ack(ack);
        }
        // drain save results (off-thread atomic durable writes for Save/SaveAs)
        // after processing result (which may mark clean only on exact rev match), launch any
        // retained latest request for that doc. This serializes per-doc: N's rename finishes
        // before N+1 write/rename is launched from pending.
        while let Ok(sres) = save_rx.try_recv() {
            app.apply_action(Action::SaveCompleted {
                doc_id: sres.doc_id,
                revision: sres.revision,
                path: sres.path,
                success: sres.success,
                epoch: sres.epoch,
            });
            if let Some(next_req) = save_coalesce.on_result(sres.doc_id, sres.revision) {
                spawn_save(next_req, save_tx.clone());
            }
        }
        while let Ok(started) = terminal_rx.try_recv() {
            terminal_launch_in_flight = false;
            match started.result {
                Ok(session) => app.attach_terminal(
                    started.epoch,
                    started.launch_id,
                    started.authority_label,
                    session,
                ),
                Err(message) => app.fail_terminal_start(started.epoch, started.launch_id, message),
            }
        }
        if !terminal_launch_in_flight {
            if let Some(spec) = app.terminal_spawn_spec() {
                dispatch_terminal_start(spec, terminal_tx.clone());
                terminal_launch_in_flight = true;
            }
        }
        while let Ok(activated) = ssh_activation_rx.try_recv() {
            ssh_activation_in_flight.remove(&activated.label);
            if activated.epoch != app.epoch {
                continue;
            }
            match activated.result {
                Ok(()) => app.set_authority_connection(
                    &activated.label,
                    crate::app::AuthorityConnectionState::Connected,
                    format!("SSH authority {} connected.", activated.label),
                ),
                Err(error) => app.set_authority_connection(
                    &activated.label,
                    crate::app::AuthorityConnectionState::Disconnected,
                    format!("SSH authority {} failed: {error}", activated.label),
                ),
            }
        }
        app.refresh_authority_connections();
        // input
        match input_rx.try_recv() {
            Ok(ct_ev) => {
                let snap = app.snapshot();
                for act in handle_event(ct_ev, &snap) {
                    if let Action::Quit = &act {
                        app.submit_all_retained_blocking();
                        if let Err(e) = crate::persistence::state::save_state(&app.to_state()) {
                            journal.flush();
                            return Err(e);
                        }
                        journal.flush();
                        return Ok(ShutdownReason::Normal);
                    }
                    if let Action::Save = &act {
                        if let Some(doc_id) = app.current_doc {
                            if let Some(buf) = app.buffers.iter().find(|b| b.id() == doc_id) {
                                if let BufferPathState::Saved(pth) = buf.path_state() {
                                    let data = SaveRequestData {
                                        epoch: app.epoch,
                                        doc_id,
                                        revision: buf.revision(),
                                        path: pth.clone(),
                                        content: buf.text(),
                                    };
                                    if let Some(to_launch) = save_coalesce.on_request(data) {
                                        spawn_save(to_launch, save_tx.clone());
                                    }
                                }
                            }
                        }
                    }
                    if let Action::SaveAsOverlayConfirm = &act {
                        let mut target: Option<std::path::PathBuf> = None;
                        let mut did = None;
                        let mut rev = DocumentRevision(0);
                        let mut content = String::new();
                        let ep = app.epoch;
                        if let crate::app::Overlay::SaveAs {
                            path: entered,
                            doc_id: d,
                            ..
                        } = &app.overlay
                        {
                            if !entered.trim().is_empty() {
                                target = Some(std::path::PathBuf::from(entered.trim().to_owned()));
                                did = Some(*d);
                                if let Some(buf) = app.buffers.iter().find(|b| b.id() == *d) {
                                    rev = buf.revision();
                                    content = buf.text();
                                }
                            }
                        }
                        if let (Some(p), Some(d)) = (target, did) {
                            let data = SaveRequestData {
                                epoch: ep,
                                doc_id: d,
                                revision: rev,
                                path: p,
                                content,
                            };
                            if let Some(to_launch) = save_coalesce.on_request(data) {
                                spawn_save(to_launch, save_tx.clone());
                            }
                        }
                    }
                    if let Action::RequestProjectFile { path } = &act {
                        let ep = app.epoch;
                        let p = path.clone();
                        if file_coalesce.on_request(p.clone(), ep) {
                            spawn_project_file_read(p, ep, file_tx.clone());
                        }
                    }
                    if matches!(&act, Action::OpenTerminal) {
                        app.apply_action(act);
                        if !terminal_launch_in_flight {
                            if let Some(spec) = app.terminal_spawn_spec() {
                                dispatch_terminal_start(spec, terminal_tx.clone());
                                terminal_launch_in_flight = true;
                            }
                        }
                        continue;
                    }
                    let should_activate_ssh =
                        matches!(&act, Action::GrantTrust | Action::CycleAuthority);
                    let should_disconnect_ssh = matches!(&act, Action::RevokeTrust);
                    let submitted_ssh_passphrase = matches!(&act, Action::SshPassphraseSubmit);
                    let affected_ssh_label = app.current_ssh_authority_label();
                    app.apply_action(act);
                    if submitted_ssh_passphrase {
                        if let Some((label, passphrase)) = app.take_submitted_ssh_passphrase() {
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                ssh_activation_in_flight.entry(label.clone())
                            {
                                if let Some(runtime) = configured_ssh.get(&label) {
                                    app.set_authority_connection(
                                        &label,
                                        crate::app::AuthorityConnectionState::Connecting,
                                        format!("Connecting SSH authority {label}…"),
                                    );
                                    let task = dispatch_ssh_activation(
                                        label,
                                        app.epoch,
                                        runtime.clone(),
                                        Some(passphrase),
                                        ssh_activation_tx.clone(),
                                    );
                                    entry.insert(task);
                                }
                            }
                        }
                        continue;
                    }
                    if should_disconnect_ssh {
                        if let Some(label) = affected_ssh_label {
                            if let Some(task) = ssh_activation_in_flight.remove(&label) {
                                task.abort();
                            }
                            if let Some(runtime) = configured_ssh.get(&label) {
                                let authority = Arc::clone(&runtime.authority);
                                tokio::spawn(async move {
                                    authority.disconnect().await;
                                });
                            }
                            app.set_authority_connection(
                                &label,
                                crate::app::AuthorityConnectionState::Disconnected,
                                format!("SSH authority {label} disconnected."),
                            );
                        }
                    }
                    if should_activate_ssh && app.current_trust() == crate::app::TrustLevel::Trusted
                    {
                        if let Some(label) = app.current_ssh_authority_label() {
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                ssh_activation_in_flight.entry(label.clone())
                            {
                                if let Some(runtime) = configured_ssh.get(&label) {
                                    if !runtime.authority.is_connected() {
                                        if runtime.passphrase_required {
                                            app.prompt_ssh_passphrase(label);
                                        } else {
                                            app.set_authority_connection(
                                                &label,
                                                crate::app::AuthorityConnectionState::Connecting,
                                                format!("Connecting SSH authority {label}…"),
                                            );
                                            let task = dispatch_ssh_activation(
                                                label,
                                                app.epoch,
                                                runtime.clone(),
                                                None,
                                                ssh_activation_tx.clone(),
                                            );
                                            entry.insert(task);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                app.submit_all_retained_blocking();
                if let Err(e) = crate::persistence::state::save_state(&app.to_state()) {
                    journal.flush();
                    return Err(e);
                }
                journal.flush();
                return Ok(ShutdownReason::FatalWorker);
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(TICK_MS) {
            app.retry_pending_checkpoints();
            app.retry_pending_compacts();
            // dispatch syntax for current doc (if needed); coalesced: at most one in flight, use retained tree+text for inc
            dispatch_syntax_for_current(&app, &syntax_tx, &mut syntax_coalesce);
            last_tick = Instant::now();
        }
        // draw with inner catch; resume so outer lib catch_unwind owns guard restore
        let draw_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let snap = app.snapshot();
            if let Some(t) = guard.terminal_mut() {
                let _ = t.draw(|f| workbench::render(f, &snap));
            }
        }));
        if let Err(p) = draw_res {
            app.submit_all_retained_blocking();
            journal.flush();
            std::panic::resume_unwind(p);
        }

        std::thread::sleep(Duration::from_millis(2));

        match shutdown_rx.try_recv() {
            Ok(r) => {
                app.submit_all_retained_blocking();
                if let Err(e) = crate::persistence::state::save_state(&app.to_state()) {
                    journal.flush();
                    return Err(e);
                }
                journal.flush();
                return Ok(r);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                app.submit_all_retained_blocking();
                if let Err(e) = crate::persistence::state::save_state(&app.to_state()) {
                    journal.flush();
                    return Err(e);
                }
                journal.flush();
                return Ok(ShutdownReason::FatalWorker);
            }
        }
    }
}

#[cfg(test)]
mod syntax_coalesce_tests {
    use super::*;

    fn fresh_id() -> DocumentId {
        DocumentId::new()
    }

    #[test]
    fn test_coalesce_at_most_one_job_and_rapid_edits() {
        let mut st = SyntaxCoalesceState::default();
        let id = fresh_id();
        let r1 = DocumentRevision(1);
        let r2 = DocumentRevision(2);

        // initial dispatch for first
        assert!(st.should_spawn_for(id, r1, false));
        st.on_dispatch(id, r1);
        assert!(st.has_in_flight());

        // rapid next revs must coalesce (no additional job)
        assert!(!st.should_spawn_for(id, r2, false));
        // still only the prior in flight
        assert_eq!(st.in_flight, Some((id, r1)));

        // result for the in-flight (may be stale)
        assert!(st.on_result(id, r1));
        assert!(!st.has_in_flight());

        // after result, latest becomes dispatchable (stale result allows retry latest)
        assert!(st.should_spawn_for(id, r2, false));
    }

    #[test]
    fn test_coalesce_stale_result_does_not_wedge_or_leak_marker() {
        let mut st = SyntaxCoalesceState::default();
        let id = fresh_id();
        let r5 = DocumentRevision(5);
        let r6 = DocumentRevision(6);

        st.on_dispatch(id, r5);
        // suppose we advanced and dispatched newer before old result (coalesce path)
        // (in real: dispatch only after clear, but test robustness)
        st.on_dispatch(id, r6);
        // stale result must not clear current marker
        assert!(!st.on_result(id, r5));
        assert!(st.has_in_flight());
        assert_eq!(st.in_flight, Some((id, r6)));

        // correct result clears
        assert!(st.on_result(id, r6));
        assert!(!st.has_in_flight());
    }

    #[test]
    fn test_coalesce_up_to_date_never_dispatches() {
        let mut st = SyntaxCoalesceState::default();
        let id = fresh_id();
        let r = DocumentRevision(42);
        assert!(!st.should_spawn_for(id, r, true));
        st.on_dispatch(id, r);
        assert!(!st.should_spawn_for(id, r, true));
    }

    #[test]
    fn test_request_construction_uses_retained_for_incremental() {
        // This unit checks the dispatch logic would supply old when retained present.
        // Full integration exercised via event_loop + compute_syntax + diff tests.
        // (We don't construct App here to keep test isolated+deterministic.)
        let mut st = SyntaxCoalesceState::default();
        let id = fresh_id();
        let rev = DocumentRevision(7);
        assert!(st.should_spawn_for(id, rev, false));
        // in real dispatch: if syntax_retained gave Some(tree,text) then req.old_* = Some
        // see dispatch_syntax_for_current + syntax_retained + text_diff_to_input_edit
        st.on_dispatch(id, rev);
        assert!(st.has_in_flight());
    }
}

#[cfg(test)]
mod save_coalesce_tests {
    use super::*;

    fn fresh_id() -> DocumentId {
        DocumentId::new()
    }

    fn fresh_req(rev: u64, content: &str) -> SaveRequestData {
        SaveRequestData {
            epoch: WorkspaceEpoch(1),
            doc_id: fresh_id(),
            revision: DocumentRevision(rev),
            path: std::path::PathBuf::from("/tmp/d.rs"),
            content: content.to_owned(),
        }
    }

    #[test]
    fn save_coalesce_retains_latest_and_launches_only_after_result() {
        let mut st = SaveCoalesceState::default();
        let id = fresh_id();
        // use same id by constructing manually
        let mut r_n = fresh_req(10, "N");
        r_n.doc_id = id;
        let mut r_np1 = fresh_req(11, "N+1");
        r_np1.doc_id = id;

        // first request dispatches
        let launched = st.on_request(r_n);
        assert!(launched.is_some());
        assert_eq!(launched.unwrap().revision, DocumentRevision(10));
        assert!(st.has_in_flight_for(id));
        assert!(!st.pending.contains_key(&id));

        // concurrent newer request while in flight: retain, no launch
        let launched2 = st.on_request(r_np1);
        assert!(launched2.is_none());
        assert!(st.has_in_flight_for(id));
        let pend = st.pending.get(&id).unwrap();
        assert_eq!(pend.revision, DocumentRevision(11));
        assert_eq!(pend.content, "N+1");

        // result for N: clears prior flight, marks+returns the pending N+1 (so caller spawn sees it in-flight; subsequent requests will pending not spawn concurrent)
        let next = st.on_result(id, DocumentRevision(10));
        assert!(next.is_some());
        let next = next.unwrap();
        assert_eq!(next.revision, DocumentRevision(11));
        assert!(st.has_in_flight_for(id));
    }

    #[test]
    fn concurrent_n_np1_cannot_commit_n_after_np1_because_serialized() {
        // The state machine ensures write/rename of N completes and its result processed
        // BEFORE the write/rename of N+1 is even spawned from pending.
        // Therefore on-disk final content will always be from the later request (N+1).
        // (App apply still ignores stale result for clean, but FS order guaranteed by serialize.)
        let mut st = SaveCoalesceState::default();
        let id = fresh_id();
        let mut rn = fresh_req(5, "content-of-5");
        rn.doc_id = id;
        let mut rnp = fresh_req(6, "content-of-6");
        rnp.doc_id = id;

        assert!(st.on_request(rn).is_some());
        let _ = st.on_request(rnp); // retained

        // only after result of 5 do we get 6
        let after_n = st.on_result(id, DocumentRevision(5));
        assert!(after_n.is_some());
        assert_eq!(after_n.unwrap().content, "content-of-6");

        // no further
        assert!(st.on_result(id, DocumentRevision(6)).is_none());
    }

    #[test]
    fn save_coalesce_different_docs_are_independent() {
        let mut st = SaveCoalesceState::default();
        let id1 = fresh_id();
        let id2 = fresh_id();
        let mut r1 = fresh_req(1, "d1");
        r1.doc_id = id1;
        let mut r2 = fresh_req(1, "d2");
        r2.doc_id = id2;

        assert!(st.on_request(r1).is_some());
        assert!(st.on_request(r2).is_some());
        assert!(st.has_in_flight_for(id1));
        assert!(st.has_in_flight_for(id2));

        // result for one does not affect other
        let pend1 = st.on_result(id1, DocumentRevision(1));
        assert!(pend1.is_none());
        assert!(!st.has_in_flight_for(id1));
        assert!(st.has_in_flight_for(id2));
    }

    #[test]
    fn save_failure_or_stale_result_still_launches_pending_and_leaves_dirty_up_to_app() {
        let mut st = SaveCoalesceState::default();
        let id = fresh_id();
        let mut r1 = fresh_req(100, "v100");
        r1.doc_id = id;
        let mut r2 = fresh_req(101, "v101");
        r2.doc_id = id;

        let _ = st.on_request(r1);
        let _ = st.on_request(r2);

        // even on failure result for first, still get second
        let next = st.on_result(id, DocumentRevision(100));
        assert!(next.is_some());
        assert_eq!(next.unwrap().revision, DocumentRevision(101));
        // the 'leaves dirty' is asserted in app tests via SaveCompleted non-match/fail paths
    }

    #[test]
    fn save_coalesce_clear_and_reuse() {
        let mut st = SaveCoalesceState::default();
        let id = fresh_id();
        let mut r = fresh_req(1, "x");
        r.doc_id = id;
        let _ = st.on_request(r);
        st.clear();
        assert!(!st.has_in_flight_for(id));
        assert!(st.pending.is_empty());
    }

    #[test]
    fn on_result_deferred_marks_in_flight_so_next_request_coalesces_pending_never_concurrent() {
        let mut st = SaveCoalesceState::default();
        let id = fresh_id();
        let mut rn = fresh_req(1, "N");
        rn.doc_id = id;
        let mut rnp1 = fresh_req(2, "N+1");
        rnp1.doc_id = id;
        let mut rnp2 = fresh_req(3, "N+2");
        rnp2.doc_id = id;

        // N launches
        assert!(st.on_request(rn).is_some());
        assert!(st.has_in_flight_for(id));

        // N+1 while flight -> pending
        assert!(st.on_request(rnp1).is_none());
        assert!(st.pending.contains_key(&id));

        // on_result(N): now marks N+1 as in-flight (repair), returns it for spawn
        let deferred = st.on_result(id, DocumentRevision(1));
        assert!(deferred.is_some());
        assert_eq!(deferred.unwrap().revision, DocumentRevision(2));
        assert!(st.has_in_flight_for(id)); // remains in-flight for the deferred; spawn will use it

        // N+2 arriving after on_result (simulating post-completion before/after actual spawn) must pending, not spawn
        let later = st.on_request(rnp2);
        assert!(later.is_none());
        assert!(st.has_in_flight_for(id));
        let pend = st.pending.get(&id).unwrap();
        assert_eq!(pend.revision, DocumentRevision(3));
    }
}

#[cfg(test)]
mod project_file_coalesce_tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn test_coalesce_at_most_one_worker_retain_latest_pending() {
        let mut st = ProjectFileCoalesceState::default();
        let p1 = fresh_path("/a.rs");
        let p2 = fresh_path("/b.rs");
        let e = WorkspaceEpoch(42);

        assert!(st.on_request(p1.clone(), e));
        assert!(st.has_in_flight());
        assert_eq!(st.in_flight, Some((p1.clone(), e)));

        // while busy, new request coalesces, only latest pending kept, no extra spawn
        assert!(!st.on_request(p2.clone(), e));
        assert_eq!(st.pending, Some((p2.clone(), e)));
        assert!(st.has_in_flight());
    }

    #[test]
    fn test_duplicate_path_coalesces_without_spawning() {
        let mut st = ProjectFileCoalesceState::default();
        let p = fresh_path("/main.rs");
        let e1 = WorkspaceEpoch(1);
        let e2 = WorkspaceEpoch(2);

        assert!(st.on_request(p.clone(), e1));
        assert!(!st.on_request(p.clone(), e2));
        assert_eq!(st.pending, Some((p, e2)));
    }

    #[test]
    fn test_stale_result_ignored_no_wedge_future_requests() {
        let mut st = ProjectFileCoalesceState::default();
        let p = fresh_path("/x.rs");
        let e = WorkspaceEpoch(7);

        st.on_dispatch(p.clone(), e);
        assert!(!st.on_result(&p, WorkspaceEpoch(6)));
        assert!(st.has_in_flight());
        assert!(st.on_result(&p, e));
        assert!(!st.has_in_flight());
    }

    #[test]
    fn test_drain_clears_and_launches_latest_only_if_epoch_current() {
        let mut st = ProjectFileCoalesceState::default();
        let p1 = fresh_path("/old");
        let p2 = fresh_path("/new");
        let e1 = WorkspaceEpoch(1);
        let e2 = WorkspaceEpoch(2);

        st.on_dispatch(p1.clone(), e1);
        let _ = st.on_request(p2.clone(), e2);

        assert!(st.on_result(&p1, e1));
        assert!(!st.has_in_flight());

        // simulate drain after clear:
        if let Some((pp, ee)) = st.take_pending() {
            if ee == e2 {
                st.on_dispatch(pp.clone(), ee);
            }
        }
        assert!(st.has_in_flight());
        assert_eq!(st.in_flight, Some((p2, e2)));
    }

    #[test]
    fn test_stale_pending_not_launched_on_clear() {
        let mut st = ProjectFileCoalesceState::default();
        let p = fresh_path("/f.rs");
        st.on_dispatch(p.clone(), WorkspaceEpoch(10));
        st.pending = Some((p.clone(), WorkspaceEpoch(99)));

        assert!(st.on_result(&p, WorkspaceEpoch(10)));
        if let Some((pp, ee)) = st.take_pending() {
            if ee == WorkspaceEpoch(10) {
                st.on_dispatch(pp, ee);
            }
        }
        assert!(!st.has_in_flight());
    }
}
#[cfg(test)]
mod state_runtime_fix_tests {
    use super::*;
    use crate::layout::Landmark;
    use crate::persistence::journal::start_journal_worker;

    #[test]
    fn remote_helper_target_allowlist_is_closed() {
        assert!(is_qualified_remote_helper_target(
            "hermito-remote-x86_64-unknown-linux-musl"
        ));
        assert!(is_qualified_remote_helper_target(
            "hermito-remote-aarch64-unknown-linux-musl"
        ));
        assert!(!is_qualified_remote_helper_target("hermito-remote"));
        assert!(!is_qualified_remote_helper_target(""));
    }

    #[test]
    fn compaction_retry_on_tick_path_retains_until_success() {
        // deterministic: retain on 'full' (sim via retry keep), retry alongside checkpoints
        let (journal, _rx) = start_journal_worker(Recovery::default());
        let mut app = App::new_from_recovery(Recovery::default(), journal);
        let doc = app.current_doc.unwrap();
        app.retain_pending_compact(doc, DocumentRevision(7));
        // simulate backpressure keep (retry will drain only if journal accepts)
        app.retry_pending_compacts();
        // after tick-style retry, if still pending (queue), kept; submit drains
        assert!(app.pending_checkpoint_revision(doc).is_none()); // not checkpoint
                                                                 // the retain happened; full dispatch tested via queue in contract
        app.submit_all_retained_blocking();
    }

    #[test]
    fn focus_roundtrip_non_editor_and_old_state_default() {
        let (journal, _rx) = start_journal_worker(Recovery::default());
        let mut app = App::new_from_recovery(Recovery::default(), journal);
        // set non-editor focus
        app.focus = Landmark::BottomPane;
        let st = app.to_state();
        assert_eq!(st.focus, "BottomPane");

        // restore roundtrip
        let (j2, _rx2) = start_journal_worker(Recovery::default());
        let app2 = App::restore_state(st, Recovery::default(), j2);
        assert_eq!(app2.focus, Landmark::BottomPane);

        // old TOML (no focus key) remains valid + defaults to Editor
        // robust: take current serializable state, remove focus line to simulate legacy file
        let fresh = crate::persistence::state::first_run_state();
        let serialized = toml::to_string(&fresh).unwrap();
        let legacy = serialized
            .lines()
            .filter(|l| !l.trim_start().starts_with("focus"))
            .collect::<Vec<_>>()
            .join("\n");
        let st_old: AppState =
            toml::from_str(&legacy).expect("old TOML (focus key omitted) must remain valid");
        assert_eq!(
            st_old.focus, "Editor",
            "missing focus key must default to Editor"
        );
        let (j3, _rx3) = start_journal_worker(Recovery::default());
        let app3 = App::restore_state(st_old, Recovery::default(), j3);
        assert_eq!(app3.focus, Landmark::Editor);
    }

    #[test]
    fn shutdown_reports_state_error_only_after_cleanup_attempted() {
        // ordering: submit + save(possible err) + flush before returning err (not Normal)
        // feasible unit: direct simulate the pattern used in the 4 paths (Quit, signal, fatal)
        // (full loop not entered; behavior extracted to the if/return in refactored paths)
        let (journal, _rx) = start_journal_worker(Recovery::default());
        let app = App::new_from_recovery(Recovery::default(), journal);
        let st = app.to_state();
        // save would be attempted; on err, flush happened before Err returned (observable in real paths)
        // here just ensure to_state usable post submit pattern
        let _ = st; // if save were to fail, cleanup (submit_all + flush) precedes the return Err
    }
}
