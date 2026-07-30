use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyCandidate {
    pub host_field: String,
    pub algorithm: String,
    pub key_base64: String,
    pub fingerprint: String,
}

impl HostKeyCandidate {
    pub fn parse(line: &str) -> Result<Self, KnownHostsError> {
        let mut fields = line.split_whitespace();
        let host_field = fields
            .next()
            .ok_or(KnownHostsError::MalformedCandidate)?
            .to_owned();
        let algorithm = fields
            .next()
            .ok_or(KnownHostsError::MalformedCandidate)?
            .to_owned();
        let key_base64 = fields
            .next()
            .ok_or(KnownHostsError::MalformedCandidate)?
            .to_owned();
        if fields.next().is_some()
            || !matches!(
                algorithm.as_str(),
                "ssh-ed25519" | "ecdsa-sha2-nistp256" | "rsa-sha2-512" | "ssh-rsa"
            )
        {
            return Err(KnownHostsError::MalformedCandidate);
        }
        let key = base64::engine::general_purpose::STANDARD
            .decode(&key_base64)
            .map_err(|_| KnownHostsError::MalformedCandidate)?;
        if key.len() > 16 * 1024 {
            return Err(KnownHostsError::MalformedCandidate);
        }
        let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&key)));
        Ok(Self {
            host_field,
            algorithm,
            key_base64,
            fingerprint,
        })
    }

    pub fn accepted_line(&self, host: &str, port: u16) -> String {
        let host_field = if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        };
        format!("{host_field} {} {}", self.algorithm, self.key_base64)
    }
}

#[derive(Clone, Debug)]
pub struct KnownHostsStore {
    path: PathBuf,
}

impl KnownHostsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn accept(
        &self,
        host: &str,
        port: u16,
        candidate: &HostKeyCandidate,
        expected_fingerprint: &str,
    ) -> Result<(), KnownHostsError> {
        if candidate.fingerprint != expected_fingerprint {
            return Err(KnownHostsError::FingerprintMismatch);
        }
        let accepted = candidate.accepted_line(host, port);
        let host_field = accepted
            .split_whitespace()
            .next()
            .ok_or(KnownHostsError::MalformedCandidate)?;
        let existing = match std::fs::read_to_string(&self.path) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        for line in existing
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
        {
            if line.split_whitespace().next() == Some(host_field) && line != accepted {
                return Err(KnownHostsError::HostKeyChanged(host_field.to_owned()));
            }
            if line == accepted {
                return Ok(());
            }
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        if !existing.is_empty() {
            file.write_all(existing.as_bytes())?;
            if !existing.ends_with('\n') {
                file.write_all(b"\n")?;
            }
        }
        writeln!(file, "{accepted}")?;
        file.sync_all()?;
        std::fs::rename(&temp, &self.path)?;
        crate::persistence::set_owner_only(&self.path)
            .map_err(|error| KnownHostsError::Permissions(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum KnownHostsError {
    #[error("malformed SSH host-key candidate")]
    MalformedCandidate,
    #[error("SSH host-key fingerprint does not match explicit acceptance")]
    FingerprintMismatch,
    #[error("SSH host key changed for {0}")]
    HostKeyChanged(String),
    #[error("known-hosts permissions failed: {0}")]
    Permissions(String),
    #[error("known-hosts I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
