pub mod dispatcher;
pub mod frame;
pub mod fs;
pub mod process;
pub mod pty;
pub mod request;
pub mod response;
mod wire_bytes;

use serde::{Deserialize, Serialize};

pub use dispatcher::{negotiate, NegotiatedVersion};
pub use frame::{
    read_frame, write_message, write_message_version, AggregateBudget, FrameError, FrameHeader,
    FrameLimits,
};
pub use request::{
    DocumentRevision, EnvironmentEpoch, ExecutionContextV1, RequestEnvelope, WorkspaceEpoch,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u8,
    pub minor: u8,
}

pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageClass {
    Control = 0,
    Pty = 1,
    Fs = 2,
    Process = 3,
    Lsp = 4,
    Git = 5,
    Container = 6,
    Relay = 7,
}

impl TryFrom<u8> for MessageClass {
    type Error = FrameError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::Pty),
            2 => Ok(Self::Fs),
            3 => Ok(Self::Process),
            4 => Ok(Self::Lsp),
            5 => Ok(Self::Git),
            6 => Ok(Self::Container),
            7 => Ok(Self::Relay),
            unknown => Err(FrameError::UnknownClass(unknown)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMessage {
    pub family_version: u16,
    pub kind: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", content = "message", rename_all = "snake_case")]
pub enum Message {
    Hello { version: ProtocolVersion },
    HelloAck { version: ProtocolVersion },
    Pty(pty::PtyMessage),
    Fs(fs::FsMessage),
    Process(process::ProcessMessage),
    Lsp(ExtensionMessage),
    Git(ExtensionMessage),
    Container(ExtensionMessage),
    Relay(ExtensionMessage),
}

impl Message {
    pub const fn class(&self) -> MessageClass {
        match self {
            Self::Hello { .. } | Self::HelloAck { .. } => MessageClass::Control,
            Self::Pty(_) => MessageClass::Pty,
            Self::Fs(_) => MessageClass::Fs,
            Self::Process(_) => MessageClass::Process,
            Self::Lsp(_) => MessageClass::Lsp,
            Self::Git(_) => MessageClass::Git,
            Self::Container(_) => MessageClass::Container,
            Self::Relay(_) => MessageClass::Relay,
        }
    }
}
