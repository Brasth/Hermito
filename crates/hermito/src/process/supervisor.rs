use std::process::Stdio;

use hermito_protocol::request::CommandSpec;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

use super::cancellation::ProcessLimits;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub struct ProcessSupervisor;

struct ProcessTree {
    #[cfg(unix)]
    group_id: i32,
    #[cfg(windows)]
    job: usize,
}

impl ProcessTree {
    #[cfg(unix)]
    fn attach(child: &Child) -> std::io::Result<Self> {
        let group_id = child
            .id()
            .ok_or_else(|| std::io::Error::other("spawned process has no PID"))?
            as i32;
        Ok(Self { group_id })
    }

    #[cfg(windows)]
    fn attach(child: &Child) -> std::io::Result<Self> {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, ERROR_INVALID_HANDLE},
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
        };

        // SAFETY: null security/name pointers request an unnamed job with default security.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` has the exact structure and byte length required by this info class.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let Some(process) = child.raw_handle() else {
            // SAFETY: `job` is a live handle created above.
            unsafe { CloseHandle(job) };
            return Err(std::io::Error::from_raw_os_error(
                ERROR_INVALID_HANDLE as i32,
            ));
        };
        // SAFETY: both handles are live and owned for at least the duration of this call.
        let assigned = unsafe { AssignProcessToJobObject(job, process.cast()) };
        if configured == 0 || assigned == 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: `job` is a live handle created above.
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        Ok(Self { job: job as usize })
    }

    async fn terminate(&self, child: &mut Child, limits: ProcessLimits) {
        #[cfg(unix)]
        {
            // Negative PID addresses the process group created for this command.
            // SAFETY: the group ID belongs to the supervised command.
            unsafe {
                libc::kill(-self.group_id, libc::SIGTERM);
            }
            let _ = tokio::time::timeout(limits.graceful_shutdown, child.wait()).await;
            // SAFETY: any descendants left in the owned process group must not escape.
            unsafe {
                libc::kill(-self.group_id, libc::SIGKILL);
            }
        }

        #[cfg(windows)]
        {
            use windows_sys::Win32::System::{
                Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT},
                JobObjects::TerminateJobObject,
            };
            if let Some(process_id) = child.id() {
                // SAFETY: the child was created as a new process group.
                unsafe {
                    GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_id);
                }
                let _ = tokio::time::timeout(limits.graceful_shutdown, child.wait()).await;
            }
            // SAFETY: the job handle is live until ProcessTree::drop.
            unsafe {
                TerminateJobObject(self.job as windows_sys::Win32::Foundation::HANDLE, 1);
            }
        }

        let _ = child.start_kill();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
    }

    async fn cleanup_descendants(&self) {
        #[cfg(unix)]
        {
            // SAFETY: this is the dedicated process group assigned during spawn.
            unsafe {
                libc::kill(-self.group_id, libc::SIGTERM);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // SAFETY: forcefully remove any descendant that ignored SIGTERM.
            unsafe {
                libc::kill(-self.group_id, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            // SAFETY: the job is owned by this ProcessTree and contains only this command tree.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self.job as windows_sys::Win32::Foundation::HANDLE,
                    0,
                );
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        // SAFETY: `job` is owned by this ProcessTree and closed exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(
                self.job as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

impl ProcessSupervisor {
    pub async fn exec(
        command: &CommandSpec,
        cancellation: CancellationToken,
        limits: ProcessLimits,
    ) -> Result<ExecResult, SupervisorError> {
        command
            .validate()
            .map_err(SupervisorError::InvalidCommand)?;
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .current_dir(&command.cwd)
            .env_clear()
            .envs(&command.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
            process
                .as_std_mut()
                .creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let mut child = process.spawn()?;
        let process_tree = match ProcessTree::attach(&child) {
            Ok(process_tree) => process_tree,
            Err(error) => {
                #[cfg(windows)]
                if let Some(process_id) = child.id() {
                    terminate_uncontained_process_tree(process_id);
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error.into());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                process_tree.terminate(&mut child, limits).await;
                return Err(SupervisorError::PipeUnavailable("stdout"));
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                drop(stdout);
                process_tree.terminate(&mut child, limits).await;
                return Err(SupervisorError::PipeUnavailable("stderr"));
            }
        };
        let stdout_task = tokio::spawn(read_bounded(stdout, limits.stdout_bytes));
        let stderr_task = tokio::spawn(read_bounded(stderr, limits.stderr_bytes));

        let status = tokio::select! {
            _ = cancellation.cancelled() => {
                process_tree.terminate(&mut child, limits).await;
                return Err(SupervisorError::Cancelled);
            }
            result = tokio::time::timeout(limits.wall_time, child.wait()) => match result {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    process_tree.terminate(&mut child, limits).await;
                    return Err(error.into());
                }
                Err(_) => {
                    process_tree.terminate(&mut child, limits).await;
                    return Err(SupervisorError::TimedOut);
                }
            }
        };
        process_tree.cleanup_descendants().await;
        let (stdout, stdout_truncated) = stdout_task.await??;
        let (stderr, stderr_truncated) = stderr_task.await??;
        Ok(ExecResult {
            exit_code: status.code(),
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }
}

#[cfg(windows)]
pub(crate) fn terminate_uncontained_process_tree(root_process_id: u32) {
    use std::collections::HashSet;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };

    fn terminate_process(process_id: u32) {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        };
        // SAFETY: the returned handle is checked and closed exactly once.
        let process =
            unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, process_id) };
        if process.is_null() {
            return;
        }
        // SAFETY: `process` is a live process handle with terminate/synchronize rights.
        unsafe {
            TerminateProcess(process, 1);
            WaitForSingleObject(process, 1_000);
            CloseHandle(process);
        }
    }

    let mut owned_processes = HashSet::from([root_process_id]);
    for _ in 0..4 {
        // SAFETY: this requests a read-only system process snapshot.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            terminate_process(root_process_id);
            return;
        }
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: `entry` has the documented size and remains valid for each call.
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            // SAFETY: the snapshot and entry buffer remain live.
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: `snapshot` is live and owned by this function.
        unsafe { CloseHandle(snapshot) };

        let before = owned_processes.len();
        loop {
            let mut changed = false;
            for &(process_id, parent_id) in &entries {
                if owned_processes.contains(&parent_id) && owned_processes.insert(process_id) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Stop the root first so it cannot create more descendants while cleanup proceeds.
        terminate_process(root_process_id);
        for &process_id in &owned_processes {
            if process_id != root_process_id {
                terminate_process(process_id);
            }
        }
        if owned_processes.len() == before {
            break;
        }
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let limit = limit.min(16 * 1024 * 1024);
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..count.min(remaining)]);
        truncated |= count > remaining;
    }
    Ok((output, truncated))
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("invalid command: {0}")]
    InvalidCommand(&'static str),
    #[error("process {0} pipe unavailable")]
    PipeUnavailable(&'static str),
    #[error("process cancelled")]
    Cancelled,
    #[error("process wall-time limit exceeded")]
    TimedOut,
    #[error("process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("process task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}
