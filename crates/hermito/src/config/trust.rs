use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TrustRecords {
    version: u32,
    records: BTreeMap<String, bool>,
}

#[derive(Clone, Debug)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn is_granted(
        &self,
        workspace_root: &Path,
        authority_id: &str,
    ) -> Result<bool, TrustStoreError> {
        Ok(self
            .load()?
            .records
            .get(&record_key(workspace_root, authority_id))
            .copied()
            .unwrap_or(false))
    }

    pub fn set_granted(
        &self,
        workspace_root: &Path,
        authority_id: &str,
        granted: bool,
    ) -> Result<(), TrustStoreError> {
        let mut records = self.load()?;
        records.version = 1;
        let key = record_key(workspace_root, authority_id);
        if granted {
            records.records.insert(key, true);
        } else {
            records.records.remove(&key);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec(&records)?;
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temp, &self.path)?;
        crate::persistence::set_owner_only(&self.path)
            .map_err(|error| TrustStoreError::Permissions(error.to_string()))?;
        Ok(())
    }

    fn load(&self) -> Result<TrustRecords, TrustStoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let records: TrustRecords = serde_json::from_slice(&bytes)?;
                if records.version != 1 {
                    return Err(TrustStoreError::UnsupportedVersion(records.version));
                }
                Ok(records)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TrustRecords {
                version: 1,
                records: BTreeMap::new(),
            }),
            Err(error) => Err(error.into()),
        }
    }
}

fn record_key(workspace_root: &Path, authority_id: &str) -> String {
    format!("{}\n{}", workspace_root.to_string_lossy(), authority_id)
}

#[derive(Debug, Error)]
pub enum TrustStoreError {
    #[error("unsupported trust store version {0}")]
    UnsupportedVersion(u32),
    #[error("trust store permissions failed: {0}")]
    Permissions(String),
    #[error("trust store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("trust store encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}
