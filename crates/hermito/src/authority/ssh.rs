use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use hermito_protocol::{
    fs::{FsMessage, ReadFile, WriteFile},
    process::{ExecRequest as WireExecRequest, ProcessMessage},
    pty::{PtySize as WirePtySize, PtySpawn},
    request::{EnvironmentEpoch, ExecutionContextV1, RequestEnvelope, WorkspaceEpoch},
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
        AuthorityKind, AuthorityRequest, AuthorityTrust, ExecRequest, PtyRequest, ReadFileRequest,
        WriteFileRequest,
    },
    Authority, AuthorityError, AuthorityFuture,
};

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
            let payload = WireExecRequest {
                command: request.payload.command.clone(),
                timeout_ms: request
                    .payload
                    .timeout
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
                stdout_limit: (request.payload.stdout_limit as u64)
                    .min(hermito_protocol::process::MAX_WIRE_OUTPUT_BYTES),
                stderr_limit: (request.payload.stderr_limit as u64)
                    .min(hermito_protocol::process::MAX_WIRE_OUTPUT_BYTES),
            };
            let message = Message::Process(ProcessMessage::Exec(self.envelope(&request, payload)));
            let cancel_transport = transport.clone();
            let request_id = request.request_id;
            let cancel_watch = cancellation.clone();
            let cancel_forward_done = CancellationToken::new();
            let cancel_forward_complete = cancel_forward_done.clone();
            let cancel_forwarder = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancel_watch.cancelled() => {
                        if cancel_transport
                            .try_send(Message::Process(ProcessMessage::Cancel { request_id }))
                            .is_err()
                        {
                            cancel_transport.abort(
                                "remote process cancellation could not be delivered",
                            );
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
                _ => Err(AuthorityError::Protocol(
                    "unexpected process response".into(),
                )),
            }
        })
    }
}
