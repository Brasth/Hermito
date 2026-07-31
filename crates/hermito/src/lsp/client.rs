//! Host LSP JSON-RPC client with bounded async transport abstraction (direct Content-Length or canonical protocol).
//! Pending requests keyed by LspContext ledger tag + JSON-RPC ID.
//! Initialize lifecycle, guarded capability intersection, didOpen/didChange/didClose, listed requests.
//! Timeouts + cancellation via $/cancelRequest. Stale produces typed discard, never mutates state.
//! Raw ingress validated via jsonrpc + protocol DTOs before any lsp-types conversion or acceptance.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hermito_protocol::{
    lsp::{
        AuthorityIdentity, LspContext, LspJsonRpcNotification, LspJsonRpcRequest,
        LspJsonRpcResponse, LspRequestId, LspV1, SentVersion, SessionGeneration,
    },
    request::{
        DocumentRevision as ProtocolDocumentRevision, ExecutionContextV1,
        WorkspaceEpoch as ProtocolWorkspaceEpoch,
    },
};
use serde_json::{json, Value};
use tokio::sync::{Mutex, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use std::future::Future;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
    document::DocumentRevision,
    lsp::{
        jsonrpc::{self, JsonRpcFrameError, VersionlessPolicy, MAX_CONTENT_LENGTH},
        LspDocumentLedger,
    },
};
const MAX_PENDING_SERVER_REQUESTS: usize = 256;

/// Typed discard reason for any mismatching response or diagnostic. Never forwarded to editor/UI.
#[derive(Debug)]
pub enum LspStaleDiscard {
    MismatchedAuthorityIdentity { expected: String, actual: String },
    MismatchedExecutionContext,
    MismatchedDocumentRevision {
        expected: Option<ProtocolDocumentRevision>,
        actual: Option<ProtocolDocumentRevision>,
    },
    MismatchedSentVersion { expected: i32, actual: i32 },
    MismatchedSessionGeneration { expected: u64, actual: u64 },
    MismatchedWorkspaceEpoch {
        expected: ProtocolWorkspaceEpoch,
        actual: ProtocolWorkspaceEpoch,
    },
    MismatchedEnvironmentEpoch,
    VersionMismatch { incoming: Option<i32>, ledger: Option<i32> },
    UnsupportedCapability { method: String },
    RequestTimeout,
    Cancelled,
    Validation(JsonRpcFrameError),
    DuplicatePending,
    AggregatePendingLimit,
    UnsupportedLspV1,
}

/// Errors surfaced by the client (typed, no raw payload leakage).
#[derive(Debug, thiserror::Error)]
pub enum LspClientError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("stale or discarded: {0:?}")]
    Stale(LspStaleDiscard),
    #[error("framing/validation: {0}")]
    Frame(#[from] JsonRpcFrameError),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("timeout or cancelled")]
    Timeout,
    #[error("capability not supported by server after initialize")]
    Unsupported { method: String },
    #[error("pending limit or duplicate")]
    Backpressure,
}
/// Bounded async transport abstraction. Direct impls perform Content-Length framing (see jsonrpc).
/// Canonical protocol impls forward full LspV1 over Message::Lsp.
pub trait LspTransport: Send + Sync + 'static {
    fn send(&self, message: LspV1) -> Pin<Box<dyn Future<Output = Result<(), LspClientError>> + Send + '_>>;
    fn recv(&self) -> Pin<Box<dyn Future<Output = Result<LspV1, LspClientError>> + Send + '_>>;
}



/// Minimal LspClient. Owns no Buffer/document state; uses caller-provided ledgers for tagging and validation.
/// All outgoing requests carry LspContext snapshot from ledger at send time.
/// Incoming results/diagnostics are matched and validated against sent context + current ledger.
/// Minimal LSP client. The transport is addressed with its immutable session
/// context while this client retains the latest document-ledger context used to
/// validate results at the host boundary.
pub struct LspClient<T: LspTransport> {
    transport: T,
    pending: Arc<Mutex<HashMap<(LspContext, LspRequestId), oneshot::Sender<LspJsonRpcResponse>>>>,
    /// Raw capabilities Value from initialize result. Guarded checks performed before every request.
    server_capabilities: Arc<Mutex<Option<Value>>>,
    next_request_id: AtomicU64,
    /// Base authority identity for this client instance.
    authority_identity: AuthorityIdentity,
    /// Default per-request timeout.
    request_timeout: Duration,
    /// Policy for versionless diagnostics (explicit hook, no silent defaulting).
    versionless_policy: VersionlessPolicy,
    /// The latest authoritative ledger tag for this document session. Direct
    /// transports reattach their immutable route context on ingress, so the
    /// host must restore this tag before stale-result validation.
    latest_context: Arc<Mutex<Option<LspContext>>>,
    cancel: CancellationToken,
}

impl<T: LspTransport> LspClient<T> {
    pub fn new(
        transport: T,
        authority_identity: AuthorityIdentity,
        request_timeout: Duration,
        versionless_policy: VersionlessPolicy,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            transport,
            pending: Arc::new(Mutex::new(HashMap::new())),
            server_capabilities: Arc::new(Mutex::new(None)),
            next_request_id: AtomicU64::new(1),
            authority_identity,
            request_timeout,
            versionless_policy,
            latest_context: Arc::new(Mutex::new(None)),
            cancel,
        }
    }

    /// Receive one validated transport frame for the supervisor lifecycle.
    ///
    /// The physical transport remains registered under the stable context used
    /// at `Start`; incoming document traffic is restored to the most recently
    /// sent ledger tag before it reaches application stale filtering.
    pub async fn recv(&self) -> Result<LspV1, LspClientError> {
        let mut message = self.transport.recv().await?;
        if matches!(message, LspV1::Exited { .. }) {
            return Ok(message);
        }
        let context = match &message {
            LspV1::JsonRpcResponse { payload, .. } => {
                let pending = self.pending.lock().await;
                pending
                    .keys()
                    .find_map(|(context, id)| (id == &payload.id).then(|| context.clone()))
            }
            _ => self.latest_context.lock().await.clone(),
        };
        if let Some(context) = context {
            match &mut message {
                LspV1::Start { context: target, .. }
                | LspV1::Started { context: target, .. }
                | LspV1::Shutdown { context: target }
                | LspV1::JsonRpcRequest { context: target, .. }
                | LspV1::JsonRpcResponse { context: target, .. }
                | LspV1::JsonRpcNotification { context: target, .. }
                | LspV1::PublishDiagnostics { context: target, .. }
                | LspV1::WorkspaceEdit { context: target, .. }
                | LspV1::WorkspaceEditResult { context: target, .. } => *target = context,
                LspV1::Exited { .. } => unreachable!("exited messages retain their route context"),
            }
        }
        Ok(message)
    }

    async fn send_transport(&self, message: LspV1) -> Result<(), LspClientError> {
        tracing::trace!(
            authority_identity = %message.context().authority_identity.0,
            execution_context = ?message.context().execution_context,
            session_generation = message.context().session_generation.0,
            sent_version = message.context().sent_version.0,
            document_revision = ?message.context().document_revision,
            "routing LSP transport message"
        );
        *self.latest_context.lock().await = Some(message.context().clone());
        self.transport.send(message).await
    }
    fn next_jsonrpc_id(&self) -> i64 {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        // fit in i64 for LspRequestId::Number
        (id % (i64::MAX as u64)) as i64
    }

    /// Build LspContext for an outgoing operation using ledger snapshot (authoritative).
    /// Caller must have obtained ledger via Buffer::lsp_ledger / ensure_lsp_ledger.
    pub fn context_from_ledger(
        &self,
        ledger: &LspDocumentLedger,
        document_revision: Option<DocumentRevision>,
    ) -> LspContext {
        LspContext {
            workspace_epoch: ProtocolWorkspaceEpoch(ledger.workspace_epoch.0),
            environment_epoch: ledger.environment_epoch,
            document_revision: document_revision
                .map(|revision| ProtocolDocumentRevision(revision.0)),
            sent_version: SentVersion(ledger.sent_version as u64),
            session_generation: SessionGeneration(ledger.session_generation),
            execution_context: ledger.context.clone(),
            authority_identity: ledger.authority_identity.clone(),
        }
    }

    /// Capability guard: never sends a request for unsupported feature.
    /// Intersection checked against stored server capabilities (post-initialize).
    async fn guard_capability(&self, method: &str) -> Result<(), LspClientError> {
        let caps = self.server_capabilities.lock().await.clone();
        let supported = match caps {
            None => false,
            Some(v) => match method {
                "textDocument/completion" => v.get("completionProvider").is_some(),
                "textDocument/hover" => v.get("hoverProvider").map_or(false, |p| !p.is_null()),
                "textDocument/definition" => v.get("definitionProvider").map_or(false, |p| !p.is_null()),
                "textDocument/declaration" => v.get("declarationProvider").map_or(false, |p| !p.is_null()),
                "textDocument/prepareRename" | "textDocument/rename" => {
                    v.get("renameProvider").map_or(false, |p| !p.is_null())
                }
                _ => true, // notifications and initialize are allowed pre/post
            },
        };
        if !supported {
            tracing::debug!(method, "blocked unsupported LSP capability request");
            return Err(LspClientError::Unsupported {
                method: method.to_string(),
            });
        }
        Ok(())
    }

    /// Core send for a JSON-RPC request. Registers pending keyed by (context, id).
    /// Timeout + outer cancellation supported. On mismatch or timeout, pending is cleaned.
    async fn send_request(
        &self,
        ctx: LspContext,
        method: &str,
        params: Option<Value>,
    ) -> Result<LspJsonRpcResponse, LspClientError> {
        self.guard_capability(method).await?;

        let id = jsonrpc::make_request_id(self.next_jsonrpc_id()).map_err(LspClientError::Frame)?;
        let payload = LspJsonRpcRequest {
            id: id.clone(),
            method: method.to_string(),
            params,
        };
        let v1 = LspV1::JsonRpcRequest {
            context: ctx.clone(),
            payload,
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
                tracing::debug!(
                    method,
                    authority_identity = %ctx.authority_identity.0,
                    execution_context = ?ctx.execution_context,
                    session_generation = ctx.session_generation.0,
                    sent_version = ctx.sent_version.0,
                    document_revision = ?ctx.document_revision,
                    discard_reason = "pending_limit",
                    "discarded LSP request"
                );
            if p.len() >= 256 {
                // bounded
                return Err(LspClientError::Backpressure);
            }
            if p.contains_key(&(ctx.clone(), id.clone())) {
                tracing::debug!(
                    method,
                    authority_identity = %ctx.authority_identity.0,
                    execution_context = ?ctx.execution_context,
                    session_generation = ctx.session_generation.0,
                    sent_version = ctx.sent_version.0,
                    document_revision = ?ctx.document_revision,
                    discard_reason = "duplicate_pending",
                    "discarded LSP request"
                );
                return Err(LspClientError::Stale(LspStaleDiscard::DuplicatePending));
            }
            p.insert((ctx.clone(), id.clone()), tx);
        }
        tracing::debug!(
            method,
            authority_identity = %ctx.authority_identity.0,
            execution_context = ?ctx.execution_context,
            session_generation = ctx.session_generation.0,
            sent_version = ctx.sent_version.0,
            document_revision = ?ctx.document_revision,
            "dispatching LSP request"
        );

        if let Err(e) = self.send_transport(v1).await {
            let mut p = self.pending.lock().await;
            p.remove(&(ctx.clone(), id.clone()));
            return Err(e);
        }

        let recv_fut = async {
            match rx.await {
                Ok(resp) => Ok(resp),
                Err(_) => Err(LspClientError::Transport("oneshot dropped".into())),
            }
        };

        tokio::select! {
            _ = self.cancel.cancelled() => {
                tracing::debug!(method, discard_reason = "cancelled", "discarded LSP request");
                self.cleanup_pending(&ctx, &id).await;
                self.send_cancel_notification(&ctx, &id).await.ok();
                Err(LspClientError::Stale(LspStaleDiscard::Cancelled))
            }
            res = timeout(self.request_timeout, recv_fut) => {
                match res {
                    Ok(Ok(r)) => {
                        // Validate on ingress path before returning to caller
                        self.validate_response_context(&ctx, &r).await?;
                        Ok(r)
                    }
                    Ok(Err(e)) => {
                        self.cleanup_pending(&ctx, &id).await;
                        Err(e)
                    }
                    Err(_) => {
                        tracing::debug!(method, discard_reason = "request_timeout", "discarded LSP request");
                        self.cleanup_pending(&ctx, &id).await;
                        self.send_cancel_notification(&ctx, &id).await.ok();
                        Err(LspClientError::Stale(LspStaleDiscard::RequestTimeout))
                    }
                }
            }
        }
    }

    async fn cleanup_pending(&self, ctx: &LspContext, id: &LspRequestId) {
        let mut p = self.pending.lock().await;
        p.remove(&(ctx.clone(), id.clone()));
    }

    async fn send_cancel_notification(&self, ctx: &LspContext, id: &LspRequestId) -> Result<(), LspClientError> {
        let notif = LspJsonRpcNotification {
            method: "$/cancelRequest".to_string(),
            params: Some(json!({ "id": id })),
        };
        let v1 = LspV1::JsonRpcNotification {
            context: ctx.clone(),
            payload: notif,
        };
        self.send_transport(v1).await
    }
    /// Acknowledge a server-initiated `workspace/applyEdit` request through the
    /// typed transport protocol. The caller owns validation and authority
    /// application; this method only preserves the request's route context.
    pub async fn workspace_edit_result(
        &self,
        context: LspContext,
        request_id: LspRequestId,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), LspClientError> {
        tracing::debug!(
            authority_identity = %context.authority_identity.0,
            execution_context = ?context.execution_context,
            session_generation = context.session_generation.0,
            applied,
            reason_present = reason.is_some(),
            "routing LSP workspace edit result"
        );
        self.send_transport(LspV1::WorkspaceEditResult {
            context,
            request_id,
            applied,
            reason,
        })
        .await
    }


    /// Handle one incoming LspV1 (from transport recv). Returns discard or accepted payload.
    /// Stale or mismatched always yields explicit LspStaleDiscard; never mutates UI/state.
    pub async fn handle_incoming(&self, v1: LspV1) -> Result<Incoming, LspClientError> {
        match v1 {
            LspV1::JsonRpcResponse { context, payload } => {
                let key = (context.clone(), payload.id.clone());
                let mut p = self.pending.lock().await;
                if let Some(sender) = p.remove(&key) {
                    // double-check context ledger values against what we sent
                    if let Err(discard) = self.check_context_match(&context, &payload).await {
                        let _ = sender.send(payload); // still deliver raw to drain, caller sees stale via separate path
                        return Err(LspClientError::Stale(discard));
                    }
                    let _ = sender.send(payload.clone());
                    Ok(Incoming::Response { context, payload })
                } else {
                    tracing::debug!(
                        authority_identity = %context.authority_identity.0,
                        execution_context = ?context.execution_context,
                        session_generation = context.session_generation.0,
                        discard_reason = "no_pending_request",
                        "discarded LSP response"
                    );
                    // unsolicited or already timed out -> explicit discard
                    Err(LspClientError::Stale(LspStaleDiscard::Validation(
                        JsonRpcFrameError::Validation {
                            reason: "no pending request for LSP context and id".into(),
                        },
                    )))
                }
            }
            LspV1::JsonRpcNotification { context, payload } => {
                if payload.method == "textDocument/publishDiagnostics" {
                    let params = payload.params.as_ref().ok_or_else(|| {
                        LspClientError::Frame(JsonRpcFrameError::Validation {
                            reason: "publishDiagnostics missing params".into(),
                        })
                    })?;
                    let uri = params
                        .get("uri")
                        .and_then(Value::as_str)
                        .filter(|uri| !uri.is_empty())
                        .ok_or_else(|| {
                            LspClientError::Frame(JsonRpcFrameError::Validation {
                                reason: "publishDiagnostics missing or invalid uri".into(),
                            })
                        })?
                        .to_owned();
                    let version = match params.get("version") {
                        None | Some(Value::Null) => None,
                        Some(value) => {
                            let raw = value.as_i64().ok_or_else(|| {
                                LspClientError::Frame(JsonRpcFrameError::NumericViolation {
                                    field: "publishDiagnostics.version".into(),
                                })
                            })?;
                            Some(i32::try_from(raw).map_err(|_| {
                                LspClientError::Frame(JsonRpcFrameError::NumericViolation {
                                    field: "publishDiagnostics.version".into(),
                                })
                            })?)
                        }
                    };
                    let diagnostics = params.get("diagnostics").ok_or_else(|| {
                        LspClientError::Frame(JsonRpcFrameError::Validation {
                            reason: "publishDiagnostics missing diagnostics".into(),
                        })
                    })?;
                    // Diagnostics must deserialize and validate before this notification
                    // is accepted. In particular, malformed input must never become an
                    // empty list that clears a document's existing Problems.
                    let diagnostics: Vec<hermito_protocol::lsp::LspDiagnostic> =
                        serde_json::from_value(diagnostics.clone()).map_err(|_| {
                            LspClientError::Frame(JsonRpcFrameError::Validation {
                                reason: "publishDiagnostics diagnostics are malformed".into(),
                            })
                        })?;
                    jsonrpc::validate_diagnostics(&diagnostics).map_err(LspClientError::Frame)?;
                    if !jsonrpc::should_accept_diagnostics(None, version, self.versionless_policy) {
                        return Ok(Incoming::Discarded(LspStaleDiscard::VersionMismatch {
                            incoming: version,
                            ledger: None,
                        }));
                    }
                    return Ok(Incoming::PublishDiagnostics {
                        context,
                        uri,
                        version,
                        diagnostics,
                    });
                }
                Ok(Incoming::Notification { context, payload })
            }
            LspV1::PublishDiagnostics { context, uri, version, diagnostics } => {
                jsonrpc::validate_diagnostics(&diagnostics).map_err(LspClientError::Frame)?;
                if !jsonrpc::should_accept_diagnostics(None, version, self.versionless_policy) {
                    return Ok(Incoming::Discarded(LspStaleDiscard::VersionMismatch { incoming: version, ledger: None }));
                }
                Ok(Incoming::PublishDiagnostics { context, uri, version, diagnostics })
            }
            LspV1::Started { context, capabilities } => {
                // Accept capabilities only after validation path (inert until stored)
                let mut caps = self.server_capabilities.lock().await;
                *caps = Some(capabilities.clone());
                Ok(Incoming::Capabilities { context, capabilities })
            }
            LspV1::WorkspaceEdit {
                context,
                request_id,
                edit,
            } => Ok(Incoming::WorkspaceEdit {
                context,
                request_id,
                edit,
            }),
            other => {
                // Other control messages are inert for the client core and remain
                // available to the supervisor.
                Ok(Incoming::Other(other))
            }
        }
    }

    async fn check_context_match(
        &self,
        received: &LspContext,
        _resp: &LspJsonRpcResponse,
    ) -> Result<(), LspStaleDiscard> {
        // Basic identity checks. Full ledger cross-check done by caller using filter API if needed.
        if received.authority_identity.0 != self.authority_identity.0 {
            return Err(LspStaleDiscard::MismatchedAuthorityIdentity {
                expected: self.authority_identity.0.clone(),
                actual: received.authority_identity.0.clone(),
            });
        }
        // Additional numeric/epoch checks can be layered by caller via exposed filter.
        Ok(())
    }

    async fn validate_response_context(
        &self,
        _sent: &LspContext,
        _resp: &LspJsonRpcResponse,
    ) -> Result<(), LspClientError> {
        Ok(())
    }

    // --- Lifecycle ---

    /// Perform initialize + initialized notification. Stores capabilities for guarded intersection.
    /// Context must be freshly minted from ledger for this authority.
    pub async fn initialize(
        &self,
        ctx: LspContext,
        root_uri: Option<String>,
        client_capabilities: Value,
        initialization_options: Option<Value>,
    ) -> Result<Value, LspClientError> {
        let params = json!({
            "processId": std::process::id() as i64,
            "rootUri": root_uri,
            "capabilities": client_capabilities,
            "initializationOptions": initialization_options,
            "trace": "off",
        });
        let resp = self.send_request(ctx.clone(), "initialize", Some(params)).await?;
        if let Some(err) = &resp.error {
            return Err(LspClientError::Protocol(format!("initialize error: {} - {}", err.code, err.message)));
        }
        let result = resp.result.clone().unwrap_or(json!({}));

        // Send initialized notification (no response expected)
        let notif = LspJsonRpcNotification {
            method: "initialized".to_string(),
            params: Some(json!({})),
        };
        let v1 = LspV1::JsonRpcNotification { context: ctx, payload: notif };
        self.send_transport(v1).await?;

        // Store for guards (inert JSON until here)
        let mut caps = self.server_capabilities.lock().await;
        if let Some(obj) = result.get("capabilities") {
            *caps = Some(obj.clone());
        } else {
            *caps = Some(result.clone());
        }
        Ok(result)
    }

    // --- Notifications (fire-and-forget, carry current ledger snapshot) ---

    pub async fn did_open(
        &self,
        ctx: LspContext,
        uri: &str,
        language_id: &str,
        version: i32,
        text: &str,
    ) -> Result<(), LspClientError> {
        if text.len() > MAX_CONTENT_LENGTH {
            return Err(LspClientError::Frame(JsonRpcFrameError::SizeViolation {
                field: "text".into(),
                len: text.len(),
            }));
        }
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": version,
                "text": text,
            }
        });
        let notif = LspJsonRpcNotification {
            method: "textDocument/didOpen".to_string(),
            params: Some(params),
        };
        let v1 = LspV1::JsonRpcNotification { context: ctx, payload: notif };
        self.send_transport(v1).await
    }

    pub async fn did_change(
        &self,
        ctx: LspContext,
        uri: &str,
        version: i32,
        text: &str, // full content sync (ledger snapshot)
    ) -> Result<(), LspClientError> {
        if text.len() > MAX_CONTENT_LENGTH {
            return Err(LspClientError::Frame(JsonRpcFrameError::SizeViolation {
                field: "text".into(),
                len: text.len(),
            }));
        }
        let params = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [ { "text": text } ]
        });
        let notif = LspJsonRpcNotification {
            method: "textDocument/didChange".to_string(),
            params: Some(params),
        };
        let v1 = LspV1::JsonRpcNotification { context: ctx, payload: notif };
        self.send_transport(v1).await
    }

    pub async fn did_close(&self, ctx: LspContext, uri: &str) -> Result<(), LspClientError> {
        let params = json!({ "textDocument": { "uri": uri } });
        let notif = LspJsonRpcNotification {
            method: "textDocument/didClose".to_string(),
            params: Some(params),
        };
        let v1 = LspV1::JsonRpcNotification { context: ctx, payload: notif };
        self.send_transport(v1).await
    }

    // --- Requests (capability guarded, context+id keyed pending, timeout/cancel) ---

    pub async fn request_completion(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Option<Value>, LspClientError> {
        // Convert outgoing position; we allow lsp_types here for construction (client originated).
        // Server data path remains validated DTO first.
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });
        let resp = self.send_request(ctx, "textDocument/completion", Some(params)).await?;
        if resp.error.is_some() {
            return Ok(None);
        }
        Ok(resp.result)
    }

    pub async fn request_hover(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Option<Value>, LspClientError> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });
        let resp = self.send_request(ctx, "textDocument/hover", Some(params)).await?;
        Ok(resp.result)
    }

    pub async fn request_definition(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Option<Value>, LspClientError> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });
        let resp = self.send_request(ctx, "textDocument/definition", Some(params)).await?;
        Ok(resp.result)
    }

    pub async fn request_declaration(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Option<Value>, LspClientError> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });
        let resp = self.send_request(ctx, "textDocument/declaration", Some(params)).await?;
        Ok(resp.result)
    }

    pub async fn request_prepare_rename(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
    ) -> Result<Option<Value>, LspClientError> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });
        let resp = self.send_request(ctx, "textDocument/prepareRename", Some(params)).await?;
        Ok(resp.result)
    }

    pub async fn request_rename(
        &self,
        ctx: LspContext,
        uri: &str,
        position: lsp_types::Position,
        new_name: &str,
    ) -> Result<Option<hermito_protocol::lsp::TransactionalWorkspaceEdit>, LspClientError> {
        if new_name.is_empty() || new_name.len() > 256 || new_name.chars().any(|c| c.is_control()) {
            return Err(LspClientError::Protocol("invalid newName".into()));
        }
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
            "newName": new_name
        });
        let resp = self.send_request(ctx, "textDocument/rename", Some(params)).await?;
        resp.result
            .map(|value| {
                let edit = serde_json::from_value(value).map_err(JsonRpcFrameError::from)?;
                jsonrpc::transactional_workspace_edit_from_lsp(edit)
            })
            .transpose()
            .map_err(LspClientError::from)
    }

    /// Shutdown notification (best effort).
    pub async fn shutdown(&self, ctx: LspContext) -> Result<(), LspClientError> {
        let notif = LspJsonRpcNotification {
            method: "shutdown".to_string(),
            params: None,
        };
        let v1 = LspV1::JsonRpcNotification { context: ctx, payload: notif };
        self.send_transport(v1).await.ok();
        // exit notification
        let exit = LspJsonRpcNotification {
            method: "exit".to_string(),
            params: None,
        };
        let v1 = LspV1::JsonRpcNotification { context: self.context_from_ledger_for_shutdown(), payload: exit };
        self.send_transport(v1).await
    }

    fn context_from_ledger_for_shutdown(&self) -> LspContext {
        // Fallback context for shutdown; real callers pass proper one.
        LspContext {
            workspace_epoch: ProtocolWorkspaceEpoch(0),
            environment_epoch: hermito_protocol::request::EnvironmentEpoch(0),
            document_revision: None,
            sent_version: SentVersion(0),
            session_generation: SessionGeneration(0),
            execution_context: ExecutionContextV1::AuthorityRoot,
            authority_identity: self.authority_identity.clone(),
        }
    }

    /// Exposed pure stale-context filter API. Callers (supervisor/providers) pass received context + optional current ledger.
    /// Returns Ok(()) only if all ledger tag values match; otherwise explicit typed discard.
    pub fn filter_stale_context(
        received: &LspContext,
        sent: &LspContext,
        current_ledger: Option<&LspDocumentLedger>,
    ) -> Result<(), LspStaleDiscard> {
        macro_rules! discard {
            ($reason:expr, $class:literal) => {{
                tracing::debug!(
                    authority_identity = %received.authority_identity.0,
                    execution_context = ?received.execution_context,
                    session_generation = received.session_generation.0,
                    sent_version = received.sent_version.0,
                    document_revision = ?received.document_revision,
                    discard_reason = $class,
                    "discarded stale LSP context"
                );
                return Err($reason);
            }};
        }

        if received.authority_identity != sent.authority_identity {
            discard!(LspStaleDiscard::MismatchedAuthorityIdentity {
                expected: sent.authority_identity.0.clone(),
                actual: received.authority_identity.0.clone(),
            }, "authority_identity");
        }
        if received.execution_context != sent.execution_context {
            discard!(LspStaleDiscard::MismatchedExecutionContext, "execution_context");
        }
        if received.session_generation != sent.session_generation {
            discard!(LspStaleDiscard::MismatchedSessionGeneration {
                expected: sent.session_generation.0,
                actual: received.session_generation.0,
            }, "session_generation");
        }
        if received.sent_version != sent.sent_version {
            discard!(LspStaleDiscard::MismatchedSentVersion {
                expected: sent.sent_version.0 as i32,
                actual: received.sent_version.0 as i32,
            }, "sent_version");
        }
        if received.document_revision != sent.document_revision {
            discard!(LspStaleDiscard::MismatchedDocumentRevision {
                expected: sent.document_revision,
                actual: received.document_revision,
            }, "document_revision");
        }
        if received.workspace_epoch != sent.workspace_epoch {
            discard!(LspStaleDiscard::MismatchedWorkspaceEpoch {
                expected: sent.workspace_epoch,
                actual: received.workspace_epoch,
            }, "workspace_epoch");
        }
        if received.environment_epoch != sent.environment_epoch {
            discard!(LspStaleDiscard::MismatchedEnvironmentEpoch, "environment_epoch");
        }
        if let Some(ledger) = current_ledger {
            if ledger.authority_identity != sent.authority_identity {
                discard!(LspStaleDiscard::MismatchedAuthorityIdentity {
                    expected: ledger.authority_identity.0.clone(),
                    actual: sent.authority_identity.0.clone(),
                }, "ledger_authority_identity");
            }
            if ledger.context != sent.execution_context {
                discard!(LspStaleDiscard::MismatchedExecutionContext, "ledger_execution_context");
            }
            if ledger.session_generation != sent.session_generation.0 {
                discard!(LspStaleDiscard::MismatchedSessionGeneration {
                    expected: ledger.session_generation,
                    actual: sent.session_generation.0,
                }, "ledger_session_generation");
            }
            if ledger.sent_version != sent.sent_version.0 as i32 {
                discard!(LspStaleDiscard::MismatchedSentVersion {
                    expected: ledger.sent_version,
                    actual: sent.sent_version.0 as i32,
                }, "ledger_sent_version");
            }
            if sent.document_revision != Some(ProtocolDocumentRevision(ledger.revision.0)) {
                discard!(LspStaleDiscard::MismatchedDocumentRevision {
                    expected: Some(ProtocolDocumentRevision(ledger.revision.0)),
                    actual: sent.document_revision,
                }, "ledger_document_revision");
            }
            if ProtocolWorkspaceEpoch(ledger.workspace_epoch.0) != sent.workspace_epoch {
                discard!(LspStaleDiscard::MismatchedWorkspaceEpoch {
                    expected: ProtocolWorkspaceEpoch(ledger.workspace_epoch.0),
                    actual: sent.workspace_epoch,
                }, "ledger_workspace_epoch");
            }
            if ledger.environment_epoch != sent.environment_epoch {
                discard!(LspStaleDiscard::MismatchedEnvironmentEpoch, "ledger_environment_epoch");
            }
        }
        Ok(())
    }

    /// Pure conversion entrypoint (position/range/edit/diag) after DTO validation.
    /// Callers must have already passed through jsonrpc validation or protocol DTO path.
    pub fn convert_position(p: &hermito_protocol::lsp::LspPosition) -> Result<lsp_types::Position, LspClientError> {
        jsonrpc::validate_and_convert_position(p).map_err(LspClientError::Frame)
    }

    pub fn convert_range(r: &hermito_protocol::lsp::LspRange) -> Result<lsp_types::Range, LspClientError> {
        jsonrpc::validate_and_convert_range(r).map_err(LspClientError::Frame)
    }

    pub fn convert_text_edit(e: &hermito_protocol::lsp::LspTextEdit) -> Result<lsp_types::TextEdit, LspClientError> {
        jsonrpc::validate_and_convert_text_edit(e).map_err(LspClientError::Frame)
    }

    pub fn convert_diagnostics(d: &[hermito_protocol::lsp::LspDiagnostic]) -> Result<Vec<lsp_types::Diagnostic>, LspClientError> {
        jsonrpc::convert_validated_diagnostics(d).map_err(LspClientError::Frame)
    }
}

/// Result type for handle_incoming so caller can dispatch without mutation side effects.
#[derive(Debug)]
pub enum Incoming {
    Response { context: LspContext, payload: LspJsonRpcResponse },
    Notification { context: LspContext, payload: LspJsonRpcNotification },
    PublishDiagnostics {
        context: LspContext,
        uri: String,
        version: Option<i32>,
        diagnostics: Vec<hermito_protocol::lsp::LspDiagnostic>,
    },
    WorkspaceEdit {
        context: LspContext,
        request_id: LspRequestId,
        edit: hermito_protocol::lsp::TransactionalWorkspaceEdit,
    },
    Capabilities { context: LspContext, capabilities: Value },
    Other(LspV1),
    Discarded(LspStaleDiscard),
}

/// Helper to construct a Direct (Content-Length) transport.
/// The provided reader/writer must already be framed at LSP stdio level.
/// Context is attached on recv for LspV1 uniformity.
pub struct DirectTransport<R, W>
where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    reader: Arc<Mutex<R>>,
    writer: Arc<Mutex<W>>,
    /// Context used to re-attach on incoming frames for unified LspV1 handling.
    context: LspContext,
    pending_server_requests: Arc<Mutex<HashSet<LspRequestId>>>,
}

impl<R, W> DirectTransport<R, W>
where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    pub fn new(reader: R, writer: W, context: LspContext) -> Self {
        Self {
            reader: Arc::new(Mutex::new(reader)),
            writer: Arc::new(Mutex::new(writer)),
            context,
            pending_server_requests: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl<R, W> LspTransport for DirectTransport<R, W>
where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    fn send(&self, message: LspV1) -> Pin<Box<dyn Future<Output = Result<(), LspClientError>> + Send + '_>> {
        let wire = match &message {
            LspV1::JsonRpcRequest { payload, .. } => {
                json!({
                    "jsonrpc": "2.0",
                    "id": payload.id,
                    "method": payload.method,
                    "params": payload.params
                })
            }
            LspV1::JsonRpcNotification { payload, .. } => {
                json!({
                    "jsonrpc": "2.0",
                    "method": payload.method,
                    "params": payload.params
                })
            }
            LspV1::JsonRpcResponse { payload, .. } => {
                json!({
                    "jsonrpc": "2.0",
                    "id": payload.id,
                    "result": payload.result,
                    "error": payload.error
                })
            }
            LspV1::WorkspaceEditResult {
                request_id,
                applied,
                reason,
                ..
            } => {
                let pending = self.pending_server_requests.clone();
                let writer = self.writer.clone();
                let request_id = request_id.clone();
                let applied = *applied;
                let reason = reason.clone();
                return Box::pin(async move {
                    if !pending.lock().await.remove(&request_id) {
                        return Err(LspClientError::Protocol(
                            "workspace edit response has no pending request".into(),
                        ));
                    }
                    let result = if applied {
                        json!({ "applied": true })
                    } else {
                        json!({ "applied": false, "failureReason": reason })
                    };
                    let bytes = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": result,
                    }))
                    .map_err(|error| LspClientError::Frame(JsonRpcFrameError::from(error)))?;
                    let mut writer = writer.lock().await;
                    jsonrpc::write_lsp_frame(&mut *writer, &bytes)
                        .await
                        .map_err(LspClientError::Frame)
                });
            }
            _ => return Box::pin(async {
                Err(LspClientError::Unsupported {
                    method: "LspV1 message is not supported by direct transport".into(),
                })
            }),
        };
        let bytes_res = serde_json::to_vec(&wire).map_err(|e| LspClientError::Frame(JsonRpcFrameError::from(e)));
        let w_arc = self.writer.clone();
        Box::pin(async move {
            let bytes = bytes_res?;
            let mut w = w_arc.lock().await;
            jsonrpc::write_lsp_frame(&mut *w, &bytes).await.map_err(LspClientError::Frame)
        })
    }

    fn recv(&self) -> Pin<Box<dyn Future<Output = Result<LspV1, LspClientError>> + Send + '_>> {
        let r_arc = self.reader.clone();
        let ctx = self.context.clone();
        Box::pin(async move {
            let mut r = r_arc.lock().await;
            let bytes = jsonrpc::read_lsp_frame(&mut *r, MAX_CONTENT_LENGTH).await.map_err(LspClientError::Frame)?;
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| LspClientError::Frame(JsonRpcFrameError::from(e)))?;
            jsonrpc::validate_jsonrpc_envelope(&v).map_err(LspClientError::Frame)?;
            if let Some(method) = v.get("method").and_then(Value::as_str) {
                if method == "workspace/applyEdit" {
                    let request_id: LspRequestId = serde_json::from_value(
                        v.get("id").cloned().ok_or_else(|| {
                            LspClientError::Frame(JsonRpcFrameError::Validation {
                                reason: "workspace/applyEdit missing request id".into(),
                            })
                        })?,
                    )
                    .map_err(|_| LspClientError::Frame(JsonRpcFrameError::Validation {
                        reason: "workspace/applyEdit has invalid request id".into(),
                    }))?;
                    let params: lsp_types::ApplyWorkspaceEditParams = serde_json::from_value(
                        v.get("params").cloned().ok_or_else(|| {
                            LspClientError::Frame(JsonRpcFrameError::Validation {
                                reason: "workspace/applyEdit missing params".into(),
                            })
                        })?,
                    )
                    .map_err(|_| LspClientError::Frame(JsonRpcFrameError::Validation {
                        reason: "workspace/applyEdit has invalid params".into(),
                    }))?;
                    let edit = jsonrpc::transactional_workspace_edit_from_lsp(params.edit)
                        .map_err(LspClientError::Frame)?;
                    let mut pending = self.pending_server_requests.lock().await;
                    if pending.len() >= MAX_PENDING_SERVER_REQUESTS || !pending.insert(request_id.clone()) {
                        return Err(LspClientError::Backpressure);
                    }
                    return Ok(LspV1::WorkspaceEdit {
                        context: ctx,
                        request_id,
                        edit,
                    });
                }
                if method == "textDocument/publishDiagnostics" || method == "$/cancelRequest" {
                    let notif: LspJsonRpcNotification = serde_json::from_value(v.clone())
                        .map_err(|_| LspClientError::Frame(JsonRpcFrameError::Validation { reason: "bad notification".into() }))?;
                    return Ok(LspV1::JsonRpcNotification { context: ctx, payload: notif });
                }
                let req: LspJsonRpcRequest = serde_json::from_value(v)
                    .map_err(|_| LspClientError::Frame(JsonRpcFrameError::Validation { reason: "bad request".into() }))?;
                return Ok(LspV1::JsonRpcRequest { context: ctx, payload: req });
            }
            let resp: LspJsonRpcResponse = serde_json::from_value(v)
                .map_err(|_| LspClientError::Frame(JsonRpcFrameError::Validation { reason: "bad response".into() }))?;
            Ok(LspV1::JsonRpcResponse { context: ctx, payload: resp })
        })
    }
}

