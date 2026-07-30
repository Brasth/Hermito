use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::request::{DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelope<T> {
    pub request_id: Uuid,
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub execution_context: ExecutionContextV1,
    pub payload: Result<T, RemoteError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: RemoteErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorCode {
    InvalidRequest,
    PermissionDenied,
    NotFound,
    Cancelled,
    TimedOut,
    OutputLimit,
    StaleEpoch,
    Internal,
}
