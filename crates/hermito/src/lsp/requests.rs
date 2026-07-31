//! Capability-, trust-, and ledger-gated LSP request providers.
//!
//! This module deliberately does not parse JSON-RPC payloads. `LspClient` owns
//! request transport and validated DTO ingress; these providers establish the
//! host-side preconditions and keep mutations on the Authority boundary.

use std::{collections::HashSet, path::Path};

use hermito_protocol::lsp::{
    LspContext, LspTextEdit, TransactionalDocumentEdit, TransactionalWorkspaceEdit,
};

use crate::{
    authority::{
        types::{
            AuthorityRequest, LspDocumentChange, LspDocumentRevisionSnapshot,
            LspWorkspaceEditPreconditions, LspWorkspaceEditRequest,
        },
        Authority, AuthorityError,
    },
    buffer::Buffer,
    lsp::{
        CoordinateMapper, LanguageServiceState, LspClient, LspClientError, LspDocumentLedger,
        LspStaleDiscard, LspSupervisor, LspTransport, SupervisorKey,
    },
};

/// A mutable, host-authoritative buffer that may participate in a transactional
/// rename. The exclusive borrow prevents an editor mutation between preflight
/// and the post-Authority buffer commit.
///
/// `relative_path` is supplied by the workspace layer and is validated again
/// by Authority. The provider never resolves paths or writes files itself.
pub struct ProviderDocument<'a> {
    pub uri: &'a str,
    pub relative_path: &'a Path,
    pub buffer: &'a mut Buffer,
}

/// Context supplied with every provider operation.
pub struct ProviderRequest<'a, A: Authority + ?Sized> {
    pub authority: &'a A,
    pub supervisor: &'a LspSupervisor,
    pub service: &'a SupervisorKey,
    pub config_digest: &'a str,
    pub context: LspContext,
    pub document: ProviderDocument<'a>,
}

/// Immutable document/ledger capture for a background provider call.  The UI
/// takes this capture before spawning; completion-style requests therefore
/// never hold a mutable editor buffer across a protocol await.
pub struct ProviderSnapshotRequest<'a, A: Authority + ?Sized> {
    pub authority: &'a A,
    pub service: &'a SupervisorKey,
    pub service_state: Option<LanguageServiceState>,
    pub config_digest: &'a str,
    pub context: LspContext,
    pub uri: &'a str,
    pub revision: crate::document::DocumentRevision,
    pub ledger: &'a LspDocumentLedger,
}

/// A successful provider response. Its payload remains opaque: only the
/// validated client ingress/conversion layer may inspect response DTOs.
#[derive(Debug)]
pub enum ProviderOutcome {
    Empty,
    Response(serde_json::Value),
}

/// Result of prepareRename. It is a capability to request/apply a rename for
/// one exact buffer revision and LSP context.
#[derive(Clone, Debug)]
pub struct RenamePreparation {
    context: LspContext,
    revision: crate::document::DocumentRevision,
}

/// Evidence that both prepareRename and rename were capability-gated and
/// accepted for one exact context/revision. Only this ticket can authorize a
/// transactional WorkspaceEdit application.
#[derive(Clone, Debug)]
pub struct RenameTicket {
    context: LspContext,
    revision: crate::document::DocumentRevision,
}

/// Outcome of dispatching the `textDocument/rename` request. The workspace
/// edit has passed typed LSP DTO ingress and protocol conversion; application
/// remains capability- and transaction-gated below.
#[derive(Debug)]
pub enum RenameRequestOutcome {
    NotRenameable,
    WorkspaceEditResponse {
        ticket: RenameTicket,
        workspace_edit: TransactionalWorkspaceEdit,
    },
}

/// Observable result of Authority's one-shot transactional mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameApplyOutcome {
    NoChanges,
    Applied,
    Rejected,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("LSP execution is inspect-only for this authority/configuration")]
    InspectOnly,
    #[error("LSP server does not advertise support for {method}")]
    UnsupportedCapability { method: String },
    #[error("LSP service is not ready: {state:?}")]
    ServiceNotReady { state: Option<LanguageServiceState> },
    #[error("no LSP ledger exists for this document and execution context")]
    MissingLedger,
    #[error("stale LSP context: {0:?}")]
    StaleContext(LspStaleDiscard),
    #[error("LSP context environment epoch does not match the document ledger")]
    EnvironmentEpochMismatch,
    #[error("LSP context has no current document revision")]
    MissingDocumentRevision,
    #[error("current buffer revision changed: expected {expected}, actual {actual}")]
    CurrentBufferRevisionChanged { expected: u64, actual: u64 },
    #[error("LSP context does not address the selected service")]
    ServiceContextMismatch,
    #[error("rename workspace edit contains no document changes")]
    EmptyWorkspaceEdit,
    #[error("rename workspace edit targets an unsnapshotted document: {uri}")]
    MissingWorkspaceDocument { uri: String },
    #[error("rename workspace edit targets an unresolvable document: {uri}")]
    UnresolvableWorkspaceDocument { uri: String },
    #[error("rename workspace edit contains duplicate document URI: {uri}")]
    DuplicateWorkspaceDocument { uri: String },
    #[error("workspace edit revision mismatch for {uri}: expected {expected}, actual {actual}")]
    WorkspaceRevisionMismatch { uri: String, expected: u64, actual: u64 },
    #[error("workspace edit LSP version mismatch for {uri}: expected {expected}, actual {actual}")]
    WorkspaceLspVersionMismatch {
        uri: String,
        expected: u64,
        actual: i32,
    },
    #[error("workspace edit has an invalid range for {uri}")]
    InvalidWorkspaceRange { uri: String },
    #[error("workspace edit has overlapping ranges for {uri}")]
    OverlappingWorkspaceEdits { uri: String },
    #[error("workspace edit conversion failed: {0}")]
    WorkspaceEdit(#[from] crate::lsp::JsonRpcFrameError),
    #[error("Authority committed a workspace edit but host buffer update failed for {uri}")]
    AuthoritativeBufferUpdate { uri: String },
    #[error(transparent)]
    Client(#[from] LspClientError),
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

impl<'a, A: Authority + ?Sized> ProviderRequest<'a, A> {
    fn ledger(&self) -> Result<&LspDocumentLedger, ProviderError> {
        self.document
            .buffer
            .lsp_ledger(
                &self.context.authority_identity,
                &self.context.execution_context,
            )
            .ok_or(ProviderError::MissingLedger)
    }

    /// Gate every external language-server request against authority trust,
    /// service readiness, and the exact current buffer ledger.
    fn validate<T: LspTransport>(
        &self,
        client: &LspClient<T>,
    ) -> Result<&LspDocumentLedger, ProviderError> {
        if !self.authority.is_lsp_execution_granted(self.config_digest) {
            return Err(ProviderError::InspectOnly);
        }
        if !matches!(self.supervisor.state(self.service), Some(LanguageServiceState::Ready)) {
            return Err(ProviderError::ServiceNotReady {
                state: self.supervisor.state(self.service).cloned(),
            });
        }
        if self.service.workspace_epoch.0 != self.context.workspace_epoch.0
            || self.service.authority_identity != self.context.authority_identity
            || self.service.execution_context != self.context.execution_context
            || self.authority.workspace_epoch().0 != self.context.workspace_epoch.0
            || self.authority.environment_epoch() != self.context.environment_epoch
            || self.authority.host_authority_id() != self.context.authority_identity.0
        {
            return Err(ProviderError::ServiceContextMismatch);
        }

        let ledger = self.ledger()?;
        if ledger.revision != self.document.buffer.revision() {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: ledger.revision.0,
                actual: self.document.buffer.revision().0,
            });
        }
        if ledger.environment_epoch != self.context.environment_epoch {
            return Err(ProviderError::EnvironmentEpochMismatch);
        }
        let actual_revision = self.context.document_revision.ok_or(ProviderError::MissingDocumentRevision)?;
        if actual_revision.0 != self.document.buffer.revision().0 {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: self.document.buffer.revision().0,
                actual: actual_revision.0,
            });
        }

        let expected = client.context_from_ledger(ledger, Some(self.document.buffer.revision()));
        LspClient::<T>::filter_stale_context(&self.context, &expected, Some(ledger))
            .map_err(ProviderError::StaleContext)?;
        Ok(ledger)
    }
}

impl<'a, A: Authority + ?Sized> ProviderSnapshotRequest<'a, A> {
    /// Validate an owned ledger capture without retaining the live buffer. The
    /// receiver must still stale-check the eventual result against that buffer.
    fn validate<T: LspTransport>(&self, client: &LspClient<T>) -> Result<(), ProviderError> {
        if !self.authority.is_lsp_execution_granted(self.config_digest) {
            return Err(ProviderError::InspectOnly);
        }
        if !matches!(self.service_state.as_ref(), Some(LanguageServiceState::Ready)) {
            return Err(ProviderError::ServiceNotReady {
                state: self.service_state.clone(),
            });
        }
        if self.service.workspace_epoch.0 != self.context.workspace_epoch.0
            || self.service.authority_identity != self.context.authority_identity
            || self.service.execution_context != self.context.execution_context
            || self.authority.workspace_epoch().0 != self.context.workspace_epoch.0
            || self.authority.environment_epoch() != self.context.environment_epoch
            || self.authority.host_authority_id() != self.context.authority_identity.0
        {
            return Err(ProviderError::ServiceContextMismatch);
        }
        if self.ledger.revision != self.revision {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: self.ledger.revision.0,
                actual: self.revision.0,
            });
        }
        if self.ledger.environment_epoch != self.context.environment_epoch {
            return Err(ProviderError::EnvironmentEpochMismatch);
        }
        let actual_revision = self
            .context
            .document_revision
            .ok_or(ProviderError::MissingDocumentRevision)?;
        if actual_revision.0 != self.revision.0 {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: self.revision.0,
                actual: actual_revision.0,
            });
        }
        let expected = client.context_from_ledger(self.ledger, Some(self.revision));
        LspClient::<T>::filter_stale_context(&self.context, &expected, Some(self.ledger))
            .map_err(ProviderError::StaleContext)?;
        Ok(())
    }
}

/// Completion provider; LspClient enforces the negotiated completion capability.
pub struct CompletionProvider;

impl CompletionProvider {
    pub async fn complete<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_completion(request.context.clone(), request.document.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }

    pub async fn complete_snapshot<T, A>(
        client: &LspClient<T>,
        request: &ProviderSnapshotRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_completion(request.context.clone(), request.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }
}

/// Hover provider; LspClient enforces the negotiated hover capability.
pub struct HoverProvider;

impl HoverProvider {
    pub async fn hover<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_hover(request.context.clone(), request.document.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }

    pub async fn hover_snapshot<T, A>(
        client: &LspClient<T>,
        request: &ProviderSnapshotRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_hover(request.context.clone(), request.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }
}

/// Definition and declaration provider; LspClient enforces each capability.
pub struct DefinitionProvider;

impl DefinitionProvider {
    pub async fn definition<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_definition(request.context.clone(), request.document.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }

    pub async fn definition_snapshot<T, A>(
        client: &LspClient<T>,
        request: &ProviderSnapshotRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_definition(request.context.clone(), request.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }

    pub async fn declaration<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        Ok(match client
            .request_declaration(request.context.clone(), request.document.uri, position)
            .await
            .map_err(provider_client_error)?
        {
            Some(response) => ProviderOutcome::Response(response),
            None => ProviderOutcome::Empty,
        })
    }
}

/// Rename provider. It never mutates buffers or files; all accepted batches go
/// through `Authority::apply_lsp_workspace_edit` exactly once.
pub struct RenameProvider;

impl RenameProvider {
    pub async fn prepare<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<Option<RenamePreparation>, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        let prepared = client
            .request_prepare_rename(request.context.clone(), request.document.uri, position)
            .await
            .map_err(provider_client_error)?;
        // The server call may have awaited while the editor changed. Never make
        // a rename token from a stale ledger.
        request.validate(client)?;
        Ok(prepared.map(|_| RenamePreparation {
            context: request.context.clone(),
            revision: request.document.buffer.revision(),
        }))
    }

    pub async fn prepare_snapshot<T, A>(
        client: &LspClient<T>,
        request: &ProviderSnapshotRequest<'_, A>,
        position: lsp_types::Position,
    ) -> Result<Option<RenamePreparation>, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        let prepared = client
            .request_prepare_rename(request.context.clone(), request.uri, position)
            .await
            .map_err(provider_client_error)?;
        request.validate(client)?;
        Ok(prepared.map(|_| RenamePreparation {
            context: request.context.clone(),
            revision: request.revision,
        }))
    }

    pub async fn request_rename<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        preparation: &RenamePreparation,
        position: lsp_types::Position,
        new_name: &str,
    ) -> Result<RenameRequestOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        Self::validate_preparation(client, request, preparation)?;
        let response = client
            .request_rename(request.context.clone(), request.document.uri, position, new_name)
            .await
            .map_err(provider_client_error)?;
        Self::validate_preparation(client, request, preparation)?;
        Ok(match response {
            Some(workspace_edit) => RenameRequestOutcome::WorkspaceEditResponse {
                ticket: RenameTicket {
                    context: preparation.context.clone(),
                    revision: preparation.revision,
                },
                workspace_edit,
            },
            None => RenameRequestOutcome::NotRenameable,
        })
    }

    pub async fn request_rename_snapshot<T, A>(
        client: &LspClient<T>,
        request: &ProviderSnapshotRequest<'_, A>,
        preparation: &RenamePreparation,
        position: lsp_types::Position,
        new_name: &str,
    ) -> Result<RenameRequestOutcome, ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        if preparation.context != request.context || preparation.revision != request.revision {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: preparation.revision.0,
                actual: request.revision.0,
            });
        }
        let response = client
            .request_rename(request.context.clone(), request.uri, position, new_name)
            .await
            .map_err(provider_client_error)?;
        request.validate(client)?;
        Ok(match response {
            Some(workspace_edit) => RenameRequestOutcome::WorkspaceEditResponse {
                ticket: RenameTicket {
                    context: preparation.context.clone(),
                    revision: preparation.revision,
                },
                workspace_edit,
            },
            None => RenameRequestOutcome::NotRenameable,
        })
    }

    /// Apply a typed WorkspaceEdit after the caller has stale-checked the
    /// captured rename ticket/context. The transaction itself retains every
    /// Authority and Buffer precondition before mutating either boundary.
    pub async fn apply_workspace_edit<'a, A>(
        request: &mut ProviderRequest<'a, A>,
        workspace_edit: &TransactionalWorkspaceEdit,
        documents: &mut [ProviderDocument<'a>],
    ) -> Result<RenameApplyOutcome, ProviderError>
    where
        A: Authority + ?Sized,
    {
        crate::lsp::validate_workspace_edit(workspace_edit)?;
        if workspace_edit.document_changes.is_empty() {
            return Ok(RenameApplyOutcome::NoChanges);
        }

        let mut changes = Vec::with_capacity(workspace_edit.document_changes.len());
        let mut precondition_snapshots =
            Vec::with_capacity(workspace_edit.document_changes.len());
        let mut prepared_buffers = Vec::with_capacity(workspace_edit.document_changes.len());
        let mut seen_uris = HashSet::with_capacity(workspace_edit.document_changes.len());
        for edit in &workspace_edit.document_changes {
            let TransactionalDocumentEdit::TextDocument {
                uri,
                expected_revision,
                expected_lsp_version,
                edits,
            } = edit;
            if !seen_uris.insert(uri.as_str()) {
                return Err(ProviderError::DuplicateWorkspaceDocument { uri: uri.clone() });
            }
            let document = if request.document.uri == uri {
                &request.document
            } else {
                documents
                    .iter()
                    .find(|document| document.uri == uri)
                    .ok_or_else(|| ProviderError::MissingWorkspaceDocument { uri: uri.clone() })?
            };
            let ledger = document
                .buffer
                .lsp_ledger(
                    &request.context.authority_identity,
                    &request.context.execution_context,
                )
                .ok_or(ProviderError::MissingLedger)?;
            if ledger.revision != document.buffer.revision() {
                return Err(ProviderError::CurrentBufferRevisionChanged {
                    expected: ledger.revision.0,
                    actual: document.buffer.revision().0,
                });
            }
            if ledger.workspace_epoch.0 != request.context.workspace_epoch.0
                || ledger.environment_epoch != request.context.environment_epoch
            {
                return Err(ProviderError::EnvironmentEpochMismatch);
            }
            if let Some(expected_lsp_version) = expected_lsp_version {
                if i32::try_from(expected_lsp_version.0).ok() != Some(ledger.sent_version) {
                    return Err(ProviderError::WorkspaceLspVersionMismatch {
                        uri: uri.clone(),
                        expected: expected_lsp_version.0,
                        actual: ledger.sent_version,
                    });
                }
            }
            let expected = expected_revision
                .map(|revision| revision.0)
                .unwrap_or(document.buffer.revision().0);
            if expected != document.buffer.revision().0 {
                return Err(ProviderError::WorkspaceRevisionMismatch {
                    uri: uri.clone(),
                    expected,
                    actual: document.buffer.revision().0,
                });
            }
            let revision = document.buffer.revision();
            let content = apply_document_edits(document, edits)?;
            changes.push(LspDocumentChange {
                relative_path: document.relative_path.to_path_buf(),
                content: content.clone().into_bytes(),
            });
            prepared_buffers.push((uri.clone(), content, revision));
            precondition_snapshots.push(LspDocumentRevisionSnapshot {
                relative_path: document.relative_path.to_path_buf(),
                authority_identity: request.context.authority_identity.clone(),
                execution_context: request.context.execution_context.clone(),
                workspace_epoch: ledger.workspace_epoch,
                environment_epoch: ledger.environment_epoch,
                revision,
                buffer: &*document.buffer,
            });
        }

        // Every host buffer is exclusively borrowed, and all exact ledger and
        // revision preconditions are checked before Authority starts its
        // transaction. Therefore an Authority error leaves both disk and
        // buffers unchanged; after success each checked replacement is safe.
        // The UI stale-check and the preflight above are both complete before
        // Authority starts its all-or-nothing transaction.
        let payload = LspWorkspaceEditRequest {
            context: request.context.clone(),
            config_digest: request.config_digest.to_owned(),
            changes,
        };
        let authority_request = AuthorityRequest::new(
            payload,
            request.context.workspace_epoch,
            request.context.environment_epoch,
            Some(hermito_protocol::DocumentRevision(request.document.buffer.revision().0)),
        );
        let applied = {
            let preconditions = LspWorkspaceEditPreconditions {
                snapshots: precondition_snapshots,
            };
            request
                .authority
                .apply_lsp_workspace_edit(authority_request, preconditions)
                .await
                .map_err(|error| match error {
                    AuthorityError::InspectOnly => ProviderError::InspectOnly,
                    error => ProviderError::Authority(error),
                })?
                .payload
        };
        if !applied {
            return Ok(RenameApplyOutcome::Rejected);
        }

        for (uri, content, revision) in prepared_buffers {
            let document = if request.document.uri == uri {
                &mut request.document
            } else {
                documents
                    .iter_mut()
                    .find(|document| document.uri == uri)
                    .ok_or_else(|| ProviderError::MissingWorkspaceDocument { uri: uri.clone() })?
            };
            document
                .buffer
                .apply_lsp_workspace_replacement(
                    &request.context.authority_identity,
                    &request.context.execution_context,
                    revision,
                    crate::document::WorkspaceEpoch(request.context.workspace_epoch.0),
                    request.context.environment_epoch,
                    &content,
                )
                .map_err(|_| ProviderError::AuthoritativeBufferUpdate { uri })?;
        }
        Ok(RenameApplyOutcome::Applied)
    }

    fn validate_preparation<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        preparation: &RenamePreparation,
    ) -> Result<(), ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        if preparation.context != request.context {
            return Err(ProviderError::StaleContext(
                LspStaleDiscard::MismatchedExecutionContext,
            ));
        }
        let current = request.document.buffer.revision();
        if preparation.revision != current {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: preparation.revision.0,
                actual: current.0,
            });
        }
        Ok(())
    }

    fn validate_ticket<T, A>(
        client: &LspClient<T>,
        request: &ProviderRequest<'_, A>,
        ticket: &RenameTicket,
    ) -> Result<(), ProviderError>
    where
        T: LspTransport,
        A: Authority + ?Sized,
    {
        request.validate(client)?;
        if ticket.context != request.context {
            return Err(ProviderError::StaleContext(
                LspStaleDiscard::MismatchedExecutionContext,
            ));
        }
        let current = request.document.buffer.revision();
        if ticket.revision != current {
            return Err(ProviderError::CurrentBufferRevisionChanged {
                expected: ticket.revision.0,
                actual: current.0,
            });
        }
        Ok(())
    }
}

fn provider_client_error(error: LspClientError) -> ProviderError {
    match error {
        LspClientError::Unsupported { method }
        | LspClientError::Stale(LspStaleDiscard::UnsupportedCapability { method }) => {
            ProviderError::UnsupportedCapability { method }
        }
        error => ProviderError::Client(error),
    }
}

fn apply_document_edits(
    document: &ProviderDocument<'_>,
    edits: &[LspTextEdit],
) -> Result<String, ProviderError> {
    let mapper = CoordinateMapper::new(document.buffer.rope());
    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let range = lsp_types::Range {
            start: lsp_types::Position {
                line: edit.range.start.line,
                character: edit.range.start.character,
            },
            end: lsp_types::Position {
                line: edit.range.end.line,
                character: edit.range.end.character,
            },
        };
        let start = range.start;
        let end = range.end;
        let start = mapper
            .lsp_position_to_byte(start)
            .ok_or_else(|| ProviderError::InvalidWorkspaceRange {
                uri: document.uri.to_owned(),
            })?;
        let end = mapper
            .lsp_position_to_byte(end)
            .filter(|end| *end >= start)
            .ok_or_else(|| ProviderError::InvalidWorkspaceRange {
                uri: document.uri.to_owned(),
            })?;
        ranges.push((start, end, edit.new_text.as_str()));
    }
    ranges.sort_unstable_by_key(|(start, _, _)| *start);
    for window in ranges.windows(2) {
        if window[0].1 > window[1].0 {
            return Err(ProviderError::OverlappingWorkspaceEdits {
                uri: document.uri.to_owned(),
            });
        }
    }

    let mut content = document.buffer.text();
    for (start, end, replacement) in ranges.into_iter().rev() {
        content.replace_range(start..end, replacement);
    }
    Ok(content)
}
