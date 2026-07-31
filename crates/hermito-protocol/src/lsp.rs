use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::request::{
    DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SentVersion(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionGeneration(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthorityIdentity(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LspContext {
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub sent_version: SentVersion,
    pub session_generation: SessionGeneration,
    pub execution_context: ExecutionContextV1,
    pub authority_identity: AuthorityIdentity,
}

impl LspContext {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if self.authority_identity.0.is_empty() {
            return Err(LspProtocolError::EmptyAuthorityIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerConfig {
    pub language_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
}

impl LspServerConfig {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if self.language_id.is_empty() {
            return Err(LspProtocolError::EmptyLanguageId);
        }
        if self.program.is_empty() {
            return Err(LspProtocolError::InvalidField {
                field: "program".to_string(),
                reason: "must not be empty".to_string(),
            });
        }
        if self.cwd.is_empty() {
            return Err(LspProtocolError::InvalidField {
                field: "cwd".to_string(),
                reason: "must not be empty".to_string(),
            });
        }
        if self.program.as_bytes().contains(&0)
            || self.cwd.as_bytes().contains(&0)
            || self.args.iter().any(|a| a.as_bytes().contains(&0))
            || self.language_id.as_bytes().contains(&0)
        {
            return Err(LspProtocolError::InvalidField {
                field: "server_config".to_string(),
                reason: "contains NUL".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LspRequestId {
    Number(i64),
    String(String),
}

impl LspRequestId {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if let LspRequestId::String(s) = self {
            if s.is_empty() {
                return Err(LspProtocolError::EmptyRequestId);
            }
            if s.len() > 256 {
                return Err(LspProtocolError::ExceedsBound {
                    field: "id".to_string(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspJsonRpcRequest {
    pub id: LspRequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl LspJsonRpcRequest {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if self.method.is_empty() {
            return Err(LspProtocolError::EmptyMethod);
        }
        if self.method.len() > 256 {
            return Err(LspProtocolError::ExceedsBound {
                field: "method".to_string(),
            });
        }
        self.id.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspJsonRpcResponse {
    pub id: LspRequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LspJsonRpcError>,
}

impl LspJsonRpcResponse {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        self.id.validate()?;
        if self.result.is_none() && self.error.is_none() {
            return Err(LspProtocolError::InvalidField {
                field: "result_or_error".to_string(),
                reason: "one must be present".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspJsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspJsonRpcNotification {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl LspJsonRpcNotification {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if self.method.is_empty() {
            return Err(LspProtocolError::EmptyMethod);
        }
        if self.method.len() > 256 {
            return Err(LspProtocolError::ExceedsBound {
                field: "method".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<serde_json::Value>,
    pub source: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<i32>>,
}

impl LspDiagnostic {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        if self.message.is_empty() {
            return Err(LspProtocolError::InvalidField {
                field: "message".to_string(),
                reason: "diagnostic message must not be empty".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionalDocumentEdit {
    TextDocument {
        uri: String,
        expected_revision: Option<DocumentRevision>,
        expected_lsp_version: Option<SentVersion>,
        edits: Vec<LspTextEdit>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionalWorkspaceEdit {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_changes: Vec<TransactionalDocumentEdit>,
}

impl TransactionalWorkspaceEdit {
    pub fn validate(&self) -> Result<(), LspProtocolError> {
        for ch in &self.document_changes {
            if let TransactionalDocumentEdit::TextDocument { uri, edits, .. } = ch {
                if uri.is_empty() {
                    return Err(LspProtocolError::EmptyDocumentUri);
                }
                for edit in edits {
                    if edit.new_text.len() > 4 * 1024 * 1024 {
                        return Err(LspProtocolError::ExceedsBound {
                            field: "edit_text".to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl TryFrom<lsp_types::WorkspaceEdit> for TransactionalWorkspaceEdit {
    type Error = LspProtocolError;

    fn try_from(edit: lsp_types::WorkspaceEdit) -> Result<Self, Self::Error> {
        fn text_edit(edit: lsp_types::TextEdit) -> LspTextEdit {
            LspTextEdit {
                range: LspRange {
                    start: LspPosition {
                        line: edit.range.start.line,
                        character: edit.range.start.character,
                    },
                    end: LspPosition {
                        line: edit.range.end.line,
                        character: edit.range.end.character,
                    },
                },
                new_text: edit.new_text,
            }
        }

        fn document_edit(
            edit: lsp_types::TextDocumentEdit,
        ) -> Result<TransactionalDocumentEdit, LspProtocolError> {
            let expected_lsp_version = edit
                .text_document
                .version
                .map(u64::try_from)
                .transpose()
                .map_err(|_| LspProtocolError::InvalidField {
                    field: "textDocument.version".to_string(),
                    reason: "must not be negative".to_string(),
                })?
                .map(SentVersion);
            let edits = edit
                .edits
                .into_iter()
                .map(|edit| match edit {
                    lsp_types::OneOf::Left(edit) => text_edit(edit),
                    lsp_types::OneOf::Right(edit) => text_edit(edit.text_edit),
                })
                .collect();
            Ok(TransactionalDocumentEdit::TextDocument {
                uri: edit.text_document.uri.to_string(),
                expected_revision: None,
                expected_lsp_version,
                edits,
            })
        }

        let document_changes = match edit.document_changes {
            Some(lsp_types::DocumentChanges::Edits(edits)) => edits
                .into_iter()
                .map(document_edit)
                .collect::<Result<Vec<_>, _>>()?,
            Some(lsp_types::DocumentChanges::Operations(changes)) => changes
                .into_iter()
                .map(|change| match change {
                    lsp_types::DocumentChangeOperation::Edit(edit) => document_edit(edit),
                    lsp_types::DocumentChangeOperation::Op(_) => Err(
                        LspProtocolError::InvalidField {
                            field: "documentChanges".to_string(),
                            reason: "workspace resource operations are unsupported".to_string(),
                        },
                    ),
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => edit
                .changes
                .unwrap_or_default()
                .into_iter()
                .map(|(uri, edits)| TransactionalDocumentEdit::TextDocument {
                    uri: uri.to_string(),
                    expected_revision: None,
                    expected_lsp_version: None,
                    edits: edits.into_iter().map(text_edit).collect(),
                })
                .collect(),
        };
        let transactional = Self { document_changes };
        transactional.validate()?;
        Ok(transactional)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LspProtocolError {
    #[error("empty authority identity")]
    EmptyAuthorityIdentity,
    #[error("empty language id")]
    EmptyLanguageId,
    #[error("empty method")]
    EmptyMethod,
    #[error("empty request id")]
    EmptyRequestId,
    #[error("empty document uri")]
    EmptyDocumentUri,
    #[error("invalid field {field}: {reason}")]
    InvalidField { field: String, reason: String },
    #[error("value exceeds protocol bound for {field}")]
    ExceedsBound { field: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LspV1 {
    Start {
        context: LspContext,
        config: LspServerConfig,
    },
    Started {
        context: LspContext,
        capabilities: serde_json::Value,
    },
    Shutdown {
        context: LspContext,
    },
    Exited {
        context: LspContext,
        exit_code: Option<i32>,
    },
    JsonRpcRequest {
        context: LspContext,
        payload: LspJsonRpcRequest,
    },
    JsonRpcResponse {
        context: LspContext,
        payload: LspJsonRpcResponse,
    },
    JsonRpcNotification {
        context: LspContext,
        payload: LspJsonRpcNotification,
    },
    PublishDiagnostics {
        context: LspContext,
        uri: String,
        version: Option<i32>,
        diagnostics: Vec<LspDiagnostic>,
    },
    WorkspaceEdit {
        context: LspContext,
        request_id: LspRequestId,
        edit: TransactionalWorkspaceEdit,
    },
    WorkspaceEditResult {
        context: LspContext,
        request_id: LspRequestId,
        applied: bool,
        reason: Option<String>,
    },
}

impl LspV1 {
    pub fn context(&self) -> &LspContext {
        match self {
            LspV1::Start { context, .. } => context,
            LspV1::Started { context, .. } => context,
            LspV1::Shutdown { context, .. } => context,
            LspV1::Exited { context, .. } => context,
            LspV1::JsonRpcRequest { context, .. } => context,
            LspV1::JsonRpcResponse { context, .. } => context,
            LspV1::JsonRpcNotification { context, .. } => context,
            LspV1::PublishDiagnostics { context, .. } => context,
            LspV1::WorkspaceEdit { context, .. } => context,
            LspV1::WorkspaceEditResult { context, .. } => context,
        }
    }

    pub fn validate(&self) -> Result<(), LspProtocolError> {
        let ctx = self.context();
        ctx.validate()?;
        match self {
            LspV1::Start { config, .. } => config.validate(),
            LspV1::JsonRpcRequest { payload, .. } => payload.validate(),
            LspV1::JsonRpcResponse { payload, .. } => payload.validate(),
            LspV1::JsonRpcNotification { payload, .. } => payload.validate(),
            LspV1::PublishDiagnostics {
                uri, diagnostics, ..
            } => {
                if uri.is_empty() {
                    return Err(LspProtocolError::EmptyDocumentUri);
                }
                for d in diagnostics {
                    d.validate()?;
                }
                Ok(())
            }
            LspV1::WorkspaceEdit { edit, .. } => edit.validate(),
            _ => Ok(()),
        }
    }
}
