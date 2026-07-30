use std::{collections::BTreeMap, path::Path};

use hermito_protocol::request::{CommandSpec, EnvironmentEpoch, WorkspaceEpoch};
use portable_pty::PtySize;
use tokio_util::sync::CancellationToken;

use super::session::{LocalPtySession, PtySessionError};

pub fn default_shell_command(cwd: &Path) -> CommandSpec {
    let mut env = BTreeMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    #[cfg(unix)]
    {
        env.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
        env.insert("LANG".into(), "C.UTF-8".into());
        CommandSpec {
            program: "/bin/sh".into(),
            args: Vec::new(),
            cwd: cwd.to_string_lossy().into_owned(),
            env,
        }
    }
    #[cfg(windows)]
    {
        env.insert("PATH".into(), r"C:\Windows\System32;C:\Windows".into());
        CommandSpec {
            program: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
            args: vec!["-NoLogo".into(), "-NoProfile".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            env,
        }
    }
}

pub fn spawn_local_pty(
    id: u64,
    command: &CommandSpec,
    rows: u16,
    cols: u16,
    workspace_epoch: WorkspaceEpoch,
    environment_epoch: EnvironmentEpoch,
    cancellation: CancellationToken,
) -> Result<LocalPtySession, PtySessionError> {
    LocalPtySession::spawn(
        id,
        command,
        PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        },
        workspace_epoch,
        environment_epoch,
        cancellation,
    )
}
