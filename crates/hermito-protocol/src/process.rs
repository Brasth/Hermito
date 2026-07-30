use serde::{Deserialize, Serialize};

use crate::{
    request::{CommandSpec, RequestEnvelope},
    response::ResponseEnvelope,
};

pub const MAX_WIRE_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProcessMessage {
    Exec(RequestEnvelope<ExecRequest>),
    Cancel { request_id: uuid::Uuid },
    Result(ResponseEnvelope<ExecOutput>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: CommandSpec,
    pub timeout_ms: u64,
    pub stdout_limit: u64,
    pub stderr_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutput {
    pub exit_code: Option<i32>,
    #[serde(with = "crate::wire_bytes")]
    pub stdout: Vec<u8>,
    #[serde(with = "crate::wire_bytes")]
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}
