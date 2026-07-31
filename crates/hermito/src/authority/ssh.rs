use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc as StdArc;

use hermito_protocol::frame::ReceivedMessage;
use hermito_protocol::lsp::{LspContext, LspV1};
use tokio::sync::{mpsc, watch};
use crate::{
    config::EffectiveLspConfig,
    lsp::{LspClientError, LspTransport},
};



use hermito_protocol::{
    fs::{FsMessage, ReadFile, WriteFile},
    process::ProcessMessage,
    pty::{PtySize as WirePtySize, PtySpawn},
    request::{CommandSpec, EnvironmentEpoch, ExecutionContextV1, RequestEnvelope, WorkspaceEpoch},
    response::ResponseEnvelope,
    Message,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::{
    process::ExecResult,
    pty::{PtySession, RemotePtySession},
    remote::{
        helper_installer::HelperInstaller, helper_launcher::HelperLauncher,
        multiplexer::Multiplexer, ssh_bootstrap::SshBootstrap,
    },
};

use super::{
    tuf_verifier::TufVerifier,
    types::{
        AuthorityKind, AuthorityRequest, AuthorityTrust, ExecRequest, LspWorkspaceEditPreconditions,
        LspWorkspaceEditRequest, PtyRequest, ReadFileRequest, WriteFileRequest,
    },
    Authority, AuthorityError, AuthorityFuture,
};

const LSP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const LSP_PROBE_OUTPUT_LIMIT: usize = 16 * 1024;
const LSP_VERSION_MISMATCH_MARKER: &str = "LSP_VERSION_MISMATCH";
pub struct SshAuthority {
    label: String,
    root: PathBuf,
    bootstrap: SshBootstrap,
    workspace_epoch: WorkspaceEpoch,
    environment_epoch: AtomicU64,
    trust: AtomicU8,
    next_stream_id: AtomicU64,
    multiplexer: RwLock<Option<Multiplexer>>,
    lifecycle: tokio::sync::Mutex<()>,
    /// Per-digest LSP execution grants. Exact digest match required; terminal trust does not confer LSP rights.
    lsp_grants: RwLock<BTreeMap<String, bool>>,
}

impl SshAuthority {
    pub fn new(
        label: impl Into<String>,
        root: PathBuf,
        bootstrap: SshBootstrap,
        workspace_epoch: WorkspaceEpoch,
    ) -> Result<Arc<Self>, AuthorityError> {
        if !root.to_string_lossy().starts_with('/') {
            return Err(AuthorityError::Ssh(
                "remote Linux root must be absolute".into(),
            ));
        }
        Ok(Arc::new(Self {
            label: label.into(),
            root,
            bootstrap,
            workspace_epoch,
            environment_epoch: AtomicU64::new(0),
            trust: AtomicU8::new(AuthorityTrust::InspectOnly as u8),
            next_stream_id: AtomicU64::new(1),
            multiplexer: RwLock::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
            lsp_grants: RwLock::new(BTreeMap::new()),
        }))
    }
    pub async fn activate(
        self: &Arc<Self>,
        verifier: &TufVerifier,
        target_name: &str,
        remote_directory: PathBuf,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<(), AuthorityError> {
        let _lifecycle = self.lifecycle.lock().await;
        {
            let mut slot = self
                .multiplexer
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.as_ref().is_some_and(Multiplexer::is_alive) {
                return Err(AuthorityError::Ssh(
                    "remote helper is already connected".into(),
                ));
            }
            if let Some(stale) = slot.take() {
                self.environment_epoch
                    .fetch_max(stale.generation(), Ordering::AcqRel);
            }
        }
        self.require_execution()?;
        let target = verifier
            .verify_target(target_name)
            .await
            .map_err(|error| AuthorityError::Verification(error.to_string()))?;
        let installer = HelperInstaller::new(&self.bootstrap, remote_directory)
            .map_err(|error| AuthorityError::Verification(error.to_string()))?;
        let installed = installer
            .install(&target, passphrase)
            .await
            .map_err(|error| AuthorityError::Verification(error.to_string()))?;
        let launcher = HelperLauncher::new(&self.bootstrap, &installer);
        let child = launcher
            .launch(&installed, true, passphrase)
            .await
            .map_err(|error| AuthorityError::Ssh(error.to_string()))?;
        let generation = self.environment_epoch.load(Ordering::Acquire);
        let multiplexer = Multiplexer::start(child, generation)
            .await
            .map_err(|error| AuthorityError::Protocol(error.to_string()))?;
        let mut loss = multiplexer.subscribe_loss();
        if let Some(event) = loss.borrow().clone() {
            self.environment_epoch
                .fetch_max(event.generation, Ordering::AcqRel);
            return Err(AuthorityError::Protocol(event.reason));
        }
        *self
            .multiplexer
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(multiplexer.clone());
        let authority = Arc::clone(self);
        tokio::spawn(async move {
            let watched_transport = multiplexer.clone();
            loop {
                if let Some(event) = loss.borrow().clone() {
                    authority
                        .environment_epoch
                        .fetch_max(event.generation, Ordering::AcqRel);
                    let mut slot = authority
                        .multiplexer
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if slot
                        .as_ref()
                        .is_some_and(|current| current.is_same_transport(&watched_transport))
                    {
                        slot.take();
                    }
                    break;
                }
                if loss.changed().await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    pub async fn disconnect(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let multiplexer = self
            .multiplexer
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(multiplexer) = multiplexer {
            multiplexer.shutdown().await;
            self.environment_epoch
                .fetch_max(multiplexer.generation(), Ordering::AcqRel);
        }
    }
    pub fn is_connected(&self) -> bool {
        self.multiplexer
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(Multiplexer::is_alive)
    }

    fn current_transport(&self) -> Result<Multiplexer, AuthorityError> {
        self.multiplexer
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .filter(Multiplexer::is_alive)
            .ok_or_else(|| AuthorityError::Ssh("remote helper is not connected".into()))
    }

    fn lsp_version_mismatch(expected: impl AsRef<str>, actual: impl AsRef<str>) -> AuthorityError {
        AuthorityError::Protocol(format!(
            "{LSP_VERSION_MISMATCH_MARKER}: expected {:?}, actual {:?}",
            expected.as_ref(),
            actual.as_ref()
        ))
    }

    fn resolve_lsp_executable(&self, executable: &Path) -> PathBuf {
        if executable.is_absolute() {
            executable.to_path_buf()
        } else {
            self.root.join(executable)
        }
    }

    async fn execute_lsp_probe(
        &self,
        command: CommandSpec,
        context: &LspContext,
        cancellation: CancellationToken,
    ) -> Result<ExecResult, AuthorityError> {
        let request = AuthorityRequest::new(
            ExecRequest {
                command,
                stdout_limit: LSP_PROBE_OUTPUT_LIMIT,
                stderr_limit: LSP_PROBE_OUTPUT_LIMIT,
                timeout: LSP_PROBE_TIMEOUT,
            },
            context.workspace_epoch,
            context.environment_epoch,
            context.document_revision,
        );
        Ok(Authority::exec(self, request, cancellation).await?.payload)
    }

    async fn validate_lsp_launch(
        &self,
        effective_config: &EffectiveLspConfig,
        context: &LspContext,
        config_digest: &str,
        cancellation: CancellationToken,
    ) -> Result<PathBuf, AuthorityError> {
        let executable = self.resolve_lsp_executable(&effective_config.executable);
        let cwd = self.root.to_string_lossy().into_owned();
        if let Some(expected_digest) = &effective_config.expected_digest {
            let digest_probe = self
                .execute_lsp_probe(
                    CommandSpec {
                        program: "sha256sum".to_owned(),
                        args: vec!["--".to_owned(), executable.to_string_lossy().into_owned()],
                        cwd: cwd.clone(),
                        env: crate::authority::types::allowlisted_environment([(
                            "LANG".to_owned(),
                            "C".to_owned(),
                        )]),
                    },
                    context,
                    cancellation.clone(),
                )
                .await?;
            let actual_digest = if digest_probe.exit_code == Some(0)
                && !digest_probe.stdout_truncated
                && !digest_probe.stderr_truncated
            {
                String::from_utf8_lossy(&digest_probe.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                String::new()
            };
            if actual_digest.is_empty() || !actual_digest.eq_ignore_ascii_case(expected_digest) {
                tracing::debug!(
                    authority_identity = %context.authority_identity.0,
                    execution_context = ?context.execution_context,
                    config_digest = %config_digest,
                    session_generation = context.session_generation.0,
                    reason = "binary_digest_mismatch",
                    "rejected SSH LSP probe"
                );
                return Err(Self::lsp_version_mismatch(expected_digest, actual_digest));
            }
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %config_digest,
                session_generation = context.session_generation.0,
                state = "accepted",
                "accepted SSH LSP digest probe"
            );
        }
        if let Some(expected_version) = &effective_config.expected_version {
            let version_probe = self
                .execute_lsp_probe(
                    CommandSpec {
                        program: executable.to_string_lossy().into_owned(),
                        args: effective_config
                            .version_probe_args
                            .clone()
                            .unwrap_or_else(|| vec!["--version".to_owned()]),
                        cwd,
                        env: crate::authority::types::allowlisted_environment([(
                            "LANG".to_owned(),
                            "C.UTF-8".to_owned(),
                        )]),
                    },
                    context,
                    cancellation,
                )
                .await?;
            let actual_version = if version_probe.exit_code == Some(0)
                && !version_probe.stdout_truncated
                && !version_probe.stderr_truncated
            {
                let stdout = String::from_utf8_lossy(&version_probe.stdout);
                let stderr = String::from_utf8_lossy(&version_probe.stderr);
                if stdout.trim().is_empty() {
                    stderr.trim().to_owned()
                } else {
                    stdout.trim().to_owned()
                }
            } else {
                format!(
                    "probe failed (exit {:?}, stdout truncated {}, stderr truncated {})",
                    version_probe.exit_code,
                    version_probe.stdout_truncated,
                    version_probe.stderr_truncated
                )
            };
            if actual_version != *expected_version {
                tracing::debug!(
                    authority_identity = %context.authority_identity.0,
                    execution_context = ?context.execution_context,
                    config_digest = %config_digest,
                    session_generation = context.session_generation.0,
                    reason = "version_mismatch",
                    "rejected SSH LSP probe"
                );
                return Err(Self::lsp_version_mismatch(expected_version, actual_version));
            }
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %config_digest,
                session_generation = context.session_generation.0,
                state = "accepted",
                "accepted SSH LSP version probe"
            );
        }
        Ok(executable)
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

    fn resolve_remote(&self, path: &Path) -> Result<String, AuthorityError> {
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(AuthorityError::PathEscapesRoot(path.to_path_buf()));
        }
        let resolved = self.root.join(path).to_string_lossy().into_owned();
        if resolved.as_bytes().contains(&0) {
            return Err(AuthorityError::PathEscapesRoot(path.to_path_buf()));
        }
        Ok(resolved)
    }

    fn validate_response<A, T>(
        &self,
        request: &AuthorityRequest<A>,
        response: &ResponseEnvelope<T>,
    ) -> Result<(), AuthorityError> {
        if response.request_id != request.request_id
            || response.document_revision != request.document_revision
            || response.execution_context != ExecutionContextV1::AuthorityRoot
        {
            return Err(AuthorityError::Protocol(
                "remote response identity does not match its request".into(),
            ));
        }
        if response.workspace_epoch != request.workspace_epoch
            || response.environment_epoch != request.environment_epoch
        {
            return Err(AuthorityError::StaleEpoch {
                expected_workspace: request.workspace_epoch,
                actual_workspace: response.workspace_epoch,
                expected_environment: request.environment_epoch,
                actual_environment: response.environment_epoch,
            });
        }
        self.validate_epochs(request)
    }

    fn envelope<A, T>(&self, request: &AuthorityRequest<A>, payload: T) -> RequestEnvelope<T> {
        RequestEnvelope {
            request_id: request.request_id,
            workspace_epoch: request.workspace_epoch,
            environment_epoch: request.environment_epoch,
            document_revision: request.document_revision,
            execution_context: ExecutionContextV1::AuthorityRoot,
            payload,
        }
    }

    fn validate_lsp_context(&self, context: &LspContext) -> Result<(), AuthorityError> {
        if context.execution_context != ExecutionContextV1::AuthorityRoot
            || context.authority_identity.0 != self.host_authority_id()
        {
            return Err(AuthorityError::UnsupportedExecutionContext);
        }
        Ok(())
    }

    /// Read a transaction original without projecting host Buffer revisions
    /// onto the remote helper protocol.
    async fn read_prevalidated_workspace_file(
        &self,
        transaction: &AuthorityRequest<LspWorkspaceEditRequest>,
        path: &Path,
    ) -> Result<Vec<u8>, AuthorityError> {
        let request = AuthorityRequest::new(
            ReadFileRequest {
                path: path.to_path_buf(),
                max_bytes: hermito_protocol::fs::MAX_WIRE_FILE_BYTES as usize,
            },
            transaction.workspace_epoch,
            transaction.environment_epoch,
            None,
        );
        self.validate_epochs(&request)?;
        let transport = self.current_transport()?;
        let payload = ReadFile {
            path: self.resolve_remote(&request.payload.path)?,
            max_bytes: hermito_protocol::fs::MAX_WIRE_FILE_BYTES,
        };
        let response = transport
            .request(
                request.request_id,
                Message::Fs(FsMessage::Read(self.envelope(&request, payload))),
                Duration::from_secs(30),
                CancellationToken::new(),
            )
            .await
            .map_err(|error| AuthorityError::Protocol(error.to_string()))?;
        match response {
            Message::Fs(FsMessage::ReadResult(response)) => {
                self.validate_response(&request, &response)?;
                let payload = response.payload.map_err(|error| {
                    AuthorityError::Ssh(format!("{:?}: {}", error.code, error.message))
                })?;
                if payload.bytes.len() as u64 > hermito_protocol::fs::MAX_WIRE_FILE_BYTES {
                    return Err(AuthorityError::OutputLimit {
                        limit: hermito_protocol::fs::MAX_WIRE_FILE_BYTES as usize,
                    });
                }
                Ok(payload.bytes)
            }
            _ => Err(AuthorityError::Protocol(
                "unexpected prevalidated workspace file-read response".into(),
            )),
        }
    }

    /// Write a transaction member after the host has already validated all
    /// Buffer preconditions. Remote mutation receives no host revision.
    async fn write_prevalidated_workspace_file(
        &self,
        transaction: &AuthorityRequest<LspWorkspaceEditRequest>,
        path: &Path,
        bytes: Vec<u8>,
    ) -> Result<(), AuthorityError> {
        if bytes.len() as u64 > hermito_protocol::fs::MAX_WIRE_FILE_BYTES {
            return Err(AuthorityError::InputLimit {
                limit: hermito_protocol::fs::MAX_WIRE_FILE_BYTES as usize,
            });
        }
        let request = AuthorityRequest::new(
            WriteFileRequest {
                path: path.to_path_buf(),
                bytes,
                create: true,
            },
            transaction.workspace_epoch,
            transaction.environment_epoch,
            None,
        );
        self.validate_epochs(&request)?;
        let transport = self.current_transport()?;
        let payload = WriteFile {
            path: self.resolve_remote(&request.payload.path)?,
            bytes: request.payload.bytes.clone(),
            create: request.payload.create,
        };
        let response = transport
            .request(
                request.request_id,
                Message::Fs(FsMessage::Write(self.envelope(&request, payload))),
                Duration::from_secs(30),
                CancellationToken::new(),
            )
            .await
            .map_err(|error| AuthorityError::Protocol(error.to_string()))?;
        match response {
            Message::Fs(FsMessage::WriteResult(response)) => {
                self.validate_response(&request, &response)?;
                response.payload.map_err(|error| {
                    AuthorityError::Ssh(format!("{:?}: {}", error.code, error.message))
                })?;
                Ok(())
            }
            _ => Err(AuthorityError::Protocol(
                "unexpected prevalidated workspace file-write response".into(),
            )),
        }
    }

    async fn apply_remote_workspace_edit_transaction(
        &self,
        request: &AuthorityRequest<LspWorkspaceEditRequest>,
    ) -> Result<(), AuthorityError> {
        let mut originals = Vec::with_capacity(request.payload.changes.len());
        let mut targets = BTreeSet::new();
        for change in &request.payload.changes {
            let target = self.resolve_remote(&change.relative_path)?;
            if !targets.insert(target) {
                return Err(AuthorityError::LspEditConflict(format!(
                    "duplicate remote workspace edit target: {:?}",
                    change.relative_path
                )));
            }
            let original = self
                .read_prevalidated_workspace_file(request, &change.relative_path)
                .await
                .map_err(|error| {
                    AuthorityError::LspEditConflict(format!(
                        "failed to stage remote workspace edit for {:?}: {error}",
                        change.relative_path
                    ))
                })?;
            originals.push((change, original));
        }

        let mut completed = 0;
        for change in &request.payload.changes {
            let write = self
                .write_prevalidated_workspace_file(
                    request,
                    &change.relative_path,
                    change.content.clone(),
                )
                .await;
            if let Err(error) = write {
                let mut rollback_failures = Vec::new();
                for (original_change, original_content) in originals.iter().take(completed).rev() {
                    if let Err(rollback_error) = self
                        .write_prevalidated_workspace_file(
                            request,
                            &original_change.relative_path,
                            original_content.clone(),
                        )
                        .await
                    {
                        rollback_failures.push(format!(
                            "{:?}: {rollback_error}",
                            original_change.relative_path
                        ));
                    }
                }
                let rollback_detail = if rollback_failures.is_empty() {
                    String::new()
                } else {
                    format!("; rollback failures: {}", rollback_failures.join(", "))
                };
                return Err(AuthorityError::LspEditConflict(format!(
                    "remote workspace edit failed for {:?}: {error}{rollback_detail}",
                    change.relative_path
                )));
            }
            completed += 1;
        }
        Ok(())
    }
}

impl Authority for SshAuthority {
    fn kind(&self) -> AuthorityKind {
        AuthorityKind::Ssh
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
        // Use the label as stable host id for ssh authorities; callers use this for digest keyed grants.
        self.label.clone()
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
            let transport = self.current_transport()?;
            let max_bytes =
                (request.payload.max_bytes as u64).min(hermito_protocol::fs::MAX_WIRE_FILE_BYTES);
            let payload = ReadFile {
                path: self.resolve_remote(&request.payload.path)?,
                max_bytes,
            };
            let message = Message::Fs(FsMessage::Read(self.envelope(&request, payload)));
            let response = transport
                .request(
                    request.request_id,
                    message,
                    Duration::from_secs(30),
                    CancellationToken::new(),
                )
                .await
                .map_err(|error| AuthorityError::Protocol(error.to_string()))?;
            match response {
                Message::Fs(FsMessage::ReadResult(response)) => {
                    self.validate_response(&request, &response)?;
                    let payload = response.payload.map_err(|error| {
                        AuthorityError::Ssh(format!("{:?}: {}", error.code, error.message))
                    })?;
                    if payload.bytes.len() as u64 > max_bytes {
                        return Err(AuthorityError::OutputLimit {
                            limit: max_bytes as usize,
                        });
                    }
                    Ok(request.respond(payload.bytes))
                }
                _ => Err(AuthorityError::Protocol(
                    "unexpected file-read response".into(),
                )),
            }
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
            if request.payload.bytes.len() as u64 > hermito_protocol::fs::MAX_WIRE_FILE_BYTES {
                return Err(AuthorityError::InputLimit {
                    limit: hermito_protocol::fs::MAX_WIRE_FILE_BYTES as usize,
                });
            }
            let transport = self.current_transport()?;
            let payload = WriteFile {
                path: self.resolve_remote(&request.payload.path)?,
                bytes: request.payload.bytes.clone(),
                create: request.payload.create,
            };
            let message = Message::Fs(FsMessage::Write(self.envelope(&request, payload)));
            let response = transport
                .request(
                    request.request_id,
                    message,
                    Duration::from_secs(30),
                    CancellationToken::new(),
                )
                .await
                .map_err(|error| AuthorityError::Protocol(error.to_string()))?;
            match response {
                Message::Fs(FsMessage::WriteResult(response)) => {
                    self.validate_response(&request, &response)?;
                    let payload = response.payload.map_err(|error| {
                        AuthorityError::Ssh(format!("{:?}: {}", error.code, error.message))
                    })?;
                    Ok(request.respond(payload.bytes_written as usize))
                }
                _ => Err(AuthorityError::Protocol(
                    "unexpected file-write response".into(),
                )),
            }
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
            if cancellation.is_cancelled() {
                return Err(AuthorityError::Ssh("PTY spawn cancelled".into()));
            }
            let transport = self.current_transport()?;
            let stream_id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
            let payload = PtySpawn {
                stream_id,
                generation: request.environment_epoch.0,
                command: request.payload.command.clone(),
                size: WirePtySize {
                    rows: request.payload.size.rows,
                    cols: request.payload.size.cols,
                    pixel_width: request.payload.size.pixel_width,
                    pixel_height: request.payload.size.pixel_height,
                },
            };
            let session =
                RemotePtySession::spawn(transport, self.envelope(&request, payload), cancellation)
                    .await?;
            Ok(request.respond(PtySession::Remote(session)))
        })
    }

    fn exec<'a>(
        &'a self,
        request: AuthorityRequest<ExecRequest>,
        cancellation: CancellationToken,
    ) -> AuthorityFuture<'a, ExecResult> {
        Box::pin(async move {
            self.require_execution()?;
            self.validate_epochs(&request)?;
            let transport = self.current_transport()?;
            let cmd = hermito_protocol::request::CommandSpec {
                program: request.payload.command.program.clone(),
                args: request.payload.command.args.clone(),
                cwd: request.payload.command.cwd.clone(),
                env: request.payload.command.env.clone(),
            };

            let payload = hermito_protocol::process::ExecRequest {
                command: cmd,
                stdout_limit: request.payload.stdout_limit as u64,
                stderr_limit: request.payload.stderr_limit as u64,
                timeout_ms: request.payload.timeout.as_millis().min(u64::MAX as u128) as u64,
            };
            let envelope = self.envelope(&request, payload);
            let message = Message::Process(ProcessMessage::Exec(envelope));
            let cancel_watch = cancellation.clone();
            let cancel_transport = transport.clone();
            let cancel_forward_done = CancellationToken::new();
            let cancel_forward_complete = cancel_forward_done.clone();
            let cancel_forwarder = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancel_watch.cancelled() => {
                        if cancel_transport
                            .try_send(Message::Process(ProcessMessage::Cancel { request_id: request.request_id }))
                            .is_err()
                        {
                            cancel_transport.abort("remote process cancellation could not be delivered");
                        }
                    }
                    _ = cancel_forward_done.cancelled() => {}
                }
            });
            let response = transport
                .request(
                    request.request_id,
                    message,
                    request.payload.timeout + Duration::from_secs(3),
                    cancellation,
                )
                .await;
            cancel_forward_complete.cancel();
            let _ = cancel_forwarder.await;
            let response = response.map_err(|error| AuthorityError::Protocol(error.to_string()))?;
            match response {
                Message::Process(ProcessMessage::Result(response)) => {
                    self.validate_response(&request, &response)?;
                    let output = response.payload.map_err(|error| {
                        AuthorityError::Ssh(format!("{:?}: {}", error.code, error.message))
                    })?;
                    Ok(request.respond(ExecResult {
                        exit_code: output.exit_code,
                        stdout: output.stdout,
                        stderr: output.stderr,
                        stdout_truncated: output.stdout_truncated,
                        stderr_truncated: output.stderr_truncated,
                    }))
                }
                _ => Err(AuthorityError::Protocol("unexpected process response".into())),
            }
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
                    "blocked SSH LSP start"
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
                    "blocked SSH LSP start"
                );
                return Err(AuthorityError::StaleEpoch {
                    expected_workspace: self.workspace_epoch,
                    actual_workspace: context.workspace_epoch,
                    expected_environment: self.environment_epoch(),
                    actual_environment: context.environment_epoch,
                });
            }
            let executable = self
                .validate_lsp_launch(&effective_config, &context, &digest, cancellation.clone())
                .await?;
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %digest,
                session_generation = context.session_generation.0,
                sent_version = context.sent_version.0,
                revision = ?context.document_revision,
                state = "starting",
                "accepted SSH LSP start"
            );
            let transport = self.current_transport()?;
            let server_config = hermito_protocol::lsp::LspServerConfig {
                language_id: "hermito".to_string(),
                program: executable.to_string_lossy().into_owned(),
                args: effective_config.args.clone(),
                cwd: self.root.to_string_lossy().into_owned(),
            };
            let start_v1 = LspV1::Start {
                context: context.clone(),
                config: server_config,
            };
            let rx = transport
                .register_lsp(context.clone())
                .await
                .map_err(|e| AuthorityError::Protocol(e.to_string()))?;
            if let Err(error) = transport.send(Message::Lsp(start_v1)).await {
                transport.unregister_lsp(&context);
                return Err(AuthorityError::Protocol(error.to_string()));
            }
            let ssh_transport: Box<dyn LspTransport> = Box::new(SshLspTransport {
                loss: StdArc::new(tokio::sync::Mutex::new(SshLspExit {
                    receiver: transport.subscribe_loss(),
                    emitted: false,
                })),
                multiplexer: transport,
                context: context.clone(),
                rx: StdArc::new(tokio::sync::Mutex::new(Some(rx))),
            });
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                config_digest = %digest,
                session_generation = context.session_generation.0,
                state = "started",
                "started SSH LSP transport"
            );
            Ok(ssh_transport)
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
                    "rejected SSH LSP workspace edit"
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
            // Remote operations receive content only. Revalidate the host-only
            // Buffer ledger snapshots immediately before the first dispatch.
            preconditions.verify(
                &request.payload,
                &self.host_authority_id(),
                self.workspace_epoch,
                self.environment_epoch(),
            )?;

            tracing::debug!(
                authority_identity = %request.payload.context.authority_identity.0,
                execution_context = ?request.payload.context.execution_context,
                config_digest = %request.payload.config_digest,
                session_generation = request.payload.context.session_generation.0,
                state = "applying",
                "accepted SSH LSP workspace edit"
            );
            self.apply_remote_workspace_edit_transaction(&request).await?;
            tracing::debug!(
                authority_identity = %request.payload.context.authority_identity.0,
                execution_context = ?request.payload.context.execution_context,
                config_digest = %request.payload.config_digest,
                session_generation = request.payload.context.session_generation.0,
                state = "applied",
                "applied SSH LSP workspace edit"
            );
            Ok(request.respond(true))
        })
    }
}

struct SshLspExit {
    receiver: watch::Receiver<Option<crate::remote::multiplexer::TransportLoss>>,
    emitted: bool,
}

struct SshLspTransport {
    loss: StdArc<tokio::sync::Mutex<SshLspExit>>,
    multiplexer: Multiplexer,
    context: LspContext,
    rx: StdArc<tokio::sync::Mutex<Option<mpsc::Receiver<ReceivedMessage>>>>,
}

impl SshLspTransport {
    fn exited(&self) -> LspV1 {
        LspV1::Exited {
            context: self.context.clone(),
            exit_code: None,
        }
    }

    fn with_routing_context(&self, mut message: LspV1) -> LspV1 {
        let context = match &mut message {
            LspV1::Start { context, .. }
            | LspV1::Started { context, .. }
            | LspV1::Shutdown { context }
            | LspV1::Exited { context, .. }
            | LspV1::JsonRpcRequest { context, .. }
            | LspV1::JsonRpcResponse { context, .. }
            | LspV1::JsonRpcNotification { context, .. }
            | LspV1::PublishDiagnostics { context, .. }
            | LspV1::WorkspaceEdit { context, .. }
            | LspV1::WorkspaceEditResult { context, .. } => context,
        };
        *context = self.context.clone();
        message
    }
}

impl LspTransport for SshLspTransport {
    fn send(&self, message: LspV1) -> Pin<Box<dyn Future<Output = Result<(), LspClientError>> + Send + '_>> {
        let m = self.multiplexer.clone();
        let message = self.with_routing_context(message);
        Box::pin(async move {
            m.send(Message::Lsp(message))
                .await
                .map_err(|e| LspClientError::Transport(e.to_string()))
        })
    }
    fn recv(&self) -> Pin<Box<dyn Future<Output = Result<LspV1, LspClientError>> + Send + '_>> {
        let rx_arc = StdArc::clone(&self.rx);
        let loss_arc = StdArc::clone(&self.loss);
        Box::pin(async move {
            let mut guard = rx_arc.lock().await;
            let rx = match guard.as_mut() {
                Some(r) => r,
                None => return Err(LspClientError::Transport("lsp stream closed".into())),
            };
            let mut loss = loss_arc.lock().await;
            if !loss.emitted && loss.receiver.borrow_and_update().is_some() {
                loss.emitted = true;
                return Ok(self.exited());
            }
            if loss.emitted {
                return Err(LspClientError::Transport("lsp stream closed".into()));
            }
            tokio::select! {
                received = rx.recv() => {
                    match received {
                        Some(received) => match received.message {
                            Message::Lsp(v1) => Ok(v1),
                            _ => Err(LspClientError::Transport("non-lsp on route".into())),
                        },
                        None => {
                            if loss.receiver.borrow_and_update().is_some() {
                                loss.emitted = true;
                                Ok(self.exited())
                            } else {
                                Err(LspClientError::Transport("lsp stream ended".into()))
                            }
                        }
                    }
                }
                changed = loss.receiver.changed() => {
                    changed.map_err(|_| LspClientError::Transport(
                        "remote lsp loss watcher closed".into(),
                    ))?;
                    if loss.receiver.borrow_and_update().is_some() {
                        loss.emitted = true;
                        Ok(self.exited())
                    } else {
                        Err(LspClientError::Transport(
                            "remote lsp loss watcher closed".into(),
                        ))
                    }
                }
            }
        })
    }
}


