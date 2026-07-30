use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;
use zeroize::Zeroizing;

use crate::authority::tuf_verifier::{verify_file, TufVerificationError, VerifiedTarget};

use super::ssh_bootstrap::{validate_absolute_helper_path, SshBootstrap, SshBootstrapError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledHelper {
    pub remote_path: PathBuf,
    pub length: u64,
    pub sha256_hex: String,
}

pub struct HelperInstaller<'a> {
    bootstrap: &'a SshBootstrap,
    remote_directory: PathBuf,
}

impl<'a> HelperInstaller<'a> {
    pub fn new(
        bootstrap: &'a SshBootstrap,
        remote_directory: PathBuf,
    ) -> Result<Self, HelperInstallError> {
        validate_absolute_helper_path(&remote_directory)?;
        Ok(Self {
            bootstrap,
            remote_directory,
        })
    }

    pub async fn install(
        &self,
        target: &VerifiedTarget,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<InstalledHelper, HelperInstallError> {
        verify_file(&target.path, target.length, &target.sha256_hex).await?;
        let remote_path = self
            .remote_directory
            .join(format!("hermito-remote-{}", target.sha256_hex));
        validate_absolute_helper_path(&remote_path)?;

        if self
            .download_and_verify(&remote_path, target, passphrase)
            .await
            .is_err()
        {
            let remote_temp = self.remote_directory.join(format!(
                ".hermito-remote-{}.{}.tmp",
                target.sha256_hex,
                uuid::Uuid::new_v4()
            ));
            validate_absolute_helper_path(&remote_temp)?;
            let batch = format!(
                "{}put {} {}\n",
                sftp_mkdir_commands(&self.remote_directory),
                sftp_quote(&target.path),
                sftp_quote(&remote_temp),
            )
            .into_bytes();
            let output = match self
                .bootstrap
                .run_sftp(batch, passphrase, 256 * 1024, Duration::from_secs(60))
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    self.remove_remote_temp(&remote_temp, passphrase).await;
                    return Err(error.into());
                }
            };
            if !output.status.success() {
                self.remove_remote_temp(&remote_temp, passphrase).await;
                return Err(HelperInstallError::Sftp(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            if let Err(error) = self
                .download_and_verify(&remote_temp, target, passphrase)
                .await
            {
                self.remove_remote_temp(&remote_temp, passphrase).await;
                return Err(error);
            }
            let batch = format!(
                "chmod 700 {}\nrename {} {}\n",
                sftp_quote(&remote_temp),
                sftp_quote(&remote_temp),
                sftp_quote(&remote_path),
            )
            .into_bytes();
            let output = match self
                .bootstrap
                .run_sftp(batch, passphrase, 256 * 1024, Duration::from_secs(30))
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    self.remove_remote_temp(&remote_temp, passphrase).await;
                    return Err(error.into());
                }
            };
            if !output.status.success() {
                self.remove_remote_temp(&remote_temp, passphrase).await;
                return Err(HelperInstallError::Sftp(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            if let Err(error) = self
                .download_and_verify(&remote_path, target, passphrase)
                .await
            {
                self.remove_remote_temp(&remote_path, passphrase).await;
                return Err(error);
            }
        }
        Ok(InstalledHelper {
            remote_path,
            length: target.length,
            sha256_hex: target.sha256_hex.clone(),
        })
    }

    pub async fn revalidate(
        &self,
        helper: &InstalledHelper,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<(), HelperInstallError> {
        let target = VerifiedTarget {
            name: "installed-helper".into(),
            path: PathBuf::new(),
            length: helper.length,
            sha256_hex: helper.sha256_hex.clone(),
        };
        self.download_and_verify(&helper.remote_path, &target, passphrase)
            .await
    }

    async fn download_and_verify(
        &self,
        remote_path: &Path,
        target: &VerifiedTarget,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<(), HelperInstallError> {
        let remote = remote_path
            .to_str()
            .ok_or(SshBootstrapError::UnsafeRemoteCommand)?
            .to_string();
        let size_output = self
            .bootstrap
            .run_remote_capture(
                &["wc".into(), "-c".into(), remote.clone()],
                passphrase,
                256,
                Duration::from_secs(10),
            )
            .await?;
        if !size_output.status.success() {
            return Err(HelperInstallError::RemoteRead(
                String::from_utf8_lossy(&size_output.stderr).into_owned(),
            ));
        }
        let actual_length = String::from_utf8_lossy(&size_output.stdout)
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                HelperInstallError::RemoteRead("remote helper size was malformed".into())
            })?;
        if actual_length != target.length {
            return Err(HelperInstallError::LengthMismatch {
                expected: target.length,
                actual: actual_length,
            });
        }
        let output_limit = usize::try_from(target.length).map_err(|_| {
            HelperInstallError::RemoteRead("remote helper length exceeds host limits".into())
        })?;
        let output = self
            .bootstrap
            .run_remote_capture(
                &["cat".into(), "--".into(), remote],
                passphrase,
                output_limit,
                Duration::from_secs(30),
            )
            .await?;
        if !output.status.success() {
            return Err(HelperInstallError::RemoteRead(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let local =
            std::env::temp_dir().join(format!("hermito-helper-verify-{}", uuid::Uuid::new_v4()));
        if let Err(error) = tokio::fs::write(&local, &output.stdout).await {
            let _ = tokio::fs::remove_file(&local).await;
            return Err(error.into());
        }
        let result = verify_file(&local, target.length, &target.sha256_hex).await;
        let _ = tokio::fs::remove_file(&local).await;
        result.map_err(Into::into)
    }

    async fn remove_remote_temp(
        &self,
        remote_temp: &Path,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) {
        let batch = format!("-rm {}\n", sftp_quote(remote_temp)).into_bytes();
        let _ = self
            .bootstrap
            .run_sftp(batch, passphrase, 64 * 1024, Duration::from_secs(10))
            .await;
    }
}

fn sftp_mkdir_commands(path: &Path) -> String {
    let mut commands = String::new();
    let mut current = String::new();
    for component in path
        .to_str()
        .expect("validated remote path is UTF-8")
        .split('/')
        .skip(1)
    {
        current.push('/');
        current.push_str(component);
        commands.push_str(&format!("-mkdir \"{current}\"\n"));
    }
    commands
}

fn sftp_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[derive(Debug, Error)]
pub enum HelperInstallError {
    #[error("SSH bootstrap contract failed: {0}")]
    Bootstrap(#[from] SshBootstrapError),
    #[error("TUF target verification failed: {0}")]
    Verification(#[from] TufVerificationError),
    #[error("SFTP helper installation failed: {0}")]
    Sftp(String),
    #[error("remote helper read-back failed: {0}")]
    RemoteRead(String),
    #[error("remote helper length mismatch (expected {expected}, got {actual})")]
    LengthMismatch { expected: u64, actual: u64 },
    #[error("local helper verification I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
