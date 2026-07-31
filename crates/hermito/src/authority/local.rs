use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        RwLock,
    },
    time::Duration,
};
use hermito_protocol::{
    lsp::{LspContext, LspV1},
    request::{CommandSpec, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;
use sha2::{Digest, Sha256};

use crate::{
    config::EffectiveLspConfig,
    lsp::LspTransport,
};


use crate::{
    process::{ProcessLimits, ProcessSupervisor},
    pty::{LocalPtySession, PtySession},
};

use super::{
    types::{
        AuthorityKind, AuthorityRequest, AuthorityTrust, ExecRequest, LspWorkspaceEditPreconditions,
        LspWorkspaceEditRequest, PtyRequest, ReadFileRequest, WriteFileRequest,
    },
    Authority, AuthorityError, AuthorityFuture,
};

const LOCAL_FILE_READ_LIMIT: usize = 16 * 1024 * 1024;

const LSP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const LSP_PROBE_OUTPUT_LIMIT: usize = 16 * 1024;
const LSP_VERSION_MISMATCH_MARKER: &str = "LSP_VERSION_MISMATCH";

struct StagedLspReplacement {
    temp: PathBuf,
    target: PathBuf,
    backup: Option<PathBuf>,
}
pub struct LocalAuthority {
    label: String,
    root: PathBuf,
    trust: AtomicU8,
    workspace_epoch: WorkspaceEpoch,
    environment_epoch: AtomicU64,
    next_session_id: AtomicU64,
    /// Digests for which LSP execution has been explicitly granted on this host authority.
    /// Only exact match authorizes; terminal trust is orthogonal.
    lsp_grants: RwLock<BTreeMap<String, bool>>,
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
            lsp_grants: RwLock::new(BTreeMap::new()),
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

    fn validate_lsp_context(&self, context: &LspContext) -> Result<(), AuthorityError> {
        if context.execution_context != ExecutionContextV1::AuthorityRoot
            || context.authority_identity.0 != self.host_authority_id()
        {
            return Err(AuthorityError::UnsupportedExecutionContext);
        }
        Ok(())
    }

    fn lsp_version_mismatch(expected: impl AsRef<str>, actual: impl AsRef<str>) -> AuthorityError {
        AuthorityError::Protocol(format!(
            "{LSP_VERSION_MISMATCH_MARKER}: expected {:?}, actual {:?}",
            expected.as_ref(),
            actual.as_ref()
        ))
    }

    fn resolve_lsp_executable(&self, executable: &Path) -> Result<PathBuf, AuthorityError> {
        let candidate = if executable.is_absolute() {
            executable.to_path_buf()
        } else {
            self.root.join(executable)
        };
        std::fs::canonicalize(candidate).map_err(AuthorityError::Io)
    }

    async fn validate_lsp_launch(
        &self,
        effective_config: &EffectiveLspConfig,
        config_digest: &str,
        cancellation: CancellationToken,
    ) -> Result<PathBuf, AuthorityError> {
        let executable = self.resolve_lsp_executable(&effective_config.executable)?;
        if let Some(expected_digest) = &effective_config.expected_digest {
            let digest_path = executable.clone();
            let actual_digest = tokio::task::spawn_blocking(move || {
                let mut file = std::fs::File::open(digest_path)?;
                let mut hasher = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                Ok::<_, std::io::Error>(hex::encode(hasher.finalize()))
            })
            .await
            .map_err(|error| AuthorityError::Protocol(error.to_string()))??;
            if !actual_digest.eq_ignore_ascii_case(expected_digest) {
                tracing::debug!(
                    authority_identity = "local",
                    config_digest = %config_digest,
                    reason = "binary_digest_mismatch",
                    "rejected local LSP probe"
                );
                return Err(Self::lsp_version_mismatch(expected_digest, actual_digest));
            }
            tracing::debug!(
                authority_identity = "local",
                config_digest = %config_digest,
                state = "accepted",
                "accepted local LSP digest probe"
            );
        }
        if let Some(expected_version) = &effective_config.expected_version {
            let command = CommandSpec {
                program: executable.to_string_lossy().into_owned(),
                args: effective_config
                    .version_probe_args
                    .clone()
                    .unwrap_or_else(|| vec!["--version".to_owned()]),
                cwd: self.root.to_string_lossy().into_owned(),
                env: crate::authority::types::allowlisted_environment([(
                    "LANG".to_owned(),
                    std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".to_owned()),
                )]),
            };
            let probe = ProcessSupervisor::exec(
                &command,
                cancellation,
                ProcessLimits {
                    stdout_bytes: LSP_PROBE_OUTPUT_LIMIT,
                    stderr_bytes: LSP_PROBE_OUTPUT_LIMIT,
                    wall_time: LSP_PROBE_TIMEOUT,
                    ..ProcessLimits::default()
                },
            )
            .await?;
            let actual_version = if probe.exit_code == Some(0)
                && !probe.stdout_truncated
                && !probe.stderr_truncated
            {
                let stdout = String::from_utf8_lossy(&probe.stdout);
                let stderr = String::from_utf8_lossy(&probe.stderr);
                if stdout.trim().is_empty() {
                    stderr.trim().to_owned()
                } else {
                    stdout.trim().to_owned()
                }
            } else {
                format!(
                    "probe failed (exit {:?}, stdout truncated {}, stderr truncated {})",
                    probe.exit_code, probe.stdout_truncated, probe.stderr_truncated
                )
            };
            if actual_version != *expected_version {
                tracing::debug!(
                    authority_identity = "local",
                    config_digest = %config_digest,
                    reason = "version_mismatch",
                    "rejected local LSP probe"
                );
                return Err(Self::lsp_version_mismatch(expected_version, actual_version));
            }
            tracing::debug!(
                authority_identity = "local",
                config_digest = %config_digest,
                state = "accepted",
                "accepted local LSP version probe"
            );
        }
        Ok(executable)
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
    fn host_authority_id(&self) -> String {
        // Stable "local" identity for grants; label may be "host" for UI but grants use canonical host id.
        "local".to_string()
    }

    fn grant_lsp_execution(&self, config_digest: &str) {
        if let Ok(mut map) = self.lsp_grants.write() {
            map.insert(config_digest.to_string(), true);
        }
    }

    fn revoke_lsp_execution(&self, config_digest: &str) {
        if let Ok(mut map) = self.lsp_grants.write() {
            map.remove(config_digest);
        }
    }

    fn is_lsp_execution_granted(&self, config_digest: &str) -> bool {
        self.lsp_grants
            .read()
            .map(|m| m.get(config_digest).copied().unwrap_or(false))
            .unwrap_or(false)
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

    fn start_lsp<'a>(
        &'a self,
        context: LspContext,
        effective_config: EffectiveLspConfig,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn LspTransport>, AuthorityError>> + Send + 'a>> {
        Box::pin(async move {
            self.validate_lsp_context(&context)?;
            let digest = crate::config::lsp_config_digest(&effective_config);
            if !self.is_lsp_execution_granted(&digest) {
                tracing::debug!(
                    authority_identity = %context.authority_identity.0,
                    execution_context = ?context.execution_context,
                    config_digest = %digest,
                    session_generation = context.session_generation.0,
                    sent_version = context.sent_version.0,
                    revision = ?context.document_revision,
                    reason = "lsp_execution_trust_not_granted",
                    "blocked local LSP start"
                );
                return Err(AuthorityError::InspectOnly);
            }
            if context.workspace_epoch != self.workspace_epoch
                || context.environment_epoch != self.environment_epoch()
            {
                tracing::debug!(
                    authority_identity = %context.authority_identity.0,
                    execution_context = ?context.execution_context,
                    config_digest = %digest,
                    session_generation = context.session_generation.0,
                    reason = "stale_epoch",
                    "blocked local LSP start"
                );
                return Err(AuthorityError::StaleEpoch {
                    expected_workspace: self.workspace_epoch,
                    actual_workspace: context.workspace_epoch,
                    expected_environment: self.environment_epoch(),
                    actual_environment: context.environment_epoch,
                });
            }
            let executable = self
                .validate_lsp_launch(&effective_config, &digest, cancellation.clone())
                .await?;
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %digest,
                session_generation = context.session_generation.0,
                sent_version = context.sent_version.0,
                revision = ?context.document_revision,
                state = "starting",
                "accepted local LSP start"
            );
            let mut cmd = tokio::process::Command::new(executable);
            cmd.args(&effective_config.args);
            cmd.current_dir(&self.root);
            cmd.env_clear();
            let base_env = vec![
                ("LANG".to_string(), std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".to_string())),
                ("TERM".to_string(), "dumb".to_string()),
            ];
            let env = crate::authority::types::allowlisted_environment(base_env);
            cmd.envs(env);
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.kill_on_drop(true);
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(AuthorityError::Io)?;
            let stdin = child.stdin.take().ok_or_else(|| {
                AuthorityError::Io(std::io::Error::new(std::io::ErrorKind::Other, "failed to take stdin for lsp"))
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                AuthorityError::Io(std::io::Error::new(std::io::ErrorKind::Other, "failed to take stdout for lsp"))
            })?;
            let child = std::sync::Arc::new(Mutex::new(child));
            let child_watch = std::sync::Arc::clone(&child);
            let cancel_watch = cancellation.clone();
            tokio::spawn(async move {
                cancel_watch.cancelled().await;
                let mut c = child_watch.lock().await;
                let _ = c.start_kill();
            });
            let (exit_tx, exit_rx) = watch::channel(None);
            let child_exit = std::sync::Arc::clone(&child);
            tokio::spawn(async move {
                let exit_code = child_exit
                    .lock()
                    .await
                    .wait()
                    .await
                    .ok()
                    .and_then(|status| status.code());
                let _ = exit_tx.send(Some(exit_code));
            });
            let direct = crate::lsp::DirectTransport::new(stdout, stdin, context.clone());
            let transport: Box<dyn LspTransport> = Box::new(LocalLspTransport {
                direct,
                context: context.clone(),
                exit: std::sync::Arc::new(Mutex::new(LocalLspExit {
                    receiver: exit_rx,
                    emitted: false,
                })),
                _child: child,
                _cancel: cancellation,
            });
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %digest,
                session_generation = context.session_generation.0,
                state = "started",
                "started local LSP transport"
            );
            Ok(transport)
        })
    }

    fn apply_lsp_workspace_edit<'a>(
        &'a self,
        request: AuthorityRequest<LspWorkspaceEditRequest>,
        preconditions: LspWorkspaceEditPreconditions<'a>,
    ) -> AuthorityFuture<'a, bool> {
        Box::pin(async move {
            self.validate_epochs(&request)?;
            self.validate_lsp_context(&request.payload.context)?;
            if !self.is_lsp_execution_granted(&request.payload.config_digest) {
                tracing::debug!(
                    authority_identity = %request.payload.context.authority_identity.0,
                    execution_context = ?request.payload.context.execution_context,
                    config_digest = %request.payload.config_digest,
                    session_generation = request.payload.context.session_generation.0,
                    reason = "lsp_execution_trust_not_granted",
                    "rejected local LSP workspace edit"
                );
                return Err(AuthorityError::InspectOnly);
            }
            if request.payload.context.workspace_epoch != self.workspace_epoch
                || request.payload.context.environment_epoch != self.environment_epoch()
            {
                return Err(AuthorityError::StaleEpoch {
                    expected_workspace: self.workspace_epoch,
                    actual_workspace: request.payload.context.workspace_epoch,
                    expected_environment: self.environment_epoch(),
                    actual_environment: request.payload.context.environment_epoch,
                });
            }

            // Last host-ledger check: no temporary file may exist unless every
            // document still matches the authority-keyed snapshot.
            preconditions.verify(
                &request.payload,
                &self.host_authority_id(),
                self.workspace_epoch,
                self.environment_epoch(),
            )?;

            let mut targets = BTreeSet::new();
            let mut staged = Vec::with_capacity(request.payload.changes.len());
            let stage_result: Result<(), AuthorityError> = async {
                for change in &request.payload.changes {
                    let target = self.resolve_write(&change.relative_path)?;
                    if !targets.insert(target.clone()) {
                        return Err(AuthorityError::LspEditConflict(format!(
                            "duplicate workspace edit target: {:?}",
                            change.relative_path
                        )));
                    }
                    let file_name = target.file_name().and_then(|name| name.to_str()).unwrap_or("file");
                    let temp = target.with_file_name(format!(
                        ".{file_name}.hermito-lsp-edit-{}.tmp",
                        uuid::Uuid::new_v4()
                    ));
                    let mut file = tokio::fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&temp)
                        .await?;
                    file.write_all(&change.content).await?;
                    file.sync_all().await?;
                    drop(file);
                    staged.push(StagedLspReplacement {
                        temp,
                        target,
                        backup: None,
                    });
                }
                Ok(())
            }
            .await;
            if let Err(error) = stage_result {
                tracing::debug!(
                    authority_identity = %request.payload.context.authority_identity.0,
                    execution_context = ?request.payload.context.execution_context,
                    config_digest = %request.payload.config_digest,
                    session_generation = request.payload.context.session_generation.0,
                    reason = "staging_failed",
                    "rejected local LSP workspace edit"
                );
                for replacement in &staged {
                    let _ = tokio::fs::remove_file(&replacement.temp).await;
                }
                return Err(error);
            }

            let backup_result: Result<(), std::io::Error> = async {
                for replacement in &mut staged {
                    match tokio::fs::metadata(&replacement.target).await {
                        Ok(_) => {
                            let file_name = replacement
                                .target
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("file");
                            let backup = replacement.target.with_file_name(format!(
                                ".{file_name}.hermito-lsp-backup-{}.tmp",
                                uuid::Uuid::new_v4()
                            ));
                            tokio::fs::rename(&replacement.target, &backup).await?;
                            replacement.backup = Some(backup);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = backup_result {
                tracing::debug!(
                    authority_identity = %request.payload.context.authority_identity.0,
                    execution_context = ?request.payload.context.execution_context,
                    config_digest = %request.payload.config_digest,
                    session_generation = request.payload.context.session_generation.0,
                    reason = "backup_failed",
                    "rejected local LSP workspace edit"
                );
                for replacement in staged.iter().rev() {
                    if let Some(backup) = &replacement.backup {
                        let _ = tokio::fs::rename(backup, &replacement.target).await;
                    }
                    let _ = tokio::fs::remove_file(&replacement.temp).await;
                }
                return Err(AuthorityError::LspEditConflict(format!(
                    "failed to preserve originals before workspace edit: {error}"
                )));
            }

            let mut completed = 0;
            for replacement in &staged {
                if let Err(error) = tokio::fs::rename(&replacement.temp, &replacement.target).await {
                    tracing::debug!(
                        authority_identity = %request.payload.context.authority_identity.0,
                        execution_context = ?request.payload.context.execution_context,
                        config_digest = %request.payload.config_digest,
                        session_generation = request.payload.context.session_generation.0,
                        reason = "commit_failed",
                        "rejected local LSP workspace edit"
                    );
                    for committed in staged.iter().take(completed).rev() {
                        let _ = tokio::fs::remove_file(&committed.target).await;
                    }
                    for original in staged.iter().rev() {
                        if let Some(backup) = &original.backup {
                            let _ = tokio::fs::rename(backup, &original.target).await;
                        }
                        let _ = tokio::fs::remove_file(&original.temp).await;
                    }
                    return Err(AuthorityError::LspEditConflict(format!(
                        "failed to commit workspace edit for {:?}: {error}",
                        replacement.target
                    )));
                }
                completed += 1;
            }

            for replacement in &staged {
                if let Some(backup) = &replacement.backup {
                    let _ = tokio::fs::remove_file(backup).await;
                }
                let _ = tokio::fs::remove_file(&replacement.temp).await;
            }
            tracing::debug!(
                authority_identity = %request.payload.context.authority_identity.0,
                execution_context = ?request.payload.context.execution_context,
                config_digest = %request.payload.config_digest,
                session_generation = request.payload.context.session_generation.0,
                state = "applied",
                "accepted local LSP workspace edit"
            );
            Ok(request.respond(true))
        })
    }
}

struct LocalLspExit {
    receiver: watch::Receiver<Option<Option<i32>>>,
    emitted: bool,
}

struct LocalLspTransport {
    direct: crate::lsp::DirectTransport<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    context: LspContext,
    exit: std::sync::Arc<Mutex<LocalLspExit>>,
    _child: std::sync::Arc<Mutex<tokio::process::Child>>,
    _cancel: CancellationToken,
}

impl LocalLspTransport {
    fn exited(&self, exit_code: Option<i32>) -> LspV1 {
        LspV1::Exited {
            context: self.context.clone(),
            exit_code,
        }
    }
}

impl LspTransport for LocalLspTransport {
    fn send(&self, message: LspV1) -> Pin<Box<dyn Future<Output = Result<(), crate::lsp::LspClientError>> + Send + '_>> {
        self.direct.send(message)
    }
    fn recv(&self) -> Pin<Box<dyn Future<Output = Result<LspV1, crate::lsp::LspClientError>> + Send + '_>> {
        Box::pin(async move {
            let mut exit = self.exit.lock().await;
            if !exit.emitted {
                let exit_code = *exit.receiver.borrow_and_update();
                if let Some(exit_code) = exit_code {
                    exit.emitted = true;
                    return Ok(self.exited(exit_code));
                }
            }
            if exit.emitted {
                return Err(crate::lsp::LspClientError::Transport(
                    "local lsp stream closed".into(),
                ));
            }
            tokio::select! {
                message = self.direct.recv() => message,
                changed = exit.receiver.changed() => {
                    changed.map_err(|_| crate::lsp::LspClientError::Transport(
                        "local lsp exit watcher closed".into(),
                    ))?;
                    let exit_code = *exit.receiver.borrow_and_update();
                    if let Some(exit_code) = exit_code {
                        exit.emitted = true;
                        Ok(self.exited(exit_code))
                    } else {
                        Err(crate::lsp::LspClientError::Transport(
                            "local lsp exit watcher closed".into(),
                        ))
                    }
                }
            }
        })
    }
}

