use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use zeroize::Zeroizing;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshIdentity {
    pub private_key: PathBuf,
    pub certificate: Option<PathBuf>,
}

impl SshIdentity {
    pub fn validate(&self) -> Result<(), SshIdentityError> {
        validate_identity_file(&self.private_key, true)?;
        if let Some(certificate) = &self.certificate {
            validate_identity_file(certificate, false)?;
        }
        Ok(())
    }
}

fn validate_identity_file(path: &Path, private: bool) -> Result<(), SshIdentityError> {
    if !path.is_absolute() {
        return Err(SshIdentityError::NotAbsolute(path.to_path_buf()));
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(SshIdentityError::NotRegularFile(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if private && metadata.mode() & 0o077 != 0 {
            return Err(SshIdentityError::InsecurePermissions {
                path: path.to_path_buf(),
                mode: metadata.mode() & 0o777,
            });
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(SshIdentityError::WrongOwner(path.to_path_buf()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskpassEndpoint {
    pub endpoint: String,
    pub authority_id: String,
}

pub struct OneShotAskpass;

impl OneShotAskpass {
    pub async fn start(
        passphrase: Zeroizing<Vec<u8>>,
        authority_id: String,
        ttl: Duration,
    ) -> Result<
        (
            AskpassEndpoint,
            tokio::task::JoinHandle<Result<(), SshIdentityError>>,
        ),
        SshIdentityError,
    > {
        if passphrase.contains(&b'\n') || passphrase.contains(&b'\r') {
            return Err(SshIdentityError::InvalidPassphrase);
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let endpoint = AskpassEndpoint {
            endpoint: format!("{}|{}|{}", address, nonce, authority_id),
            authority_id: authority_id.clone(),
        };
        let task_nonce = nonce;
        let task = tokio::spawn(async move {
            let accepted = tokio::time::timeout(ttl, listener.accept())
                .await
                .map_err(|_| SshIdentityError::AskpassExpired)?;
            let (stream, peer) = accepted?;
            if !peer.ip().is_loopback() {
                return Err(SshIdentityError::NonLoopbackPeer);
            }
            let (reader, mut writer) = stream.into_split();
            let mut request = String::new();
            let count = tokio::time::timeout(
                Duration::from_secs(2),
                BufReader::new(reader).read_line(&mut request),
            )
            .await
            .map_err(|_| SshIdentityError::AskpassExpired)??;
            if count == 0 || request.trim_end() != format!("{}|{}", task_nonce, authority_id) {
                return Err(SshIdentityError::NonceMismatch);
            }
            writer.write_all(&passphrase).await?;
            writer.write_all(b"\n").await?;
            writer.shutdown().await?;
            Ok(())
        });
        Ok((endpoint, task))
    }
}

pub async fn askpass_client(endpoint: &str) -> Result<Zeroizing<Vec<u8>>, SshIdentityError> {
    let mut parts = endpoint.splitn(3, '|');
    let address = parts.next().ok_or(SshIdentityError::MalformedEndpoint)?;
    let nonce = parts.next().ok_or(SshIdentityError::MalformedEndpoint)?;
    let authority_id = parts.next().ok_or(SshIdentityError::MalformedEndpoint)?;
    let socket: std::net::SocketAddr = address
        .parse()
        .map_err(|_| SshIdentityError::MalformedEndpoint)?;
    if !socket.ip().is_loopback() {
        return Err(SshIdentityError::NonLoopbackPeer);
    }
    let mut stream = TcpStream::connect(socket).await?;
    stream
        .write_all(format!("{nonce}|{authority_id}\n").as_bytes())
        .await?;
    stream.shutdown().await?;
    let mut response = Vec::with_capacity(128);
    tokio::io::AsyncReadExt::take(&mut stream, 16 * 1024)
        .read_to_end(&mut response)
        .await?;
    while matches!(response.last(), Some(b'\n' | b'\r')) {
        response.pop();
    }
    Ok(Zeroizing::new(response))
}

#[derive(Debug, Error)]
pub enum SshIdentityError {
    #[error("SSH identity path must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("SSH identity must be a regular non-symlink file: {0}")]
    NotRegularFile(PathBuf),
    #[error("SSH identity permissions are too broad ({mode:o}): {path}")]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("SSH identity is not owned by the current user: {0}")]
    WrongOwner(PathBuf),
    #[error("SSH passphrase may not contain a newline")]
    InvalidPassphrase,
    #[error("SSH askpass endpoint malformed")]
    MalformedEndpoint,
    #[error("SSH askpass endpoint peer is not loopback")]
    NonLoopbackPeer,
    #[error("SSH askpass nonce or authority binding did not match")]
    NonceMismatch,
    #[error("SSH askpass capability expired")]
    AskpassExpired,
    #[error("SSH identity I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
