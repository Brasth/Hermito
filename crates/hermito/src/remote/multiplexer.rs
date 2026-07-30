use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use hermito_protocol::{
    frame::{AggregateBudget, FrameLimits, ReceivedMessage},
    fs::FsMessage,
    process::ProcessMessage,
    pty::{PtyMessage, PtyStreamContext, StreamId},
    Message, ProtocolVersion, CURRENT_VERSION,
};
use thiserror::Error;
use tokio::{
    process::Child,
    sync::{mpsc, oneshot, watch, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OUTBOUND_QUEUE: usize = 128;
const PTY_STREAM_QUEUE: usize = 32;
const MAX_PENDING_REQUESTS: usize = 64;

fn encode_version(version: ProtocolVersion) -> u16 {
    u16::from_be_bytes([version.major, version.minor])
}

fn decode_version(version: u16) -> ProtocolVersion {
    let [major, minor] = version.to_be_bytes();
    ProtocolVersion { major, minor }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportLoss {
    pub generation: u64,
    pub reason: String,
}

pub(crate) enum RoutedPtyMessage {
    Received(ReceivedMessage),
    Synthetic(PtyMessage),
}

impl RoutedPtyMessage {
    pub(crate) fn into_message(self) -> PtyMessage {
        match self {
            Self::Received(received) => match received.message {
                Message::Pty(message) => message,
                _ => unreachable!("only PTY messages are routed to PTY streams"),
            },
            Self::Synthetic(message) => message,
        }
    }
}

#[derive(Clone)]
pub struct Multiplexer {
    inner: Arc<Inner>,
}

struct Inner {
    outbound: mpsc::Sender<Message>,
    pending: Mutex<HashMap<Uuid, oneshot::Sender<ReceivedMessage>>>,
    pty_streams: Mutex<HashMap<PtyStreamContext, mpsc::Sender<RoutedPtyMessage>>>,
    child: Mutex<Child>,
    alive: AtomicBool,
    protocol_version: AtomicU16,
    negotiated: AtomicBool,
    generation: AtomicU64,
    loss: watch::Sender<Option<TransportLoss>>,
    shutdown: CancellationToken,
}

impl Multiplexer {
    pub async fn start(mut child: Child, generation: u64) -> Result<Self, MultiplexerError> {
        let stdin = child
            .stdin
            .take()
            .ok_or(MultiplexerError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(MultiplexerError::MissingPipe("stdout"))?;
        let (outbound, mut outbound_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE);
        let (loss, _) = watch::channel(None);
        let shutdown = CancellationToken::new();
        let inner = Arc::new(Inner {
            outbound,
            pending: Mutex::new(HashMap::new()),
            pty_streams: Mutex::new(HashMap::new()),
            child: Mutex::new(child),
            alive: AtomicBool::new(true),
            protocol_version: AtomicU16::new(encode_version(CURRENT_VERSION)),
            negotiated: AtomicBool::new(false),
            generation: AtomicU64::new(generation),
            loss,
            shutdown,
        });

        let writer_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let mut stdin = stdin;
            let limits = FrameLimits::default();
            loop {
                let message = tokio::select! {
                    _ = writer_inner.shutdown.cancelled() => return,
                    message = outbound_rx.recv() => {
                        let Some(message) = message else { return };
                        message
                    }
                };
                let version = decode_version(writer_inner.protocol_version.load(Ordering::Acquire));
                let result = tokio::select! {
                    _ = writer_inner.shutdown.cancelled() => return,
                    result = hermito_protocol::write_message_version(
                        &mut stdin,
                        &message,
                        limits,
                        version,
                    ) => result,
                };
                if let Err(error) = result {
                    mark_lost(&writer_inner, format!("protocol write failed: {error}")).await;
                    return;
                }
            }
        });

        let reader_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let mut stdout = stdout;
            let limits = FrameLimits::default();
            let budget = AggregateBudget::new(limits.aggregate);
            loop {
                let frame = match hermito_protocol::read_frame(&mut stdout, limits, &budget).await {
                    Ok(frame) => frame,
                    Err(error) => {
                        mark_lost(&reader_inner, format!("protocol read failed: {error}")).await;
                        return;
                    }
                };
                let frame_version = frame.header.version;
                let received = match frame.into_message() {
                    Ok(received) => received,
                    Err(error) => {
                        mark_lost(&reader_inner, format!("protocol decode failed: {error}")).await;
                        return;
                    }
                };

                if !reader_inner.negotiated.load(Ordering::Acquire) {
                    let Message::HelloAck { version } = &received.message else {
                        mark_lost(
                            &reader_inner,
                            "expected HelloAck as first protocol response".into(),
                        )
                        .await;
                        return;
                    };
                    let negotiated = match hermito_protocol::negotiate(*version) {
                        Ok(negotiated) if negotiated.0 == *version => negotiated,
                        Ok(_) => {
                            mark_lost(
                                &reader_inner,
                                "HelloAck did not contain the negotiated protocol version".into(),
                            )
                            .await;
                            return;
                        }
                        Err(error) => {
                            mark_lost(
                                &reader_inner,
                                format!("protocol negotiation failed: {error}"),
                            )
                            .await;
                            return;
                        }
                    };
                    if frame_version != negotiated.0 {
                        mark_lost(
                            &reader_inner,
                            "HelloAck frame version does not match its payload".into(),
                        )
                        .await;
                        return;
                    }
                    reader_inner
                        .protocol_version
                        .store(encode_version(negotiated.0), Ordering::Release);
                    reader_inner.negotiated.store(true, Ordering::Release);
                } else if let Err(error) = hermito_protocol::dispatcher::validate_frame_version(
                    frame_version,
                    hermito_protocol::dispatcher::NegotiatedVersion(decode_version(
                        reader_inner.protocol_version.load(Ordering::Acquire),
                    )),
                ) {
                    mark_lost(
                        &reader_inner,
                        format!("protocol frame version failed: {error}"),
                    )
                    .await;
                    return;
                }
                route_message(&reader_inner, received).await;
            }
        });

        let this = Self { inner };
        let (hello_tx, hello_rx) = oneshot::channel();
        this.inner
            .pending
            .lock()
            .await
            .insert(Uuid::nil(), hello_tx);
        if let Err(error) = this
            .send(Message::Hello {
                version: CURRENT_VERSION,
            })
            .await
        {
            this.shutdown().await;
            return Err(error);
        }
        let negotiation = match tokio::time::timeout(Duration::from_secs(5), hello_rx).await {
            Ok(Ok(received)) => match received.message {
                Message::HelloAck { version } => hermito_protocol::negotiate(version)
                    .map(|_| ())
                    .map_err(|error| MultiplexerError::Version(error.to_string())),
                _ => Err(MultiplexerError::Version("expected HelloAck".into())),
            },
            Ok(Err(_)) => Err(MultiplexerError::Closed),
            Err(_) => Err(MultiplexerError::NegotiationTimeout),
        };
        if let Err(error) = negotiation {
            this.shutdown().await;
            return Err(error);
        }
        Ok(this)
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    pub(crate) fn is_same_transport(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn subscribe_loss(&self) -> watch::Receiver<Option<TransportLoss>> {
        self.inner.loss.subscribe()
    }

    pub async fn send(&self, message: Message) -> Result<(), MultiplexerError> {
        if !self.is_alive() {
            return Err(MultiplexerError::Closed);
        }
        self.inner
            .outbound
            .send(message)
            .await
            .map_err(|_| MultiplexerError::Closed)
    }

    pub fn try_send(&self, message: Message) -> Result<(), MultiplexerError> {
        if !self.is_alive() {
            return Err(MultiplexerError::Closed);
        }
        self.inner
            .outbound
            .try_send(message)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => MultiplexerError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => MultiplexerError::Closed,
            })
    }

    pub fn abort(&self, reason: impl Into<String>) {
        let inner = Arc::clone(&self.inner);
        let reason = reason.into();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                mark_lost(&inner, reason).await;
            });
        } else {
            std::thread::spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(mark_lost(&inner, reason));
                }
            });
        }
    }

    pub async fn request(
        &self,
        request_id: Uuid,
        message: Message,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<Message, MultiplexerError> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().await;
            if pending.len() >= MAX_PENDING_REQUESTS {
                return Err(MultiplexerError::Backpressure);
            }
            if pending.contains_key(&request_id) {
                return Err(MultiplexerError::DuplicateRequest(request_id));
            }
            pending.insert(request_id, sender);
        }
        if let Err(error) = self.send(message).await {
            self.inner.pending.lock().await.remove(&request_id);
            return Err(error);
        }
        tokio::select! {
            _ = cancellation.cancelled() => {
                self.inner.pending.lock().await.remove(&request_id);
                Err(MultiplexerError::Cancelled)
            }
            result = tokio::time::timeout(timeout, receiver) => match result {
                Ok(Ok(received)) => Ok(received.message),
                Ok(Err(_)) => Err(MultiplexerError::Closed),
                Err(_) => {
                    self.inner.pending.lock().await.remove(&request_id);
                    Err(MultiplexerError::TimedOut)
                }
            }
        }
    }

    pub(crate) async fn register_pty(
        &self,
        context: PtyStreamContext,
    ) -> Result<mpsc::Receiver<RoutedPtyMessage>, MultiplexerError> {
        if context.document_revision.is_some() {
            return Err(MultiplexerError::InvalidStreamContext);
        }
        let (sender, receiver) = mpsc::channel(PTY_STREAM_QUEUE);
        let mut streams = self.inner.pty_streams.lock().await;
        if streams.contains_key(&context) {
            return Err(MultiplexerError::DuplicateStream(context.stream_id));
        }
        streams.insert(context, sender);
        Ok(receiver)
    }

    pub async fn cancel_pty(&self, context: &PtyStreamContext) -> Result<(), MultiplexerError> {
        self.inner.pty_streams.lock().await.remove(context);
        self.send(Message::Pty(PtyMessage::Cancel {
            context: context.clone(),
        }))
        .await
    }

    pub async fn shutdown(&self) {
        mark_lost(&self.inner, "transport shut down".into()).await;
    }
}

async fn route_message(inner: &Arc<Inner>, received: ReceivedMessage) {
    if let Message::HelloAck { .. } = &received.message {
        if let Some(sender) = inner.pending.lock().await.remove(&Uuid::nil()) {
            let _ = sender.send(received);
        } else {
            mark_lost(inner, "unexpected duplicate protocol HelloAck".into()).await;
        }
        return;
    }
    let request_id = match &received.message {
        Message::Fs(FsMessage::ReadResult(response)) => Some(response.request_id),
        Message::Fs(FsMessage::WriteResult(response)) => Some(response.request_id),
        Message::Process(ProcessMessage::Result(response)) => Some(response.request_id),
        _ => None,
    };
    if matches!(
        &received.message,
        Message::Hello { .. }
            | Message::Fs(FsMessage::Read(_) | FsMessage::Write(_))
            | Message::Process(ProcessMessage::Exec(_) | ProcessMessage::Cancel { .. })
            | Message::Pty(
                PtyMessage::Spawn(_)
                    | PtyMessage::Input { .. }
                    | PtyMessage::Resize { .. }
                    | PtyMessage::Cancel { .. }
            )
    ) {
        mark_lost(
            inner,
            "remote helper sent a request-direction protocol message".into(),
        )
        .await;
        return;
    }
    if let Some(request_id) = request_id {
        if let Some(sender) = inner.pending.lock().await.remove(&request_id) {
            let _ = sender.send(received);
        }
        return;
    }
    if let Message::Pty(pty_message) = &received.message {
        let context = match pty_message {
            PtyMessage::Started { context, .. }
            | PtyMessage::Output { context, .. }
            | PtyMessage::Exited { context, .. }
            | PtyMessage::Lost { context, .. } => Some(context.clone()),
            _ => None,
        };
        if let Some(context) = context {
            let cancel_on_drop = matches!(
                pty_message,
                PtyMessage::Started { .. } | PtyMessage::Output { .. }
            );
            let terminal = matches!(
                pty_message,
                PtyMessage::Exited { .. } | PtyMessage::Lost { .. }
            );
            let sender = {
                let mut streams = inner.pty_streams.lock().await;
                if terminal {
                    streams.remove(&context)
                } else {
                    streams.get(&context).cloned()
                }
            };
            if let Some(sender) = sender {
                if sender
                    .try_send(RoutedPtyMessage::Received(received))
                    .is_err()
                {
                    inner.pty_streams.lock().await.remove(&context);
                    if cancel_on_drop
                        && inner
                            .outbound
                            .try_send(Message::Pty(PtyMessage::Cancel { context }))
                            .is_err()
                    {
                        mark_lost(inner, "unable to cancel backpressured remote PTY".into()).await;
                    }
                }
            }
        }
        return;
    }
    mark_lost(
        inner,
        "remote helper sent an unsupported protocol message".into(),
    )
    .await;
}

async fn mark_lost(inner: &Arc<Inner>, reason: String) {
    if !inner.alive.swap(false, Ordering::AcqRel) {
        return;
    }
    inner.shutdown.cancel();
    let generation = inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
    for (context, sender) in inner.pty_streams.lock().await.drain() {
        let _ = sender.try_send(RoutedPtyMessage::Synthetic(PtyMessage::Lost {
            context,
            reason: reason.clone(),
        }));
    }
    inner.pending.lock().await.clear();
    let _ = inner.loss.send(Some(TransportLoss { generation, reason }));
    let mut child = inner.child.lock().await;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[derive(Debug, Error)]
pub enum MultiplexerError {
    #[error("remote helper {0} pipe unavailable")]
    MissingPipe(&'static str),
    #[error("remote protocol version rejected: {0}")]
    Version(String),
    #[error("duplicate stream {0}")]
    DuplicateStream(StreamId),
    #[error("duplicate request {0}")]
    DuplicateRequest(Uuid),
    #[error("remote transport is backpressured")]
    Backpressure,
    #[error("remote transport is closed")]
    Closed,
    #[error("remote request was cancelled")]
    Cancelled,
    #[error("remote request timed out")]
    TimedOut,
    #[error("remote helper protocol negotiation timed out")]
    NegotiationTimeout,
    #[error("remote PTY stream context is invalid")]
    InvalidStreamContext,
}

impl From<hermito_protocol::FrameError> for MultiplexerError {
    fn from(error: hermito_protocol::FrameError) -> Self {
        Self::Version(error.to_string())
    }
}
