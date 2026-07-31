use std::collections::HashMap;

pub use crate::document::{DocumentRevision, WorkspaceEpoch};
pub use hermito_protocol::request::{EnvironmentEpoch, ExecutionContextV1};
pub use hermito_protocol::lsp::{
    AuthorityIdentity, LspContext, SentVersion, SessionGeneration,
};

pub mod coordinate;
pub mod supervisor;
pub mod jsonrpc;
pub mod client;
pub mod diagnostics;
pub mod requests;

pub use coordinate::CoordinateMapper;
pub use client::{
    DirectTransport, Incoming, LspClient, LspClientError, LspStaleDiscard, LspTransport,
};
pub use supervisor::{
    LanguageId, LanguageIdError, LanguageServiceState, LspSupervisor, RestartBudget,
    RestartDecision, SupervisorEvent, SupervisorKey, LSP_EXECUTION_TRUST_REQUIRED,
};
pub use jsonrpc::{
    make_request_id, make_string_request_id,
    parse_and_validate_frame, position_from_json, range_from_json, read_lsp_frame,
    should_accept_diagnostics, transactional_workspace_edit_from_lsp,
    validate_and_convert_diagnostic, validate_and_convert_position,
    validate_and_convert_range, validate_and_convert_text_edit, validate_diagnostics,
    validate_jsonrpc_envelope, validate_workspace_edit, write_lsp_frame, JsonRpcFrameError,
    VersionlessPolicy, MAX_AGGREGATE_PENDING, MAX_ARRAY, MAX_CONTENT_LENGTH, MAX_DIAGNOSTICS,
    MAX_STRING, MAX_TEXT_EDITS, MAX_VERSION,
};
pub use diagnostics::{
    route_publish_diagnostics, DiagnosticDiscard, DiagnosticError, DiagnosticEvent,
    DiagnosticEventSink, DiagnosticRoute, NormalizedDiagnostic, NormalizedDiagnosticSeverity,
};
pub use requests::{
    CompletionProvider, DefinitionProvider, HoverProvider, ProviderDocument, ProviderError,
    ProviderOutcome, ProviderRequest, RenameApplyOutcome, RenamePreparation, RenameProvider,
    RenameRequestOutcome, RenameTicket,
};

/// Per-authority, per-execution-context LSP revision ledger entry.
///
/// Buffer is the sole owner of rope + revision. This ledger records the
/// correspondence after each accepted edit for one exact authority identity
/// and LSP context. Used for didChange scheduling, version tagging, session
/// correlation, epoch validation, and stale detection.
#[derive(Clone, Debug)]
pub struct LspDocumentLedger {
    /// Host authority identity this ledger tracks.
    pub authority_identity: AuthorityIdentity,
    /// Execution context within the host authority this ledger tracks.
    pub context: ExecutionContextV1,
    /// Document revision (from Buffer) at this ledger entry.
    pub revision: DocumentRevision,
    /// LSP protocol version number corresponding to this revision (incremented atomically on edits).
    pub sent_version: i32,
    /// Session generation. Bumped on explicit reset when LSP server or session for this context restarts.
    pub session_generation: u64,
    /// Workspace epoch captured for this ledger state.
    pub workspace_epoch: WorkspaceEpoch,
    /// Environment epoch captured for this ledger state.
    pub environment_epoch: EnvironmentEpoch,
    /// Full-text snapshot at the revision/sent_version. Allocation only on ledger updates.
    pub text: String,
}

/// Ledgers are partitioned by host authority, then execution context, so a
/// local and SSH authority with the same context can never share state.
pub type LspLedgerMap = HashMap<AuthorityIdentity, HashMap<ExecutionContextV1, LspDocumentLedger>>;
