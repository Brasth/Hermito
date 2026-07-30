use thiserror::Error;

use crate::{Message, ProtocolVersion, CURRENT_VERSION};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiatedVersion(pub ProtocolVersion);

fn negotiate_minor(local: u8, peer: u8) -> u8 {
    local.min(peer)
}

pub fn negotiate(peer: ProtocolVersion) -> Result<NegotiatedVersion, DispatchError> {
    if peer.major != CURRENT_VERSION.major {
        return Err(DispatchError::MajorVersion {
            local: CURRENT_VERSION.major,
            peer: peer.major,
        });
    }
    Ok(NegotiatedVersion(ProtocolVersion {
        major: CURRENT_VERSION.major,
        minor: negotiate_minor(CURRENT_VERSION.minor, peer.minor),
    }))
}

pub fn validate_frame_version(
    actual: ProtocolVersion,
    negotiated: NegotiatedVersion,
) -> Result<(), DispatchError> {
    if actual != negotiated.0 {
        return Err(DispatchError::FrameVersion {
            expected_major: negotiated.0.major,
            expected_minor: negotiated.0.minor,
            actual_major: actual.major,
            actual_minor: actual.minor,
        });
    }
    Ok(())
}

pub fn validate_for_dispatch(
    message: &Message,
    negotiated: NegotiatedVersion,
) -> Result<(), DispatchError> {
    match message {
        Message::Hello { .. } | Message::HelloAck { .. } => Ok(()),
        _ if negotiated.0.major != CURRENT_VERSION.major => Err(DispatchError::NotNegotiated),
        _ => Ok(()),
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("protocol major mismatch: local {local}, peer {peer}")]
    MajorVersion { local: u8, peer: u8 },
    #[error(
        "frame version mismatch: expected {expected_major}.{expected_minor}, got {actual_major}.{actual_minor}"
    )]
    FrameVersion {
        expected_major: u8,
        expected_minor: u8,
        actual_major: u8,
        actual_minor: u8,
    },
    #[error("message received before protocol negotiation")]
    NotNegotiated,
}
