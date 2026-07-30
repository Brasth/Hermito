use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use hermito_protocol::request::{EnvironmentEpoch, WorkspaceEpoch};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    process::{ExecResult, SupervisorError},
    pty::{PtySession, PtySessionError},
};

use types::{
    AuthorityKind, AuthorityRequest, AuthorityResult, AuthorityTrust, ExecRequest, PtyRequest,
    ReadFileRequest, WriteFileRequest,
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
}
