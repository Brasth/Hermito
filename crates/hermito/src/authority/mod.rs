use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use hermito_protocol::request::{EnvironmentEpoch, WorkspaceEpoch};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    config::EffectiveLspConfig,
    lsp::LspTransport,
    process::{ExecResult, SupervisorError},
    pty::{PtySession, PtySessionError},
};
use hermito_protocol::lsp::LspContext;

use types::{
    AuthorityKind, AuthorityRequest, AuthorityResult, AuthorityTrust, ExecRequest,
    LspWorkspaceEditPreconditions, PtyRequest, ReadFileRequest, WriteFileRequest,
};



pub mod local;
pub mod ssh;
pub mod tuf_verifier;
pub mod types;

pub type AuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<AuthorityResult<T>, AuthorityError>> + Send + 'a>>;

pub trait Authority: Send + Sync {
    fn kind(&self) -> AuthorityKind;
    fn label(&self) -> &str;
    fn root(&self) -> &Path;
    fn workspace_epoch(&self) -> WorkspaceEpoch;
    fn environment_epoch(&self) -> EnvironmentEpoch;
    fn trust(&self) -> AuthorityTrust;
    fn grant_execution(&self);
    fn revoke_execution(&self);
    /// Stable host-side authority identity string. Does not expose transport-specific types
    /// (Local vs SSH details). Used for scoping persisted LSP grants.
    fn host_authority_id(&self) -> String;
    fn grant_lsp_execution(&self, config_digest: &str);
    fn revoke_lsp_execution(&self, config_digest: &str);
    /// True iff this authority holds an explicit grant for the *exact* given effective config digest.
    /// Generic terminal trust is independent; absent or mismatched digest always yields false (InspectOnly semantics for LSP).
    fn is_lsp_execution_granted(&self, config_digest: &str) -> bool;


    /// Typed LSP start. MUST check exact digest grant using is_lsp_execution_granted BEFORE any PATH lookup, version probe, argv, cwd, env construction or spawn/dispatch.
    /// Local: direct spawn with root cwd + constructed allowlisted env, no shell. Returns cancellation-owned transport.
    /// SSH: validates then sends canonical LspV1::Start over multiplexer.
    /// DevContainer: returns explicit Unsupported.
    fn start_lsp<'a>(
        &'a self,
        context: LspContext,
        effective_config: EffectiveLspConfig,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn LspTransport>, AuthorityError>> + Send + 'a>>;

    /// Transactional authority WorkspaceEdit. Full batch validation of paths,
    /// trust, epochs, context, and host-only Buffer preconditions happens
    /// before any staging, commit, or remote dispatch.
    fn apply_lsp_workspace_edit<'a>(
        &'a self,
        request: AuthorityRequest<types::LspWorkspaceEditRequest>,
        preconditions: LspWorkspaceEditPreconditions<'a>,
    ) -> AuthorityFuture<'a, bool>;

    fn read_file<'a>(
        &'a self,
        request: AuthorityRequest<ReadFileRequest>,
    ) -> AuthorityFuture<'a, Vec<u8>>;
    fn write_file<'a>(
        &'a self,
        request: AuthorityRequest<WriteFileRequest>,
    ) -> AuthorityFuture<'a, usize>;
    fn spawn_pty<'a>(
        &'a self,
        request: AuthorityRequest<PtyRequest>,
        cancellation: CancellationToken,
    ) -> AuthorityFuture<'a, PtySession>;
    fn exec<'a>(
        &'a self,
        request: AuthorityRequest<ExecRequest>,
        cancellation: CancellationToken,
    ) -> AuthorityFuture<'a, ExecResult>;
}

#[derive(Debug, Error)]
pub enum AuthorityError {
    #[error("authority is inspect only; explicit execution trust is required")]
    InspectOnly,
    #[error("authority path escapes workspace root: {0}")]
    PathEscapesRoot(PathBuf),
    #[error("document file request requires a document revision")]
    MissingDocumentRevision,
    #[error("stale authority epoch: workspace {actual_workspace:?}/{expected_workspace:?}, environment {actual_environment:?}/{expected_environment:?}")]
    StaleEpoch {
        expected_workspace: WorkspaceEpoch,
        actual_workspace: WorkspaceEpoch,
        expected_environment: EnvironmentEpoch,
        actual_environment: EnvironmentEpoch,
    },
    #[error("authority output exceeds limit {limit}")]
    OutputLimit { limit: usize },
    #[error("authority input exceeds limit {limit}")]
    InputLimit { limit: usize },
    #[error("authority I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("PTY failed: {0}")]
    Pty(#[from] PtySessionError),
    #[error("process failed: {0}")]
    Process(#[from] SupervisorError),
    #[error("SSH transport failed: {0}")]
    Ssh(String),
    #[error("remote helper verification failed: {0}")]
    Verification(String),
    #[error("remote protocol failed: {0}")]
    Protocol(String),
    #[error("devcontainer lsp execution context is unsupported until phase 5 adapter")]
    UnsupportedExecutionContext,
    #[error("lsp workspace edit conflict: {0}")]
    LspEditConflict(String),
    #[error("LSP workspace edit is missing a validated host precondition for {path:?}")]
    MissingLspEditPrecondition { path: PathBuf },
    #[error("LSP workspace edit precondition revision mismatch for {path:?}: expected {expected:?}, actual {actual:?}")]
    LspEditPreconditionMismatch {
        path: PathBuf,
        expected: crate::document::DocumentRevision,
        actual: crate::document::DocumentRevision,
    },
    #[error("LSP workspace edit host precondition context mismatch for {path:?}")]
    LspEditPreconditionContextMismatch { path: PathBuf },
}

