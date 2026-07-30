use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::config::known_hosts::HostKeyCandidate;

use super::ssh_identity::{AskpassEndpoint, OneShotAskpass, SshIdentity};

const KEYSCAN_OUTPUT_LIMIT: usize = 64 * 1024;
const MAX_KEY_CANDIDATES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
}

impl SshTarget {
    pub fn validate(&self) -> Result<(), SshBootstrapError> {
        if self.host.is_empty()
            || self.host.len() > 253
            || !self
                .host
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
        {
            return Err(SshBootstrapError::InvalidTarget("host"));
        }
        if self.user.is_empty()
            || self.user.len() > 64
            || !self
                .user
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(SshBootstrapError::InvalidTarget("user"));
        }
        if self.port == 0 {
            return Err(SshBootstrapError::InvalidTarget("port"));
        }
        Ok(())
    }

    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenSshInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub stdin_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct SshBootstrap {
    pub target: SshTarget,
    pub identity: SshIdentity,
    pub known_hosts: PathBuf,
    pub fixed_path: String,
}

impl SshBootstrap {
    pub fn new(
        target: SshTarget,
        identity: SshIdentity,
        known_hosts: PathBuf,
    ) -> Result<Self, SshBootstrapError> {
        target.validate()?;
        identity
            .validate()
            .map_err(|error| SshBootstrapError::Identity(error.to_string()))?;
        if !known_hosts.is_absolute() {
            return Err(SshBootstrapError::KnownHostsNotAbsolute);
        }
        Ok(Self {
            target,
            identity,
            known_hosts,
            fixed_path: if cfg!(windows) {
                r"C:\Windows\System32\OpenSSH;C:\Windows\System32".into()
            } else {
                "/usr/bin:/bin".into()
            },
        })
    }

    pub fn ssh_invocation(
        &self,
        remote_argv: &[String],
        askpass: Option<&AskpassEndpoint>,
    ) -> Result<OpenSshInvocation, SshBootstrapError> {
        if remote_argv.is_empty() || remote_argv.iter().any(|arg| !safe_remote_argument(arg)) {
            return Err(SshBootstrapError::UnsafeRemoteCommand);
        }
        let mut args = vec!["-F".into(), "none".into()];
        args.extend(self.common_auth_options());
        args.extend(prompt_options(askpass.is_some()));
        args.extend([
            "-p".into(),
            self.target.port.to_string(),
            "-T".into(),
            self.target.destination(),
        ]);
        args.extend(remote_argv.iter().cloned());
        Ok(self.invocation("ssh", args, askpass, None))
    }

    pub fn sftp_invocation(
        &self,
        batch: Vec<u8>,
        askpass: Option<&AskpassEndpoint>,
    ) -> OpenSshInvocation {
        let mut args = vec!["-F".into(), "none".into()];
        args.extend(self.common_auth_options());
        args.extend(prompt_options(askpass.is_some()));
        args.extend([
            "-P".into(),
            self.target.port.to_string(),
            "-b".into(),
            "-".into(),
            self.target.destination(),
        ]);
        self.invocation("sftp", args, askpass, Some(batch))
    }

    pub async fn scan_host_keys(&self) -> Result<Vec<HostKeyCandidate>, SshBootstrapError> {
        let mut command = Command::new("ssh-keyscan");
        command
            .args([
                "-T",
                "5",
                "-p",
                &self.target.port.to_string(),
                &self.target.host,
            ])
            .env_clear()
            .env("PATH", &self.fixed_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or(SshBootstrapError::MissingPipe)?;
        let read = tokio::spawn(async move {
            let mut bytes = Vec::with_capacity(4096);
            stdout
                .take((KEYSCAN_OUTPUT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            Ok::<_, std::io::Error>(bytes)
        });
        let status = tokio::time::timeout(Duration::from_secs(7), child.wait())
            .await
            .map_err(|_| SshBootstrapError::KeyscanTimeout)??;
        let bytes = read
            .await
            .map_err(|error| SshBootstrapError::Join(error.to_string()))??;
        if bytes.len() > KEYSCAN_OUTPUT_LIMIT {
            return Err(SshBootstrapError::KeyscanTooLarge);
        }
        if !status.success() {
            return Err(SshBootstrapError::KeyscanFailed);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| SshBootstrapError::MalformedKeyscan)?;
        let mut candidates = Vec::new();
        for line in text
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        {
            if candidates.len() >= MAX_KEY_CANDIDATES {
                return Err(SshBootstrapError::TooManyCandidates);
            }
            candidates.push(
                HostKeyCandidate::parse(line).map_err(|_| SshBootstrapError::MalformedKeyscan)?,
            );
        }
        if candidates.is_empty() {
            return Err(SshBootstrapError::KeyscanFailed);
        }
        Ok(candidates)
    }

    pub async fn run(
        &self,
        invocation: &OpenSshInvocation,
        output_limit: usize,
        timeout: Duration,
    ) -> Result<std::process::Output, SshBootstrapError> {
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(&invocation.cwd)
            .env_clear()
            .envs(&invocation.env)
            .stdin(if invocation.stdin_bytes.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or(SshBootstrapError::MissingPipe)?;
        let stderr = child.stderr.take().ok_or(SshBootstrapError::MissingPipe)?;
        if let Some(bytes) = &invocation.stdin_bytes {
            let mut stdin = child.stdin.take().ok_or(SshBootstrapError::MissingPipe)?;
            tokio::io::AsyncWriteExt::write_all(&mut stdin, bytes).await?;
            tokio::io::AsyncWriteExt::shutdown(&mut stdin).await?;
        }
        let output_limit = output_limit.min(32 * 1024 * 1024);
        let output_exceeded = CancellationToken::new();
        let stdout_task = tokio::spawn(read_capped(stdout, output_limit, output_exceeded.clone()));
        let stderr_task = tokio::spawn(read_capped(stderr, output_limit, output_exceeded.clone()));
        let status = tokio::select! {
            biased;
            _ = output_exceeded.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(SshBootstrapError::OutputTooLarge);
            }
            result = tokio::time::timeout(timeout, child.wait()) => match result {
                Ok(status) => status?,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(SshBootstrapError::CommandTimeout);
                }
            },
        };
        let (stdout, stdout_too_large) = stdout_task
            .await
            .map_err(|error| SshBootstrapError::Join(error.to_string()))??;
        let (stderr, stderr_too_large) = stderr_task
            .await
            .map_err(|error| SshBootstrapError::Join(error.to_string()))??;
        if stdout_too_large || stderr_too_large {
            return Err(SshBootstrapError::OutputTooLarge);
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    pub async fn run_sftp(
        &self,
        batch: Vec<u8>,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
        output_limit: usize,
        timeout: Duration,
    ) -> Result<std::process::Output, SshBootstrapError> {
        let (endpoint, askpass_task) = self.start_askpass(passphrase).await?;
        let invocation = self.sftp_invocation(batch, endpoint.as_ref());
        let result = self.run(&invocation, output_limit, timeout).await;
        if let Some(task) = askpass_task {
            task.abort();
        }
        result
    }
    pub async fn run_remote_capture(
        &self,
        remote_argv: &[String],
        passphrase: Option<&Zeroizing<Vec<u8>>>,
        output_limit: usize,
        timeout: Duration,
    ) -> Result<std::process::Output, SshBootstrapError> {
        let (endpoint, askpass_task) = self.start_askpass(passphrase).await?;
        let invocation = self.ssh_invocation(remote_argv, endpoint.as_ref())?;
        let result = self.run(&invocation, output_limit, timeout).await;
        if let Some(task) = askpass_task {
            task.abort();
        }
        result
    }

    pub async fn spawn_ssh(
        &self,
        remote_argv: &[String],
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<tokio::process::Child, SshBootstrapError> {
        let (endpoint, askpass_task) = self.start_askpass(passphrase).await?;
        let invocation = self.ssh_invocation(remote_argv, endpoint.as_ref())?;
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(&invocation.cwd)
            .env_clear()
            .envs(&invocation.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match command.spawn() {
            Ok(child) => {
                // Dropping a JoinHandle detaches it. The endpoint serves exactly one
                // authority-bound request, then zeroizes the cloned passphrase.
                drop(askpass_task);
                Ok(child)
            }
            Err(error) => {
                if let Some(task) = askpass_task {
                    task.abort();
                }
                Err(error.into())
            }
        }
    }

    async fn start_askpass(
        &self,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<
        (
            Option<AskpassEndpoint>,
            Option<tokio::task::JoinHandle<Result<(), super::ssh_identity::SshIdentityError>>>,
        ),
        SshBootstrapError,
    > {
        let Some(passphrase) = passphrase else {
            return Ok((None, None));
        };
        let (endpoint, task) = OneShotAskpass::start(
            Zeroizing::new(passphrase.as_slice().to_vec()),
            self.target.destination(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|error| SshBootstrapError::Identity(error.to_string()))?;
        Ok((Some(endpoint), Some(task)))
    }

    fn common_auth_options(&self) -> Vec<String> {
        let mut args = Vec::new();
        for option in [
            format!("UserKnownHostsFile={}", self.known_hosts.to_string_lossy()),
            "GlobalKnownHostsFile=none".into(),
            "StrictHostKeyChecking=yes".into(),
            "IdentitiesOnly=yes".into(),
            "IdentityAgent=none".into(),
            "UpdateHostKeys=no".into(),
            "PreferredAuthentications=publickey".into(),
            "PasswordAuthentication=no".into(),
            "KbdInteractiveAuthentication=no".into(),
            "ForwardAgent=no".into(),
            "ClearAllForwardings=yes".into(),
            "PermitLocalCommand=no".into(),
            "ProxyCommand=none".into(),
            "ControlMaster=no".into(),
            "ControlPath=none".into(),
        ] {
            args.push("-o".into());
            args.push(option);
        }
        args.push("-i".into());
        args.push(self.identity.private_key.to_string_lossy().into_owned());
        if let Some(certificate) = &self.identity.certificate {
            args.push("-o".into());
            args.push(format!("CertificateFile={}", certificate.to_string_lossy()));
        }
        args
    }

    fn invocation(
        &self,
        program: &str,
        args: Vec<String>,
        askpass: Option<&AskpassEndpoint>,
        stdin_bytes: Option<Vec<u8>>,
    ) -> OpenSshInvocation {
        let mut env = BTreeMap::from([
            ("PATH".into(), self.fixed_path.clone()),
            ("LC_ALL".into(), "C".into()),
        ]);
        if let Some(askpass) = askpass {
            env.insert("SSH_ASKPASS_REQUIRE".into(), "force".into());
            env.insert(
                "SSH_ASKPASS".into(),
                std::env::current_exe()
                    .unwrap_or_else(|_| PathBuf::from("hermito"))
                    .to_string_lossy()
                    .into_owned(),
            );
            env.insert("HERMITO_ASKPASS_ENDPOINT".into(), askpass.endpoint.clone());
            env.insert("DISPLAY".into(), "hermito-askpass".into());
        }
        OpenSshInvocation {
            program: program.into(),
            args,
            env,
            cwd: platform_root(),
            stdin_bytes,
        }
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    limit: usize,
    exceeded: CancellationToken,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    let too_large = bytes.len() > limit;
    if too_large {
        exceeded.cancel();
        bytes.truncate(limit);
    }
    Ok((bytes, too_large))
}

fn platform_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\")
    } else {
        PathBuf::from("/")
    }
}

fn safe_remote_argument(argument: &str) -> bool {
    !argument.is_empty()
        && argument.len() <= 4096
        && argument.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'=')
        })
}

fn prompt_options(has_askpass: bool) -> Vec<String> {
    vec![
        "-o".into(),
        format!("BatchMode={}", if has_askpass { "no" } else { "yes" }),
        "-o".into(),
        "NumberOfPasswordPrompts=1".into(),
    ]
}

pub fn validate_absolute_helper_path(path: &Path) -> Result<(), SshBootstrapError> {
    let text = path
        .to_str()
        .ok_or(SshBootstrapError::UnsafeRemoteCommand)?;
    let invalid_component = text
        .split('/')
        .skip(1)
        .any(|component| component.is_empty() || matches!(component, "." | ".."));
    if !text.starts_with('/')
        || text.starts_with("//")
        || invalid_component
        || !safe_remote_argument(text)
    {
        return Err(SshBootstrapError::UnsafeRemoteCommand);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SshBootstrapError {
    #[error("invalid SSH target {0}")]
    InvalidTarget(&'static str),
    #[error("SSH identity rejected: {0}")]
    Identity(String),
    #[error("Hermito known-hosts path must be absolute")]
    KnownHostsNotAbsolute,
    #[error("remote helper command is not a safe fixed argv")]
    UnsafeRemoteCommand,
    #[error("SSH process pipe unavailable")]
    MissingPipe,
    #[error("ssh-keyscan timed out")]
    KeyscanTimeout,
    #[error("ssh-keyscan output exceeded limit")]
    KeyscanTooLarge,
    #[error("ssh-keyscan returned too many candidates")]
    TooManyCandidates,
    #[error("ssh-keyscan failed")]
    KeyscanFailed,
    #[error("ssh-keyscan output malformed")]
    MalformedKeyscan,
    #[error("SSH command timed out")]
    CommandTimeout,
    #[error("SSH command output exceeded limit")]
    OutputTooLarge,
    #[error("SSH task join failed: {0}")]
    Join(String),
    #[error("SSH I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
