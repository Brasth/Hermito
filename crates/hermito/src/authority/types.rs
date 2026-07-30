use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use hermito_protocol::request::{CommandSpec, DocumentRevision, EnvironmentEpoch, WorkspaceEpoch};
use portable_pty::PtySize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityKind {
    Local,
    Ssh,
    DevContainer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityTrust {
    InspectOnly,
    ExecutionGranted,
}

#[derive(Clone, Debug)]
pub struct AuthorityRequest<T> {
    pub request_id: Uuid,
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub payload: T,
}

impl<T> AuthorityRequest<T> {
    pub fn new(
        payload: T,
        workspace_epoch: WorkspaceEpoch,
        environment_epoch: EnvironmentEpoch,
        document_revision: Option<DocumentRevision>,
    ) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            workspace_epoch,
            environment_epoch,
            document_revision,
            payload,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AuthorityResult<T> {
    pub request_id: Uuid,
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub payload: T,
}

impl<T> AuthorityRequest<T> {
    pub fn respond<U>(self, payload: U) -> AuthorityResult<U> {
        AuthorityResult {
            request_id: self.request_id,
            workspace_epoch: self.workspace_epoch,
            environment_epoch: self.environment_epoch,
            document_revision: self.document_revision,
            payload,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReadFileRequest {
    pub path: PathBuf,
    pub max_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct WriteFileRequest {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub create: bool,
}

#[derive(Clone, Debug)]
pub struct PtyRequest {
    pub command: CommandSpec,
    pub size: PtySize,
}

#[derive(Clone, Debug)]
pub struct ExecRequest {
    pub command: CommandSpec,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub timeout: Duration,
}

pub fn allowlisted_environment(
    entries: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    entries
        .into_iter()
        .filter(|(key, value)| {
            !key.is_empty()
                && !key.contains('=')
                && !key.as_bytes().contains(&0)
                && !value.as_bytes().contains(&0)
        })
        .collect()
}
