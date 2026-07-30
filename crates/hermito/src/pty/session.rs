use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, RwLock,
    },
    thread::JoinHandle,
};

use hermito_protocol::{
    pty::{PtyMessage, PtySize as WirePtySize, PtySpawn, PtyStreamContext},
    request::{CommandSpec, EnvironmentEpoch, RequestEnvelope, WorkspaceEpoch},
    Message,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::terminal::{
    surface::{TerminalSurface, DEFAULT_SCROLLBACK_LINES},
    vt_parser::VtParser,
};

const PTY_READ_CHUNK: usize = 64 * 1024;
const PTY_TOTAL_OUTPUT_BUDGET: usize = 100 * 1024 * 1024;
const PTY_INPUT_CHUNK_LIMIT: usize = 64 * 1024;
const PTY_INPUT_QUEUE: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PtySessionState {
    Running = 0,
    Exited = 1,
    Lost = 2,
}

impl PtySessionState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Exited,
            2 => Self::Lost,
            _ => Self::Running,
        }
    }
}

struct PtyProcessTree {
    #[cfg(unix)]
    group_id: i32,
    #[cfg(windows)]
    job: usize,
}

impl PtyProcessTree {
    fn attach(
        _master: &(dyn MasterPty + Send),
        child: &(dyn Child + Send + Sync),
    ) -> Result<Self, PtySessionError> {
        #[cfg(unix)]
        {
            let group_id = _master
                .process_group_leader()
                .or_else(|| child.process_id().map(|process_id| process_id as i32))
                .ok_or(PtySessionError::MissingProcessId)?;
            Ok(Self { group_id })
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{
                Foundation::CloseHandle,
                System::JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
            };
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(PtySessionError::Io(std::io::Error::last_os_error()));
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            let assigned = child
                .as_raw_handle()
                .map(|process| unsafe { AssignProcessToJobObject(job, process.cast()) })
                .unwrap_or(0);
            if configured == 0 || assigned == 0 {
                let error = std::io::Error::last_os_error();
                unsafe {
                    CloseHandle(job);
                }
                return Err(PtySessionError::Io(error));
            }
            Ok(Self { job: job as usize })
        }
    }

    fn terminate(&self) {
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(-self.group_id, libc::SIGTERM);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe {
                libc::kill(-self.group_id, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(
                self.job as windows_sys::Win32::Foundation::HANDLE,
                1,
            );
        }
    }
}

#[cfg(windows)]
impl Drop for PtyProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(
                self.job as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

pub struct LocalPtySession {
    id: u64,
    workspace_epoch: WorkspaceEpoch,
    environment_epoch: EnvironmentEpoch,
    surface: Arc<RwLock<TerminalSurface>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_tree: Arc<PtyProcessTree>,
    cancellation: CancellationToken,
    state: Arc<AtomicU8>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    writer_thread: Mutex<Option<JoinHandle<()>>>,
}

impl LocalPtySession {
    pub fn spawn(
        id: u64,
        command: &CommandSpec,
        size: PtySize,
        workspace_epoch: WorkspaceEpoch,
        environment_epoch: EnvironmentEpoch,
        cancellation: CancellationToken,
    ) -> Result<Self, PtySessionError> {
        command
            .validate()
            .map_err(PtySessionError::InvalidCommand)?;
        if size.rows == 0 || size.cols == 0 {
            return Err(PtySessionError::InvalidSize);
        }
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        builder.cwd(&command.cwd);
        builder.env_clear();
        for (key, value) in &command.env {
            builder.env(key, value);
        }

        let pair = native_pty_system().openpty(size)?;
        let mut child = pair.slave.spawn_command(builder)?;
        let process_tree = match PtyProcessTree::attach(pair.master.as_ref(), child.as_ref()) {
            Ok(process_tree) => Arc::new(process_tree),
            Err(error) => {
                #[cfg(windows)]
                if let Some(process_id) = child.process_id() {
                    crate::process::supervisor::terminate_uncontained_process_tree(process_id);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        drop(pair.slave);
        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                process_tree.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => Arc::new(Mutex::new(writer)),
            Err(error) => {
                process_tree.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let surface = Arc::new(RwLock::new(TerminalSurface::new(
            size.cols,
            size.rows,
            DEFAULT_SCROLLBACK_LINES,
        )));
        let state = Arc::new(AtomicU8::new(PtySessionState::Running as u8));
        let child = Arc::new(Mutex::new(child));
        let (input, input_rx) = std::sync::mpsc::sync_channel(PTY_INPUT_QUEUE);
        let writer_thread = match spawn_writer(
            Arc::clone(&writer),
            input_rx,
            cancellation.clone(),
            Arc::clone(&state),
            Arc::clone(&child),
            Arc::clone(&process_tree),
        ) {
            Ok(thread) => thread,
            Err(error) => {
                process_tree.terminate();
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(error);
            }
        };
        let reader_thread = match spawn_reader(
            reader,
            Arc::clone(&surface),
            cancellation.clone(),
            Arc::clone(&state),
            Arc::clone(&child),
            Arc::clone(&process_tree),
        ) {
            Ok(thread) => thread,
            Err(error) => {
                cancellation.cancel();
                process_tree.terminate();
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = writer_thread.join();
                return Err(error);
            }
        };
        Ok(Self {
            id,
            workspace_epoch,
            environment_epoch,
            surface,
            master: Arc::new(Mutex::new(pair.master)),
            input,
            child,
            process_tree,
            cancellation,
            state,
            reader_thread: Mutex::new(Some(reader_thread)),
            writer_thread: Mutex::new(Some(writer_thread)),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn workspace_epoch(&self) -> WorkspaceEpoch {
        self.workspace_epoch
    }
    pub fn environment_epoch(&self) -> EnvironmentEpoch {
        self.environment_epoch
    }
    pub fn state(&self) -> PtySessionState {
        PtySessionState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn snapshot(&self) -> TerminalSurface {
        self.surface
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub fn surface_handle(&self) -> Arc<RwLock<TerminalSurface>> {
        Arc::clone(&self.surface)
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<(), PtySessionError> {
        if self.state() != PtySessionState::Running {
            return Err(PtySessionError::NotRunning(self.state()));
        }
        if bytes.len() > PTY_INPUT_CHUNK_LIMIT {
            return Err(PtySessionError::InputTooLarge(bytes.len()));
        }
        self.input
            .try_send(bytes.to_vec())
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => PtySessionError::InputBackpressure,
                std::sync::mpsc::TrySendError::Disconnected(_) => PtySessionError::InputClosed,
            })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtySessionError> {
        if rows == 0 || cols == 0 {
            return Err(PtySessionError::InvalidSize);
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master
            .lock()
            .map_err(|_| PtySessionError::Poisoned)?
            .resize(size)?;
        self.surface
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .resize(cols, rows);
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
        if self
            .state
            .swap(PtySessionState::Exited as u8, Ordering::AcqRel)
            == PtySessionState::Running as u8
        {
            self.process_tree.terminate();
            if let Ok(mut child) = self.child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    pub fn mark_lost(&self) {
        self.cancellation.cancel();
        self.state
            .store(PtySessionState::Lost as u8, Ordering::Release);
        self.process_tree.terminate();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }

    pub fn join_reader(&self) -> Result<(), PtySessionError> {
        if let Some(thread) = self
            .reader_thread
            .lock()
            .map_err(|_| PtySessionError::Poisoned)?
            .take()
        {
            thread.join().map_err(|_| PtySessionError::ReaderPanicked)?;
        }
        if let Some(thread) = self
            .writer_thread
            .lock()
            .map_err(|_| PtySessionError::Poisoned)?
            .take()
        {
            thread.join().map_err(|_| PtySessionError::WriterPanicked)?;
        }
        Ok(())
    }
}

impl Drop for LocalPtySession {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn spawn_writer(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    input: std::sync::mpsc::Receiver<Vec<u8>>,
    cancellation: CancellationToken,
    state: Arc<AtomicU8>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_tree: Arc<PtyProcessTree>,
) -> Result<JoinHandle<()>, PtySessionError> {
    std::thread::Builder::new()
        .name("hermito-pty-writer".into())
        .spawn(move || {
            while !cancellation.is_cancelled() {
                match input.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(bytes) => {
                        let result = writer
                            .lock()
                            .map_err(|_| ())
                            .and_then(|mut writer| writer.write_all(&bytes).map_err(|_| ()));
                        if result.is_err() {
                            cancellation.cancel();
                            state.store(PtySessionState::Exited as u8, Ordering::Release);
                            process_tree.terminate();
                            if let Ok(mut child) = child.lock() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            return;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .map_err(PtySessionError::SpawnWriter)
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    surface: Arc<RwLock<TerminalSurface>>,
    cancellation: CancellationToken,
    state: Arc<AtomicU8>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_tree: Arc<PtyProcessTree>,
) -> Result<JoinHandle<()>, PtySessionError> {
    std::thread::Builder::new()
        .name("hermito-pty-reader".into())
        .spawn(move || {
            let mut parser = VtParser::default();
            let mut chunk = vec![0_u8; PTY_READ_CHUNK];
            let mut total = 0_usize;
            while !cancellation.is_cancelled() {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        total = total.saturating_add(count);
                        if total > PTY_TOTAL_OUTPUT_BUDGET {
                            surface
                                .write()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .mark_truncated();
                            cancellation.cancel();
                            process_tree.terminate();
                            if let Ok(mut child) = child.lock() {
                                let _ = child.kill();
                            }
                            break;
                        }
                        parser.feed(
                            &chunk[..count],
                            &mut surface
                                .write()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            cancellation.cancel();
            if state.load(Ordering::Acquire) == PtySessionState::Running as u8 {
                state.store(PtySessionState::Exited as u8, Ordering::Release);
            }
            if let Ok(mut child) = child.lock() {
                let _ = child.wait();
            }
        })
        .map_err(PtySessionError::SpawnReader)
}

pub struct RemotePtySession {
    context: PtyStreamContext,
    surface: Arc<RwLock<TerminalSurface>>,
    state: Arc<AtomicU8>,
    multiplexer: crate::remote::multiplexer::Multiplexer,
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RemotePtySession {
    pub async fn spawn(
        multiplexer: crate::remote::multiplexer::Multiplexer,
        request: RequestEnvelope<PtySpawn>,
        cancellation: CancellationToken,
    ) -> Result<Self, PtySessionError> {
        request
            .payload
            .size
            .validate()
            .map_err(|_| PtySessionError::InvalidSize)?;
        let context = PtyStreamContext::from_spawn(&request);
        if context.document_revision.is_some() {
            return Err(PtySessionError::Protocol(
                "PTY stream document revision must be None".into(),
            ));
        }
        let size = request.payload.size;
        let mut receiver = multiplexer.register_pty(context.clone()).await?;
        let send_result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                let _ = multiplexer.cancel_pty(&context).await;
                return Err(PtySessionError::SpawnCancelled);
            }
            result = multiplexer.send(Message::Pty(PtyMessage::Spawn(request))) => result,
        };
        if let Err(error) = send_result {
            let _ = multiplexer.cancel_pty(&context).await;
            return Err(error.into());
        }
        let startup = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(PtySessionError::SpawnCancelled),
            result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                receiver.recv(),
            ) => match result {
                Ok(Some(message)) => match message.into_message() {
                    PtyMessage::Started {
                        context: started_context,
                        ..
                    } if started_context == context => Ok(()),
                    PtyMessage::Lost { reason, .. } => {
                        Err(PtySessionError::RemoteLost(reason))
                    }
                    _ => Err(PtySessionError::Protocol(
                        "expected remote PTY Started".into(),
                    )),
                },
                Ok(None) => Err(PtySessionError::RemoteLost("PTY stream closed".into())),
                Err(_) => Err(PtySessionError::Protocol(
                    "remote PTY spawn timed out".into(),
                )),
            },
        };
        if let Err(error) = startup {
            let _ = multiplexer.cancel_pty(&context).await;
            return Err(error);
        }
        let surface = Arc::new(RwLock::new(TerminalSurface::new(
            size.cols,
            size.rows,
            DEFAULT_SCROLLBACK_LINES,
        )));
        let state = Arc::new(AtomicU8::new(PtySessionState::Running as u8));
        let task_surface = Arc::clone(&surface);
        let task_state = Arc::clone(&state);
        let reader_context = context.clone();
        let reader_task = tokio::spawn(async move {
            let mut parser = VtParser::default();
            while let Some(message) = receiver.recv().await {
                match message.into_message() {
                    PtyMessage::Output {
                        context: chunk_context,
                        bytes,
                    } if chunk_context == reader_context => {
                        parser.feed(
                            &bytes,
                            &mut task_surface
                                .write()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        );
                    }
                    PtyMessage::Exited {
                        context: chunk_context,
                        truncated,
                        ..
                    } if chunk_context == reader_context => {
                        if truncated {
                            task_surface
                                .write()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .mark_truncated();
                        }
                        task_state.store(PtySessionState::Exited as u8, Ordering::Release);
                        return;
                    }
                    PtyMessage::Lost {
                        context: chunk_context,
                        ..
                    } if chunk_context == reader_context => {
                        task_state.store(PtySessionState::Lost as u8, Ordering::Release);
                        return;
                    }
                    _ => {}
                }
            }
            if task_state.load(Ordering::Acquire) == PtySessionState::Running as u8 {
                task_state.store(PtySessionState::Lost as u8, Ordering::Release);
            }
        });
        Ok(Self {
            context,
            surface,
            state,
            multiplexer,
            reader_task: Mutex::new(Some(reader_task)),
        })
    }

    pub fn id(&self) -> u64 {
        self.context.stream_id
    }
    pub fn workspace_epoch(&self) -> WorkspaceEpoch {
        self.context.workspace_epoch
    }
    pub fn environment_epoch(&self) -> EnvironmentEpoch {
        self.context.environment_epoch
    }
    pub fn state(&self) -> PtySessionState {
        PtySessionState::from_u8(self.state.load(Ordering::Acquire))
    }
    pub fn snapshot(&self) -> TerminalSurface {
        self.surface
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub fn surface_handle(&self) -> Arc<RwLock<TerminalSurface>> {
        Arc::clone(&self.surface)
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<(), PtySessionError> {
        if self.state() != PtySessionState::Running {
            return Err(PtySessionError::NotRunning(self.state()));
        }
        if bytes.len() > PTY_INPUT_CHUNK_LIMIT {
            return Err(PtySessionError::InputTooLarge(bytes.len()));
        }
        self.multiplexer.try_send(Message::Pty(PtyMessage::Input {
            context: self.context.clone(),
            bytes: bytes.to_vec(),
        }))?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtySessionError> {
        let size = WirePtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
        .validate()
        .map_err(|_| PtySessionError::InvalidSize)?;
        self.multiplexer.try_send(Message::Pty(PtyMessage::Resize {
            context: self.context.clone(),
            size,
        }))?;
        self.surface
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .resize(cols, rows);
        Ok(())
    }

    pub fn cancel(&self) -> Result<(), PtySessionError> {
        let result = self.multiplexer.try_send(Message::Pty(PtyMessage::Cancel {
            context: self.context.clone(),
        }));
        if result.is_err() {
            self.multiplexer
                .abort("remote PTY cancellation could not be delivered");
            self.state
                .store(PtySessionState::Lost as u8, Ordering::Release);
        } else {
            self.state
                .store(PtySessionState::Exited as u8, Ordering::Release);
        }
        result.map_err(Into::into)
    }
}

impl Drop for RemotePtySession {
    fn drop(&mut self) {
        if let Ok(mut task) = self.reader_task.lock() {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
        if self
            .multiplexer
            .try_send(Message::Pty(PtyMessage::Cancel {
                context: self.context.clone(),
            }))
            .is_err()
        {
            self.multiplexer
                .abort("remote PTY drop cancellation could not be delivered");
        }
    }
}

pub enum PtySession {
    Local(LocalPtySession),
    Remote(RemotePtySession),
}

impl PtySession {
    pub fn state(&self) -> PtySessionState {
        match self {
            Self::Local(session) => session.state(),
            Self::Remote(session) => session.state(),
        }
    }
    pub fn snapshot(&self) -> TerminalSurface {
        match self {
            Self::Local(session) => session.snapshot(),
            Self::Remote(session) => session.snapshot(),
        }
    }
    pub fn surface_handle(&self) -> Arc<RwLock<TerminalSurface>> {
        match self {
            Self::Local(session) => session.surface_handle(),
            Self::Remote(session) => session.surface_handle(),
        }
    }
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), PtySessionError> {
        match self {
            Self::Local(session) => session.write_input(bytes),
            Self::Remote(session) => session.write_input(bytes),
        }
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtySessionError> {
        match self {
            Self::Local(session) => session.resize(rows, cols),
            Self::Remote(session) => session.resize(rows, cols),
        }
    }
    pub fn cancel(&self) {
        match self {
            Self::Local(session) => session.cancel(),
            Self::Remote(session) => {
                let _ = session.cancel();
            }
        }
    }
    pub fn join_reader(&self) -> Result<(), PtySessionError> {
        match self {
            Self::Local(session) => session.join_reader(),
            Self::Remote(_) => Ok(()),
        }
    }
    pub fn workspace_epoch(&self) -> WorkspaceEpoch {
        match self {
            Self::Local(session) => session.workspace_epoch(),
            Self::Remote(session) => session.workspace_epoch(),
        }
    }
    pub fn environment_epoch(&self) -> EnvironmentEpoch {
        match self {
            Self::Local(session) => session.environment_epoch(),
            Self::Remote(session) => session.environment_epoch(),
        }
    }
}

#[derive(Debug, Error)]
pub enum PtySessionError {
    #[error("invalid command: {0}")]
    InvalidCommand(&'static str),
    #[error("PTY dimensions must be non-zero")]
    InvalidSize,
    #[error("PTY child has no process identifier")]
    MissingProcessId,
    #[error("PTY input chunk {0} exceeds 64 KiB")]
    InputTooLarge(usize),
    #[error("PTY input queue is full")]
    InputBackpressure,
    #[error("PTY input worker stopped")]
    InputClosed,
    #[error("PTY is not running ({0:?})")]
    NotRunning(PtySessionState),
    #[error("PTY state lock poisoned")]
    Poisoned,
    #[error("PTY reader thread panicked")]
    ReaderPanicked,
    #[error("PTY writer thread panicked")]
    WriterPanicked,
    #[error("failed to spawn PTY reader: {0}")]
    SpawnReader(std::io::Error),
    #[error("failed to spawn PTY writer: {0}")]
    SpawnWriter(std::io::Error),
    #[error("remote PTY transport failed: {0}")]
    RemoteTransport(#[from] crate::remote::multiplexer::MultiplexerError),
    #[error("remote PTY spawn was cancelled")]
    SpawnCancelled,
    #[error("remote PTY lost: {0}")]
    RemoteLost(String),
    #[error("remote PTY protocol failed: {0}")]
    Protocol(String),
    #[error("PTY I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("PTY backend failed: {0}")]
    Backend(#[from] anyhow::Error),
}
