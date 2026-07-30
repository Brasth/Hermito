use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceEpoch(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentEpoch(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentRevision(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionContextV1 {
    AuthorityRoot,
    DevContainer {
        container_id: String,
        environment_epoch: EnvironmentEpoch,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope<T> {
    pub request_id: Uuid,
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub document_revision: Option<DocumentRevision>,
    pub execution_context: ExecutionContextV1,
    pub payload: T,
}

impl<T> RequestEnvelope<T> {
    pub fn authority_root(
        payload: T,
        workspace_epoch: WorkspaceEpoch,
        environment_epoch: EnvironmentEpoch,
    ) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            workspace_epoch,
            environment_epoch,
            document_revision: None,
            execution_context: ExecutionContextV1::AuthorityRoot,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.program.is_empty() {
            return Err("program must not be empty");
        }
        if self.cwd.is_empty() {
            return Err("cwd must not be empty");
        }
        if self.program.as_bytes().contains(&0)
            || self.cwd.as_bytes().contains(&0)
            || self.args.iter().any(|arg| arg.as_bytes().contains(&0))
            || self.env.iter().any(|(key, value)| {
                key.is_empty()
                    || key.contains('=')
                    || key.as_bytes().contains(&0)
                    || value.as_bytes().contains(&0)
            })
        {
            return Err("command contains an invalid NUL byte or environment key");
        }
        Ok(())
    }
}
