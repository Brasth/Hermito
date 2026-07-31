//! Bounded Content-Length framed JSON-RPC ingress/egress with strict DTO validation.
//! All parsing, length caps, and DTO checks complete before any lsp-types conversion.
//! External payloads remain as inert JSON or protocol DTOs until explicitly accepted via pure fns.

use std::io;

use hermito_protocol::lsp::{
    LspDiagnostic, LspPosition, LspRange, LspRequestId, LspTextEdit, TransactionalDocumentEdit,
    TransactionalWorkspaceEdit,
};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Explicit hard caps. Checked before any allocation of payload buffers or collections.
pub const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024;
pub const MAX_AGGREGATE_PENDING: usize = 32 * 1024 * 1024;
pub const MAX_STRING: usize = 4 * 1024 * 1024;
pub const MAX_ARRAY: usize = 64 * 1024;
pub const MAX_DIAGNOSTICS: usize = 1024;
pub const MAX_TEXT_EDITS: usize = 4096;
pub const MAX_VERSION: i64 = i32::MAX as i64;

/// Errors for framing and ingress validation. Never carries raw server bytes into app state.
#[derive(Debug, thiserror::Error)]
pub enum JsonRpcFrameError {
    #[error("content-length exceeds hard cap before allocation")]
    ContentLengthExceeded { requested: usize, cap: usize },
    #[error("aggregate pending bytes cap exceeded")]
    AggregateExceeded { requested: usize, cap: usize },
    #[error("invalid or missing Content-Length header")]
    InvalidHeader,
    #[error("header too large or malformed")]
    HeaderTooLarge,
    #[error("I/O error during framed read/write")]
    Io(#[from] io::Error),
    #[error("JSON parse error before DTO validation")]
    Json(#[from] serde_json::Error),
    #[error("DTO validation rejected: {reason}")]
    Validation { reason: String },
    #[error("numeric overflow or fractional value in position/version/id")]
    NumericViolation { field: String },
    #[error("string or array exceeds explicit limit")]
    SizeViolation { field: String, len: usize },
    #[error("control character or NUL in value field")]
    ControlBearingValue { field: String },
    #[error("invalid request id (empty, oversized, or malformed)")]
    InvalidId,
}

/// Read exactly one Content-Length framed LSP message.
/// Hard cap checked on declared length BEFORE allocating the payload buffer.
pub async fn read_lsp_frame<R>(reader: &mut R, hard_cap: usize) -> Result<Vec<u8>, JsonRpcFrameError>
where
    R: AsyncRead + Unpin,
{
    let cap = hard_cap.min(MAX_CONTENT_LENGTH);
    let declared = read_content_length(reader, cap).await?;
    if declared > cap {
        return Err(JsonRpcFrameError::ContentLengthExceeded {
            requested: declared,
            cap,
        });
    }
    // Allocation only after cap check.
    let mut buf = vec![0u8; declared];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a payload as Content-Length framed LSP message.
pub async fn write_lsp_frame<W>(writer: &mut W, payload: &[u8]) -> Result<(), JsonRpcFrameError>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_CONTENT_LENGTH {
        return Err(JsonRpcFrameError::ContentLengthExceeded {
            requested: payload.len(),
            cap: MAX_CONTENT_LENGTH,
        });
    }
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_content_length<R>(reader: &mut R, cap: usize) -> Result<usize, JsonRpcFrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = Vec::with_capacity(256);
    let mut buf = [0u8; 1];
    loop {
        reader.read_exact(&mut buf).await?;
        header.push(buf[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 4096 {
            return Err(JsonRpcFrameError::HeaderTooLarge);
        }
    }
    let text = std::str::from_utf8(&header).map_err(|_| JsonRpcFrameError::InvalidHeader)?;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            let n = rest.trim().parse::<usize>().map_err(|_| JsonRpcFrameError::InvalidHeader)?;
            if n > cap {
                return Err(JsonRpcFrameError::ContentLengthExceeded { requested: n, cap });
            }
            return Ok(n);
        }
    }
    Err(JsonRpcFrameError::InvalidHeader)
}

/// Validate raw JSON-RPC envelope structure (id/method presence) with limits. Does not allocate oversized structures.
pub fn validate_jsonrpc_envelope(v: &Value) -> Result<(), JsonRpcFrameError> {
    if let Some(idv) = v.get("id") {
        validate_request_id_json(idv)?;
    }
    if let Some(m) = v.get("method").and_then(Value::as_str) {
        if m.is_empty() || m.len() > 256 {
            return Err(JsonRpcFrameError::Validation {
                reason: "method empty or exceeds bound".into(),
            });
        }
        if m.chars().any(|c| c.is_control() && c != '\t') {
            return Err(JsonRpcFrameError::ControlBearingValue {
                field: "method".into(),
            });
        }
    }
    Ok(())
}

fn validate_request_id_json(v: &Value) -> Result<(), JsonRpcFrameError> {
    match v {
        Value::Number(n) => {
            if !n.is_i64() {
                return Err(JsonRpcFrameError::NumericViolation {
                    field: "id".into(),
                });
            }
            let i = n.as_i64().unwrap();
            if i < 0 || i > (i64::MAX) {
                return Err(JsonRpcFrameError::NumericViolation {
                    field: "id".into(),
                });
            }
            Ok(())
        }
        Value::String(s) => {
            if s.is_empty() {
                return Err(JsonRpcFrameError::InvalidId);
            }
            if s.len() > 256 {
                return Err(JsonRpcFrameError::SizeViolation {
                    field: "id".into(),
                    len: s.len(),
                });
            }
            if s.chars().any(|c| c.is_control()) {
                return Err(JsonRpcFrameError::ControlBearingValue { field: "id".into() });
            }
            Ok(())
        }
        _ => Err(JsonRpcFrameError::InvalidId),
    }
}

/// Parse bytes to Value then validate basic envelope. Used for ingress before DTO or lsp-types.
pub fn parse_and_validate_frame(bytes: &[u8]) -> Result<Value, JsonRpcFrameError> {
    if bytes.len() > MAX_CONTENT_LENGTH {
        return Err(JsonRpcFrameError::ContentLengthExceeded {
            requested: bytes.len(),
            cap: MAX_CONTENT_LENGTH,
        });
    }
    let v: Value = serde_json::from_slice(bytes)?;
    validate_jsonrpc_envelope(&v)?;
    Ok(v)
}

/// Strict position validation + conversion. Rejects negative, fractional, overflow before lsp-types.
/// Positions arrive via protocol DTOs (u32) or raw json for direct; we normalize through DTO.
pub fn validate_and_convert_position(p: &LspPosition) -> Result<lsp_types::Position, JsonRpcFrameError> {
    // Protocol LspPosition already uses u32 (rejects neg/overflow at serde time for direct DTO path).
    // Additional explicit checks for the contract.
    if p.line > u32::MAX || p.character > u32::MAX {
        return Err(JsonRpcFrameError::NumericViolation {
            field: "position".into(),
        });
    }
    Ok(lsp_types::Position {
        line: p.line,
        character: p.character,
    })
}

pub fn validate_and_convert_range(r: &LspRange) -> Result<lsp_types::Range, JsonRpcFrameError> {
    Ok(lsp_types::Range {
        start: validate_and_convert_position(&r.start)?,
        end: validate_and_convert_position(&r.end)?,
    })
}

pub fn validate_and_convert_text_edit(e: &LspTextEdit) -> Result<lsp_types::TextEdit, JsonRpcFrameError> {
    if e.new_text.len() > MAX_STRING {
        return Err(JsonRpcFrameError::SizeViolation {
            field: "new_text".into(),
            len: e.new_text.len(),
        });
    }
    if e.new_text.chars().any(|c| c == '\0') {
        return Err(JsonRpcFrameError::ControlBearingValue {
            field: "new_text".into(),
        });
    }
    Ok(lsp_types::TextEdit {
        range: validate_and_convert_range(&e.range)?,
        new_text: e.new_text.clone(),
    })
}

pub fn validate_and_convert_diagnostic(d: &LspDiagnostic) -> Result<lsp_types::Diagnostic, JsonRpcFrameError> {
    if d.message.is_empty() {
        return Err(JsonRpcFrameError::Validation {
            reason: "empty diagnostic message".into(),
        });
    }
    if d.message.len() > MAX_STRING {
        return Err(JsonRpcFrameError::SizeViolation {
            field: "diagnostic.message".into(),
            len: d.message.len(),
        });
    }
    if d.message.chars().any(|c| c == '\0') {
        return Err(JsonRpcFrameError::ControlBearingValue {
            field: "diagnostic.message".into(),
        });
    }
    if let Some(tags) = &d.tags {
        if tags.len() > 16 {
            return Err(JsonRpcFrameError::SizeViolation {
                field: "diagnostic.tags".into(),
                len: tags.len(),
            });
        }
    }
    let severity = match d.severity {
        None => None,
        Some(1) => Some(lsp_types::DiagnosticSeverity::ERROR),
        Some(2) => Some(lsp_types::DiagnosticSeverity::WARNING),
        Some(3) => Some(lsp_types::DiagnosticSeverity::INFORMATION),
        Some(4) => Some(lsp_types::DiagnosticSeverity::HINT),
        Some(value) => {
            return Err(JsonRpcFrameError::Validation {
                reason: format!("unsupported diagnostic severity {value}"),
            });
        }
    };
    let code = match d.code.as_ref() {
        None => None,
        Some(Value::String(value)) => Some(lsp_types::NumberOrString::String(value.clone())),
        Some(Value::Number(value)) => {
            let value = value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| JsonRpcFrameError::NumericViolation {
                    field: "diagnostic.code".into(),
                })?;
            Some(lsp_types::NumberOrString::Number(value))
        }
        Some(_) => {
            return Err(JsonRpcFrameError::Validation {
                reason: "diagnostic.code must be a string or i32".into(),
            });
        }
    };
    let tags = d.tags.as_ref().map(|tags| {
        tags.iter()
            .map(|tag| match tag {
                1 => Ok(lsp_types::DiagnosticTag::UNNECESSARY),
                2 => Ok(lsp_types::DiagnosticTag::DEPRECATED),
                value => Err(JsonRpcFrameError::Validation {
                    reason: format!("unsupported diagnostic tag {value}"),
                }),
            })
            .collect::<Result<Vec<_>, _>>()
    }).transpose()?;
    Ok(lsp_types::Diagnostic {
        range: validate_and_convert_range(&d.range)?,
        severity,
        code,
        code_description: None,
        source: d.source.clone(),
        message: d.message.clone(),
        related_information: None,
        tags,
        data: None,
    })
}

/// Validate a collection of diagnostics under aggregate and per-item limits.
pub fn validate_diagnostics(diagnostics: &[LspDiagnostic]) -> Result<(), JsonRpcFrameError> {
    if diagnostics.len() > MAX_DIAGNOSTICS {
        return Err(JsonRpcFrameError::SizeViolation {
            field: "diagnostics".into(),
            len: diagnostics.len(),
        });
    }
    for d in diagnostics {
        d.validate().map_err(|e| JsonRpcFrameError::Validation {
            reason: e.to_string(),
        })?;
        // re-apply size after protocol validate
        if d.message.len() > MAX_STRING {
            return Err(JsonRpcFrameError::SizeViolation {
                field: "diagnostic.message".into(),
                len: d.message.len(),
            });
        }
    }
    Ok(())
}

/// Validate workspace edit under protocol + extra size caps.
pub fn validate_workspace_edit(edit: &TransactionalWorkspaceEdit) -> Result<(), JsonRpcFrameError> {
    edit.validate().map_err(|e| JsonRpcFrameError::Validation {
        reason: e.to_string(),
    })?;
    for ch in &edit.document_changes {
        if let TransactionalDocumentEdit::TextDocument { edits, .. } = ch {
            if edits.len() > MAX_TEXT_EDITS {
                return Err(JsonRpcFrameError::SizeViolation {
                    field: "textDocument.edits".into(),
                    len: edits.len(),
                });
            }
            for e in edits {
                if e.new_text.len() > MAX_STRING {
                    return Err(JsonRpcFrameError::SizeViolation {
                        field: "edit.new_text".into(),
                        len: e.new_text.len(),
                    });
                }
            }
        }
    }
    Ok(())
}
/// Convert a typed LSP WorkspaceEdit into the protocol's transactional DTO.
/// Resource operations are deliberately rejected because Authority transactions
/// only accept text replacements with host-buffer preconditions.
pub fn transactional_workspace_edit_from_lsp(
    edit: lsp_types::WorkspaceEdit,
) -> Result<TransactionalWorkspaceEdit, JsonRpcFrameError> {
    let transactional = TransactionalWorkspaceEdit::try_from(edit).map_err(|error| {
        JsonRpcFrameError::Validation {
            reason: error.to_string(),
        }
    })?;
    validate_workspace_edit(&transactional)?;
    Ok(transactional)
}

/// Convert validated protocol diagnostic vec to lsp_types after all ingress checks.
pub fn convert_validated_diagnostics(
    diags: &[LspDiagnostic],
) -> Result<Vec<lsp_types::Diagnostic>, JsonRpcFrameError> {
    validate_diagnostics(diags)?;
    diags.iter().map(validate_and_convert_diagnostic).collect()
}

/// Versioned or versionless diagnostic policy hook (explicit, no silent refresh).
/// Callers supply current ledger sent_version for comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionlessPolicy {
    /// Explicit safe discard for versionless push (recommended default for untrusted).
    SafeDiscard,
    /// Accept but only for refresh (caller decides to replace full set).
    AcceptForRefresh,
}

pub fn should_accept_diagnostics(
    ledger: Option<&crate::lsp::LspDocumentLedger>,
    incoming_version: Option<i32>,
    policy: VersionlessPolicy,
) -> bool {
    match (ledger, incoming_version) {
        (Some(l), Some(v)) => {
            // sent_version in ledger is i32; compare directly. Mismatch is stale.
            (v as i64) >= (l.sent_version as i64)
        }
        (Some(_), None) => match policy {
            VersionlessPolicy::SafeDiscard => false,
            VersionlessPolicy::AcceptForRefresh => true,
        },
        (None, _) => true, // no ledger yet: first push accepted (will be reconciled by supervisor)
    }
}

/// Build a validated LspRequestId from raw numeric or string. Rejects invalids per contract.
pub fn make_request_id(id: i64) -> Result<LspRequestId, JsonRpcFrameError> {
    if id < 0 {
        return Err(JsonRpcFrameError::NumericViolation {
            field: "id".into(),
        });
    }
    Ok(LspRequestId::Number(id))
}

pub fn make_string_request_id(s: String) -> Result<LspRequestId, JsonRpcFrameError> {
    if s.is_empty() || s.len() > 256 || s.chars().any(|c| c.is_control()) {
        return Err(JsonRpcFrameError::InvalidId);
    }
    Ok(LspRequestId::String(s))
}

/// Pure conversion helpers exposed for later integration (completion/hover etc use Value until accepted).
pub fn position_from_json(v: &Value) -> Result<LspPosition, JsonRpcFrameError> {
    let line = v
        .get("line")
        .and_then(Value::as_u64)
        .ok_or_else(|| JsonRpcFrameError::NumericViolation {
            field: "line".into(),
        })?;
    let character = v
        .get("character")
        .and_then(Value::as_u64)
        .ok_or_else(|| JsonRpcFrameError::NumericViolation {
            field: "character".into(),
        })?;
    if line > u32::MAX as u64 || character > u32::MAX as u64 {
        return Err(JsonRpcFrameError::NumericViolation {
            field: "position".into(),
        });
    }
    // fractional already rejected by as_u64 returning None for 1.5
    Ok(LspPosition {
        line: line as u32,
        character: character as u32,
    })
}

pub fn range_from_json(v: &Value) -> Result<LspRange, JsonRpcFrameError> {
    let start = position_from_json(v.get("start").ok_or_else(|| JsonRpcFrameError::Validation {
        reason: "missing start".into(),
    })?)?;
    let end = position_from_json(v.get("end").ok_or_else(|| JsonRpcFrameError::Validation {
        reason: "missing end".into(),
    })?)?;
    Ok(LspRange { start, end })
}
