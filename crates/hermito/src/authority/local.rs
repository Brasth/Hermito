use std::{
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, AtomicU8, Ordering},
};

use hermito_protocol::request::{EnvironmentEpoch, WorkspaceEpoch};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{
    process::{ProcessLimits, ProcessSupervisor},
    pty::{LocalPtySession, PtySession},
};

use super::{
    types::{
        AuthorityKind, AuthorityRequest, AuthorityTrust, ExecRequest, PtyRequest, ReadFileRequest,
        WriteFileRequest,
    },
    Authority, AuthorityError, AuthorityFuture,
};

const LOCAL_FILE_READ_LIMIT: usize = 16 * 1024 * 1024;

pub struct LocalAuthority {
    label: String,
    root: PathBuf,
    trust: AtomicU8,
    workspace_epoch: WorkspaceEpoch,
    environment_epoch: AtomicU64,
    next_session_id: AtomicU64,
}

impl LocalAuthority {
    pub fn new(
        label: impl Into<String>,
        root: PathBuf,
        workspace_epoch: WorkspaceEpoch,
    ) -> Result<Self, AuthorityError> {
        let root = std::fs::canonicalize(root).map_err(AuthorityError::Io)?;
        Ok(Self {
            label: label.into(),
            root,
            trust: AtomicU8::new(AuthorityTrust::InspectOnly as u8),
            workspace_epoch,
            environment_epoch: AtomicU64::new(0),
            next_session_id: AtomicU64::new(1),
        })
    }

    fn resolve_existing(&self, path: &Path) -> Result<PathBuf, AuthorityError> {
        let candidate = self.resolve_relative(path)?;
        let canonical = std::fs::canonicalize(candidate).map_err(AuthorityError::Io)?;
        if !canonical.starts_with(&self.root) {
            return Err(AuthorityError::PathEscapesRoot(path.to_path_buf()));
        }
        Ok(canonical)
    }

    fn resolve_write(&self, path: &Path) -> Result<PathBuf, AuthorityError> {
        let candidate = self.resolve_relative(path)?;
        let parent = candidate
            .parent()
            .ok_or_else(|| AuthorityError::PathEscapesRoot(path.to_path_buf()))?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(AuthorityError::Io)?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(AuthorityError::PathEscapesRoot(path.to_path_buf()));
        }
        let file_name = candidate
            .file_name()
            .ok_or_else(|| AuthorityError::PathEscapesRoot(path.to_path_buf()))?;
        Ok(canonical_parent.join(file_name))
    }

    fn resolve_relative(&self, path: &Path) -> Result<PathBuf, AuthorityError> {
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AuthorityError::PathEscapesRoot(path.to_path_buf()));
        }
        Ok(self.root.join(path))
    }

    fn require_execution(&self) -> Result<(), AuthorityError> {
        if self.trust() == AuthorityTrust::ExecutionGranted {
            Ok(())
        } else {
            Err(AuthorityError::InspectOnly)
        }
    }

    fn validate_epochs<T>(&self, request: &AuthorityRequest<T>) -> Result<(), AuthorityError> {
        let current_environment = self.environment_epoch();
        if request.workspace_epoch != self.workspace_epoch
            || request.environment_epoch != current_environment
        {
            return Err(AuthorityError::StaleEpoch {
                expected_workspace: self.workspace_epoch,
                actual_workspace: request.workspace_epoch,
                expected_environment: current_environment,
                actual_environment: request.environment_epoch,
            });
        }
        Ok(())
    }
}

impl Authority for LocalAuthority {
    fn kind(&self) -> AuthorityKind {
        AuthorityKind::Local
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn root(&self) -> &Path {
        &self.root
    }
    fn workspace_epoch(&self) -> WorkspaceEpoch {
        self.workspace_epoch
    }
    fn environment_epoch(&self) -> EnvironmentEpoch {
        EnvironmentEpoch(self.environment_epoch.load(Ordering::Acquire))
    }
    fn trust(&self) -> AuthorityTrust {
        if self.trust.load(Ordering::Acquire) == AuthorityTrust::ExecutionGranted as u8 {
            AuthorityTrust::ExecutionGranted
        } else {
            AuthorityTrust::InspectOnly
        }
    }
    fn grant_execution(&self) {
        self.trust
            .store(AuthorityTrust::ExecutionGranted as u8, Ordering::Release);
    }
    fn revoke_execution(&self) {
        self.trust
            .store(AuthorityTrust::InspectOnly as u8, Ordering::Release);
    }

    fn read_file<'a>(
        &'a self,
        request: AuthorityRequest<ReadFileRequest>,
    ) -> AuthorityFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.validate_epochs(&request)?;
            if request.document_revision.is_none() {
                return Err(AuthorityError::MissingDocumentRevision);
            }
            let path = self.resolve_existing(&request.payload.path)?;
            let max_bytes = request.payload.max_bytes.min(LOCAL_FILE_READ_LIMIT);
            let read_limit = u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut file = tokio::fs::File::open(path).await?.take(read_limit);
            let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
            file.read_to_end(&mut bytes).await?;
            if bytes.len() > max_bytes {
                return Err(AuthorityError::OutputLimit { limit: max_bytes });
            }
            Ok(request.respond(bytes))
        })
    }

    fn write_file<'a>(
        &'a self,
        request: AuthorityRequest<WriteFileRequest>,
    ) -> AuthorityFuture<'a, usize> {
        Box::pin(async move {
            self.validate_epochs(&request)?;
            if request.document_revision.is_none() {
                return Err(AuthorityError::MissingDocumentRevision);
            }
            let path = self.resolve_write(&request.payload.path)?;
            if !request.payload.create && tokio::fs::metadata(&path).await.is_err() {
                return Err(AuthorityError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "target does not exist",
                )));
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file");
            let temp =
                path.with_file_name(format!(".{file_name}.hermito-{}.tmp", uuid::Uuid::new_v4()));
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .await?;
            file.write_all(&request.payload.bytes).await?;
            file.sync_all().await?;
            drop(file);
            if let Err(error) = tokio::fs::rename(&temp, &path).await {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(AuthorityError::Io(error));
            }
            let written = request.payload.bytes.len();
            Ok(request.respond(written))
        })
    }

    fn spawn_pty<'a>(
        &'a self,
        request: AuthorityRequest<PtyRequest>,
        cancellation: CancellationToken,
    ) -> AuthorityFuture<'a, PtySession> {
        Box::pin(async move {
            self.require_execution()?;
            self.validate_epochs(&request)?;
            let session_id = self.next_session_id.fetch_add(1, Ordering::AcqRel);
            let session = LocalPtySession::spawn(
                session_id,
                &request.payload.command,
                request.payload.size,
                request.workspace_epoch,
                request.environment_epoch,
                cancellation,
            )?;
            Ok(request.respond(PtySession::Local(session)))
        })
    }

    fn exec<'a>(
        &'a self,
        request: AuthorityRequest<ExecRequest>,
        cancellation: CancellationToken,
    ) -> AuthorityFuture<'a, crate::process::ExecResult> {
        Box::pin(async move {
            self.require_execution()?;
            self.validate_epochs(&request)?;
            let limits = ProcessLimits {
                stdout_bytes: request.payload.stdout_limit,
                stderr_bytes: request.payload.stderr_limit,
                wall_time: request.payload.timeout,
                ..ProcessLimits::default()
            };
            let result =
                ProcessSupervisor::exec(&request.payload.command, cancellation, limits).await?;
            Ok(request.respond(result))
        })
    }
}
