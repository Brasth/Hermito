use std::time::Duration;

use thiserror::Error;
use tokio::process::Child;
use zeroize::Zeroizing;

use super::{
    helper_installer::{HelperInstallError, HelperInstaller, InstalledHelper},
    ssh_bootstrap::{SshBootstrap, SshBootstrapError},
};

pub struct HelperLauncher<'a> {
    bootstrap: &'a SshBootstrap,
    installer: &'a HelperInstaller<'a>,
}

impl<'a> HelperLauncher<'a> {
    pub fn new(bootstrap: &'a SshBootstrap, installer: &'a HelperInstaller<'a>) -> Self {
        Self {
            bootstrap,
            installer,
        }
    }

    pub async fn launch(
        &self,
        helper: &InstalledHelper,
        execution_trusted: bool,
        passphrase: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<Child, HelperLaunchError> {
        if !execution_trusted {
            return Err(HelperLaunchError::InspectOnly);
        }
        self.installer.revalidate(helper, passphrase).await?;
        let remote = vec![
            "/usr/bin/env".into(),
            "-i".into(),
            "HOME=/".into(),
            "LANG=C".into(),
            "PATH=/usr/bin:/bin".into(),
            "TERM=dumb".into(),
            helper.remote_path.to_string_lossy().into_owned(),
            "--stdio".into(),
        ];
        self.bootstrap
            .spawn_ssh(&remote, passphrase)
            .await
            .map_err(Into::into)
    }
}

pub async fn terminate_helper(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[derive(Debug, Error)]
pub enum HelperLaunchError {
    #[error("remote authority is inspect only; helper launch blocked")]
    InspectOnly,
    #[error("helper installation verification failed: {0}")]
    Install(#[from] HelperInstallError),
    #[error("SSH launcher contract failed: {0}")]
    Bootstrap(#[from] SshBootstrapError),
    #[error("failed to launch verified helper: {0}")]
    Io(#[from] std::io::Error),
}
