//! Normalized, ledger-filtered diagnostic routing.
//!
//! This module has no App dependency and does not mutate editor state. A
//! caller must explicitly hand an accepted `DiagnosticEvent` to its app event
//! path after the stale ledger filter succeeds.

use hermito_protocol::lsp::LspContext;

use crate::lsp::{
    LspClient, LspClientError, LspDocumentLedger, LspStaleDiscard, LspTransport, VersionlessPolicy,
};

/// Severity reduced to the display-facing LSP 3.17 set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizedDiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A validated diagnostic with only editor-facing fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedDiagnostic {
    pub range: lsp_types::Range,
    pub severity: Option<NormalizedDiagnosticSeverity>,
    pub message: String,
    pub source: Option<String>,
}

/// Inert diagnostic data eligible for explicit application by the App layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticEvent {
    pub context: LspContext,
    pub uri: String,
    pub diagnostics: Vec<NormalizedDiagnostic>,
}

/// The only interface this module needs from application event routing.
/// Implementations decide how accepted events update Problems/Services state.
pub trait DiagnosticEventSink {
    fn record_lsp_event(&mut self, event: DiagnosticEvent);
}

impl DiagnosticEvent {
    /// Make the state transition explicit; conversion and filtering alone are
    /// inert and cannot change application state.
    pub fn apply_to<S: DiagnosticEventSink>(self, sink: &mut S) {
        sink.record_lsp_event(self);
    }
}

/// A safely discarded batch. `request_refresh` tells the event loop that a
/// versionless push may be reconciled by re-syncing; it never permits direct
/// diagnostic application.
#[derive(Debug)]
pub enum DiagnosticDiscard {
    StaleContext(LspStaleDiscard),
    EnvironmentEpochMismatch,
    MissingDocumentRevision,
    VersionMismatch { expected: i32, actual: i32 },
    Versionless { request_refresh: bool },
}

/// The explicit result of ledger-aware diagnostic routing.
#[derive(Debug)]
pub enum DiagnosticRoute {
    Apply(DiagnosticEvent),
    Discard(DiagnosticDiscard),
}

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticError {
    #[error(transparent)]
    Client(#[from] LspClientError),
}

/// Filter an already validated publishDiagnostics DTO against the exact sent
/// request context and the buffer-owned current ledger, then normalize it.
///
/// Versionless diagnostics are never surfaced directly. Under
/// `AcceptForRefresh`, callers receive a discard with `request_refresh=true`;
/// under `SafeDiscard`, they receive a plain discard. A versioned batch must
/// match the ledger's current sent version exactly, so late and speculative
/// diagnostics are both inert.
pub fn route_publish_diagnostics<T: LspTransport>(
    _client: &LspClient<T>,
    received: LspContext,
    sent: &LspContext,
    ledger: &LspDocumentLedger,
    uri: String,
    version: Option<i32>,
    diagnostics: &[hermito_protocol::lsp::LspDiagnostic],
    versionless_policy: VersionlessPolicy,
) -> Result<DiagnosticRoute, DiagnosticError> {
    if received.environment_epoch != ledger.environment_epoch {
        return Ok(DiagnosticRoute::Discard(
            DiagnosticDiscard::EnvironmentEpochMismatch,
        ));
    }
    if received.document_revision.is_none() {
        return Ok(DiagnosticRoute::Discard(
            DiagnosticDiscard::MissingDocumentRevision,
        ));
    }
    if let Err(discard) = LspClient::<T>::filter_stale_context(&received, sent, Some(ledger)) {
        return Ok(DiagnosticRoute::Discard(DiagnosticDiscard::StaleContext(
            discard,
        )));
    }
    // `filter_stale_context` deliberately permits absent revisions for generic
    // notifications; diagnostics require an exact revision tag.
    if received.document_revision != sent.document_revision {
        return Ok(DiagnosticRoute::Discard(DiagnosticDiscard::StaleContext(
            LspStaleDiscard::MismatchedDocumentRevision {
                expected: sent.document_revision,
                actual: received.document_revision,
            },
        )));
    }

    let version = match version {
        Some(version) => version,
        None => {
            return Ok(DiagnosticRoute::Discard(DiagnosticDiscard::Versionless {
                request_refresh: matches!(versionless_policy, VersionlessPolicy::AcceptForRefresh),
            }));
        }
    };
    if version != ledger.sent_version {
        return Ok(DiagnosticRoute::Discard(DiagnosticDiscard::VersionMismatch {
            expected: ledger.sent_version,
            actual: version,
        }));
    }

    let diagnostics = LspClient::<T>::convert_diagnostics(diagnostics)?
        .into_iter()
        .map(|diagnostic| NormalizedDiagnostic {
            range: diagnostic.range,
            severity: diagnostic.severity.map(normalize_severity),
            message: diagnostic.message,
            source: diagnostic.source,
        })
        .collect();
    Ok(DiagnosticRoute::Apply(DiagnosticEvent {
        context: received,
        uri,
        diagnostics,
    }))
}

fn normalize_severity(severity: lsp_types::DiagnosticSeverity) -> NormalizedDiagnosticSeverity {
    match severity {
        lsp_types::DiagnosticSeverity::ERROR => NormalizedDiagnosticSeverity::Error,
        lsp_types::DiagnosticSeverity::WARNING => NormalizedDiagnosticSeverity::Warning,
        lsp_types::DiagnosticSeverity::INFORMATION => NormalizedDiagnosticSeverity::Information,
        lsp_types::DiagnosticSeverity::HINT => NormalizedDiagnosticSeverity::Hint,
        _ => NormalizedDiagnosticSeverity::Information,
    }
}
