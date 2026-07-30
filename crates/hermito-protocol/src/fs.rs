use serde::{Deserialize, Serialize};

use crate::{request::RequestEnvelope, response::ResponseEnvelope};
pub const MAX_WIRE_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FsMessage {
    Read(RequestEnvelope<ReadFile>),
    ReadResult(ResponseEnvelope<FileContent>),
    Write(RequestEnvelope<WriteFile>),
    WriteResult(ResponseEnvelope<WriteResult>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFile {
    pub path: String,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    #[serde(with = "crate::wire_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFile {
    pub path: String,
    #[serde(with = "crate::wire_bytes")]
    pub bytes: Vec<u8>,
    pub create: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub bytes_written: u64,
}
