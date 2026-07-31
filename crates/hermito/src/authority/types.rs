use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, time::Duration};

use hermito_protocol::{
    lsp::{AuthorityIdentity, LspContext},
    request::{
        CommandSpec, DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch,
    },
};
use crate::buffer::Buffer;
use super::AuthorityError;
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

/// A transactional WorkspaceEdit payload with no claim that an authority or
/// remote helper owns the host's Buffer revisions.
#[derive(Clone, Debug)]
pub struct LspWorkspaceEditRequest {
    pub context: LspContext,
    pub config_digest: String,
    pub changes: Vec<LspDocumentChange>,
}

#[derive(Clone, Debug)]
pub struct LspDocumentChange {
    pub relative_path: PathBuf,
    pub content: Vec<u8>,
}

/// Host-only, authority-keyed evidence that every WorkspaceEdit target was
/// validated against its owning Buffer ledger. Its constructor and contents
/// remain crate-private so no wire payload can manufacture a host revision precondition.
pub struct LspWorkspaceEditPreconditions<'a> {
    pub(crate) snapshots: Vec<LspDocumentRevisionSnapshot<'a>>,
}
pub(crate) struct LspDocumentRevisionSnapshot<'a> {
    pub(crate) relative_path: PathBuf,
    pub(crate) authority_identity: AuthorityIdentity,
    pub(crate) execution_context: ExecutionContextV1,
    pub(crate) workspace_epoch: crate::document::WorkspaceEpoch,
    pub(crate) environment_epoch: EnvironmentEpoch,
    pub(crate) revision: crate::document::DocumentRevision,
    pub(crate) buffer: &'a Buffer,
}

impl<'a> LspWorkspaceEditPreconditions<'a> {
    pub(crate) fn verify(
        &self,
        request: &LspWorkspaceEditRequest,
        authority_identity: &str,
        workspace_epoch: WorkspaceEpoch,
        environment_epoch: EnvironmentEpoch,
    ) -> Result<(), AuthorityError> {
        let mut snapshots = BTreeMap::new();
        for snapshot in &self.snapshots {
            if snapshots
                .insert(snapshot.relative_path.as_path(), snapshot)
                .is_some()
            {
                return Err(AuthorityError::LspEditConflict(format!(
                    "duplicate host workspace edit precondition: {:?}",
                    snapshot.relative_path
                )));
            }
        }

        let mut targets = BTreeSet::new();
        for change in &request.changes {
            if !targets.insert(change.relative_path.as_path()) {
                return Err(AuthorityError::LspEditConflict(format!(
                    "duplicate workspace edit target: {:?}",
                    change.relative_path
                )));
            }
            let snapshot = snapshots.remove(change.relative_path.as_path()).ok_or_else(|| {
                AuthorityError::MissingLspEditPrecondition {
                    path: change.relative_path.clone(),
                }
            })?;
            if snapshot.authority_identity.0 != authority_identity
                || snapshot.authority_identity != request.context.authority_identity
                || snapshot.execution_context != request.context.execution_context
                || snapshot.workspace_epoch.0 != workspace_epoch.0
                || snapshot.workspace_epoch.0 != request.context.workspace_epoch.0
                || snapshot.environment_epoch != environment_epoch
                || snapshot.environment_epoch != request.context.environment_epoch
            {
                return Err(AuthorityError::LspEditPreconditionContextMismatch {
                    path: change.relative_path.clone(),
                });
            }
            let ledger = snapshot
                .buffer
                .lsp_ledger(&snapshot.authority_identity, &snapshot.execution_context)
                .ok_or_else(|| AuthorityError::MissingLspEditPrecondition {
                    path: change.relative_path.clone(),
                })?;
            let actual = snapshot.buffer.revision();
            if actual != snapshot.revision || ledger.revision != snapshot.revision {
                return Err(AuthorityError::LspEditPreconditionMismatch {
                    path: change.relative_path.clone(),
                    expected: snapshot.revision,
                    actual,
                });
            }
            if ledger.workspace_epoch != snapshot.workspace_epoch
                || ledger.environment_epoch != snapshot.environment_epoch
            {
                return Err(AuthorityError::LspEditPreconditionContextMismatch {
                    path: change.relative_path.clone(),
                });
            }
        }
        if let Some((path, _)) = snapshots.into_iter().next() {
            return Err(AuthorityError::MissingLspEditPrecondition {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }
}

