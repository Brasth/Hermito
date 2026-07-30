use serde::{Deserialize, Serialize};

use crate::request::{
    CommandSpec, DocumentRevision, EnvironmentEpoch, ExecutionContextV1, RequestEnvelope,
    WorkspaceEpoch,
};

pub type StreamId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.rows == 0 || self.cols == 0 {
            Err("PTY rows and columns must be non-zero")
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PtyStreamContext {
    pub request_id: uuid::Uuid,
    pub stream_id: StreamId,
    pub generation: u64,
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub execution_context: ExecutionContextV1,
}

impl PtyStreamContext {
    pub fn from_spawn(request: &RequestEnvelope<PtySpawn>) -> Self {
        Self {
            request_id: request.request_id,
            stream_id: request.payload.stream_id,
            generation: request.payload.generation,
            workspace_epoch: request.workspace_epoch,
            environment_epoch: request.environment_epoch,
            document_revision: request.document_revision,
            execution_context: request.execution_context.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PtyMessage {
    Spawn(RequestEnvelope<PtySpawn>),
    Input {
        context: PtyStreamContext,
        #[serde(with = "crate::wire_bytes")]
        bytes: Vec<u8>,
    },
    Resize {
        context: PtyStreamContext,
        size: PtySize,
    },
    Cancel {
        context: PtyStreamContext,
    },
    Started {
        context: PtyStreamContext,
        process_id: Option<u32>,
    },
    Output {
        context: PtyStreamContext,
        #[serde(with = "crate::wire_bytes")]
        bytes: Vec<u8>,
    },
    Exited {
        context: PtyStreamContext,
        exit_code: Option<i32>,
        truncated: bool,
    },
    Lost {
        context: PtyStreamContext,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySpawn {
    pub stream_id: StreamId,
    pub generation: u64,
    pub command: CommandSpec,
    pub size: PtySize,
}
