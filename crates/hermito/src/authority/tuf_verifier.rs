use std::path::PathBuf;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tough::{RepositoryLoader, TargetName};

use crate::remote::tuf::TufPolicy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTarget {
    pub name: String,
    pub path: PathBuf,
    pub length: u64,
    pub sha256_hex: String,
}

struct TempTarget {
    path: PathBuf,
    committed: bool,
}

impl TempTarget {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempTarget {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub struct TufVerifier {
    policy: TufPolicy,
}

impl TufVerifier {
    pub fn new(policy: TufPolicy) -> Result<Self, TufVerificationError> {
        policy
            .validate()
            .map_err(TufVerificationError::InvalidPolicy)?;
        Ok(Self { policy })
    }

    pub async fn verify_target(
        &self,
        target_name: &str,
    ) -> Result<VerifiedTarget, TufVerificationError> {
        let root = tokio::fs::read(&self.policy.trusted_root)
            .await
            .map_err(|source| TufVerificationError::Role {
                role: "root",
                detail: source.to_string(),
            })?;
        tokio::fs::create_dir_all(&self.policy.datastore).await?;
        tokio::fs::create_dir_all(&self.policy.target_cache).await?;

        let repository = match RepositoryLoader::new(
            &root,
            self.policy.metadata_base_url.clone(),
            self.policy.targets_base_url.clone(),
        )
        .datastore(self.policy.datastore.clone())
        .load()
        .await
        {
            Ok(repository) => repository,
            Err(online_error) if self.policy.allow_offline_cache => {
                let metadata = self.policy.offline_metadata_url.clone().ok_or(
                    TufVerificationError::InvalidPolicy("offline metadata URL missing"),
                )?;
                let targets = self.policy.offline_targets_url.clone().ok_or(
                    TufVerificationError::InvalidPolicy("offline targets URL missing"),
                )?;
                RepositoryLoader::new(&root, metadata, targets)
                    .datastore(self.policy.datastore.clone())
                    .load()
                    .await
                    .map_err(|offline_error| TufVerificationError::Metadata {
                        online: online_error.to_string(),
                        offline: Some(offline_error.to_string()),
                    })?
            }
            Err(error) => {
                return Err(TufVerificationError::Metadata {
                    online: error.to_string(),
                    offline: None,
                })
            }
        };

        let name = TargetName::new(target_name)
            .map_err(|source| TufVerificationError::TargetName(source.to_string()))?;
        let stream = repository
            .read_target(&name)
            .await
            .map_err(|source| TufVerificationError::Target {
                name: target_name.into(),
                detail: source.to_string(),
            })?
            .ok_or_else(|| TufVerificationError::TargetMissing(target_name.into()))?;
        tokio::pin!(stream);

        let temp_path = self.policy.target_cache.join(format!(
            ".{}.{}.tmp",
            sanitize_name(target_name),
            uuid::Uuid::new_v4()
        ));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await?;
        let mut temp_target = TempTarget::new(temp_path.clone());
        let mut length = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| TufVerificationError::Target {
                name: target_name.into(),
                detail: source.to_string(),
            })?;
            length = length.checked_add(chunk.len() as u64).ok_or(
                TufVerificationError::TargetTooLarge {
                    name: target_name.into(),
                    limit: self.policy.max_target_bytes,
                },
            )?;
            if length > self.policy.max_target_bytes {
                drop(file);
                return Err(TufVerificationError::TargetTooLarge {
                    name: target_name.into(),
                    limit: self.policy.max_target_bytes,
                });
            }
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);
        let sha256_hex = hex::encode(digest.finalize());
        let final_path = self
            .policy
            .target_cache
            .join(format!("{}-{sha256_hex}", sanitize_name(target_name)));
        if tokio::fs::metadata(&final_path).await.is_ok() {
            let existing = verify_file(&final_path, length, &sha256_hex).await;
            if existing.is_err() {
                tokio::fs::remove_file(&final_path).await?;
            }
        }
        if tokio::fs::metadata(&final_path).await.is_err() {
            tokio::fs::rename(&temp_path, &final_path).await?;
            temp_target.commit();
        } else {
            tokio::fs::remove_file(&temp_path).await?;
            temp_target.commit();
        }
        if let Err(error) = verify_file(&final_path, length, &sha256_hex).await {
            let _ = tokio::fs::remove_file(&final_path).await;
            return Err(error);
        }
        Ok(VerifiedTarget {
            name: target_name.into(),
            path: final_path,
            length,
            sha256_hex,
        })
    }
}

pub async fn verify_file(
    path: &std::path::Path,
    expected_length: u64,
    expected_sha256: &str,
) -> Result<(), TufVerificationError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        digest.update(&buffer[..count]);
    }
    let actual = hex::encode(digest.finalize());
    if length != expected_length || actual != expected_sha256 {
        return Err(TufVerificationError::HashMismatch {
            path: path.to_path_buf(),
            expected_length,
            actual_length: length,
            expected_sha256: expected_sha256.into(),
            actual_sha256: actual,
        });
    }
    Ok(())
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum TufVerificationError {
    #[error("invalid TUF policy: {0}")]
    InvalidPolicy(&'static str),
    #[error("TUF {role} verification failed: {detail}")]
    Role { role: &'static str, detail: String },
    #[error("TUF metadata verification failed online: {online}; offline: {offline:?}")]
    Metadata {
        online: String,
        offline: Option<String>,
    },
    #[error("invalid TUF target name: {0}")]
    TargetName(String),
    #[error("TUF target not found: {0}")]
    TargetMissing(String),
    #[error("TUF target {name} verification failed: {detail}")]
    Target { name: String, detail: String },
    #[error("TUF target {name} exceeds byte limit {limit}")]
    TargetTooLarge { name: String, limit: u64 },
    #[error("verified target changed at {path:?}: length {actual_length}/{expected_length}, sha256 {actual_sha256}/{expected_sha256}")]
    HashMismatch {
        path: PathBuf,
        expected_length: u64,
        actual_length: u64,
        expected_sha256: String,
        actual_sha256: String,
    },
    #[error("TUF storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
