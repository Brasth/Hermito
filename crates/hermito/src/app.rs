use crate::action::Action;
use crate::buffer::{Buffer, CheckpointPayload};
use crate::document::{BufferPathState, DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::layout::{EditorTabState, Landmark, WorkbenchLayout};
use crate::persistence::journal::{JournalAck, JournalHandle, RecoveredBuffer, Recovery};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tree_sitter::Tree;
use zeroize::Zeroizing;

const LSP_SUPERVISOR_EVENT_CHANNEL_CAPACITY: usize = 64;
const LSP_RUNTIME_EVENT_CHANNEL_CAPACITY: usize = 64;
const LSP_SESSION_COMMAND_CHANNEL_CAPACITY: usize = 32;
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustLevel {
    Trusted,
    InspectOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthorityKind {
    Local,
    Ssh,
    DevContainer,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum AuthorityConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Lost,
}

#[derive(Clone, Debug)]
pub struct AuthorityState {
    pub kind: AuthorityKind,
    pub label: String,
    pub trust: TrustLevel,
    pub connection: AuthorityConnectionState,
}

#[derive(Clone, Debug)]
pub struct EditorTabSnapshot {
    pub id: DocumentId,
    pub revision: DocumentRevision,
    pub title: String,
    pub path_label: String,
    pub language: Language,
    pub text: String,
    pub dirty: bool,
    pub cursor_byte: usize,
    pub selection: Option<(usize, usize)>,
    pub scroll_line: u16,
    pub highlights: Vec<crate::syntax::highlight::HighlightSpan>,
}
#[derive(Clone, Debug)]
pub struct CompletionCandidateSnapshot {
    pub label: String,
    pub detail: Option<String>,
}


#[derive(Clone, Debug)]
pub enum OverlaySnapshot {
    None,
    TrustReview {
        workspace_root: String,
        authority_label: String,
        trust: TrustLevel,
        capabilities: Vec<String>,
        focused_grant: bool,
        invoker: Landmark,
    },
    CommandPalette {
        query: String,
        items: Vec<String>,
        selected: usize,
        invoker: Landmark,
    },
    SaveAs {
        path: String,
        invoker: Landmark,
    },
    RenameInput {
        new_name: String,
        invoker: Landmark,
    },
    SshPassphrase {
        authority_label: String,
        length: usize,
        invoker: Landmark,
    },
    Completion {
        position: lsp_types::Position,
        candidates: Vec<CompletionCandidateSnapshot>,
        selected: usize,
        invoker: Landmark,
    },
    Hover {
        document: String,
        invoker: Landmark,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ProjectTreeSnapshot {
    pub tree: Option<crate::project::tree::ProjectTree>,
    pub loading: bool,
    pub scroll: u16,
    pub selected_row: u16,
}

#[derive(Clone, Debug, Default)]
pub struct StatusSnapshot {
    pub view: String,
    pub branch: Option<String>,
    pub problems: usize,
    pub service: String,
    pub message: Option<String>,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalViewState {
    #[default]
    None,
    Starting,
    Running,
    Exited,
    Lost,
}

#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub state: TerminalViewState,
    pub surface: Option<Arc<RwLock<crate::terminal::TerminalSurface>>>,
    pub captured: bool,
    pub authority_label: String,
}

pub(crate) enum TerminalSpawnSpec {
    Local {
        root: std::path::PathBuf,
        epoch: WorkspaceEpoch,
        launch_id: u64,
        rows: u16,
        cols: u16,
        lsp_grants: Vec<crate::persistence::state::LspGrantRecord>,
    },
    Remote {
        epoch: WorkspaceEpoch,
        launch_id: u64,
        authority_label: String,
        authority: Arc<crate::authority::ssh::SshAuthority>,
        request: crate::authority::types::AuthorityRequest<crate::authority::types::PtyRequest>,
    },
}

struct AppLspTransport(Box<dyn crate::lsp::LspTransport>);

impl crate::lsp::LspTransport for AppLspTransport {
    fn send(
        &self,
        message: hermito_protocol::lsp::LspV1,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), crate::lsp::LspClientError>> + Send + '_>,
    > {
        self.0.send(message)
    }

    fn recv(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<hermito_protocol::lsp::LspV1, crate::lsp::LspClientError>,
                > + Send
                + '_,
        >,
    > {
        self.0.recv()
    }
}
fn send_workspace_edit_result(
    client: Arc<crate::lsp::LspClient<AppLspTransport>>,
    context: hermito_protocol::lsp::LspContext,
    request_id: hermito_protocol::lsp::LspRequestId,
    applied: bool,
    reason: Option<&'static str>,
) {
    tokio::spawn(async move {
        let _ = tokio::time::timeout(
            LSP_REQUEST_TIMEOUT,
            client.workspace_edit_result(context, request_id, applied, reason.map(str::to_owned)),
        )
        .await;
    });
}


#[derive(Clone)]
struct LspChange {
    context: hermito_protocol::lsp::LspContext,
    uri: String,
    version: i32,
    text: String,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LspProviderKind {
    Completion,
    Hover,
    Definition,
    RenamePreparation,
    Rename,
}

#[derive(Clone)]
struct LspProviderSnapshot {
    context: hermito_protocol::lsp::LspContext,
    uri: String,
    revision: DocumentRevision,
    ledger: crate::lsp::LspDocumentLedger,
    config_digest: String,
    service_state: crate::lsp::LanguageServiceState,
}

pub(crate) enum LspProviderResult {
    ReadOnly(crate::lsp::ProviderOutcome),
    RenamePrepared(Option<crate::lsp::RenamePreparation>),
    RenameWorkspaceEdit(crate::lsp::RenameRequestOutcome),
}

enum LspSessionCommand {
    Change(LspChange),
    Provider {
        kind: LspProviderKind,
        snapshot: LspProviderSnapshot,
        position: lsp_types::Position,
    },
    Rename {
        snapshot: LspProviderSnapshot,
        preparation: crate::lsp::RenamePreparation,
        position: lsp_types::Position,
        new_name: String,
    },
}

struct LspSession {
    command_tx: tokio::sync::mpsc::Sender<LspSessionCommand>,
    cancellation: tokio_util::sync::CancellationToken,
    context: hermito_protocol::lsp::LspContext,
    pending_change: Option<LspChange>,
    config_digest: String,
}

pub(crate) enum LspRuntimeEvent {
    State {
        key: crate::lsp::SupervisorKey,
        context: hermito_protocol::lsp::LspContext,
        state: crate::lsp::LanguageServiceState,
    },
    Initialized {
        key: crate::lsp::SupervisorKey,
        context: hermito_protocol::lsp::LspContext,
    },
    TransportLoss {
        key: crate::lsp::SupervisorKey,
        context: hermito_protocol::lsp::LspContext,
        detail: String,
    },
    Restart {
        key: crate::lsp::SupervisorKey,
        generation: hermito_protocol::lsp::SessionGeneration,
    },
    Diagnostics {
        key: crate::lsp::SupervisorKey,
        document_id: DocumentId,
        context: hermito_protocol::lsp::LspContext,
        uri: String,
        version: Option<i32>,
        diagnostics: Vec<hermito_protocol::lsp::LspDiagnostic>,
    },
    WorkspaceEdit {
        key: crate::lsp::SupervisorKey,
        document_id: DocumentId,
        context: hermito_protocol::lsp::LspContext,
        request_id: hermito_protocol::lsp::LspRequestId,
        edit: hermito_protocol::lsp::TransactionalWorkspaceEdit,
        client: Arc<crate::lsp::LspClient<AppLspTransport>>,
    },
    ProviderResult {
        key: crate::lsp::SupervisorKey,
        document_id: DocumentId,
        context: hermito_protocol::lsp::LspContext,
        revision: DocumentRevision,
        position: lsp_types::Position,
        kind: LspProviderKind,
        result: Result<LspProviderResult, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemDiagnostic {
    pub document_id: DocumentId,
    pub uri: String,
    pub range: lsp_types::Range,
    pub severity: Option<crate::lsp::NormalizedDiagnosticSeverity>,
    pub message: String,
    pub source: Option<String>,
}

/// Restore only grants scoped to this exact workspace and canonical authority
/// identity. UI labels are intentionally excluded from this trust boundary.
pub(crate) fn apply_persisted_lsp_grants<A: crate::authority::Authority + ?Sized>(
    grants: &[crate::persistence::state::LspGrantRecord],
    workspace_root: &std::path::Path,
    authority: &A,
) {
    let authority_id = crate::authority::Authority::host_authority_id(authority);
    for grant in grants {
        if grant.authority == authority_id && grant.workspace_root.as_path() == workspace_root {
            crate::authority::Authority::grant_lsp_execution(authority, &grant.config_digest);
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppSnapshot {
    pub epoch: WorkspaceEpoch,
    pub layout: WorkbenchLayout,
    pub focus: Landmark,
    pub overlay: OverlaySnapshot,
    pub authorities: Vec<AuthorityState>,
    pub current_authority_idx: usize,
    pub current_trust: TrustLevel,
    pub current_buffer: Option<EditorTabSnapshot>,
    pub open_editor_tabs: Vec<EditorTabSnapshot>,
    pub active_editor_tab: usize,
    pub project: ProjectTreeSnapshot,
    pub status: StatusSnapshot,
    pub terminal: TerminalSnapshot,
    pub journal_lagging: bool,
    pub workspace_root: String,
    pub workspace_name: String,
    pub diagnostics: Vec<ProblemDiagnostic>,
}

pub struct App {
    pub epoch: WorkspaceEpoch,
    pub layout: WorkbenchLayout,
    pub buffers: Vec<Buffer>,
    pub current_doc: Option<DocumentId>,
    pub authorities: Vec<AuthorityState>,
    pub current_authority: usize,
    pub focus: Landmark,
    pending_checkpoints: std::collections::HashMap<DocumentId, CheckpointPayload>,
    pending_compactions: std::collections::HashMap<DocumentId, DocumentRevision>,
    pub(crate) overlay: Overlay,
    journal: Option<JournalHandle>,
    syntax_highlights: std::collections::HashMap<
        DocumentId,
        (
            DocumentRevision,
            Vec<crate::syntax::highlight::HighlightSpan>,
        ),
    >,
    retained_syntax:
        std::collections::HashMap<DocumentId, (DocumentRevision, Option<Tree>, String)>,
    project: ProjectTreeSnapshot,
    status_message: String,
    workspace_root: String,
    workspace_name: String,
    terminal: Option<crate::pty::PtySession>,
    ssh_authorities: std::collections::HashMap<String, Arc<crate::authority::ssh::SshAuthority>>,
    lsp_grants: Vec<crate::persistence::state::LspGrantRecord>,
    language_service_states:
        std::collections::HashMap<crate::lsp::SupervisorKey, crate::lsp::LanguageServiceState>,
    lsp_supervisor: crate::lsp::LspSupervisor,
    lsp_event_rx: tokio::sync::mpsc::Receiver<crate::lsp::SupervisorEvent>,
    language_diagnostic_counts: std::collections::HashMap<crate::lsp::SupervisorKey, usize>,
    language_servers: Vec<crate::config::LanguageServerConfig>,
    local_authority: Option<Arc<crate::authority::local::LocalAuthority>>,
    lsp_sessions: std::collections::HashMap<(crate::lsp::SupervisorKey, DocumentId), LspSession>,
    lsp_runtime_tx: tokio::sync::mpsc::Sender<LspRuntimeEvent>,
    lsp_runtime_rx: tokio::sync::mpsc::Receiver<LspRuntimeEvent>,
    diagnostics: std::collections::HashMap<
        (crate::lsp::SupervisorKey, DocumentId),
        Vec<ProblemDiagnostic>,
    >,
    terminal_starting: bool,
    terminal_launch_id: u64,
    terminal_capture: bool,
    submitted_ssh_passphrase: Option<(String, Zeroizing<Vec<u8>>)>,
    pending_definition: Option<PendingDefinition>,
    pending_definition_open: Option<std::path::PathBuf>,
}
#[derive(Clone, Debug)]
struct CompletionCandidate {
    label: String,
    detail: Option<String>,
    replacement: String,
}

#[derive(Clone, Debug)]
struct PendingDefinition {
    path: std::path::PathBuf,
    range: lsp_types::Range,
}


pub(crate) enum Overlay {
    None,
    TrustReview {
        focused_grant: bool,
        invoker: Landmark,
        workspace_root: String,
        authority_label: String,
    },
    CommandPalette {
        query: String,
        items: Vec<String>,
        selected: usize,
        invoker: Landmark,
    },
    SaveAs {
        path: String,
        invoker: Landmark,
        doc_id: DocumentId,
    },
    RenameInput {
        document_id: DocumentId,
        key: crate::lsp::SupervisorKey,
        context: hermito_protocol::lsp::LspContext,
        position: lsp_types::Position,
        preparation: crate::lsp::RenamePreparation,
        new_name: String,
        invoker: Landmark,
    },
    SshPassphrase {
        authority_label: String,
        passphrase: Zeroizing<String>,
        invoker: Landmark,
    },
    Completion {
        document_id: DocumentId,
        revision: DocumentRevision,
        position: lsp_types::Position,
        candidates: Vec<CompletionCandidate>,
        selected: usize,
        invoker: Landmark,
    },
    Hover {
        document: String,
        invoker: Landmark,
    },
}

fn document_uri(buffer: &Buffer, document_id: DocumentId) -> String {
    buffer
        .path()
        .and_then(|path| url::Url::from_file_path(path).ok())
        .map(|url| url.into())
        .unwrap_or_else(|| format!("untitled:{document_id:?}"))
}

struct WorkspaceEditTarget {
    uri: String,
    relative_path: std::path::PathBuf,
    buffer_index: Option<usize>,
}

fn workspace_edit_target_path(
    uri: &str,
    root: &std::path::Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf), crate::lsp::ProviderError> {
    let path = url::Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| crate::lsp::ProviderError::UnresolvableWorkspaceDocument {
            uri: uri.to_owned(),
        })?;
    let relative_path = path
        .strip_prefix(root)
        .ok()
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|path| {
            path.components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .map(std::path::PathBuf::from)
        .ok_or_else(|| crate::lsp::ProviderError::UnresolvableWorkspaceDocument {
            uri: uri.to_owned(),
        })?;
    Ok((path, relative_path))
}

fn buffers_at_indices_mut<'a>(
    buffers: &'a mut [Buffer],
    indices: &[usize],
    base_index: usize,
) -> Option<Vec<&'a mut Buffer>> {
    let (&index, remaining) = indices.split_first()?;
    let offset = index.checked_sub(base_index)?;
    let (_, from_index) = buffers.split_at_mut(offset);
    let (buffer, after) = from_index.split_first_mut()?;
    let mut selected = vec![buffer];
    if !remaining.is_empty() {
        selected.extend(buffers_at_indices_mut(after, remaining, index.checked_add(1)?)?);
    }
    Some(selected)
}

fn spawn_lsp_session<A: crate::authority::Authority + 'static>(
    authority: Arc<A>,
    key: crate::lsp::SupervisorKey,
    document_id: DocumentId,
    context: hermito_protocol::lsp::LspContext,
    uri: String,
    language_id: String,
    text: String,
    effective_config: crate::config::EffectiveLspConfig,
    mut commands: tokio::sync::mpsc::Receiver<LspSessionCommand>,
    runtime_events: tokio::sync::mpsc::Sender<LspRuntimeEvent>,
    cancellation: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let initialization_options = effective_config.initialization_options.clone();
        let expected_constraint = effective_config
            .expected_version
            .clone()
            .or_else(|| effective_config.expected_digest.clone())
            .unwrap_or_default();
        let transport = match crate::authority::Authority::start_lsp(
            authority.as_ref(),
            context.clone(),
            effective_config,
            cancellation.clone(),
        )
        .await
        {
            Ok(transport) => transport,
            Err(error) => {
                let message = error.to_string();
                let state = if message.contains("LSP_VERSION_MISMATCH:") {
                    crate::lsp::LanguageServiceState::VersionMismatch {
                        expected: expected_constraint,
                        actual: message,
                    }
                } else {
                    crate::lsp::LanguageServiceState::Failed { message }
                };
                let _ = runtime_events.try_send(LspRuntimeEvent::State {
                    key,
                    context,
                    state,
                });
                return;
            }
        };
        let client = Arc::new(crate::lsp::LspClient::new(
            AppLspTransport(transport),
            context.authority_identity.clone(),
            LSP_REQUEST_TIMEOUT,
            crate::lsp::VersionlessPolicy::SafeDiscard,
            cancellation.clone(),
        ));
        let receive_client = Arc::clone(&client);
        let receive_events = runtime_events.clone();
        let receive_key = key.clone();
        let receive_context = context.clone();
        let receive_cancellation = cancellation.clone();
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    _ = receive_cancellation.cancelled() => return,
                    result = receive_client.recv() => match result {
                        Ok(message) => message,
                        Err(error) => {
                            let _ = receive_events.try_send(LspRuntimeEvent::TransportLoss {
                                key: receive_key.clone(),
                                context: receive_context.clone(),
                                detail: error.to_string(),
                            });
                            receive_cancellation.cancel();
                            return;
                        }
                    }
                };
                if matches!(message, hermito_protocol::lsp::LspV1::Exited { .. }) {
                    let _ = receive_events.try_send(LspRuntimeEvent::TransportLoss {
                        key: receive_key.clone(),
                        context: receive_context.clone(),
                        detail: "language server session exited".into(),
                    });
                    receive_cancellation.cancel();
                    return;
                }
                match receive_client.handle_incoming(message).await {
                    Ok(crate::lsp::Incoming::PublishDiagnostics {
                        context,
                        uri,
                        version,
                        diagnostics,
                    }) => {
                        let _ = receive_events.try_send(LspRuntimeEvent::Diagnostics {
                            key: receive_key.clone(),
                            document_id,
                            context,
                            uri,
                            version,
                            diagnostics,
                        });
                    }
                    Ok(crate::lsp::Incoming::WorkspaceEdit {
                        context,
                        request_id,
                        edit,
                    }) => {
                        let event = LspRuntimeEvent::WorkspaceEdit {
                            key: receive_key.clone(),
                            document_id,
                            context,
                            request_id,
                            edit,
                            client: Arc::clone(&receive_client),
                        };
                        if let Err(error) = receive_events.try_send(event) {
                            let reason = match &error {
                                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                    "workspace edit queue is busy"
                                }
                                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                    "workspace edit handler is unavailable"
                                }
                            };
                            let LspRuntimeEvent::WorkspaceEdit {
                                client,
                                context,
                                request_id,
                                ..
                            } = error.into_inner()
                            else {
                                unreachable!("workspace edit event was just constructed");
                            };
                            send_workspace_edit_result(
                                client,
                                context,
                                request_id,
                                false,
                                Some(reason),
                            );
                        }
                    }
                    Ok(_) | Err(crate::lsp::LspClientError::Stale(_)) => {}
                    Err(error) => {
                        let _ = receive_events.try_send(LspRuntimeEvent::TransportLoss {
                            key: receive_key.clone(),
                            context: receive_context.clone(),
                            detail: error.to_string(),
                        });
                        receive_cancellation.cancel();
                        return;
                    }
                }
            }
        });
        if let Err(error) = client
            .initialize(
                context.clone(),
                None,
                serde_json::json!({}),
                initialization_options,
            )
            .await
        {
            let _ = runtime_events.try_send(LspRuntimeEvent::State {
                key,
                context: context.clone(),
                state: crate::lsp::LanguageServiceState::Failed {
                    message: error.to_string(),
                },
            });
            cancellation.cancel();
            return;
        }
        if let Err(error) = client
            .did_open(
                context.clone(),
                &uri,
                &language_id,
                context.sent_version.0 as i32,
                &text,
            )
            .await
        {
            let _ = runtime_events.try_send(LspRuntimeEvent::State {
                key,
                context: context.clone(),
                state: crate::lsp::LanguageServiceState::Failed {
                    message: error.to_string(),
                },
            });
            cancellation.cancel();
            return;
        }
        let _ = runtime_events.try_send(LspRuntimeEvent::Initialized {
            key: key.clone(),
            context: context.clone(),
        });
        while let Some(command) = commands.recv().await {
            match command {
                LspSessionCommand::Change(change) => {
                    let context = change.context.clone();
                    if let Err(error) = client
                        .did_change(change.context, &change.uri, change.version, &change.text)
                        .await
                    {
                        let _ = runtime_events.try_send(LspRuntimeEvent::State {
                            key: key.clone(),
                            context,
                            state: crate::lsp::LanguageServiceState::Failed {
                                message: error.to_string(),
                            },
                        });
                        cancellation.cancel();
                        return;
                    }
                }
                LspSessionCommand::Provider {
                    kind,
                    snapshot,
                    position,
                } => {
                    let result = {
                        let request = crate::lsp::requests::ProviderSnapshotRequest {
                            authority: authority.as_ref(),
                            service: &key,
                            service_state: Some(snapshot.service_state.clone()),
                            config_digest: &snapshot.config_digest,
                            context: snapshot.context.clone(),
                            uri: &snapshot.uri,
                            revision: snapshot.revision,
                            ledger: &snapshot.ledger,
                        };
                        match kind {
                            LspProviderKind::Completion => {
                                crate::lsp::CompletionProvider::complete_snapshot(
                                    client.as_ref(),
                                    &request,
                                    position,
                                )
                                .await
                                .map(LspProviderResult::ReadOnly)
                            }
                            LspProviderKind::Hover => crate::lsp::HoverProvider::hover_snapshot(
                                client.as_ref(),
                                &request,
                                position,
                            )
                            .await
                            .map(LspProviderResult::ReadOnly),
                            LspProviderKind::Definition => {
                                crate::lsp::DefinitionProvider::definition_snapshot(
                                    client.as_ref(),
                                    &request,
                                    position,
                                )
                                .await
                                .map(LspProviderResult::ReadOnly)
                            }
                            LspProviderKind::RenamePreparation => {
                                crate::lsp::RenameProvider::prepare_snapshot(
                                    client.as_ref(),
                                    &request,
                                    position,
                                )
                                .await
                                .map(LspProviderResult::RenamePrepared)
                            }
                            LspProviderKind::Rename => unreachable!("rename has its own command"),
                        }
                        .map_err(|error| error.to_string())
                    };
                    let _ = runtime_events.try_send(LspRuntimeEvent::ProviderResult {
                        key: key.clone(),
                        document_id,
                        context: snapshot.context,
                        revision: snapshot.revision,
                        position,
                        kind,
                        result,
                    });
                }
                LspSessionCommand::Rename {
                    snapshot,
                    preparation,
                    position,
                    new_name,
                } => {
                    let request = crate::lsp::requests::ProviderSnapshotRequest {
                        authority: authority.as_ref(),
                        service: &key,
                        service_state: Some(snapshot.service_state.clone()),
                        config_digest: &snapshot.config_digest,
                        context: snapshot.context.clone(),
                        uri: &snapshot.uri,
                        revision: snapshot.revision,
                        ledger: &snapshot.ledger,
                    };
                    let result = crate::lsp::RenameProvider::request_rename_snapshot(
                        client.as_ref(),
                        &request,
                        &preparation,
                        position,
                        &new_name,
                    )
                    .await
                    .map(LspProviderResult::RenameWorkspaceEdit)
                    .map_err(|error| error.to_string());
                    let _ = runtime_events.try_send(LspRuntimeEvent::ProviderResult {
                        key: key.clone(),
                        document_id,
                        context: snapshot.context,
                        revision: snapshot.revision,
                        position,
                        kind: LspProviderKind::Rename,
                        result,
                    });
                }
            }
        }
        cancellation.cancel();
    });
}

fn completion_candidates(payload: &serde_json::Value) -> Vec<CompletionCandidate> {
    let items = payload
        .as_array()
        .or_else(|| payload.get("items").and_then(serde_json::Value::as_array))
        .into_iter()
        .flatten();
    items
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_owned();
            Some(CompletionCandidate {
                replacement: item
                    .get("insertText")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&label)
                    .to_owned(),
                detail: item
                    .get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                label,
            })
        })
        .collect()
}

fn hover_document(payload: &serde_json::Value) -> Option<String> {
    let contents = payload.get("contents")?;
    match contents {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(hover_document_part)
                .collect::<Vec<_>>()
                .join("\n\n");
            (!text.is_empty()).then_some(text)
        }
        _ => hover_document_part(contents),
    }
}

fn hover_document_part(part: &serde_json::Value) -> Option<String> {
    part.as_str()
        .map(str::to_owned)
        .or_else(|| part.get("value")?.as_str().map(str::to_owned))
}

fn definition_target(payload: &serde_json::Value) -> Option<(url::Url, lsp_types::Range)> {
    let target = payload
        .as_array()
        .and_then(|locations| locations.first())
        .unwrap_or(payload);
    let uri = target
        .get("uri")
        .or_else(|| target.get("targetUri"))?
        .as_str()?
        .parse()
        .ok()?;
    let range = target
        .get("range")
        .or_else(|| target.get("targetSelectionRange"))
        .or_else(|| target.get("targetRange"))?;
    serde_json::from_value(range.clone()).ok().map(|range| (uri, range))
}

fn command_palette_items(trust: TrustLevel, query: &str) -> Vec<String> {
    let mut items = vec!["Review authority trust…".to_string()];
    if trust == TrustLevel::Trusted {
        items.push("Revoke authority trust".to_string());
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        items
    } else {
        items
            .into_iter()
            .filter(|item| item.to_lowercase().contains(&query))
            .collect()
    }
}

impl App {
    /// Construct from startup recovery (journal first, before any authority use).
    pub fn new_from_recovery(recovery: Recovery, journal: JournalHandle) -> Self {
        let epoch = WorkspaceEpoch(0);
        let mut buffers = Vec::new();
        for RecoveredBuffer {
            id,
            revision,
            content,
            path,
            language,
        } in recovery.buffers
        {
            let path_state = match path {
                Some(p) => BufferPathState::Recovered { original: p },
                None => BufferPathState::Untitled {
                    suggested_name: "recovered".into(),
                },
            };
            let b = Buffer::recover(id, language, &content, revision, path_state);
            buffers.push(b);
        }
        if buffers.is_empty() {
            let id = DocumentId::new();
            let buffer = Buffer::new(id, crate::document::Language::PlainText, "fn main() {}\n");
            buffers.push(buffer);
        }
        let current_doc = buffers.first().map(|b| b.id());
        let mut layout = WorkbenchLayout::default();
        if let Some(id) = current_doc {
            layout.open_or_focus_editor(id);
        }
        let (lsp_supervisor, lsp_event_rx) =
            crate::lsp::LspSupervisor::channel(LSP_SUPERVISOR_EVENT_CHANNEL_CAPACITY);
        let (lsp_runtime_tx, lsp_runtime_rx) =
            tokio::sync::mpsc::channel(LSP_RUNTIME_EVENT_CHANNEL_CAPACITY);
        App {
            epoch,
            layout,
            buffers,
            current_doc,
            authorities: vec![AuthorityState {
                kind: AuthorityKind::Local,
                label: "host".into(),
                trust: TrustLevel::InspectOnly,
                connection: AuthorityConnectionState::Connected,
            }],
            current_authority: 0,
            focus: Landmark::Editor,
            overlay: Overlay::None,
            journal: Some(journal),
            pending_checkpoints: std::collections::HashMap::new(),
            pending_compactions: std::collections::HashMap::new(),
            syntax_highlights: std::collections::HashMap::new(),
            retained_syntax: std::collections::HashMap::new(),
            project: ProjectTreeSnapshot {
                tree: None,
                loading: true,
                scroll: 0,
                selected_row: 0,
            },
            status_message: String::new(),
            workspace_root: "/workspace".into(),
            workspace_name: "workspace".into(),
            terminal: None,
            ssh_authorities: std::collections::HashMap::new(),
            lsp_grants: vec![],
            language_service_states: std::collections::HashMap::new(),
            language_diagnostic_counts: std::collections::HashMap::new(),
            language_servers: Vec::new(),
            local_authority: None,
            lsp_sessions: std::collections::HashMap::new(),
            lsp_runtime_tx,
            lsp_runtime_rx,
            diagnostics: std::collections::HashMap::new(),
            terminal_starting: false,
            terminal_launch_id: 0,
            terminal_capture: false,
            lsp_supervisor,
            lsp_event_rx,
            submitted_ssh_passphrase: None,
            pending_definition: None,
            pending_definition_open: None,
        }
    }

    pub fn apply_action(&mut self, action: Action) {
        match action {
            Action::Quit => {}
            Action::CycleLandmarkForward => {
                self.focus = self.focus.next(self.layout.context_visible);
            }
            Action::CycleLandmarkBackward => {
                self.focus = self.focus.prev(self.layout.context_visible);
            }
            Action::FocusLandmark(l) => {
                self.focus = l;
                self.terminal_capture = l == Landmark::BottomPane
                    && self.layout.bottom_active_tab == 0
                    && self.terminal.as_ref().is_some_and(|session| {
                        session.state() == crate::pty::PtySessionState::Running
                    });
            }
            Action::CycleAuthority => {
                if self.authorities.len() < 2 {
                    self.status_message = "No configured SSH authority.".into();
                } else {
                    self.current_authority = (self.current_authority + 1) % self.authorities.len();
                    self.terminal_starting = false;
                    self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                    self.terminal_capture = false;
                    if let Some(session) = self.terminal.take() {
                        session.cancel();
                        std::thread::spawn(move || {
                            let _ = session.join_reader();
                        });
                    }
                    if let Some(authority) = self.authorities.get(self.current_authority) {
                        self.status_message = format!(
                            "{} authority selected ({}).",
                            authority.label,
                            match authority.connection {
                                AuthorityConnectionState::Disconnected => "disconnected",
                                AuthorityConnectionState::Connecting => "connecting",
                                AuthorityConnectionState::Connected => "connected",
                                AuthorityConnectionState::Lost => "lost",
                            }
                        );
                    }
                    self.focus = Landmark::Authority;
                    // Dropping command senders cancels sessions routed through
                    // the previously selected authority before a new exact
                    // authority/document association is started.
                    self.lsp_sessions.clear();
                    self.start_language_service_for_current_document();
                }
            }
            Action::NextControl => {
                if let Overlay::TrustReview { focused_grant, .. } = &mut self.overlay {
                    *focused_grant = !*focused_grant;
                } else if let Overlay::CommandPalette {
                    selected, items, ..
                } = &mut self.overlay
                {
                    if !items.is_empty() {
                        *selected = (*selected + 1) % items.len();
                    }
                } else if let Overlay::Completion {
                    selected, candidates, ..
                } = &mut self.overlay
                {
                    if !candidates.is_empty() {
                        *selected = (*selected + 1) % candidates.len();
                    }
                }
            }
            Action::PrevControl => {
                if let Overlay::TrustReview { focused_grant, .. } = &mut self.overlay {
                    *focused_grant = !*focused_grant;
                } else if let Overlay::CommandPalette {
                    selected, items, ..
                } = &mut self.overlay
                {
                    if !items.is_empty() {
                        *selected = if *selected == 0 {
                            items.len() - 1
                        } else {
                            *selected - 1
                        };
                    }
                } else if let Overlay::Completion {
                    selected, candidates, ..
                } = &mut self.overlay
                {
                    if !candidates.is_empty() {
                        *selected = if *selected == 0 {
                            candidates.len() - 1
                        } else {
                            *selected - 1
                        };
                    }
                }
            }
            Action::ActivateFocused => {
                if let Overlay::CommandPalette {
                    selected, items, ..
                } = &self.overlay
                {
                    if let Some(item) = items.get(*selected) {
                        let item = item.clone();
                        self.overlay = Overlay::None;
                        if item == "Review authority trust…" {
                            self.apply_action(Action::ReviewTrust);
                        } else if item == "Revoke authority trust" {
                            self.apply_action(Action::RevokeTrust);
                        }
                        return;
                    }
                }
                let completion = match &self.overlay {
                    Overlay::Completion {
                        document_id,
                        revision,
                        position,
                        candidates,
                        selected,
                        ..
                    } => candidates.get(*selected).cloned().map(|candidate| {
                        (*document_id, *revision, *position, candidate)
                    }),
                    _ => None,
                };
                if let Some((document_id, revision, position, candidate)) = completion {
                    let insertion = self
                        .buffers
                        .iter()
                        .find(|buffer| buffer.id() == document_id && buffer.revision() == revision)
                        .and_then(|buffer| {
                            crate::lsp::CoordinateMapper::new(buffer.rope())
                                .lsp_position_to_byte(position)
                        });
                    if self.current_doc == Some(document_id) {
                        if let Some(byte) = insertion {
                            self.overlay = Overlay::None;
                            self.apply_action(Action::ApplyBufferEdit {
                                doc_id: document_id,
                                expected_rev: revision,
                                edit: crate::edit::TextEdit::insert(byte, candidate.replacement.clone()),
                            });
                            self.layout.set_editor_cursor(byte + candidate.replacement.len());
                            self.queue_current_lsp_change();
                            self.status_message = format!("Completion inserted: {}.", candidate.label);
                            return;
                        }
                    }
                    self.overlay = Overlay::None;
                    self.status_message = "Completion discarded because its editor context changed.".into();
                    return;
                }
                self.overlay = Overlay::None;
            }
            Action::ActivateLeftTool(n) => {
                self.layout.primary_visible = true;
                self.layout
                    .set_active_tab(crate::layout::Pane::Primary, (n.saturating_sub(1)) as usize);
                self.focus = Landmark::PrimaryPane;
            }
            Action::OpenCommandPalette => {
                self.overlay = Overlay::CommandPalette {
                    query: String::new(),
                    items: command_palette_items(self.current_trust(), ""),
                    selected: 0,
                    invoker: self.focus,
                };
            }
            Action::PaletteInput(character) => {
                let trust = self.current_trust();
                if let Overlay::CommandPalette {
                    query,
                    items,
                    selected,
                    ..
                } = &mut self.overlay
                {
                    query.push(character);
                    *items = command_palette_items(trust, query);
                    *selected = 0;
                }
            }
            Action::PaletteBackspace => {
                let trust = self.current_trust();
                if let Overlay::CommandPalette {
                    query,
                    items,
                    selected,
                    ..
                } = &mut self.overlay
                {
                    query.pop();
                    *items = command_palette_items(trust, query);
                    *selected = 0;
                }
            }
            Action::MouseDown { x, y, button: _ } => {
                let ar = self.layout.rect_authority();
                if x >= ar.x && x < ar.x + ar.width && y >= ar.y && y < ar.y + ar.height {
                    self.apply_action(Action::ReviewTrust);
                }
                // Editor mouse translated upstream in input/mouse.rs -> EditorSetCursor / EditorSetSelection
                // using coordinate::editor_mouse_to_byte (Rect + gutter + scroll + Rope -> CellPos -> cell_to_byte)
            }
            Action::MouseDrag { x: _, y: _ } => {
                // pre-mapped via coordinate in mouse layer
            }
            Action::MouseUp => {}
            Action::Wheel { landmark, lines } => {
                if landmark == Landmark::Editor {
                    if let Some(t) = self.layout.current_editor_mut() {
                        t.scroll_line = ((t.scroll_line as i32) + lines).max(0) as u16;
                    }
                } else if landmark == Landmark::PrimaryPane {
                    self.project.scroll = ((self.project.scroll as i32) + lines).max(0) as u16;
                }
            }
            Action::OpenTerminal => {
                self.layout.bottom_visible = true;
                self.layout.bottom_active_tab = 0;
                self.focus = Landmark::BottomPane;
                if self.current_trust() != TrustLevel::Trusted {
                    self.terminal_starting = false;
                    self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                    self.terminal_capture = false;
                    self.status_message =
                        "Terminal blocked: current authority is INSPECT ONLY.".into();
                } else if self
                    .authorities
                    .get(self.current_authority)
                    .is_some_and(|authority| authority.kind == AuthorityKind::Ssh)
                    && self
                        .current_ssh_authority()
                        .is_none_or(|authority| !authority.is_connected())
                {
                    self.terminal_starting = false;
                    self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                    self.terminal_capture = false;
                    self.status_message =
                        "SSH helper is not connected. Activate this authority before opening a terminal."
                            .into();
                } else if self
                    .terminal
                    .as_ref()
                    .is_some_and(|session| session.state() == crate::pty::PtySessionState::Running)
                {
                    self.terminal_capture = true;
                } else {
                    if let Some(session) = self.terminal.take() {
                        session.cancel();
                        std::thread::spawn(move || {
                            let _ = session.join_reader();
                        });
                    }
                    self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                    self.terminal_starting = true;
                    self.terminal_capture = false;
                    let kind = self
                        .authorities
                        .get(self.current_authority)
                        .map(|authority| authority.kind)
                        .unwrap_or(AuthorityKind::Local);
                    self.status_message = format!(
                        "Starting {} terminal…",
                        crate::ui::authority_kind_label(kind)
                    );
                }
            }
            Action::TerminalInput(bytes) => {
                if self.terminal_capture {
                    let input_error = self
                        .terminal
                        .as_ref()
                        .and_then(|session| session.write_input(&bytes).err());
                    if let Some(error) = input_error {
                        if let Some(session) = self.terminal.take() {
                            session.cancel();
                            std::thread::spawn(move || {
                                let _ = session.join_reader();
                            });
                        }
                        self.status_message = error.to_string();
                        self.terminal_capture = false;
                    }
                }
            }
            Action::ReleaseTerminalCapture => {
                self.terminal_capture = false;
                self.focus = Landmark::Editor;
                self.status_message = "Terminal capture released.".into();
            }
            Action::CloseTerminal => {
                self.terminal_starting = false;
                self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                self.terminal_capture = false;
                if let Some(session) = self.terminal.take() {
                    session.cancel();
                    std::thread::spawn(move || {
                        let _ = session.join_reader();
                    });
                }
            }
            Action::TerminalResize { width, height } => {
                self.layout.resize(width, height);
                let body = self.layout.rect_bottom();
                if let Some(session) = &self.terminal {
                    let _ = session.resize(
                        body.height.saturating_sub(2).max(1),
                        body.width.saturating_sub(2).max(1),
                    );
                }
            }
            Action::ReviewTrust => {
                let label = self
                    .authorities
                    .get(self.current_authority)
                    .map(|a| a.label.clone())
                    .unwrap_or_default();
                self.overlay = Overlay::TrustReview {
                    focused_grant: false,
                    invoker: self.focus,
                    workspace_root: self.workspace_root.clone(),
                    authority_label: label,
                };
                self.focus = Landmark::Authority;
            }
            Action::GrantTrust => {
                // SOLE GRANT PATH: only when the modal is TrustReview and grant is focused.
                let invoker = match &self.overlay {
                    Overlay::TrustReview {
                        focused_grant: true,
                        invoker,
                        ..
                    } => Some(*invoker),
                    _ => None,
                };
                if let Some(invoker) = invoker {
                    if let Some(authority) = self.authorities.get_mut(self.current_authority) {
                        authority.trust = TrustLevel::Trusted;
                    }
                    if let Some(authority) = self.current_ssh_authority() {
                        crate::authority::Authority::grant_execution(authority.as_ref());
                    }
                    let selected_config = self.current_doc.and_then(|document_id| {
                        self.buffers
                            .iter()
                            .find(|buffer| buffer.id() == document_id)
                            .and_then(|buffer| {
                                crate::config::resolve_language_server(
                                    &self.language_servers,
                                    buffer.language().as_str(),
                                    buffer.path().map(std::path::Path::new),
                                    &hermito_protocol::request::ExecutionContextV1::AuthorityRoot,
                                )
                            })
                    });
                    if let Some(config) = selected_config {
                        let workspace_root = std::path::PathBuf::from(&self.workspace_root);
                        let authority_id = match self
                            .authorities
                            .get(self.current_authority)
                            .map(|authority| authority.kind)
                        {
                            Some(AuthorityKind::Local) => self.local_authority.as_ref().map(
                                |authority| {
                                    crate::authority::Authority::grant_lsp_execution(
                                        authority.as_ref(),
                                        &config.digest,
                                    );
                                    crate::authority::Authority::host_authority_id(authority.as_ref())
                                },
                            ),
                            Some(AuthorityKind::Ssh) => self
                                .current_ssh_authority()
                                .map(|authority| {
                                    crate::authority::Authority::grant_lsp_execution(
                                        authority.as_ref(),
                                        &config.digest,
                                    );
                                    crate::authority::Authority::host_authority_id(authority.as_ref())
                                }),
                            _ => None,
                        };
                        if let Some(authority) = authority_id {
                            if !self.lsp_grants.iter().any(|grant| {
                                grant.workspace_root == workspace_root
                                    && grant.authority == authority
                                    && grant.config_digest == config.digest
                            }) {
                                self.lsp_grants.push(crate::persistence::state::LspGrantRecord {
                                    workspace_root,
                                    authority,
                                    config_digest: config.digest,
                                });
                            }
                        }
                    }
                    self.start_language_service_for_current_document();
                    self.focus = invoker;
                    self.overlay = Overlay::None;
                    self.status_message = "Execution granted for current authority.".into();
                }
            }
            Action::RevokeTrust => {
                let invoker = match &self.overlay {
                    Overlay::TrustReview { invoker, .. }
                    | Overlay::CommandPalette { invoker, .. }
                    | Overlay::SaveAs { invoker, .. }
                    | Overlay::SshPassphrase { invoker, .. }
                    | Overlay::RenameInput { invoker, .. }
                    | Overlay::Completion { invoker, .. }
                    | Overlay::Hover { invoker, .. } => Some(*invoker),
                    Overlay::None => None,
                };
                if let Some(authority) = self.authorities.get_mut(self.current_authority) {
                    authority.trust = TrustLevel::InspectOnly;
                }
                if let Some(authority) = self.current_ssh_authority() {
                    crate::authority::Authority::revoke_execution(authority.as_ref());
                }
                self.terminal_capture = false;
                self.terminal_starting = false;
                self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
                if let Some(session) = self.terminal.take() {
                    session.cancel();
                    std::thread::spawn(move || {
                        let _ = session.join_reader();
                    });
                }
                if let Some(invoker) = invoker {
                    self.focus = invoker;
                }
                self.overlay = Overlay::None;
                self.status_message = "Execution revoked. Now INSPECT ONLY.".into();
            }
            Action::CancelModal => {
                if let Overlay::TrustReview { invoker, .. }
                | Overlay::CommandPalette { invoker, .. }
                | Overlay::SaveAs { invoker, .. }
                | Overlay::SshPassphrase { invoker, .. }
                | Overlay::Completion { invoker, .. }
                | Overlay::Hover { invoker, .. } = &self.overlay
                {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
            }
            Action::SshPassphraseInput(character) => {
                if !character.is_control() {
                    if let Overlay::SshPassphrase { passphrase, .. } = &mut self.overlay {
                        if passphrase.len() + character.len_utf8() <= 4096 {
                            passphrase.push(character);
                        }
                    }
                }
            }
            Action::SshPassphraseBackspace => {
                if let Overlay::SshPassphrase { passphrase, .. } = &mut self.overlay {
                    passphrase.pop();
                }
            }
            Action::SshPassphraseSubmit => {
                let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
                if let Overlay::SshPassphrase {
                    authority_label,
                    passphrase,
                    invoker,
                } = overlay
                {
                    self.submitted_ssh_passphrase = Some((
                        authority_label,
                        Zeroizing::new(passphrase.as_bytes().to_vec()),
                    ));
                    self.focus = invoker;
                    self.status_message = "SSH passphrase submitted.".into();
                }
            }
            Action::JournalAck {
                doc_id,
                revision,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if buf.revision() == revision {
                        buf.record_last_checkpoint(revision);
                    }
                }
                // retain newer pending (N+1) when ack arrives for older N
                if let Some(p) = self.pending_checkpoints.get(&doc_id) {
                    if p.revision <= revision {
                        self.pending_checkpoints.remove(&doc_id);
                    }
                }
            }
            Action::ApplySyntaxHighlights {
                doc_id,
                revision,
                spans,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if buf.revision() == revision {
                        self.syntax_highlights.insert(doc_id, (revision, spans));
                    }
                }
            }
            Action::UpdateProjectState { tree, epoch } => {
                if epoch != self.epoch {
                    return;
                }
                self.project.tree = tree;
                self.project.loading = false;
                if let Some(t) = &self.project.tree {
                    let n = t.visible_entry_count();
                    if n > 0 && (self.project.selected_row as usize) >= n {
                        self.project.selected_row = (n - 1) as u16;
                    }
                }
            }
            Action::ProjectMoveSelection { delta } => {
                if let Some(t) = &self.project.tree {
                    let n = t.visible_entry_count();
                    if n > 0 {
                        let mut r = self.project.selected_row as i32 + delta;
                        if r < 0 {
                            r = 0;
                        }
                        if r >= n as i32 {
                            r = (n - 1) as i32;
                        }
                        self.project.selected_row = r as u16;
                    }
                }
            }
            Action::ProjectActivateSelected => {
                // Selection change only; actual open triggered via RequestProjectFile from keyboard
                // (to keep request/result boundary explicit and spawn off-thread).
                if self.focus != Landmark::PrimaryPane {
                    self.focus = Landmark::PrimaryPane;
                }
            }
            Action::ProjectToggleDir { path } => {
                if let Some(t) = &mut self.project.tree {
                    // capture logical selection by name path (numeric row shifts when visible rows rebuild)
                    let prev_names = t.entry_path_at_row(self.project.selected_row as usize);
                    if t.toggle_path(&path) {
                        let mut new_row = None;
                        if let Some(names) = &prev_names {
                            let segs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                            new_row = t.row_for_entry_path(&segs);
                        }
                        if new_row.is_none() {
                            // fallback select the dir we toggled (if selection was inside subtree now hidden)
                            if let Ok(rel) = path.strip_prefix(&t.root) {
                                let tnames: Vec<String> = rel
                                    .components()
                                    .filter_map(|c| {
                                        if let std::path::Component::Normal(s) = c {
                                            Some(s.to_string_lossy().into_owned())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let tsegs: Vec<&str> = tnames.iter().map(|s| s.as_str()).collect();
                                new_row = t.row_for_entry_path(&tsegs);
                            }
                        }
                        if let Some(r) = new_row {
                            self.project.selected_row = r as u16;
                        } else {
                            let n = t.visible_entry_count();
                            if n > 0 && (self.project.selected_row as usize) >= n {
                                self.project.selected_row = (n - 1) as u16;
                            }
                        }
                    }
                }
            }
            Action::RequestProjectFile { path } => {
                self.status_message = format!("Opening {}…", path.to_string_lossy());
                // Spawn of off-thread read + result action happens in event_loop plumbing.
            }
            Action::ProjectFileLoaded {
                path,
                content,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(c) = content {
                    // canonical/safe path match: focus existing buffer (recovered or prior open) rather than dup
                    if let Some(existing) = self
                        .buffers
                        .iter()
                        .find(|buffer| buffer.path() == Some(path.as_path()))
                    {
                        let id = existing.id();
                        self.layout.open_or_focus_editor(id);
                        self.current_doc = Some(id);
                        self.focus = Landmark::Editor;
                        self.status_message = format!("Focused {}", path.to_string_lossy());
                    } else {
                        let id = DocumentId::new();
                        let language = crate::document::Language::from_path(&path);
                        let buffer = Buffer::restore_clean(
                            id,
                            language,
                            &c,
                            DocumentRevision(0),
                            BufferPathState::Saved(path.clone()),
                        );
                        self.buffers.push(buffer);
                        self.layout.open_or_focus_editor(id);
                        self.current_doc = Some(id);
                        self.focus = Landmark::Editor;
                        self.status_message = format!("Opened {}", path.to_string_lossy());
                    }
                    if self
                        .pending_definition
                        .as_ref()
                        .is_some_and(|target| target.path == path)
                    {
                        if let Some(target) = self.pending_definition.take() {
                            if let Some((start, end)) = self
                                .current_doc
                                .and_then(|id| self.buffers.iter().find(|buffer| buffer.id() == id))
                                .and_then(|buffer| {
                                    let mapper = crate::lsp::CoordinateMapper::new(buffer.rope());
                                    Some((
                                        mapper.lsp_position_to_byte(target.range.start)?,
                                        mapper.lsp_position_to_byte(target.range.end)?,
                                    ))
                                })
                            {
                                self.layout.set_editor_selection(start, end);
                                self.status_message = "Definition opened.".into();
                            }
                        }
                    }
                } else {
                    self.status_message = format!("Failed to open {}", path.to_string_lossy());
                }
            }
            Action::Save => {
                if let Some(doc_id) = self.current_doc {
                    let has_saved_path = self.buffers.iter().any(|b| {
                        b.id() == doc_id && matches!(b.path_state(), BufferPathState::Saved(_))
                    });
                    if !has_saved_path {
                        // Untitled or Recovered: open path-entry overlay (never silent overwrite of old path)
                        self.overlay = Overlay::SaveAs {
                            path: String::new(),
                            invoker: self.focus,
                            doc_id,
                        };
                        self.status_message =
                            "Save As: type path, Enter to save, Esc to cancel".to_string();
                    } else {
                        self.status_message = "Saving…".to_string();
                    }
                }
            }
            Action::SaveAsOverlayInput(c) => {
                if let Overlay::SaveAs { path, .. } = &mut self.overlay {
                    path.push(c);
                }
            }
            Action::SaveAsOverlayBackspace => {
                if let Overlay::SaveAs { path, .. } = &mut self.overlay {
                    path.pop();
                }
            }
            Action::SaveAsOverlayConfirm => {
                // spawn of off-thread write happens in event_loop before this apply
                if let Overlay::SaveAs { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
            }
            Action::RenameOverlayInput(c) => {
                if let Overlay::RenameInput { new_name, .. } = &mut self.overlay {
                    new_name.push(c);
                }
            }
            Action::RenameOverlayBackspace => {
                if let Overlay::RenameInput { new_name, .. } = &mut self.overlay {
                    new_name.pop();
                }
            }
            Action::RenameOverlayConfirm => {
                let Overlay::RenameInput {
                    document_id,
                    key,
                    context,
                    position,
                    preparation,
                    new_name,
                    invoker,
                } = &self.overlay
                else {
                    return;
                };
                if new_name.trim().is_empty() {
                    self.status_message = "Rename requires a non-empty name.".into();
                    return;
                }
                let Some(session) = self.lsp_sessions.get(&(key.clone(), *document_id)) else {
                    self.status_message = "Language service is unavailable for rename.".into();
                    return;
                };
                let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == *document_id) else {
                    self.status_message = "Rename requires the selected buffer.".into();
                    return;
                };
                let Some(ledger) = buffer
                    .lsp_ledger(&key.authority_identity, &key.execution_context)
                    .cloned()
                else {
                    self.status_message = "Rename discarded because its document snapshot is stale.".into();
                    return;
                };
                if ledger.revision != buffer.revision() || *context != (hermito_protocol::lsp::LspContext {
                    workspace_epoch: hermito_protocol::WorkspaceEpoch(ledger.workspace_epoch.0),
                    environment_epoch: ledger.environment_epoch,
                    document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
                    sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
                    session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
                    execution_context: ledger.context.clone(),
                    authority_identity: ledger.authority_identity.clone(),
                }) {
                    self.status_message = "Rename discarded because its document snapshot is stale.".into();
                    return;
                }
                let snapshot = LspProviderSnapshot {
                    context: context.clone(),
                    uri: document_uri(buffer, *document_id),
                    revision: ledger.revision,
                    ledger,
                    config_digest: session.config_digest.clone(),
                    service_state: self
                        .language_service_states
                        .get(key)
                        .cloned()
                        .unwrap_or(crate::lsp::LanguageServiceState::Failed {
                            message: "language service state unavailable".into(),
                        }),
                };
                match session.command_tx.try_send(LspSessionCommand::Rename {
                    snapshot,
                    preparation: preparation.clone(),
                    position: *position,
                    new_name: new_name.clone(),
                }) {
                    Ok(()) => {
                        self.focus = *invoker;
                        self.overlay = Overlay::None;
                        self.status_message = "Renaming…".into();
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        self.status_message = "Language request queue is busy.".into();
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        self.status_message = "Language service is unavailable for rename.".into();
                    }
                }
            }
            Action::RenameOverlayCancel => {
                if let Overlay::RenameInput { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
                self.status_message = "Rename cancelled.".into();
            }
            Action::SaveAsOverlayCancel => {
                if let Overlay::SaveAs { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
                self.status_message = "Save cancelled".to_string();
            }
            Action::SaveCompleted {
                doc_id,
                revision,
                path,
                success,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if success && buf.revision() == revision {
                        buf.mark_clean(revision);
                        buf.set_path_state(BufferPathState::Saved(path.clone()));
                        self.status_message = format!("Saved {}", path.to_string_lossy());
                        // schedule compaction (try; retain on backpressure for lossless)
                        if let Some(j) = &self.journal {
                            if j.try_ack_save(doc_id, revision).is_err() {
                                self.retain_pending_compact(doc_id, revision);
                            }
                        }
                    } else if !success {
                        self.status_message = format!("Save failed: {}", path.to_string_lossy());
                        // dirty + path unchanged
                    } else {
                        self.status_message =
                            "Save stale (concurrent edit); left dirty".to_string();
                        // dirty + path unchanged
                    }
                }
            }
            Action::ApplyBufferEdit {
                doc_id,
                expected_rev,
                edit,
            } => {
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if let Ok((_r, payload)) = buf.apply_edit(expected_rev, edit, self.epoch) {
                        self.try_checkpoint(doc_id, payload);
                        // syntax dispatch happens in event loop after apply (tagged result)
                    }
                }
            }
            Action::EditorInsert(c) => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rev = buf.revision();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let edit = crate::edit::TextEdit::insert(cur, c.to_string());
                        if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                            self.layout.set_editor_cursor(cur + c.len_utf8());
                            self.try_checkpoint(doc_id, payload);
                        }
                    }
                }
            }
            Action::EditorPaste(text) => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rev = buf.revision();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let edit = crate::edit::TextEdit::insert(cur, text.clone());
                        if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                            self.layout.set_editor_cursor(cur + text.len());
                            self.try_checkpoint(doc_id, payload);
                        }
                    }
                }
            }
            Action::EditorDeleteBackward => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let rev = buf.revision();
                        if let Some(tab) = self.layout.current_editor() {
                            let (del_start, del_end) = if let Some(anchor) = tab.selection_anchor {
                                let a = crate::coordinate::snap_to_grapheme_start(rope, anchor);
                                let c = crate::coordinate::snap_to_grapheme_start(
                                    rope,
                                    tab.cursor_byte,
                                );
                                if a != c {
                                    (a.min(c), a.max(c))
                                } else {
                                    let cur = crate::coordinate::snap_to_grapheme_start(
                                        rope,
                                        tab.cursor_byte,
                                    );
                                    let prev = crate::coordinate::move_left(rope, cur);
                                    (prev, cur)
                                }
                            } else {
                                let cur = crate::coordinate::snap_to_grapheme_start(
                                    rope,
                                    tab.cursor_byte,
                                );
                                let prev = crate::coordinate::move_left(rope, cur);
                                (prev, cur)
                            };
                            if del_start != del_end {
                                let edit = crate::edit::TextEdit::delete(del_start..del_end);
                                if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                                    self.layout.set_editor_cursor(del_start);
                                    if let Some(t) = self.layout.current_editor_mut() {
                                        t.selection_anchor = None;
                                    }
                                    self.try_checkpoint(doc_id, payload);
                                }
                            }
                        }
                    }
                }
            }
            Action::EditorMoveCursor {
                line_delta,
                col_delta,
                extend_selection,
            } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let cur = crate::coordinate::snap_to_grapheme_start(rope, cur);
                        let nb = if line_delta == 0 && col_delta != 0 {
                            let mut p = cur;
                            let steps = col_delta.unsigned_abs() as usize;
                            for _ in 0..steps {
                                p = if col_delta > 0 {
                                    crate::coordinate::move_right(rope, p)
                                } else {
                                    crate::coordinate::move_left(rope, p)
                                };
                            }
                            p
                        } else {
                            crate::coordinate::move_vertical(rope, cur, line_delta)
                        };
                        let nb = crate::coordinate::snap_to_grapheme_start(rope, nb);
                        self.layout.extend_or_move_cursor(nb, extend_selection);
                        // normalize selection anchor if present
                        if let Some(t) = self.layout.current_editor_mut() {
                            if let Some(a) = t.selection_anchor {
                                t.selection_anchor =
                                    Some(crate::coordinate::snap_to_grapheme_start(rope, a));
                            }
                        }
                    }
                }
            }
            Action::EditorSetCursor { byte } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter().find(|b| b.id() == doc_id) {
                        let snapped = crate::coordinate::snap_to_grapheme_start(buf.rope(), byte);
                        self.layout.set_editor_cursor(snapped);
                    } else {
                        self.layout.set_editor_cursor(byte);
                    }
                } else {
                    self.layout.set_editor_cursor(byte);
                }
                self.focus = Landmark::Editor;
            }
            Action::EditorSetSelection { anchor, cursor } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let a = crate::coordinate::snap_to_grapheme_start(rope, anchor);
                        let c = crate::coordinate::snap_to_grapheme_start(rope, cursor);
                        self.layout.set_editor_selection(a, c);
                    } else {
                        self.layout.set_editor_selection(anchor, cursor);
                    }
                } else {
                    self.layout.set_editor_selection(anchor, cursor);
                }
            }
            Action::EditorPage { up, extend } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let cur = crate::coordinate::snap_to_grapheme_start(rope, cur);
                        let h = self.layout.rect_editor().height as i32;
                        let page = if h > 2 { h - 2 } else { 1 };
                        let d = if up { -page } else { page };
                        let nb = crate::coordinate::move_vertical(rope, cur, d);
                        let nb = crate::coordinate::snap_to_grapheme_start(rope, nb);
                        self.layout.extend_or_move_cursor(nb, extend);
                        if let Some(t) = self.layout.current_editor_mut() {
                            if let Some(a) = t.selection_anchor {
                                t.selection_anchor =
                                    Some(crate::coordinate::snap_to_grapheme_start(rope, a));
                            }
                        }
                    }
                }
            }
            Action::RequestCompletion => self.enqueue_current_lsp_provider(LspProviderKind::Completion),
            Action::RequestHover => self.enqueue_current_lsp_provider(LspProviderKind::Hover),
            Action::RequestDefinition => {
                self.enqueue_current_lsp_provider(LspProviderKind::Definition)
            }
            Action::RequestRename => {
                self.enqueue_current_lsp_provider(LspProviderKind::RenamePreparation)
            }
        }
    }
    fn enqueue_current_lsp_provider(&mut self, kind: LspProviderKind) {
        let Some(document_id) = self.current_doc else {
            self.status_message = "Language request requires an open document.".into();
            return;
        };
        let Some((key, command_tx, config_digest)) = self
            .lsp_sessions
            .iter()
            .find(|((key, id), _)| *id == document_id && self.is_current_language_service_context(key))
            .map(|((key, _), session)| {
                (
                    key.clone(),
                    session.command_tx.clone(),
                    session.config_digest.clone(),
                )
            })
        else {
            self.status_message = "Language service is unavailable for the selected document.".into();
            return;
        };
        let Some(state) = self.language_service_states.get(&key).cloned() else {
            self.status_message = "Language service is not ready for requests.".into();
            return;
        };
        if !matches!(state, crate::lsp::LanguageServiceState::Ready) {
            self.status_message = "Language service is not ready for requests.".into();
            return;
        }
        let cursor = self
            .layout

            .current_editor()
            .map(|editor| editor.cursor_byte)
            .unwrap_or(0);
        let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == document_id) else {
            self.status_message = "Language request requires the selected buffer.".into();
            return;
        };
        let Some(ledger) = buffer
            .lsp_ledger(&key.authority_identity, &key.execution_context)
            .cloned()
        else {
            self.status_message = "Language request has no current document ledger.".into();
            return;
        };
        if ledger.revision != buffer.revision() {
            self.status_message = "Language request discarded because its document snapshot is stale.".into();
            return;
        }
        let Some(position) = crate::lsp::CoordinateMapper::new(buffer.rope()).byte_to_lsp_position(cursor)
        else {
            self.status_message = "Language request cursor is not a valid text position.".into();
            return;
        };
        let snapshot = LspProviderSnapshot {
            context: hermito_protocol::lsp::LspContext {
                workspace_epoch: hermito_protocol::WorkspaceEpoch(ledger.workspace_epoch.0),
                environment_epoch: ledger.environment_epoch,
                document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
                sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
                session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
                execution_context: ledger.context.clone(),
                authority_identity: ledger.authority_identity.clone(),
            },
            uri: document_uri(buffer, document_id),
            revision: ledger.revision,
            ledger,
            config_digest,
            service_state: state,
        };
        match command_tx.try_send(LspSessionCommand::Provider {
            kind,
            snapshot,
            position,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.status_message = "Language request queue is busy.".into();
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.status_message = "Language service is unavailable for the selected document.".into();
            }
        }
    }

    fn apply_rename_workspace_edit<A: crate::authority::Authority + ?Sized>(
        &mut self,
        authority: &A,
        key: &crate::lsp::SupervisorKey,
        document_id: DocumentId,
        context: hermito_protocol::lsp::LspContext,
        workspace_edit: &hermito_protocol::lsp::TransactionalWorkspaceEdit,
    ) -> Result<crate::lsp::RenameApplyOutcome, crate::lsp::ProviderError> {
        let config_digest = self
            .lsp_sessions
            .get(&(key.clone(), document_id))
            .ok_or(crate::lsp::ProviderError::MissingLedger)?
            .config_digest
            .clone();
        let document_index = self
            .buffers
            .iter()
            .position(|buffer| buffer.id() == document_id)
            .ok_or_else(|| crate::lsp::ProviderError::MissingWorkspaceDocument {
                uri: format!("document:{document_id:?}"),
            })?;
        let source_uri = document_uri(&self.buffers[document_index], document_id);
        let (_, source_relative_path) = workspace_edit_target_path(&source_uri, authority.root())
            .map_err(|_| crate::lsp::ProviderError::MissingWorkspaceDocument {
                uri: source_uri.clone(),
            })?;

        let mut seen_uris = HashSet::with_capacity(workspace_edit.document_changes.len());
        let mut targets = Vec::with_capacity(workspace_edit.document_changes.len());
        let mut request_uri = source_uri.clone();
        let mut source_is_target = false;
        for edit in &workspace_edit.document_changes {
            let hermito_protocol::lsp::TransactionalDocumentEdit::TextDocument { uri, .. } = edit;
            if !seen_uris.insert(uri.as_str()) {
                continue;
            }
            let (path, relative_path) = workspace_edit_target_path(uri, authority.root())?;
            let buffer_index = self
                .buffers
                .iter()
                .position(|buffer| buffer.path() == Some(path.as_path()));
            if buffer_index == Some(document_index) {
                if source_is_target {
                    return Err(crate::lsp::ProviderError::DuplicateWorkspaceDocument {
                        uri: uri.clone(),
                    });
                }
                source_is_target = true;
                request_uri = uri.clone();
            }
            targets.push(WorkspaceEditTarget {
                uri: uri.clone(),
                relative_path,
                buffer_index,
            });
        }

        for target in targets
            .iter_mut()
            .filter(|target| target.buffer_index.is_none())
        {
            let request = crate::authority::types::AuthorityRequest::new(
                crate::authority::types::ReadFileRequest {
                    path: target.relative_path.clone(),
                    max_bytes: hermito_protocol::fs::MAX_WIRE_FILE_BYTES as usize,
                },
                context.workspace_epoch,
                context.environment_epoch,
                context.document_revision,
            );
            let bytes = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(authority.read_file(request))
            })
            .map_err(|_| crate::lsp::ProviderError::UnresolvableWorkspaceDocument {
                uri: target.uri.clone(),
            })?
            .payload;
            let content = std::str::from_utf8(&bytes).map_err(|_| {
                crate::lsp::ProviderError::UnresolvableWorkspaceDocument {
                    uri: target.uri.clone(),
                }
            })?;
            let path = authority.root().join(&target.relative_path);
            let mut buffer = Buffer::restore_clean(
                DocumentId::new(),
                Language::from_path(&path),
                content,
                DocumentRevision(0),
                BufferPathState::Saved(path),
            );
            buffer.ensure_lsp_ledger(
                context.authority_identity.clone(),
                context.execution_context.clone(),
                WorkspaceEpoch(context.workspace_epoch.0),
                context.environment_epoch,
            );
            target.buffer_index = Some(self.buffers.len());
            self.buffers.push(buffer);
        }

        let mut selected_indices = targets
            .iter()
            .filter_map(|target| target.buffer_index)
            .collect::<Vec<_>>();
        selected_indices.push(document_index);
        selected_indices.sort_unstable();
        selected_indices.dedup();
        let source_position = selected_indices
            .binary_search(&document_index)
            .map_err(|_| crate::lsp::ProviderError::MissingLedger)?;
        let selected_buffers = buffers_at_indices_mut(&mut self.buffers, &selected_indices, 0)
            .ok_or(crate::lsp::ProviderError::MissingLedger)?;
        let mut selected = selected_indices
            .into_iter()
            .zip(selected_buffers)
            .collect::<Vec<_>>();
        let (_, source_buffer) = selected.remove(source_position);
        let mut documents = Vec::with_capacity(targets.len().saturating_sub(source_is_target as usize));
        for target in &targets {
            let Some(buffer_index) = target.buffer_index else {
                return Err(crate::lsp::ProviderError::MissingWorkspaceDocument {
                    uri: target.uri.clone(),
                });
            };
            if buffer_index == document_index {
                continue;
            }
            let position = selected
                .iter()
                .position(|(index, _)| *index == buffer_index)
                .ok_or_else(|| crate::lsp::ProviderError::MissingWorkspaceDocument {
                    uri: target.uri.clone(),
                })?;
            let (_, buffer) = selected.remove(position);
            documents.push(crate::lsp::ProviderDocument {
                uri: &target.uri,
                relative_path: &target.relative_path,
                buffer,
            });
        }

        let mut request = crate::lsp::ProviderRequest {
            authority,
            supervisor: &self.lsp_supervisor,
            service: key,
            config_digest: &config_digest,
            context,
            document: crate::lsp::ProviderDocument {
                uri: &request_uri,
                relative_path: &source_relative_path,
                buffer: source_buffer,
            },
        };
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                crate::lsp::RenameProvider::apply_workspace_edit(
                    &mut request,
                    workspace_edit,
                    &mut documents,
                ),
            )
        })
    }

    pub fn journal_handle_mut(&mut self) -> Option<&mut JournalHandle> {
        self.journal.as_mut()
    }

    fn try_checkpoint(&mut self, _doc_id: DocumentId, payload: CheckpointPayload) {
        if let Some(journal) = &self.journal {
            if journal.try_checkpoint(payload.clone()).is_err() {
                self.retain_pending_checkpoint(payload);
            }
        }
    }

    pub fn retain_pending_checkpoint(&mut self, payload: CheckpointPayload) {
        self.pending_checkpoints
            .entry(payload.doc_id)
            .and_modify(|pending| {
                if payload.revision > pending.revision {
                    *pending = payload.clone();
                }
            })
            .or_insert(payload);
    }

    pub fn pending_checkpoint_revision(&self, doc_id: DocumentId) -> Option<DocumentRevision> {
        self.pending_checkpoints
            .get(&doc_id)
            .map(|payload| payload.revision)
    }

    pub fn apply_journal_ack(&mut self, ack: JournalAck) {
        if ack.epoch != self.epoch {
            return;
        }
        if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == ack.doc_id) {
            if buf.revision() == ack.revision {
                buf.record_last_checkpoint(ack.revision);
            }
        }
        // retain newer pending checkpoint N+1 when ack for older N arrives
        if let Some(p) = self.pending_checkpoints.get(&ack.doc_id) {
            if p.revision <= ack.revision {
                self.pending_checkpoints.remove(&ack.doc_id);
            }
        }
    }

    pub fn retry_pending_checkpoints(&mut self) {
        if let Some(j) = &self.journal {
            let mut still = std::collections::HashMap::new();
            for (id, p) in self.pending_checkpoints.drain() {
                if j.try_checkpoint(p.clone()).is_err() {
                    still.insert(id, p);
                }
            }
            self.pending_checkpoints = still;
        }
    }

    pub fn retain_pending_compact(&mut self, id: DocumentId, rev: DocumentRevision) {
        self.pending_compactions
            .entry(id)
            .and_modify(|r| {
                if rev > *r {
                    *r = rev;
                }
            })
            .or_insert(rev);
    }

    pub fn retry_pending_compacts(&mut self) {
        if let Some(j) = &self.journal {
            let mut still = std::collections::HashMap::new();
            for (id, rev) in self.pending_compactions.drain() {
                if j.try_ack_save(id, rev).is_err() {
                    still.insert(id, rev);
                }
            }
            self.pending_compactions = still;
        }
    }

    /// Blocking submit of *all* retained latest checkpoints + pending compacts.
    /// Called before flush on every shutdown/panic return path while App owned.
    /// Blocking send allowed only here.
    pub fn submit_all_retained_blocking(&mut self) {
        if let Some(j) = &self.journal {
            for (_, p) in self.pending_checkpoints.drain() {
                j.submit_checkpoint_blocking(p);
            }
            for (id, rev) in self.pending_compactions.drain() {
                j.submit_ack_blocking(id, rev);
            }
        }
    }
    /// Project a supervisor state transition without creating or driving a
    /// language-service process from the app layer.
    pub fn record_language_service_state(
        &mut self,
        key: crate::lsp::SupervisorKey,
        state: crate::lsp::LanguageServiceState,
    ) {
        self.language_service_states.insert(key, state);
    }

    /// Store a validated per-service diagnostic count. The protocol's
    /// aggregate diagnostic bound keeps malformed or stale event values from
    /// inflating the status bar.
    pub fn record_language_diagnostic_count(
        &mut self,
        key: crate::lsp::SupervisorKey,
        count: usize,
    ) {
        self.language_diagnostic_counts
            .insert(key, count.min(crate::lsp::MAX_DIAGNOSTICS));
    }

    /// Explicit event-loop entry point for bounded supervisor events.
    pub fn apply_language_service_event(&mut self, event: crate::lsp::SupervisorEvent) {
        let key = match &event {
            crate::lsp::SupervisorEvent::StateChanged { key, .. }
            | crate::lsp::SupervisorEvent::DiagnosticsCountChanged { key, .. } => key,
        };
        if !self.is_current_language_service_context(key) {
            return;
        }
        match event {
            crate::lsp::SupervisorEvent::StateChanged { key, state } => {
                self.record_language_service_state(key, state);
            }
            crate::lsp::SupervisorEvent::DiagnosticsCountChanged { key, count } => {
                self.record_language_diagnostic_count(key, count);
            }
        }
    }

    fn is_current_language_service_context(&self, key: &crate::lsp::SupervisorKey) -> bool {
        if key.workspace_epoch.0 != self.epoch.0 {
            return false;
        }
        let Some(authority) = self.authorities.get(self.current_authority) else {
            return false;
        };
        match authority.kind {
            AuthorityKind::Local => {
                key.authority_identity.0 == "local"
                    && key.environment_epoch == hermito_protocol::EnvironmentEpoch(0)
            }
            AuthorityKind::Ssh => self.ssh_authorities.get(&authority.label).is_some_and(
                |ssh_authority| {
                    key.authority_identity.0
                        == crate::authority::Authority::host_authority_id(ssh_authority.as_ref())
                        && key.environment_epoch
                            == crate::authority::Authority::environment_epoch(
                                ssh_authority.as_ref(),
                            )
                },
            ),
            AuthorityKind::DevContainer => false,
        }
    }

    /// Own the service lifecycle source while its bounded receiver remains on
    /// the UI loop. Callers must report transitions through this supervisor.
    pub fn language_service_supervisor(&mut self) -> &mut crate::lsp::LspSupervisor {
        &mut self.lsp_supervisor
    }

    /// Poll the bounded supervisor receiver. The event loop applies returned
    /// updates through `apply_language_service_event` on the UI thread.
    pub(crate) fn try_recv_language_service_event(
        &mut self,
    ) -> Result<crate::lsp::SupervisorEvent, tokio::sync::mpsc::error::TryRecvError> {
        self.lsp_event_rx.try_recv()
    }

    /// Install user-owned server configuration and establish the host authority
    /// used for exact-digest LSP authorization. This performs no probe or spawn.
    pub(crate) fn configure_language_servers(
        &mut self,
        language_servers: Vec<crate::config::LanguageServerConfig>,
    ) {
        self.language_servers = language_servers;
        if self.local_authority.is_none() {
            match crate::authority::local::LocalAuthority::new(
                "host",
                std::path::PathBuf::from(&self.workspace_root),
                hermito_protocol::WorkspaceEpoch(self.epoch.0),
            ) {
                Ok(authority) => {
                    let authority = Arc::new(authority);
                    if self
                        .authorities
                        .first()
                        .is_some_and(|state| state.trust == TrustLevel::Trusted)
                    {
                        crate::authority::Authority::grant_execution(authority.as_ref());
                    }
                    apply_persisted_lsp_grants(
                        &self.lsp_grants,
                        std::path::Path::new(&self.workspace_root),
                        authority.as_ref(),
                    );
                    self.local_authority = Some(authority);
                }
                Err(error) => self.status_message = format!("LSP host unavailable: {error}"),
            }
        }
        self.start_language_service_for_current_document();
    }

    /// Associate the active opened buffer with its configured service. Missing
    /// configuration and inspect-only authority both stop before any probe or
    /// process creation.
    pub(crate) fn start_language_service_for_current_document(&mut self) {
        let Some(document_id) = self.current_doc else {
            return;
        };
        let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == document_id) else {
            return;
        };
        if self
            .authorities
            .get(self.current_authority)
            .is_some_and(|authority| {
                authority.kind == AuthorityKind::Ssh
                    && authority.connection != AuthorityConnectionState::Connected
            })
        {
            return;
        }
        let execution_context = hermito_protocol::request::ExecutionContextV1::AuthorityRoot;
        let language = buffer.language().as_str().to_owned();
        let path = buffer.path().map(std::path::Path::new);
        let authority_identity = match self.authorities.get(self.current_authority) {
            Some(state) if state.kind == AuthorityKind::Ssh => self
                .ssh_authorities
                .get(&state.label)
                .map(|authority| {
                    hermito_protocol::lsp::AuthorityIdentity(
                        crate::authority::Authority::host_authority_id(authority.as_ref()),
                    )
                })
                .unwrap_or_else(|| hermito_protocol::lsp::AuthorityIdentity("ssh".into())),
            _ => hermito_protocol::lsp::AuthorityIdentity("local".into()),
        };
        let environment_epoch = match self.authorities.get(self.current_authority) {
            Some(state) if state.kind == AuthorityKind::Ssh => self
                .ssh_authorities
                .get(&state.label)
                .map(|authority| crate::authority::Authority::environment_epoch(authority.as_ref()))
                .unwrap_or(hermito_protocol::EnvironmentEpoch(0)),
            _ => hermito_protocol::EnvironmentEpoch(0),
        };
        let key = crate::lsp::SupervisorKey::new(
            hermito_protocol::WorkspaceEpoch(self.epoch.0),
            environment_epoch,
            authority_identity,
            execution_context.clone(),
            crate::lsp::LanguageId::from(buffer.language()),
        );
        let Some(config) = crate::config::resolve_language_server(
            &self.language_servers,
            &language,
            path,
            &execution_context,
        ) else {
            let _ = self.lsp_supervisor.enter_state(
                key,
                crate::lsp::LanguageServiceState::NotFound {
                    detail: "LSP configuration not found for document association".into(),
                },
            );
            return;
        };
        match self.authorities.get(self.current_authority).map(|state| state.kind) {
            Some(AuthorityKind::Ssh) => {
                if let Some(authority) = self
                    .authorities
                    .get(self.current_authority)
                    .and_then(|state| self.ssh_authorities.get(&state.label))
                    .cloned()
                {
                    self.start_lsp_session(authority, key, document_id, execution_context, config);
                }
            }
            Some(AuthorityKind::Local) => {
                if let Some(authority) = self.local_authority.clone() {
                    self.start_lsp_session(authority, key, document_id, execution_context, config);
                }
            }
            _ => {}
        }
    }

    fn start_lsp_session<A: crate::authority::Authority + 'static>(
        &mut self,
        authority: Arc<A>,
        key: crate::lsp::SupervisorKey,
        document_id: DocumentId,
        execution_context: hermito_protocol::request::ExecutionContextV1,
        config: crate::config::ResolvedLanguageServerConfig,
    ) {
        let Ok(state) = self
            .lsp_supervisor
            .inspect(authority.as_ref(), key.clone(), &config.digest)
        else {
            return;
        };
        if !matches!(state, crate::lsp::LanguageServiceState::Starting) {
            return;
        }
        if !self.buffers.iter().any(|buffer| buffer.id() == document_id) {
            return;
        }
        let session_key = (key.clone(), document_id);
        if let Some(superseded) = self.lsp_sessions.remove(&session_key) {
            superseded.cancellation.cancel();
            if let Err(error) = self
                .lsp_supervisor
                .request_restart(&key, &superseded.context)
            {
                let _ = self.lsp_supervisor.enter_state(
                    key,
                    crate::lsp::LanguageServiceState::Failed {
                        message: error.to_string(),
                    },
                );
                return;
            }
        }
        let Some(buffer) = self.buffers.iter_mut().find(|buffer| buffer.id() == document_id) else {
            return;
        };
        let ledger = buffer.reset_lsp_session(
            key.authority_identity.clone(),
            execution_context,
            WorkspaceEpoch(self.epoch.0),
            key.environment_epoch,
        );
        let context = hermito_protocol::lsp::LspContext {
            workspace_epoch: hermito_protocol::WorkspaceEpoch(self.epoch.0),
            environment_epoch: key.environment_epoch,
            document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
            sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
            session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
            execution_context: ledger.context.clone(),
            authority_identity: key.authority_identity.clone(),
        };
        if let Err(error) = self.lsp_supervisor.activate_session(&key, &context) {
            let _ = self.lsp_supervisor.enter_state(
                key,
                crate::lsp::LanguageServiceState::Failed {
                    message: error.to_string(),
                },
            );
            return;
        }
        let uri = document_uri(buffer, document_id);
        let language_id = buffer.language().as_str().to_owned();
        let (command_tx, command_rx) =
            tokio::sync::mpsc::channel(LSP_SESSION_COMMAND_CHANNEL_CAPACITY);
        let cancellation = tokio_util::sync::CancellationToken::new();
        self.lsp_sessions.insert(
            session_key,
            LspSession {
                command_tx,
                cancellation: cancellation.clone(),
                context: context.clone(),
                pending_change: None,
                config_digest: config.digest.clone(),
            },
        );
        spawn_lsp_session(
            authority,
            key,
            document_id,
            context,
            uri,
            language_id,
            ledger.text,
            config.effective,
            command_rx,
            self.lsp_runtime_tx.clone(),
            cancellation,
        );
    }

    pub(crate) fn queue_current_lsp_change(&mut self) {
        let Some(document_id) = self.current_doc else {
            return;
        };
        let keys: Vec<_> = self
            .lsp_sessions
            .keys()
            .filter(|(_, id)| *id == document_id)
            .cloned()
            .collect();
        for (key, id) in keys {
            let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == id) else {
                continue;
            };
            let Some(ledger) = buffer.lsp_ledger(&key.authority_identity, &key.execution_context) else {
                continue;
            };
            let change = LspChange {
                context: hermito_protocol::lsp::LspContext {
                    workspace_epoch: hermito_protocol::WorkspaceEpoch(ledger.workspace_epoch.0),
                    environment_epoch: ledger.environment_epoch,
                    document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
                    sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
                    session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
                    execution_context: ledger.context.clone(),
                    authority_identity: ledger.authority_identity.clone(),
                },
                uri: document_uri(buffer, id),
                version: ledger.sent_version,
                text: ledger.text.clone(),
            };
            if let Some(session) = self.lsp_sessions.get_mut(&(key, id)) {
                match session.command_tx.try_send(LspSessionCommand::Change(change.clone())) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        session.pending_change = Some(change);
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        session.pending_change = None;
                    }
                }
            }
        }
    }

    pub(crate) fn flush_pending_lsp_changes(&mut self) {
        for session in self.lsp_sessions.values_mut() {
            let Some(change) = session.pending_change.clone() else {
                continue;
            };
            if session
                .command_tx
                .try_send(LspSessionCommand::Change(change))
                .is_ok()
            {
                session.pending_change = None;
            }
        }
    }

    pub(crate) fn take_pending_definition_open(&mut self) -> Option<std::path::PathBuf> {
        self.pending_definition_open.take()
    }

    pub(crate) fn try_recv_lsp_runtime_event(
        &mut self,
    ) -> Result<LspRuntimeEvent, tokio::sync::mpsc::error::TryRecvError> {
        self.lsp_runtime_rx.try_recv()
    }

    pub(crate) fn apply_lsp_runtime_event(&mut self, event: LspRuntimeEvent) {
        match event {
            LspRuntimeEvent::State {
                key,
                context,
                state,
            } => {
                if self.is_current_language_service_context(&key)
                    && self.lsp_supervisor.is_current_session(&key, &context)
                {
                    self.record_language_service_state(key, state);
                }
            }
            LspRuntimeEvent::Initialized { key, context } => {
                if !self.is_current_language_service_context(&key)
                    || !self.lsp_supervisor.is_current_session(&key, &context)
                {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        reason = "stale_session",
                        "discarded stale LSP initialization"
                    );
                    return;
                }
                if let Err(error) = self.lsp_supervisor.mark_ready(&key, &context) {
                    self.record_language_service_state(
                        key,
                        crate::lsp::LanguageServiceState::Failed {
                            message: error.to_string(),
                        },
                    );
                }
            }
            LspRuntimeEvent::TransportLoss {
                key,
                context,
                detail,
            } => {
                if !self.is_current_language_service_context(&key) {
                    return;
                }
                match self
                    .lsp_supervisor
                    .observe_transport_loss(&key, &context, detail)
                {
                    crate::lsp::RestartDecision::Restart {
                        session_generation,
                        delay,
                    } => {
                        self.lsp_sessions.retain(|(session_key, _), _| session_key != &key);
                        let events = self.lsp_runtime_tx.clone();
                        tokio::spawn(async move {
                            let cancellation = tokio_util::sync::CancellationToken::new();
                            if crate::lsp::LspSupervisor::wait_for_restart(delay, &cancellation).await {
                                let _ = events.try_send(LspRuntimeEvent::Restart {
                                    key,
                                    generation: session_generation,
                                });
                            }
                        });
                    }
                    crate::lsp::RestartDecision::Exhausted
                    | crate::lsp::RestartDecision::IgnoredStale => {}
                }
            }
            LspRuntimeEvent::Restart { key, generation } => {
                if self.is_current_language_service_context(&key)
                    && self.lsp_supervisor.session_generation(&key) == Some(generation)
                {
                    self.start_language_service_for_current_document();
                }
            }
            LspRuntimeEvent::WorkspaceEdit {
                key,
                document_id,
                context,
                request_id,
                edit,
                client,
            } => {
                if !self.is_current_language_service_context(&key)
                    || !self.lsp_supervisor.is_current_session(&key, &context)
                {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        sent_version = context.sent_version.0,
                        revision = ?context.document_revision,
                        reason = "stale_session",
                        "rejected LSP workspace edit"
                    );
                    send_workspace_edit_result(
                        client,
                        context,
                        request_id,
                        false,
                        Some("workspace edit request is stale"),
                    );
                    return;
                }
                let mut rejection = None;
                if edit.validate().is_err() {
                    rejection = Some("workspace edit is malformed");
                } else {
                    match self.buffers.iter().find(|buffer| buffer.id() == document_id) {
                        Some(buffer) => match buffer
                            .lsp_ledger(&key.authority_identity, &key.execution_context)
                        {
                            Some(ledger) => {
                                let sent = hermito_protocol::lsp::LspContext {
                                    workspace_epoch: hermito_protocol::WorkspaceEpoch(
                                        ledger.workspace_epoch.0,
                                    ),
                                    environment_epoch: ledger.environment_epoch,
                                    document_revision: Some(
                                        hermito_protocol::DocumentRevision(ledger.revision.0),
                                    ),
                                    sent_version: hermito_protocol::lsp::SentVersion(
                                        ledger.sent_version as u64,
                                    ),
                                    session_generation: hermito_protocol::lsp::SessionGeneration(
                                        ledger.session_generation,
                                    ),
                                    execution_context: ledger.context.clone(),
                                    authority_identity: ledger.authority_identity.clone(),
                                };
                                if crate::lsp::LspClient::<AppLspTransport>::filter_stale_context(
                                    &context,
                                    &sent,
                                    Some(ledger),
                                )
                                .is_err()
                                {
                                    tracing::debug!(
                                        authority_identity = %context.authority_identity.0,
                                        execution_context = ?context.execution_context,
                                        session_generation = context.session_generation.0,
                                        sent_version = context.sent_version.0,
                                        revision = ?context.document_revision,
                                        reason = "stale_document",
                                        "rejected LSP workspace edit"
                                    );
                                    send_workspace_edit_result(
                                        client,
                                        context,
                                        request_id,
                                        false,
                                        Some("workspace edit request is stale"),
                                    );
                                    return;
                                }
                            }
                            None => rejection = Some("workspace edit has no current document ledger"),
                        },
                        None => rejection = Some("workspace edit document is unavailable"),
                    }
                }
                if let Some(reason) = rejection {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        sent_version = context.sent_version.0,
                        revision = ?context.document_revision,
                        reason,
                        "rejected LSP workspace edit"
                    );
                    self.status_message = reason.into();
                    send_workspace_edit_result(client, context, request_id, false, Some(reason));
                    return;
                }

                let authority = self.authorities.get(self.current_authority).cloned();
                let applied = match authority.map(|authority| authority.kind) {
                    Some(AuthorityKind::Local) => self.local_authority.clone().map(|authority| {
                        self.apply_rename_workspace_edit(
                            authority.as_ref(),
                            &key,
                            document_id,
                            context.clone(),
                            &edit,
                        )
                    }),
                    Some(AuthorityKind::Ssh) => self.current_ssh_authority().map(|authority| {
                        self.apply_rename_workspace_edit(
                            authority.as_ref(),
                            &key,
                            document_id,
                            context.clone(),
                            &edit,
                        )
                    }),
                    Some(AuthorityKind::DevContainer) | None => None,
                };
                match applied {
                    Some(Ok(crate::lsp::RenameApplyOutcome::Applied))
                    | Some(Ok(crate::lsp::RenameApplyOutcome::NoChanges)) => {
                        tracing::debug!(
                            authority_identity = %context.authority_identity.0,
                            execution_context = ?context.execution_context,
                            session_generation = context.session_generation.0,
                            sent_version = context.sent_version.0,
                            revision = ?context.document_revision,
                            state = "applied",
                            "accepted LSP workspace edit"
                        );
                        self.status_message = "Workspace edit applied.".into();
                        send_workspace_edit_result(client, context, request_id, true, None);
                    }
                    Some(Ok(crate::lsp::RenameApplyOutcome::Rejected)) => {
                        tracing::debug!(
                            authority_identity = %context.authority_identity.0,
                            execution_context = ?context.execution_context,
                            session_generation = context.session_generation.0,
                            reason = "authority_rejected",
                            "rejected LSP workspace edit"
                        );
                        self.status_message = "Workspace edit was rejected by the authority.".into();
                        send_workspace_edit_result(
                            client,
                            context,
                            request_id,
                            false,
                            Some("workspace edit was rejected by the authority"),
                        );
                    }
                    Some(Err(_)) => {
                        tracing::debug!(
                            authority_identity = %context.authority_identity.0,
                            execution_context = ?context.execution_context,
                            session_generation = context.session_generation.0,
                            reason = "apply_failed",
                            "rejected LSP workspace edit"
                        );
                        self.status_message = "Workspace edit apply failed.".into();
                        send_workspace_edit_result(
                            client,
                            context,
                            request_id,
                            false,
                            Some("workspace edit apply failed"),
                        );
                    }
                    None => {
                        tracing::debug!(
                            authority_identity = %context.authority_identity.0,
                            execution_context = ?context.execution_context,
                            session_generation = context.session_generation.0,
                            reason = "authority_unavailable",
                            "rejected LSP workspace edit"
                        );
                        self.status_message = "Workspace edit authority is unavailable.".into();
                        send_workspace_edit_result(
                            client,
                            context,
                            request_id,
                            false,
                            Some("workspace edit authority is unavailable"),
                        );
                    }
                }
            }
            LspRuntimeEvent::ProviderResult {
                key,
                document_id,
                context,
                revision,
                position,
                kind,
                result,
            } => {
                if !self.is_current_language_service_context(&key)
                    || !self.lsp_supervisor.is_current_session(&key, &context)
                {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        sent_version = context.sent_version.0,
                        revision = ?context.document_revision,
                        reason = "stale_session",
                        "discarded stale LSP provider result"
                    );
                    return;
                }
                let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == document_id) else {
                    return;
                };
                let Some(ledger) = buffer.lsp_ledger(&key.authority_identity, &key.execution_context) else {
                    return;
                };
                if buffer.revision() != revision {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        sent_version = context.sent_version.0,
                        revision = revision.0,
                        reason = "document_revision_mismatch",
                        "discarded stale LSP provider result"
                    );
                    return;
                }
                let sent = hermito_protocol::lsp::LspContext {
                    workspace_epoch: hermito_protocol::WorkspaceEpoch(ledger.workspace_epoch.0),
                    environment_epoch: ledger.environment_epoch,
                    document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
                    sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
                    session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
                    execution_context: ledger.context.clone(),
                    authority_identity: ledger.authority_identity.clone(),
                };
                if crate::lsp::LspClient::<AppLspTransport>::filter_stale_context(
                    &context,
                    &sent,
                    Some(ledger),
                )
                .is_err()
                {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        sent_version = context.sent_version.0,
                        revision = revision.0,
                        reason = "context_or_ledger_mismatch",
                        "discarded stale LSP provider result"
                    );
                    return;
                }
                let label = match kind {
                    LspProviderKind::Completion => "Completion",
                    LspProviderKind::Hover => "Hover",
                    LspProviderKind::Definition => "Definition",
                    LspProviderKind::RenamePreparation | LspProviderKind::Rename => "Rename",
                };
                match result {
                    Err(error) => {
                        self.status_message = format!("{label} request failed: {error}");
                    }
                    Ok(LspProviderResult::ReadOnly(crate::lsp::ProviderOutcome::Response(payload))) => {
                        match kind {
                            LspProviderKind::Completion => {
                                let candidates = completion_candidates(&payload);
                                if candidates.is_empty() {
                                    self.status_message = "Completion returned no candidates.".into();
                                } else {
                                    let count = candidates.len();
                                    self.overlay = Overlay::Completion {
                                        document_id,
                                        revision,
                                        position,
                                        candidates,
                                        selected: 0,
                                        invoker: self.focus,
                                    };
                                    self.status_message = format!("Completion: {count} candidates.");
                                }
                            }
                            LspProviderKind::Hover => {
                                if let Some(document) = hover_document(&payload) {
                                    self.overlay = Overlay::Hover {
                                        document,
                                        invoker: self.focus,
                                    };
                                    self.status_message = "Hover details available.".into();
                                } else {
                                    self.status_message = "Hover returned no displayable details.".into();
                                }
                            }
                            LspProviderKind::Definition => {
                                if let Some((uri, range)) = definition_target(&payload) {
                                    if let Some((target_id, start, end)) = self
                                        .buffers
                                        .iter()
                                        .find(|candidate| document_uri(candidate, candidate.id()) == uri.as_str())
                                        .and_then(|target| {
                                            let mapper = crate::lsp::CoordinateMapper::new(target.rope());
                                            Some((
                                                target.id(),
                                                mapper.lsp_position_to_byte(range.start)?,
                                                mapper.lsp_position_to_byte(range.end)?,
                                            ))
                                        })
                                    {
                                        self.layout.open_or_focus_editor(target_id);
                                        self.current_doc = Some(target_id);
                                        self.layout.set_editor_selection(start, end);
                                        self.focus = Landmark::Editor;
                                        self.status_message = "Definition opened.".into();
                                    } else if let Ok(path) = uri.to_file_path() {
                                        self.pending_definition = Some(PendingDefinition {
                                            path: path.clone(),
                                            range,
                                        });
                                        self.pending_definition_open = Some(path);
                                        self.status_message = "Opening definition…".into();
                                    } else {
                                        self.status_message = "Definition target cannot be opened by this workspace.".into();
                                    }
                                } else {
                                    self.status_message = "Definition returned no location.".into();
                                }
                            }
                            LspProviderKind::RenamePreparation | LspProviderKind::Rename => {
                                self.status_message = format!("{label} response received.");
                            }
                        }
                    }
                    Ok(LspProviderResult::ReadOnly(crate::lsp::ProviderOutcome::Empty)) => {
                        self.status_message = format!("{label} returned no result.");
                    }
                    Ok(LspProviderResult::RenamePrepared(Some(preparation))) => {
                        self.overlay = Overlay::RenameInput {
                            document_id,
                            key,
                            context,
                            position,
                            preparation,
                            new_name: String::new(),
                            invoker: self.focus,
                        };
                        self.status_message = "Rename: type a new name, Enter to confirm, Esc to cancel.".into();
                    }
                    Ok(LspProviderResult::RenamePrepared(None)) => {
                        self.status_message = "Rename is unavailable at the cursor.".into();
                    }
                    Ok(LspProviderResult::RenameWorkspaceEdit(
                        crate::lsp::RenameRequestOutcome::NotRenameable,
                    )) => {
                        self.status_message = "Rename returned no changes.".into();
                    }
                    Ok(LspProviderResult::RenameWorkspaceEdit(
                        crate::lsp::RenameRequestOutcome::WorkspaceEditResponse {
                            ticket: _,
                            workspace_edit,
                        },
                    )) => {
                        let authority = self.authorities.get(self.current_authority).cloned();
                        let applied = match authority.map(|authority| authority.kind) {
                            Some(AuthorityKind::Local) => self.local_authority.clone().map(|authority| {
                                self.apply_rename_workspace_edit(
                                    authority.as_ref(),
                                    &key,
                                    document_id,
                                    context,
                                    &workspace_edit,
                                )
                            }),
                            Some(AuthorityKind::Ssh) => self.current_ssh_authority().map(|authority| {
                                self.apply_rename_workspace_edit(
                                    authority.as_ref(),
                                    &key,
                                    document_id,
                                    context,
                                    &workspace_edit,
                                )
                            }),
                            Some(AuthorityKind::DevContainer) | None => None,
                        };
                        match applied {
                            Some(Ok(crate::lsp::RenameApplyOutcome::Applied)) => {
                                self.status_message = "Rename applied.".into();
                            }
                            Some(Ok(crate::lsp::RenameApplyOutcome::NoChanges)) => {
                                self.status_message = "Rename returned no changes.".into();
                            }
                            Some(Ok(crate::lsp::RenameApplyOutcome::Rejected)) => {
                                self.status_message = "Rename was rejected by the authority.".into();
                            }
                            Some(Err(error)) => {
                                self.status_message = format!("Rename apply failed: {error}");
                            }
                            None => {
                                self.status_message =
                                    "Rename apply failed: authority is unavailable.".into();
                            }
                        }
                    }
                };
            }
            LspRuntimeEvent::Diagnostics {
                key,
                document_id,
                context,
                uri,
                version,
                diagnostics,
            } => {
                if !self.is_current_language_service_context(&key)
                    || !self.lsp_supervisor.is_current_session(&key, &context)
                {
                    return;
                }
                let Some(buffer) = self.buffers.iter().find(|buffer| buffer.id() == document_id) else {
                    return;
                };
                let Some(ledger) = buffer.lsp_ledger(&key.authority_identity, &key.execution_context) else {
                    return;
                };
                let sent = hermito_protocol::lsp::LspContext {
                    workspace_epoch: hermito_protocol::WorkspaceEpoch(ledger.workspace_epoch.0),
                    environment_epoch: ledger.environment_epoch,
                    document_revision: Some(hermito_protocol::DocumentRevision(ledger.revision.0)),
                    sent_version: hermito_protocol::lsp::SentVersion(ledger.sent_version as u64),
                    session_generation: hermito_protocol::lsp::SessionGeneration(ledger.session_generation),
                    execution_context: ledger.context.clone(),
                    authority_identity: ledger.authority_identity.clone(),
                };
                if version != Some(ledger.sent_version)
                    || crate::lsp::LspClient::<AppLspTransport>::filter_stale_context(
                        &context,
                        &sent,
                        Some(ledger),
                    )
                    .is_err()
                {
                    return;
                }
                let Ok(normalized) =
                    crate::lsp::LspClient::<AppLspTransport>::convert_diagnostics(&diagnostics)
                else {
                    return;
                };
                let entries = normalized
                    .into_iter()
                    .map(|diagnostic| ProblemDiagnostic {
                        document_id,
                        uri: uri.clone(),
                        range: diagnostic.range,
                        severity: diagnostic.severity.map(|severity| match severity {
                            lsp_types::DiagnosticSeverity::ERROR => crate::lsp::NormalizedDiagnosticSeverity::Error,
                            lsp_types::DiagnosticSeverity::WARNING => crate::lsp::NormalizedDiagnosticSeverity::Warning,
                            lsp_types::DiagnosticSeverity::INFORMATION => crate::lsp::NormalizedDiagnosticSeverity::Information,
                            lsp_types::DiagnosticSeverity::HINT => crate::lsp::NormalizedDiagnosticSeverity::Hint,
                            _ => crate::lsp::NormalizedDiagnosticSeverity::Information,
                        }),
                        message: diagnostic.message,
                        source: diagnostic.source,
                    })
                    .collect::<Vec<_>>();
                self.diagnostics.insert((key.clone(), document_id), entries);
                let count = self
                    .diagnostics
                    .iter()
                    .filter(|((service, _), _)| service == &key)
                    .map(|(_, diagnostics)| diagnostics.len())
                    .sum();
                self.record_language_diagnostic_count(key, count);
            }
        }
    }

    fn language_service_summary(&self) -> &'static str {
        self.language_service_states
            .values()
            .map(|state| match state {
                crate::lsp::LanguageServiceState::Failed { .. } => (0, state.status_label()),
                crate::lsp::LanguageServiceState::VersionMismatch { .. } => {
                    (1, state.status_label())
                }
                crate::lsp::LanguageServiceState::NotFound { .. } => (2, state.status_label()),
                crate::lsp::LanguageServiceState::Blocked { .. } => (3, state.status_label()),
                crate::lsp::LanguageServiceState::Starting => (4, state.status_label()),
                crate::lsp::LanguageServiceState::Ready => (5, state.status_label()),
            })
            .min_by_key(|(priority, _)| *priority)
            .map(|(_, label)| label)
            .unwrap_or("idle")
    }

    fn normalized_diagnostic_count(&self) -> usize {
        self.language_diagnostic_counts
            .values()
            .copied()
            .fold(0usize, usize::saturating_add)
    }


    pub fn snapshot(&self) -> AppSnapshot {
        let open_tabs: Vec<EditorTabSnapshot> = self
            .layout
            .editor_tabs
            .iter()
            .filter_map(|tab| {
                self.buffers
                    .iter()
                    .find(|b| b.id() == tab.doc_id)
                    .map(|buf| {
                        let (hl_rev, hls) = self
                            .syntax_highlights
                            .get(&tab.doc_id)
                            .cloned()
                            .unwrap_or((buf.revision(), vec![]));
                        let sel = tab.selection_anchor.map(|a| (a, tab.cursor_byte));
                        let ps = buf.path_state();
                        let title = ps.display_name();
                        let path_label = match ps {
                            BufferPathState::Saved(p) => p.to_string_lossy().into_owned(),
                            BufferPathState::Recovered { original } => {
                                format!("Recovered · {}", original.to_string_lossy())
                            }
                            BufferPathState::Untitled { suggested_name } => {
                                format!("<{}>", suggested_name)
                            }
                        };
                        EditorTabSnapshot {
                            id: tab.doc_id,
                            revision: buf.revision(),
                            title,
                            path_label,
                            language: buf.language(),
                            text: buf.text(),
                            dirty: buf.is_dirty(),
                            cursor_byte: tab.cursor_byte,
                            selection: sel,
                            scroll_line: tab.scroll_line,
                            highlights: if hl_rev == buf.revision() {
                                hls
                            } else {
                                vec![]
                            },
                        }
                    })
            })
            .collect();
        let current_buffer = self
            .current_doc
            .and_then(|id| open_tabs.iter().find(|t| t.id == id).cloned());
        let cur_trust = self
            .authorities
            .get(self.current_authority)
            .map(|a| a.trust)
            .unwrap_or(TrustLevel::InspectOnly);
        let overlay = match &self.overlay {
            Overlay::None => OverlaySnapshot::None,
            Overlay::TrustReview {
                focused_grant,
                invoker,
                workspace_root,
                authority_label,
            } => OverlaySnapshot::TrustReview {
                workspace_root: workspace_root.clone(),
                authority_label: authority_label.clone(),
                trust: cur_trust,
                capabilities: vec!["execution".into(), "git".into(), "terminal".into()],
                focused_grant: *focused_grant,
                invoker: *invoker,
            },
            Overlay::CommandPalette {
                query,
                items,
                selected,
                invoker,
            } => OverlaySnapshot::CommandPalette {
                query: query.clone(),
                items: items.clone(),
                selected: *selected,
                invoker: *invoker,
            },
            Overlay::SaveAs { path, invoker, .. } => OverlaySnapshot::SaveAs {
                path: path.clone(),
                invoker: *invoker,
            },
            Overlay::RenameInput {
                new_name, invoker, ..
            } => OverlaySnapshot::RenameInput {
                new_name: new_name.clone(),
                invoker: *invoker,
            },
            Overlay::SshPassphrase {
                authority_label,
                passphrase,
                invoker,
            } => OverlaySnapshot::SshPassphrase {
                authority_label: authority_label.clone(),
                length: passphrase.chars().count(),
                invoker: *invoker,
            },
            Overlay::Completion {
                position,
                candidates,
                selected,
                invoker,
                ..
            } => OverlaySnapshot::Completion {
                position: *position,
                candidates: candidates
                    .iter()
                    .map(|candidate| CompletionCandidateSnapshot {
                        label: candidate.label.clone(),
                        detail: candidate.detail.clone(),
                    })
                    .collect(),
                selected: *selected,
                invoker: *invoker,
            },
            Overlay::Hover { document, invoker } => OverlaySnapshot::Hover {
                document: document.clone(),
                invoker: *invoker,
            },
        };
        let authority_label = self
            .authorities
            .get(self.current_authority)
            .map(|authority| authority.label.clone())
            .unwrap_or_default();
        let terminal = if self.terminal_starting {
            TerminalSnapshot {
                state: TerminalViewState::Starting,
                surface: None,
                captured: false,
                authority_label,
            }
        } else if let Some(session) = &self.terminal {
            let state = match session.state() {
                crate::pty::PtySessionState::Running => TerminalViewState::Running,
                crate::pty::PtySessionState::Exited => TerminalViewState::Exited,
                crate::pty::PtySessionState::Lost => TerminalViewState::Lost,
            };
            TerminalSnapshot {
                state,
                surface: Some(session.surface_handle()),
                captured: self.terminal_capture && state == TerminalViewState::Running,
                authority_label,
            }
        } else {
            TerminalSnapshot {
                authority_label,
                ..TerminalSnapshot::default()
            }
        };
        AppSnapshot {
            epoch: self.epoch,
            layout: self.layout.clone(),
            focus: self.focus,
            overlay,
            authorities: self.authorities.clone(),
            current_authority_idx: self.current_authority,
            current_trust: cur_trust,
            current_buffer,
            open_editor_tabs: open_tabs,
            active_editor_tab: self.layout.active_editor_tab,
            project: self.project.clone(),
            status: StatusSnapshot {
                view: "editor".into(),
                branch: None,
                problems: self.normalized_diagnostic_count(),
                service: self.language_service_summary().into(),
                message: if self.status_message.is_empty() {
                    None
                } else {
                    Some(self.status_message.clone())
                },
                line: 1,
                column: 1,
            },
            terminal,
            journal_lagging: !self.pending_checkpoints.is_empty()
                || !self.pending_compactions.is_empty(),
            workspace_root: self.workspace_root.clone(),
            workspace_name: self.workspace_name.clone(),
            diagnostics: self
                .diagnostics
                .values()
                .flat_map(|diagnostics| diagnostics.iter().cloned())
                .collect(),
        }
    }
    /// Restore full persisted state + journal recovery (called by lib::run after load+validate).
    /// Dirty recovery buffers first (journal), then validated clean tabs supplied by state (content read
    /// off-thread at startup in validate before UI). Rebuild layout.editor_tabs/current/selection/cursor/scroll
    /// strictly from validated state.tabs (not unvalidated paths or old layout tabs).
    /// Missing clean dropped (by validate), recovered dirty missing files kept. First-run welcome always
    /// gets an editor tab entry so typing visible.
    pub fn restore_state(
        state: crate::persistence::state::AppState,
        recovery: Recovery,
        journal: JournalHandle,
    ) -> Self {
        let epoch = state.epoch;
        let mut buffers: Vec<Buffer> = Vec::new();
        let first_recovered = recovery.buffers.first().map(|recovered| recovered.id);
        // Dirty first from journal recovery (may be missing on disk).
        for recovered in &recovery.buffers {
            let path_state = match &recovered.path {
                Some(path) => BufferPathState::Recovered {
                    original: path.clone(),
                },
                None => BufferPathState::Untitled {
                    suggested_name: "recovered".into(),
                },
            };
            buffers.push(Buffer::recover(
                recovered.id,
                recovered.language,
                &recovered.content,
                recovered.revision,
                path_state,
            ));
        }
        // Clean tabs from validated state (content loaded off-thread before event loop).
        for t in &state.tabs {
            if buffers.iter().any(|b| b.id() == t.id) {
                continue;
            }
            let content = match (&t.path, &t.content) {
                (Some(_), Some(content)) => content.clone(),
                (Some(_), None) => continue,
                (None, Some(content)) => content.clone(),
                (None, None) => String::new(),
            };
            let lang = t.language;
            let path_state = match &t.path {
                Some(p) => BufferPathState::Saved(p.clone()),
                None => BufferPathState::Untitled {
                    suggested_name: "untitled".into(),
                },
            };
            buffers.push(Buffer::restore_clean(
                t.id,
                lang,
                &content,
                t.last_known_revision,
                path_state,
            ));
        }
        if buffers.is_empty() {
            let id = DocumentId::new();
            buffers.push(Buffer::new(id, Language::PlainText, "fn main() {}\n"));
        }
        // current: prefer a valid persisted current_tab if present in buffers (recovered or clean),
        // else recovered (first) or first buffer.
        let current_doc = if let Some(cid) = state.current_tab {
            if buffers.iter().any(|b| b.id() == cid) {
                Some(cid)
            } else {
                first_recovered.or_else(|| buffers.first().map(|buffer| buffer.id()))
            }
        } else {
            first_recovered.or_else(|| buffers.first().map(|buffer| buffer.id()))
        };

        // Rebuild editor_tabs from recovered dirty buffers FIRST, then validated clean state tabs
        // without duplicates. Use default view state for recovered (no persisted view if path was
        // missing at validate); preserve view state from state.tabs for clean. This makes dirty
        // missing-file recoveries always visible as tabs; dropped clean missing stay out.
        let mut layout = state.layout;
        layout.editor_tabs.clear();
        layout.active_editor_tab = 0;
        // recovered dirties first
        for rec in &recovery.buffers {
            if !layout.editor_tabs.iter().any(|t| t.doc_id == rec.id) {
                layout.editor_tabs.push(EditorTabState {
                    doc_id: rec.id,
                    scroll_line: 0,
                    cursor_byte: 0,
                    selection_anchor: None,
                });
            }
        }
        // then additional clean from state (with their saved views)
        for t in &state.tabs {
            if buffers.iter().any(|buffer| buffer.id() == t.id)
                && !layout.editor_tabs.iter().any(|tab| tab.doc_id == t.id)
            {
                let idx = layout.editor_tabs.len();
                layout.editor_tabs.push(EditorTabState {
                    doc_id: t.id,
                    scroll_line: t.scroll_top_line as u16,
                    cursor_byte: t.cursor_byte,
                    selection_anchor: t.selection_start_byte,
                });
                if Some(t.id) == current_doc {
                    layout.active_editor_tab = idx;
                }
            }
        }
        // ensure active for current if from recovered prefix
        if let Some(cur) = current_doc {
            if let Some(pos) = layout.editor_tabs.iter().position(|et| et.doc_id == cur) {
                layout.active_editor_tab = pos;
            }
        }
        if layout.editor_tabs.is_empty() {
            if let Some(id) = current_doc {
                layout.open_or_focus_editor(id);
            }
        }

        let mut authorities = Vec::new();
        for tr in &state.trust {
            let level = if tr.level == "trusted" {
                TrustLevel::Trusted
            } else {
                TrustLevel::InspectOnly
            };
            let kind = match tr.kind.as_str() {
                "ssh" => AuthorityKind::Ssh,
                "dev_container" => AuthorityKind::DevContainer,
                _ => AuthorityKind::Local,
            };
            authorities.push(AuthorityState {
                kind,
                label: tr.authority.clone(),
                trust: level,
                connection: if kind == AuthorityKind::Local {
                    AuthorityConnectionState::Connected
                } else {
                    AuthorityConnectionState::Disconnected
                },
            });
        }
        if authorities.is_empty() {
            authorities.push(AuthorityState {
                kind: AuthorityKind::Local,
                label: "host".into(),
                trust: TrustLevel::InspectOnly,
                connection: AuthorityConnectionState::Connected,
            });
        }
        let lsp_grants = state.lsp_grants;
        let workspace_root = state
            .trust
            .first()
            .map(|t| t.workspace_root.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/workspace".into());
        let focus = match state.focus.as_str() {
            "Toolbar" => Landmark::Toolbar,
            "Authority" => Landmark::Authority,
            "LeftStripe" => Landmark::LeftStripe,
            "PrimaryPane" => Landmark::PrimaryPane,
            "Editor" => Landmark::Editor,
            "ContextPane" => Landmark::ContextPane,
            "RightStripe" => Landmark::RightStripe,
            "BottomPane" => Landmark::BottomPane,
            "StatusBar" => Landmark::StatusBar,
            _ => Landmark::Editor,
        };
        let (lsp_supervisor, lsp_event_rx) =
            crate::lsp::LspSupervisor::channel(LSP_SUPERVISOR_EVENT_CHANNEL_CAPACITY);
        let (lsp_runtime_tx, lsp_runtime_rx) =
            tokio::sync::mpsc::channel(LSP_RUNTIME_EVENT_CHANNEL_CAPACITY);
        App {
            epoch,
            layout,
            buffers,
            current_doc,
            authorities,
            current_authority: 0,
            focus,
            overlay: Overlay::None,
            journal: Some(journal),
            pending_checkpoints: std::collections::HashMap::new(),
            pending_compactions: std::collections::HashMap::new(),
            syntax_highlights: std::collections::HashMap::new(),
            retained_syntax: std::collections::HashMap::new(),
            project: ProjectTreeSnapshot {
                tree: None,
                loading: true,
                scroll: 0,
                selected_row: 0,
            },
            status_message: String::new(),
            workspace_root,
            workspace_name: "workspace".into(),
            terminal: None,
            ssh_authorities: std::collections::HashMap::new(),
            lsp_grants,
            language_service_states: std::collections::HashMap::new(),
            language_diagnostic_counts: std::collections::HashMap::new(),
            language_servers: Vec::new(),
            local_authority: None,
            lsp_sessions: std::collections::HashMap::new(),
            lsp_runtime_tx,
            lsp_runtime_rx,
            diagnostics: std::collections::HashMap::new(),
            terminal_starting: false,
            terminal_launch_id: 0,
            terminal_capture: false,
            submitted_ssh_passphrase: None,
            pending_definition: None,
            pending_definition_open: None,
            lsp_supervisor,
            lsp_event_rx,
        }
    }
    /// Produce versioned state for durable save at shutdown.
    pub fn to_state(&self) -> crate::persistence::state::AppState {
        use std::path::PathBuf;
        let trust: Vec<crate::persistence::state::TrustRecord> = self
            .authorities
            .iter()
            .map(|a| crate::persistence::state::TrustRecord {
                workspace_root: PathBuf::from(&self.workspace_root),
                authority: a.label.clone(),
                kind: match a.kind {
                    AuthorityKind::Local => "local".into(),
                    AuthorityKind::Ssh => "ssh".into(),
                    AuthorityKind::DevContainer => "dev_container".into(),
                },
                level: match a.trust {
                    TrustLevel::Trusted => "trusted".into(),
                    TrustLevel::InspectOnly => "inspect_only".into(),
                },
            })
            .collect();
        let tabs: Vec<crate::persistence::state::TabMetadata> = self
            .layout
            .editor_tabs
            .iter()
            .filter_map(|t| {
                self.buffers.iter().find(|b| b.id() == t.doc_id).map(|buf| {
                    crate::persistence::state::TabMetadata {
                        id: t.doc_id,
                        path: buf.path().map(|p| p.to_path_buf()),
                        last_known_revision: buf.revision(),
                        cursor_byte: t.cursor_byte,
                        scroll_top_line: t.scroll_line as usize,
                        selection_start_byte: t.selection_anchor,
                        selection_end_byte: Some(t.cursor_byte),
                        content: None,
                        language: buf.language(),
                    }
                })
            })
            .collect();
        crate::persistence::state::AppState {
            version: 1,
            epoch: self.epoch,
            layout: self.layout.clone(),
            tabs,
            current_tab: self.current_doc,
            focus: match self.focus {
                Landmark::Toolbar => "Toolbar".into(),
                Landmark::Authority => "Authority".into(),
                Landmark::LeftStripe => "LeftStripe".into(),
                Landmark::PrimaryPane => "PrimaryPane".into(),
                Landmark::Editor => "Editor".into(),
                Landmark::ContextPane => "ContextPane".into(),
                Landmark::RightStripe => "RightStripe".into(),
                Landmark::BottomPane => "BottomPane".into(),
                Landmark::StatusBar => "StatusBar".into(),
            },
            trust,
            lsp_grants: self.lsp_grants.clone(),
        }
    }
    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }
    pub fn register_ssh_authority(&mut self, authority: Arc<crate::authority::ssh::SshAuthority>) {
        let label = crate::authority::Authority::label(authority.as_ref()).to_owned();
        let state = self
            .authorities
            .iter_mut()
            .find(|state| state.kind == AuthorityKind::Ssh && state.label == label);
        let trusted = state
            .as_ref()
            .is_some_and(|state| state.trust == TrustLevel::Trusted);
        if trusted {
            crate::authority::Authority::grant_execution(authority.as_ref());
        } else {
            crate::authority::Authority::revoke_execution(authority.as_ref());
        }
        // Generic terminal trust does not authorize LSP; restoration requires
        // the authority's canonical identity and the exact configuration digest.
        apply_persisted_lsp_grants(
            &self.lsp_grants,
            std::path::Path::new(&self.workspace_root),
            authority.as_ref(),
        );
        if let Some(state) = state {
            state.connection = if authority.is_connected() {
                AuthorityConnectionState::Connected
            } else {
                AuthorityConnectionState::Disconnected
            };
        } else {
            self.authorities.push(AuthorityState {
                kind: AuthorityKind::Ssh,
                label: label.clone(),
                trust: TrustLevel::InspectOnly,
                connection: if authority.is_connected() {
                    AuthorityConnectionState::Connected
                } else {
                    AuthorityConnectionState::Disconnected
                },
            });
        }
        self.ssh_authorities.insert(label, authority);
    }

    pub fn select_ssh_authority(&mut self, label: impl Into<String>) {
        let label = label.into();
        if let Some(index) = self
            .authorities
            .iter()
            .position(|authority| authority.kind == AuthorityKind::Ssh && authority.label == label)
        {
            self.current_authority = index;
        } else {
            self.authorities.push(AuthorityState {
                kind: AuthorityKind::Ssh,
                label,
                trust: TrustLevel::InspectOnly,
                connection: AuthorityConnectionState::Disconnected,
            });
            self.current_authority = self.authorities.len() - 1;
        }
        self.terminal_capture = false;
        self.terminal_starting = false;
        self.terminal_launch_id = self.terminal_launch_id.wrapping_add(1);
        if let Some(session) = self.terminal.take() {
            session.cancel();
            std::thread::spawn(move || {
                let _ = session.join_reader();
            });
        }
        self.status_message =
            "SSH authority selected in INSPECT ONLY mode. Accept its host key before granting execution."
                .into();
        self.focus = Landmark::Authority;
    }

    pub(crate) fn current_ssh_authority_label(&self) -> Option<String> {
        self.authorities
            .get(self.current_authority)
            .filter(|authority| authority.kind == AuthorityKind::Ssh)
            .map(|authority| authority.label.clone())
    }

    pub(crate) fn prompt_ssh_passphrase(&mut self, authority_label: String) {
        let invoker = self.focus;
        self.overlay = Overlay::SshPassphrase {
            authority_label,
            passphrase: Zeroizing::new(String::new()),
            invoker,
        };
        self.focus = Landmark::Authority;
        self.status_message = "Enter the selected SSH key passphrase.".into();
    }

    pub(crate) fn take_submitted_ssh_passphrase(&mut self) -> Option<(String, Zeroizing<Vec<u8>>)> {
        self.submitted_ssh_passphrase.take()
    }

    pub(crate) fn set_authority_connection(
        &mut self,
        label: &str,
        connection: AuthorityConnectionState,
        message: String,
    ) {
        if let Some(authority) = self
            .authorities
            .iter_mut()
            .find(|authority| authority.kind == AuthorityKind::Ssh && authority.label == label)
        {
            authority.connection = connection;
        }
        self.status_message = message;
        if connection == AuthorityConnectionState::Connected
            && self
                .authorities
                .get(self.current_authority)
                .is_some_and(|authority| authority.kind == AuthorityKind::Ssh && authority.label == label)
        {
            self.start_language_service_for_current_document();
        }
    }

    pub fn set_current_authority_connection(&mut self, connection: AuthorityConnectionState) {
        if let Some(authority) = self.authorities.get_mut(self.current_authority) {
            authority.connection = connection;
        }
        self.status_message = match connection {
            AuthorityConnectionState::Disconnected => "Authority disconnected.".into(),
            AuthorityConnectionState::Connecting => "Authority bootstrap in progress…".into(),
            AuthorityConnectionState::Connected => "Authority connected.".into(),
            AuthorityConnectionState::Lost => {
                "Authority transport Lost – request a new terminal session.".into()
            }
        };
    }
    pub fn refresh_authority_connections(&mut self) {
        for state in &mut self.authorities {
            if state.kind != AuthorityKind::Ssh {
                continue;
            }
            let connected = self
                .ssh_authorities
                .get(&state.label)
                .is_some_and(|authority| authority.is_connected());
            if connected {
                state.connection = AuthorityConnectionState::Connected;
            } else if state.connection == AuthorityConnectionState::Connected {
                state.connection = AuthorityConnectionState::Lost;
            }
        }
    }

    pub fn terminal_start_in_flight(&self) -> bool {
        self.terminal_starting
    }

    pub(crate) fn terminal_spawn_spec(&self) -> Option<TerminalSpawnSpec> {
        if !self.terminal_starting
            || self.terminal.is_some()
            || self.current_trust() != TrustLevel::Trusted
        {
            return None;
        }
        let area = self.layout.rect_bottom();
        let rows = area.height.saturating_sub(2).max(1);
        let cols = area.width.saturating_sub(2).max(1);
        let current = self.authorities.get(self.current_authority)?;
        match current.kind {
            AuthorityKind::Local => Some(TerminalSpawnSpec::Local {
                root: std::path::PathBuf::from(&self.workspace_root),
                epoch: self.epoch,
                launch_id: self.terminal_launch_id,
                rows,
                cols,
                lsp_grants: self.lsp_grants.clone(),
            }),
            AuthorityKind::Ssh => {
                let authority = self.ssh_authorities.get(&current.label)?.clone();
                if !authority.is_connected() {
                    return None;
                }
                let remote_root = crate::authority::Authority::root(authority.as_ref())
                    .to_string_lossy()
                    .into_owned();
                let command = hermito_protocol::request::CommandSpec {
                    program: "/bin/sh".into(),
                    args: vec!["-l".into()],
                    cwd: remote_root,
                    env: crate::authority::types::allowlisted_environment([
                        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
                        ("TERM".into(), "xterm-256color".into()),
                    ]),
                };
                let request = crate::authority::types::AuthorityRequest::new(
                    crate::authority::types::PtyRequest {
                        command,
                        size: portable_pty::PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        },
                    },
                    hermito_protocol::WorkspaceEpoch(self.epoch.0),
                    crate::authority::Authority::environment_epoch(authority.as_ref()),
                    None,
                );
                Some(TerminalSpawnSpec::Remote {
                    epoch: self.epoch,
                    launch_id: self.terminal_launch_id,
                    authority_label: current.label.clone(),
                    authority,
                    request,
                })
            }
            AuthorityKind::DevContainer => None,
        }
    }

    pub fn attach_terminal(
        &mut self,
        epoch: WorkspaceEpoch,
        launch_id: u64,
        authority_label: String,
        session: crate::pty::PtySession,
    ) {
        if epoch != self.epoch
            || launch_id != self.terminal_launch_id
            || !self.terminal_starting
            || session.workspace_epoch().0 != epoch.0
        {
            session.cancel();
            std::thread::spawn(move || {
                let _ = session.join_reader();
            });
            return;
        }
        let area = self.layout.rect_bottom();
        if let Err(error) = session.resize(
            area.height.saturating_sub(2).max(1),
            area.width.saturating_sub(2).max(1),
        ) {
            session.cancel();
            std::thread::spawn(move || {
                let _ = session.join_reader();
            });
            self.terminal_starting = false;
            self.terminal_capture = false;
            self.status_message = format!("Terminal resize failed during startup: {error}");
            return;
        }
        self.terminal = Some(session);
        self.terminal_starting = false;
        self.terminal_capture = true;
        self.focus = Landmark::BottomPane;
        self.status_message = format!("{authority_label} terminal running. Esc releases capture.");
    }

    pub fn fail_terminal_start(&mut self, epoch: WorkspaceEpoch, launch_id: u64, message: String) {
        if epoch == self.epoch && launch_id == self.terminal_launch_id && self.terminal_starting {
            self.terminal_starting = false;
            self.terminal_capture = false;
            self.status_message = format!("Terminal failed: {message}");
        }
    }

    pub fn syntax_is_current(&self, doc_id: DocumentId, revision: DocumentRevision) -> bool {
        self.syntax_highlights
            .get(&doc_id)
            .is_some_and(|(highlight_revision, _)| *highlight_revision == revision)
    }
    pub fn syntax_retained(
        &self,
        doc_id: DocumentId,
    ) -> Option<(DocumentRevision, Option<Tree>, String)> {
        self.retained_syntax.get(&doc_id).cloned()
    }

    /// Update highlights + retain Tree+source for the rev if still current (epoch/rev checked).
    /// Used by event_loop syntax result drain to support incremental next parses.
    pub(crate) fn apply_syntax_result(&mut self, sres: crate::syntax::SyntaxResult) {
        if sres.epoch != self.epoch {
            return;
        }
        if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == sres.doc_id) {
            if buf.revision() == sres.revision {
                self.syntax_highlights
                    .insert(sres.doc_id, (sres.revision, sres.highlights));
                self.retained_syntax
                    .insert(sres.doc_id, (sres.revision, sres.tree, sres.source));
            }
        }
    }

    fn current_ssh_authority(&self) -> Option<Arc<crate::authority::ssh::SshAuthority>> {
        let current = self.authorities.get(self.current_authority)?;
        if current.kind != AuthorityKind::Ssh {
            return None;
        }
        self.ssh_authorities.get(&current.label).cloned()
    }

    pub fn current_trust(&self) -> TrustLevel {
        self.authorities
            .get(self.current_authority)
            .map(|a| a.trust)
            .unwrap_or(TrustLevel::InspectOnly)
    }
}
