use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use hermito_protocol::{
    frame::{AggregateBudget, FrameLimits},
    fs::{FileContent, FsMessage, WriteResult},
    lsp::{LspContext, LspV1},
    process::{ExecOutput, ProcessMessage},
    pty::{PtyMessage, PtySize as WirePtySize, PtyStreamContext, StreamId},
    request::{ExecutionContextV1, RequestEnvelope},
    response::{RemoteError, RemoteErrorCode, ResponseEnvelope},
    Message, MessageClass,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;

mod lsp;

const OUTBOUND_QUEUE: usize = 128;
const PTY_CHUNK: usize = 64 * 1024;
const PTY_SESSION_BUDGET: usize = 64 * 1024 * 1024;
const REMOTE_FILE_READ_LIMIT: u64 = hermito_protocol::fs::MAX_WIRE_FILE_BYTES;
const PTY_INPUT_CHUNK_LIMIT: usize = 64 * 1024;
const PTY_INPUT_QUEUE: usize = 8;
const MAX_PTY_SESSIONS: usize = 32;
const MAX_IN_FLIGHT_OPERATIONS: usize = 16;

#[derive(Clone)]
pub(crate) struct OutboundSender {
    sender: mpsc::Sender<OutboundMessage>,
    budget: Arc<Semaphore>,
    limits: FrameLimits,
    shutdown: CancellationToken,
}

struct OutboundMessage {
    message: Message,
    _permit: OwnedSemaphorePermit,
}

impl OutboundSender {
    async fn send(&self, message: Message) -> Result<()> {
        let permit = self.reserve_class(message.class()).await?;
        self.send_reserved(message, permit).await
    }

    async fn reserve_class(&self, class: MessageClass) -> Result<OwnedSemaphorePermit> {
        let permits = u32::try_from(self.limits.max_for(class))
            .expect("frame class limits fit the outbound aggregate semaphore");
        tokio::select! {
            _ = self.shutdown.cancelled() => {
                Err(anyhow::anyhow!("protocol transport shut down"))
            }
            result = Arc::clone(&self.budget).acquire_many_owned(permits) => {
                result.map_err(anyhow::Error::from)
            }
        }
    }

    async fn send_reserved(&self, message: Message, permit: OwnedSemaphorePermit) -> Result<()> {
        tokio::select! {
            _ = self.shutdown.cancelled() => {
                Err(anyhow::anyhow!("protocol transport shut down"))
            }
            result = self.sender.send(OutboundMessage {
                message,
                _permit: permit,
            }) => result.map_err(|_| anyhow::anyhow!("protocol writer stopped")),
        }
    }

    fn try_send(&self, message: Message) -> Result<(), ()> {
        if self.shutdown.is_cancelled() {
            return Err(());
        }
        let permit = Arc::clone(&self.budget)
            .try_acquire_many_owned(self.permits_for(&message))
            .map_err(|_| ())?;
        self.sender
            .try_send(OutboundMessage {
                message,
                _permit: permit,
            })
            .map_err(|_| ())
    }

    fn permits_for(&self, message: &Message) -> u32 {
        u32::try_from(self.limits.max_for(message.class()))
            .expect("frame class limits fit the outbound aggregate semaphore")
    }
}

struct RemotePty {
    context: PtyStreamContext,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    #[cfg(unix)]
    process_group: i32,
    input: std_mpsc::SyncSender<Vec<u8>>,
    cancellation: CancellationToken,
    cleanup_started: AtomicBool,
    cleanup_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Clone)]
struct DispatchState {
    operations: Arc<Semaphore>,
    ptys: Arc<AsyncMutex<HashMap<StreamId, Arc<RemotePty>>>>,
    exec_tokens: Arc<AsyncMutex<HashMap<uuid::Uuid, CancellationToken>>>,
    exec_tasks: Arc<AsyncMutex<Vec<tokio::task::JoinHandle<()>>>>,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<lsp::RemoteLsp>>>>,
    tx: OutboundSender,
    shutdown: CancellationToken,
}

impl RemotePty {
    fn cancel(&self) {
        self.cancellation.cancel();
        if self.cleanup_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let writer = Arc::clone(&self.writer);
        let child = Arc::clone(&self.child);
        #[cfg(unix)]
        let process_group = self.process_group;
        #[cfg(not(unix))]
        let process_group: Option<i32> = None;
        let handle = std::thread::spawn(move || {
            let _ = writer.lock().map(|mut w| w.flush());
            #[cfg(unix)]
            if process_group > 0 {
                unsafe {
                    libc::kill(-process_group, libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_millis(50));
                unsafe {
                    libc::kill(-process_group, libc::SIGKILL);
                }
            }
            let _ = child.lock().map(|mut c| {
                let _ = c.kill();
                let _ = c.wait();
            });
        });
        *self.cleanup_thread.lock().unwrap() = Some(handle);
    }

    fn join_cleanup(&self) {
        if let Some(handle) = self.cleanup_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let input = tokio::io::stdin();
    let output = tokio::io::stdout();
    run(input, output).await
}

async fn run<R, W>(mut input: R, output: W) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let limits = FrameLimits::default();
    let ingress_budget = AggregateBudget::new(limits.aggregate);
    let first_frame = hermito_protocol::read_frame(&mut input, limits, &ingress_budget)
        .await
        .context("reading protocol hello")?;
    let first_header_version = first_frame.header.version;
    let first = first_frame
        .decode_message()
        .context("decoding protocol hello")?;
    let peer_version = match first {
        Message::Hello { version } => version,
        _ => anyhow::bail!("first message must be protocol hello"),
    };
    let negotiated =
        hermito_protocol::negotiate(peer_version).context("protocol version mismatch")?;
    if first_header_version != peer_version {
        anyhow::bail!("hello frame version does not match advertised version");
    }
    let (outbound, mut rx) = mpsc::channel::<OutboundMessage>(OUTBOUND_QUEUE);
    let shutdown = CancellationToken::new();
    let tx = OutboundSender {
        sender: outbound,
        budget: Arc::new(Semaphore::new(limits.aggregate)),
        limits,
        shutdown: shutdown.clone(),
    };
    let writer_shutdown = shutdown.clone();
    let writer = tokio::spawn(async move {
        let mut output = output;
        loop {
            let queued = tokio::select! {
                _ = writer_shutdown.cancelled() => return Ok(()),
                queued = rx.recv() => {
                    let Some(queued) = queued else { return Ok(()) };
                    queued
                }
            };
            let result = tokio::select! {
                _ = writer_shutdown.cancelled() => return Ok(()),
                result = hermito_protocol::write_message_version(
                    &mut output,
                    &queued.message,
                    limits,
                    negotiated.0,
                ) => result,
            };
            if let Err(error) = result {
                writer_shutdown.cancel();
                return Err(error);
            }
        }
    });
    tx.send(Message::HelloAck {
        version: negotiated.0,
    })
    .await?;

    let ptys = Arc::new(AsyncMutex::new(HashMap::<StreamId, Arc<RemotePty>>::new()));
    let exec_tokens = Arc::new(AsyncMutex::new(HashMap::new()));
    let exec_tasks = Arc::new(AsyncMutex::new(Vec::new()));
    let lsps = Arc::new(AsyncMutex::new(HashMap::<LspContext, Arc<lsp::RemoteLsp>>::new()));
    let mut input_closed = false;
    let operations = Arc::new(Semaphore::new(MAX_IN_FLIGHT_OPERATIONS));
    let dispatch_state = DispatchState {
        operations: Arc::clone(&operations),
        ptys: Arc::clone(&ptys),
        exec_tokens: Arc::clone(&exec_tokens),
        exec_tasks: Arc::clone(&exec_tasks),
        lsps: Arc::clone(&lsps),
        tx: tx.clone(),
        shutdown: shutdown.clone(),
    };

    let service_result = async {
        loop {
            let frame_result = tokio::select! {
                _ = shutdown.cancelled() => break,
                result = hermito_protocol::read_frame(&mut input, limits, &ingress_budget) => result,
            };
            let frame = match frame_result {
                Ok(frame) => frame,
                Err(hermito_protocol::FrameError::Io(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    input_closed = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            hermito_protocol::dispatcher::validate_frame_version(
                frame.header.version,
                negotiated,
            )?;
            let received = frame.into_message()?;
            hermito_protocol::dispatcher::validate_for_dispatch(
                &received.message,
                negotiated,
            )?;
            let (message, frame) = received.into_parts();
            dispatch(message, frame, dispatch_state.clone()).await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if input_closed || service_result.is_err() {
        shutdown.cancel();
    }

    let sessions = ptys
        .lock()
        .await
        .drain()
        .map(|(_, session)| session)
        .collect::<Vec<_>>();
    for session in &sessions {
        session.cancel();
    }
    for session in sessions {
        session.join_cleanup();
    }
    for token in exec_tokens.lock().await.drain().map(|(_, token)| token) {
        token.cancel();
    }
    let tasks = exec_tasks.lock().await.drain(..).collect::<Vec<_>>();
    for task in tasks {
        let _ = task.await;
    }

    let lsp_sessions = lsps
        .lock()
        .await
        .drain()
        .map(|(_, s)| s)
        .collect::<Vec<_>>();
    for s in &lsp_sessions {
        s.cancel();
    }
    drop(lsp_sessions);

    if let Err(error) = service_result {
        writer.abort();
        let _ = writer.await;
        return Err(error);
    }
    if shutdown.is_cancelled() {
        writer.abort();
        let _ = writer.await;
        if input_closed {
            return Ok(());
        }
        anyhow::bail!("protocol transport shut down");
    }
    writer.await.context("joining protocol writer")??;
    Ok(())
}

async fn dispatch(
    message: Message,
    frame: hermito_protocol::frame::ReceivedFrame,
    state: DispatchState,
) -> Result<()> {
    let DispatchState {
        operations,
        ptys,
        exec_tokens,
        exec_tasks,
        lsps,
        tx,
        shutdown,
    } = state;
    match message {
        Message::Pty(PtyMessage::Spawn(request)) => {
            let context = PtyStreamContext::from_spawn(&request);
            if let Err(error) = spawn_pty(request, ptys, tx.clone(), shutdown).await {
                tx.send(Message::Pty(PtyMessage::Lost {
                    context,
                    reason: error.to_string(),
                }))
                .await?;
            }
            Ok(())
        }
        Message::Pty(PtyMessage::Input { context, bytes }) => {
            if let Some(session) = ptys.lock().await.get(&context.stream_id).cloned() {
                if session.context == context && !session.cancellation.is_cancelled() {
                    if bytes.len() > PTY_INPUT_CHUNK_LIMIT {
                        return lose_pty(&ptys, &tx, session, context, "PTY input exceeds 64 KiB")
                            .await;
                    }
                    match session.input.try_send(bytes) {
                        Ok(()) => {}
                        Err(std_mpsc::TrySendError::Full(_)) => {
                            return lose_pty(
                                &ptys,
                                &tx,
                                session,
                                context,
                                "PTY input queue exceeded",
                            )
                            .await;
                        }
                        Err(std_mpsc::TrySendError::Disconnected(_)) => {
                            return lose_pty(
                                &ptys,
                                &tx,
                                session,
                                context,
                                "PTY input worker stopped",
                            )
                            .await;
                        }
                    }
                }
            }
            Ok(())
        }
        Message::Pty(PtyMessage::Resize { context, size }) => {
            if let Some(session) = ptys.lock().await.get(&context.stream_id).cloned() {
                if session.context == context {
                    if let Err(error) = session
                        .master
                        .lock()
                        .map_err(|_| anyhow::anyhow!("PTY master poisoned"))
                        .and_then(|master| master.resize(to_portable_size(size)))
                    {
                        return lose_pty(
                            &ptys,
                            &tx,
                            session,
                            context,
                            &format!("PTY resize failed: {error}"),
                        )
                        .await;
                    }
                }
            }
            Ok(())
        }
        Message::Pty(PtyMessage::Cancel { context }) => {
            if let Some(session) = remove_matching_pty(&ptys, &context).await {
                session.cancel();
                tx.send(Message::Pty(PtyMessage::Exited {
                    context,
                    exit_code: None,
                    truncated: false,
                }))
                .await?;
            }
            Ok(())
        }
        Message::Fs(FsMessage::Read(request)) => {
            if request.document_revision.is_none() {
                tx.send(Message::Fs(FsMessage::ReadResult(error_response(
                    &request,
                    RemoteErrorCode::InvalidRequest,
                    "document file request requires a document revision",
                ))))
                .await?;
                return Ok(());
            }
            let permit = match Arc::clone(&operations).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tx.send(Message::Fs(FsMessage::ReadResult(error_response(
                        &request,
                        RemoteErrorCode::OutputLimit,
                        "concurrent request limit reached",
                    ))))
                    .await?;
                    return Ok(());
                }
            };
            tokio::spawn(async move {
                let _frame = frame;
                let _permit = permit;
                let Ok(output_permit) = tx.reserve_class(MessageClass::Fs).await else {
                    return;
                };
                handle_read(request, tx, output_permit).await;
            });
            Ok(())
        }
        Message::Fs(FsMessage::Write(request)) => {
            if request.document_revision.is_none() {
                tx.send(Message::Fs(FsMessage::WriteResult(error_response(
                    &request,
                    RemoteErrorCode::InvalidRequest,
                    "document file request requires a document revision",
                ))))
                .await?;
                return Ok(());
            }
            let permit = match Arc::clone(&operations).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tx.send(Message::Fs(FsMessage::WriteResult(error_response(
                        &request,
                        RemoteErrorCode::OutputLimit,
                        "concurrent request limit reached",
                    ))))
                    .await?;
                    return Ok(());
                }
            };
            tokio::spawn(async move {
                let _frame = frame;
                let _permit = permit;
                let Ok(output_permit) = tx.reserve_class(MessageClass::Fs).await else {
                    return;
                };
                handle_write(request, tx, output_permit).await;
            });
            Ok(())
        }
        Message::Process(ProcessMessage::Exec(request)) => {
            let permit = match Arc::clone(&operations).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tx.send(Message::Process(ProcessMessage::Result(error_response(
                        &request,
                        RemoteErrorCode::OutputLimit,
                        "concurrent request limit reached",
                    ))))
                    .await?;
                    return Ok(());
                }
            };
            let token = CancellationToken::new();
            {
                let mut tokens = exec_tokens.lock().await;
                if tokens.contains_key(&request.request_id) {
                    drop(tokens);
                    anyhow::bail!("duplicate process request ID");
                }
                tokens.insert(request.request_id, token.clone());
            }
            let task = tokio::spawn(async move {
                let _frame = frame;
                let _permit = permit;
                let output_permit = tokio::select! {
                    _ = token.cancelled() => return,
                    result = tx.reserve_class(MessageClass::Process) => {
                        let Ok(permit) = result else { return; };
                        permit
                    }
                };
                handle_exec(request, token, exec_tokens, tx, output_permit).await;
            });
            let mut tasks = exec_tasks.lock().await;
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
            Ok(())
        }
        Message::Process(ProcessMessage::Cancel { request_id }) => {
            if let Some(token) = exec_tokens.lock().await.remove(&request_id) {
                token.cancel();
            }
            Ok(())
        }
        Message::Lsp(lsp) => {
            if matches!(
                &lsp,
                LspV1::Started { .. }
                    | LspV1::Exited { .. }
                    | LspV1::JsonRpcResponse { .. }
                    | LspV1::JsonRpcNotification { .. }
                    | LspV1::PublishDiagnostics { .. }
                    | LspV1::WorkspaceEdit { .. }
            ) {
                anyhow::bail!("LSP response/notification received as a request");
            }
            let lsps = Arc::clone(&lsps);
            let tx = tx.clone();
            let sd = shutdown.clone();
            tokio::spawn(async move {
                let _frame = frame;
                let _ = lsp::handle_lsp(lsp, lsps, tx, sd).await;
            });
            Ok(())
        }
        Message::Hello { .. } | Message::HelloAck { .. } => {
            anyhow::bail!("duplicate protocol negotiation")
        }
        Message::Git(_) | Message::Container(_) | Message::Relay(_) => {
            anyhow::bail!("protocol family reserved but not enabled")
        }
        Message::Pty(_) | Message::Fs(_) | Message::Process(_) => {
            anyhow::bail!("response-only message received as a request")
        }
    }
}
fn error_response<U, T>(
    request: &RequestEnvelope<U>,
    code: RemoteErrorCode,
    message: &str,
) -> ResponseEnvelope<T> {
    ResponseEnvelope {
        request_id: request.request_id,
        workspace_epoch: request.workspace_epoch,
        environment_epoch: request.environment_epoch,
        document_revision: request.document_revision,
        execution_context: request.execution_context.clone(),
        payload: Err(RemoteError {
            code,
            message: message.to_string(),
        }),
    }
}

async fn lose_pty(
    ptys: &Arc<AsyncMutex<HashMap<StreamId, Arc<RemotePty>>>>,
    tx: &OutboundSender,
    session: Arc<RemotePty>,
    context: PtyStreamContext,
    reason: &str,
) -> Result<()> {
    ptys.lock().await.remove(&context.stream_id);
    session.cancel();
    tx.send(Message::Pty(PtyMessage::Lost {
        context,
        reason: reason.to_string(),
    }))
    .await
}

async fn remove_matching_pty(
    ptys: &AsyncMutex<HashMap<StreamId, Arc<RemotePty>>>,
    context: &PtyStreamContext,
) -> Option<Arc<RemotePty>> {
    let mut guard = ptys.lock().await;
    if let Some(s) = guard.get(&context.stream_id) {
        if s.context == *context {
            return guard.remove(&context.stream_id);
        }
    }
    None
}

async fn spawn_pty(
    request: RequestEnvelope<hermito_protocol::pty::PtySpawn>,
    ptys: Arc<AsyncMutex<HashMap<StreamId, Arc<RemotePty>>>>,
    tx: OutboundSender,
    shutdown: CancellationToken,
) -> Result<()> {
    if let Err(error) = validated_execution_context(&request.execution_context) {
        anyhow::bail!(error.message);
    }
    request
        .payload
        .command
        .validate()
        .map_err(anyhow::Error::msg)?;
    request
        .payload
        .size
        .validate()
        .map_err(anyhow::Error::msg)?;
    let context = PtyStreamContext::from_spawn(&request);
    if context.document_revision.is_some() {
        anyhow::bail!("PTY stream document revision must be None");
    }
    let stream_id = context.stream_id;
    let mut sessions = ptys.lock().await;
    if sessions.contains_key(&stream_id) {
        anyhow::bail!("duplicate PTY stream id {stream_id}");
    }
    if sessions.len() >= MAX_PTY_SESSIONS {
        anyhow::bail!("remote PTY session limit reached");
    }

    let mut command = CommandBuilder::new(&request.payload.command.program);
    command.args(&request.payload.command.args);
    command.cwd(&request.payload.command.cwd);
    command.env_clear();
    for (key, value) in &request.payload.command.env {
        command.env(key, value);
    }
    let pair = native_pty_system().openpty(to_portable_size(request.payload.size))?;
    let mut child = pair.slave.spawn_command(command)?;
    #[cfg(unix)]
    let process_group = match pair
        .master
        .process_group_leader()
        .or_else(|| child.process_id().map(|process_id| process_id as i32))
    {
        Some(process_group) => process_group,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("remote PTY child has no process identifier");
        }
    };
    #[cfg(unix)]
    let cleanup_process_group = Some(process_group);
    #[cfg(not(unix))]
    let cleanup_process_group: Option<i32> = None;
    drop(pair.slave);
    let process_id = child.process_id();
    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            terminate_unregistered_pty(child.as_mut(), cleanup_process_group);
            return Err(error);
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => Arc::new(Mutex::new(writer)),
        Err(error) => {
            terminate_unregistered_pty(child.as_mut(), cleanup_process_group);
            return Err(error);
        }
    };
    let cancellation = CancellationToken::new();
    let (input, input_rx) = std_mpsc::sync_channel::<Vec<u8>>(PTY_INPUT_QUEUE);
    let session = Arc::new(RemotePty {
        context: context.clone(),
        master: Arc::new(Mutex::new(pair.master)),
        writer: Arc::clone(&writer),
        child: Arc::new(Mutex::new(child)),
        #[cfg(unix)]
        process_group,
        input,
        cancellation: cancellation.clone(),
        cleanup_started: AtomicBool::new(false),
        cleanup_thread: Mutex::new(None),
    });
    sessions.insert(stream_id, Arc::clone(&session));
    drop(sessions);
    if let Err(error) = tx
        .send(Message::Pty(PtyMessage::Started {
            context: context.clone(),
            process_id,
        }))
        .await
    {
        if let Some(session) = ptys.lock().await.remove(&stream_id) {
            session.cancel();
        }
        return Err(error);
    }

    let tx_output = tx.clone();
    let ptys_output = Arc::clone(&ptys);
    let output_cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || {
        drain_pty(
            reader,
            context,
            output_cancellation,
            tx_output,
            ptys_output,
            shutdown,
        )
    });
    let input_session = Arc::clone(&session);
    tokio::task::spawn_blocking(move || {
        while !input_session.cancellation.is_cancelled() {
            match input_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => {
                    let result = writer
                        .lock()
                        .map_err(|_| ())
                        .and_then(|mut writer| writer.write_all(&bytes).map_err(|_| ()));
                    if result.is_err() {
                        input_session.cancel();
                        return;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    Ok(())
}

fn terminate_unregistered_pty(child: &mut dyn Child, process_group: Option<i32>) {
    #[cfg(unix)]
    if let Some(pg) = process_group {
        unsafe { libc::kill(-pg, libc::SIGTERM); }
        std::thread::sleep(Duration::from_millis(50));
        unsafe { libc::kill(-pg, libc::SIGKILL); }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn drain_pty(
    mut reader: Box<dyn Read + Send>,
    context: PtyStreamContext,
    cancellation: CancellationToken,
    tx: OutboundSender,
    ptys: Arc<AsyncMutex<HashMap<StreamId, Arc<RemotePty>>>>,
    shutdown: CancellationToken,
) {
    let mut buffer = [0u8; PTY_CHUNK];
    let mut total: u64 = 0;
    let budget = PTY_SESSION_BUDGET as u64;
    loop {
        if cancellation.is_cancelled() || shutdown.is_cancelled() {
            break;
        }
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let chunk = &buffer[..n];
        total += n as u64;
        let truncated = total > budget;
        let to_send = if truncated {
            &chunk[..chunk.len().saturating_sub((total - budget) as usize)]
        } else {
            chunk
        };
        if !to_send.is_empty() {
            if tx
                .try_send(Message::Pty(PtyMessage::Output {
                    context: context.clone(),
                    bytes: to_send.to_vec(),
                }))
                .is_err()
            {
                break;
            }
        }
        if truncated {
            break;
        }
    }
    let _ = tx.try_send(Message::Pty(PtyMessage::Exited {
        context,
        exit_code: None,
        truncated: total > budget,
    }));
}

async fn handle_read(
    request: RequestEnvelope<hermito_protocol::fs::ReadFile>,
    tx: OutboundSender,
    output_permit: OwnedSemaphorePermit,
) {
    let response_context = validated_execution_context(&request.execution_context);
    let result = match &response_context {
        Ok(_) => {
            let path = &request.payload.path;
            if path.as_bytes().contains(&0) {
                Err(RemoteError {
                    code: RemoteErrorCode::InvalidRequest,
                    message: "path contains NUL".into(),
                })
            } else {
                match tokio::fs::read(path).await {
                    Ok(bytes) => {
                        if bytes.len() as u64 > REMOTE_FILE_READ_LIMIT {
                            Err(RemoteError {
                                code: RemoteErrorCode::OutputLimit,
                                message: "file too large".into(),
                            })
                        } else {
                            Ok(FileContent { bytes })
                        }
                    }
                    Err(e) => Err(remote_error(e.into())),
                }
            }
        }
        Err(e) => Err(e.clone()),
    };
    let response = ResponseEnvelope {
        request_id: request.request_id,
        workspace_epoch: request.workspace_epoch,
        environment_epoch: request.environment_epoch,
        document_revision: request.document_revision,
        execution_context: response_context.unwrap_or(ExecutionContextV1::AuthorityRoot),
        payload: result,
    };
    let _ = tx
        .send_reserved(Message::Fs(FsMessage::ReadResult(response)), output_permit)
        .await;
}

async fn handle_write(
    request: RequestEnvelope<hermito_protocol::fs::WriteFile>,
    tx: OutboundSender,
    output_permit: OwnedSemaphorePermit,
) {
    let response_context = validated_execution_context(&request.execution_context);
    let result = match &response_context {
        Ok(_) => {
            let path = &request.payload.path;
            if path.as_bytes().contains(&0) || request.payload.bytes.as_slice().contains(&0) {
                Err(RemoteError {
                    code: RemoteErrorCode::InvalidRequest,
                    message: "path or content contains NUL".into(),
                })
            } else {
                match tokio::fs::write(path, &request.payload.bytes).await {
                    Ok(()) => Ok(WriteResult { bytes_written: request.payload.bytes.len() as u64 }),
                    Err(e) => Err(remote_error(e.into())),
                }
            }
        }
        Err(e) => Err(e.clone()),
    };
    let response = ResponseEnvelope {
        request_id: request.request_id,
        workspace_epoch: request.workspace_epoch,
        environment_epoch: request.environment_epoch,
        document_revision: request.document_revision,
        execution_context: response_context.unwrap_or(ExecutionContextV1::AuthorityRoot),
        payload: result,
    };
    let _ = tx
        .send_reserved(Message::Fs(FsMessage::WriteResult(response)), output_permit)
        .await;
}

async fn handle_exec(
    request: RequestEnvelope<hermito_protocol::process::ExecRequest>,
    token: CancellationToken,
    tokens: Arc<AsyncMutex<HashMap<uuid::Uuid, CancellationToken>>>,
    tx: OutboundSender,
    output_permit: OwnedSemaphorePermit,
) {
    let response_context = validated_execution_context(&request.execution_context);
    let result = match &response_context {
        Ok(_) => execute(&request.payload, &token).await,
        Err(error) => Err(error.clone()),
    };
    tokens.lock().await.remove(&request.request_id);
    let response = ResponseEnvelope {
        request_id: request.request_id,
        workspace_epoch: request.workspace_epoch,
        environment_epoch: request.environment_epoch,
        document_revision: request.document_revision,
        execution_context: response_context.unwrap_or(ExecutionContextV1::AuthorityRoot),
        payload: result,
    };
    let _ = tx
        .send_reserved(
            Message::Process(ProcessMessage::Result(response)),
            output_permit,
        )
        .await;
}

fn validated_execution_context(
    context: &ExecutionContextV1,
) -> Result<ExecutionContextV1, RemoteError> {
    match context {
        ExecutionContextV1::AuthorityRoot => Ok(context.clone()),
        ExecutionContextV1::DevContainer { .. } => Err(RemoteError {
            code: RemoteErrorCode::InvalidRequest,
            message: "development-container execution context is not enabled".into(),
        }),
    }
}

async fn execute(
    request: &hermito_protocol::process::ExecRequest,
    cancellation: &CancellationToken,
) -> Result<ExecOutput, RemoteError> {
    request.command.validate().map_err(|message| RemoteError {
        code: RemoteErrorCode::InvalidRequest,
        message: message.into(),
    })?;
    let mut command = Command::new(&request.command.program);
    command
        .args(&request.command.args)
        .current_dir(&request.command.cwd)
        .env_clear()
        .envs(&request.command.env)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| remote_error(error.into()))?;
    #[cfg(unix)]
    let process_group = child.id().map(|process_id| process_id as i32);
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate(&mut child).await;
            return Err(RemoteError {
                code: RemoteErrorCode::Internal,
                message: "stdout pipe unavailable".into(),
            });
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdout);
            terminate(&mut child).await;
            return Err(RemoteError {
                code: RemoteErrorCode::Internal,
                message: "stderr pipe unavailable".into(),
            });
        }
    };
    let stdout_task = tokio::spawn(read_limited(
        stdout,
        request
            .stdout_limit
            .min(hermito_protocol::process::MAX_WIRE_OUTPUT_BYTES),
    ));
    let stderr_task = tokio::spawn(read_limited(
        stderr,
        request
            .stderr_limit
            .min(hermito_protocol::process::MAX_WIRE_OUTPUT_BYTES),
    ));
    let timeout = Duration::from_millis(request.timeout_ms.max(1));
    let status = tokio::select! {
        _ = cancellation.cancelled() => {
            terminate(&mut child).await;
            return Err(RemoteError {
                code: RemoteErrorCode::Cancelled,
                message: "process cancelled".into(),
            });
        }
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                terminate(&mut child).await;
                return Err(remote_error(error.into()));
            }
            Err(_) => {
                terminate(&mut child).await;
                return Err(RemoteError {
                    code: RemoteErrorCode::TimedOut,
                    message: "process wall-time limit exceeded".into(),
                });
            }
        }
    };
    #[cfg(unix)]
    if let Some(process_group) = process_group {
        cleanup_process_group(process_group).await;
    }
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| remote_error(error.into()))?
        .map_err(|error| remote_error(error.into()))?;
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| remote_error(error.into()))?
        .map_err(|error| remote_error(error.into()))?;
    Ok(ExecOutput {
        exit_code: status.code(),
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

async fn read_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: u64,
) -> std::io::Result<(Vec<u8>, bool)> {
    let limit = usize::try_from(limit)
        .unwrap_or(usize::MAX)
        .min(16 * 1024 * 1024);
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining {
            truncated = true;
        }
    }
    Ok((output, truncated))
}

#[cfg(unix)]
async fn cleanup_process_group(process_group: i32) {
    unsafe {
        libc::kill(-process_group, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(process_id) = child.id() {
        unsafe {
            libc::kill(-(process_id as i32), libc::SIGTERM);
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
        unsafe {
            libc::kill(-(process_id as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

fn to_portable_size(size: WirePtySize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn remote_error(error: anyhow::Error) -> RemoteError {
    let code = match error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
    {
        Some(std::io::ErrorKind::NotFound) => RemoteErrorCode::NotFound,
        Some(std::io::ErrorKind::PermissionDenied) => RemoteErrorCode::PermissionDenied,
        _ => RemoteErrorCode::Internal,
    };
    RemoteError {
        code,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}