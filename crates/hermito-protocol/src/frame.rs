use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{Message, MessageClass, ProtocolVersion, CURRENT_VERSION};

pub const FRAME_MAGIC: [u8; 4] = *b"HMT2";
pub const HEADER_LEN: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: ProtocolVersion,
    pub class: MessageClass,
    pub flags: u8,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut out = [0_u8; HEADER_LEN];
        out[..4].copy_from_slice(&FRAME_MAGIC);
        out[4] = self.version.major;
        out[5] = self.version.minor;
        out[6] = self.class as u8;
        out[7] = self.flags;
        out[8..12].copy_from_slice(&self.payload_len.to_be_bytes());
        out
    }

    pub fn decode(bytes: [u8; HEADER_LEN]) -> Result<Self, FrameError> {
        if bytes[..4] != FRAME_MAGIC {
            return Err(FrameError::BadMagic);
        }
        let class = MessageClass::try_from(bytes[6])?;
        Ok(Self {
            version: ProtocolVersion {
                major: bytes[4],
                minor: bytes[5],
            },
            class,
            flags: bytes[7],
            payload_len: u32::from_be_bytes(bytes[8..12].try_into().expect("fixed width")),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameLimits {
    pub control: usize,
    pub pty: usize,
    pub fs: usize,
    pub process: usize,
    pub extension: usize,
    pub aggregate: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            control: 64 * 1024,
            pty: 1024 * 1024,
            fs: 16 * 1024 * 1024,
            process: 8 * 1024 * 1024,
            extension: 4 * 1024 * 1024,
            aggregate: 32 * 1024 * 1024,
        }
    }
}

impl FrameLimits {
    pub fn max_for(self, class: MessageClass) -> usize {
        match class {
            MessageClass::Control => self.control,
            MessageClass::Pty => self.pty,
            MessageClass::Fs => self.fs,
            MessageClass::Process => self.process,
            MessageClass::Lsp
            | MessageClass::Git
            | MessageClass::Container
            | MessageClass::Relay => self.extension,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AggregateBudget {
    inner: Arc<BudgetState>,
}

#[derive(Debug)]
struct BudgetState {
    used: AtomicUsize,
    limit: usize,
    available: Arc<Semaphore>,
}

impl AggregateBudget {
    pub fn new(limit: usize) -> Self {
        assert!(
            u32::try_from(limit).is_ok(),
            "aggregate frame limit exceeds semaphore capacity"
        );
        Self {
            inner: Arc::new(BudgetState {
                used: AtomicUsize::new(0),
                limit,
                available: Arc::new(Semaphore::new(limit)),
            }),
        }
    }

    pub fn used(&self) -> usize {
        self.inner.used.load(Ordering::Acquire)
    }

    pub fn try_reserve(&self, bytes: usize) -> Result<BudgetPermit, FrameError> {
        let permits = self.validated_permits(bytes)?;
        let permit = Arc::clone(&self.inner.available)
            .try_acquire_many_owned(permits)
            .map_err(|_| FrameError::AggregateLimit {
                requested: bytes,
                available: self.inner.limit.saturating_sub(self.used()),
            })?;
        self.inner.used.fetch_add(bytes, Ordering::AcqRel);
        Ok(BudgetPermit {
            inner: Arc::clone(&self.inner),
            bytes,
            _permit: permit,
        })
    }

    async fn reserve(&self, bytes: usize) -> Result<BudgetPermit, FrameError> {
        let permits = self.validated_permits(bytes)?;
        let permit = Arc::clone(&self.inner.available)
            .acquire_many_owned(permits)
            .await
            .expect("aggregate frame semaphore is never closed");
        self.inner.used.fetch_add(bytes, Ordering::AcqRel);
        Ok(BudgetPermit {
            inner: Arc::clone(&self.inner),
            bytes,
            _permit: permit,
        })
    }

    fn validated_permits(&self, bytes: usize) -> Result<u32, FrameError> {
        if bytes > self.inner.limit {
            return Err(FrameError::AggregateLimit {
                requested: bytes,
                available: self.inner.limit.saturating_sub(self.used()),
            });
        }
        u32::try_from(bytes).map_err(|_| FrameError::AggregateLimit {
            requested: bytes,
            available: self.inner.limit.saturating_sub(self.used()),
        })
    }
}

#[derive(Debug)]
pub struct BudgetPermit {
    inner: Arc<BudgetState>,
    bytes: usize,
    _permit: OwnedSemaphorePermit,
}

impl Drop for BudgetPermit {
    fn drop(&mut self) {
        self.inner.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub struct ReceivedFrame {
    pub header: FrameHeader,
    payload: Vec<u8>,
    _permit: BudgetPermit,
}

#[derive(Debug)]
pub struct ReceivedMessage {
    pub message: Message,
    _frame: ReceivedFrame,
}

impl ReceivedMessage {
    pub fn into_parts(self) -> (Message, ReceivedFrame) {
        (self.message, self._frame)
    }
}

impl ReceivedFrame {
    pub fn decode_message(&self) -> Result<Message, FrameError> {
        let message: Message = serde_json::from_slice(&self.payload)?;
        if message.class() != self.header.class {
            return Err(FrameError::ClassMismatch {
                header: self.header.class,
                payload: message.class(),
            });
        }
        Ok(message)
    }

    pub fn into_message(self) -> Result<ReceivedMessage, FrameError> {
        let message: Message = serde_json::from_slice(&self.payload)?;
        if message.class() != self.header.class {
            return Err(FrameError::ClassMismatch {
                header: self.header.class,
                payload: message.class(),
            });
        }
        Ok(ReceivedMessage {
            message,
            _frame: self,
        })
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    limits: FrameLimits,
    budget: &AggregateBudget,
) -> Result<ReceivedFrame, FrameError> {
    let mut header_bytes = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    let header = FrameHeader::decode(header_bytes)?;
    let payload_len =
        usize::try_from(header.payload_len).map_err(|_| FrameError::LengthOverflow)?;
    let class_limit = limits.max_for(header.class);
    if payload_len > class_limit {
        return Err(FrameError::ClassLimit {
            class: header.class,
            length: payload_len,
            limit: class_limit,
        });
    }
    let permit = budget.reserve(payload_len).await?;
    let mut payload = vec![0_u8; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(ReceivedFrame {
        header,
        payload,
        _permit: permit,
    })
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
    limits: FrameLimits,
) -> Result<(), FrameError> {
    write_message_version(writer, message, limits, CURRENT_VERSION).await
}

pub async fn write_message_version<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
    limits: FrameLimits,
    version: ProtocolVersion,
) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(message)?;
    let class = message.class();
    let limit = limits.max_for(class);
    if payload.len() > limit {
        return Err(FrameError::ClassLimit {
            class,
            length: payload.len(),
            limit,
        });
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    let header = FrameHeader {
        version,
        class,
        flags: 0,
        payload_len,
    };
    writer.write_all(&header.encode()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("invalid frame magic")]
    BadMagic,
    #[error("unknown frame class {0}")]
    UnknownClass(u8),
    #[error("frame payload length cannot be represented")]
    LengthOverflow,
    #[error("{class:?} frame length {length} exceeds cap {limit}")]
    ClassLimit {
        class: MessageClass,
        length: usize,
        limit: usize,
    },
    #[error("aggregate frame budget exceeded: requested {requested}, available {available}")]
    AggregateLimit { requested: usize, available: usize },
    #[error("frame class {header:?} does not match decoded message class {payload:?}")]
    ClassMismatch {
        header: MessageClass,
        payload: MessageClass,
    },
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame payload is invalid: {0}")]
    Json(#[from] serde_json::Error),
}
