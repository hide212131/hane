//! Private control protocol between the application and its updater helper.
//!
//! The only transport is a pair of anonymous pipes the parent creates for the
//! one child it spawns; no other process can write to them, and there is no
//! name or argument that selects a path or a command. Every frame carries the
//! protocol number and the session's 256-bit nonce. Frames are length-prefixed
//! JSON, bounded before allocation, and parsed strictly.
//!
//! The nonce and identifiers only correlate frames on that private pipe. They
//! authorize nothing by themselves: the helper still derives its expected
//! installation and transaction from where it runs, and mutation additionally
//! needs the OS leases (see [`crate::platform::lease`]).

use std::io::{Read, Write};

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::model::UPDATER_PROTOCOL;
use crate::util::{hex_encode, is_lower_hex, parse_strict};

pub const PROTOCOL_VERSION: u64 = UPDATER_PROTOCOL;
/// Upper bound of one frame body; checked before anything is allocated.
pub const MAX_FRAME_BYTES: usize = 4096;
pub const MAX_REASON_BYTES: usize = 512;

/// 256-bit session secret from OS randomness, rendered as 64 lowercase hex.
#[derive(Clone, PartialEq, Eq)]
pub struct Nonce(String);

impl std::fmt::Debug for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Nonce(..)")
    }
}

impl Nonce {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; 32];
        SystemRandom::new().fill(&mut bytes).map_err(|_| {
            UpdateError::new(ErrorCode::Io, "operating system randomness is unavailable")
        })?;
        Ok(Self(hex_encode(&bytes)))
    }

    pub fn parse(text: &str) -> Result<Self> {
        if is_lower_hex(text, 64) {
            Ok(Self(text.to_owned()))
        } else {
            Err(invalid("nonce must be 64 lowercase hex digits"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn matches(&self, other: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), other.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        // Accumulate every byte difference so the time does not depend on
        // where the first mismatch is.
        let diff = a
            .iter()
            .zip(b)
            .fold(0_u8, |acc, (x, y)| acc | std::hint::black_box(x ^ y));
        diff == 0
    }
}

fn invalid(detail: &str) -> UpdateError {
    UpdateError::new(ErrorCode::MalformedMetadata, format!("protocol: {detail}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Prepare for a restart the application will authorize.
    Prepare,
    /// Settle a pending journal found at startup.
    Recover,
}

/// What recovery concluded. `Blocked` means another Hane process holds the
/// runtime lease, so nothing was touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryReport {
    Nothing,
    KeptOld,
    KeptNew,
    RolledBack,
    RecoveryRequired(String),
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    // application -> helper
    Hello {
        mode: Mode,
        install_id: String,
        transaction_id: String,
        candidate_id: Option<String>,
        parent_pid: u32,
    },
    ExitAuthorized {
        transaction_id: String,
    },
    Abort,
    // helper -> application
    Prepared {
        install_id: String,
        transaction_id: String,
        helper_pid: u32,
    },
    Recovered {
        transaction_id: String,
        report: RecoveryReport,
    },
    Refused {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub protocol: u64,
    pub nonce: String,
    pub body: Body,
}

impl Frame {
    pub fn new(nonce: &Nonce, body: Body) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            nonce: nonce.as_str().to_owned(),
            body,
        }
    }

    /// Checks protocol and nonce; a frame that fails is never acted on.
    pub fn check(&self, nonce: &Nonce) -> Result<()> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(UpdateError::new(
                ErrorCode::ProtocolUnsupported,
                "unsupported updater protocol",
            ));
        }
        if !nonce.matches(&self.nonce) {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "frame nonce does not belong to this session",
            ));
        }
        Ok(())
    }
}

pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<()> {
    let bytes = serde_json::to_vec(frame)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(UpdateError::new(ErrorCode::LimitExceeded, "frame is too large"));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame. `Ok(None)` is a clean end of stream between frames; an end
/// inside a frame is an error.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Frame>> {
    let mut len_bytes = [0_u8; 4];
    let mut filled = 0;
    while filled < len_bytes.len() {
        let read = reader.read(&mut len_bytes[filled..])?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(UpdateError::new(ErrorCode::Truncated, "pipe closed inside a frame"))
            };
        }
        filled += read;
    }
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(UpdateError::new(ErrorCode::LimitExceeded, "frame length is out of range"));
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body).map_err(|err| {
        UpdateError::new(ErrorCode::Truncated, format!("pipe closed inside a frame: {err}"))
    })?;
    let frame: Frame = parse_strict(&body)?;
    Ok(Some(frame))
}

/// Trims a free-text reason to the protocol bound.
pub fn bounded_reason(text: &str) -> String {
    let mut end = text.len().min(MAX_REASON_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(nonce: &Nonce) -> Frame {
        Frame::new(
            nonce,
            Body::Hello {
                mode: Mode::Prepare,
                install_id: "0".repeat(32),
                transaction_id: "1".repeat(32),
                candidate_id: None,
                parent_pid: 7,
            },
        )
    }

    #[test]
    fn nonces_are_256_bits_and_distinct() {
        let a = Nonce::generate().unwrap();
        let b = Nonce::generate().unwrap();
        assert_eq!(a.as_str().len(), 64);
        assert_ne!(a, b);
        assert!(Nonce::parse(a.as_str()).is_ok());
        assert!(Nonce::parse(&"A".repeat(64)).is_err());
        assert!(Nonce::parse(&"a".repeat(63)).is_err());
        assert_eq!(format!("{a:?}"), "Nonce(..)");
    }

    #[test]
    fn frames_round_trip_and_check_the_nonce() {
        let nonce = Nonce::generate().unwrap();
        let mut wire = Vec::new();
        write_frame(&mut wire, &hello(&nonce)).unwrap();
        let mut reader = wire.as_slice();
        let frame = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(frame, hello(&nonce));
        assert!(frame.check(&nonce).is_ok());
        assert!(read_frame(&mut reader).unwrap().is_none());

        let other = Nonce::generate().unwrap();
        assert_eq!(frame.check(&other).unwrap_err().code, ErrorCode::IdentityMismatch);
        let mut old = frame.clone();
        old.protocol = 0;
        assert_eq!(old.check(&nonce).unwrap_err().code, ErrorCode::ProtocolUnsupported);
    }

    #[test]
    fn oversized_truncated_and_malformed_frames_are_rejected() {
        let mut huge = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        huge.extend(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            read_frame(&mut huge.as_slice()).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
        assert_eq!(
            read_frame(&mut 0_u32.to_le_bytes().as_slice()).unwrap_err().code,
            ErrorCode::LimitExceeded
        );

        let nonce = Nonce::generate().unwrap();
        let mut wire = Vec::new();
        write_frame(&mut wire, &hello(&nonce)).unwrap();
        wire.truncate(wire.len() - 3);
        assert_eq!(
            read_frame(&mut wire.as_slice()).unwrap_err().code,
            ErrorCode::Truncated
        );
        assert_eq!(
            read_frame(&mut [1_u8, 0].as_slice()).unwrap_err().code,
            ErrorCode::Truncated
        );

        let garbage = b"{\"protocol\":1,\"nonce\":\"x\",\"body\":\"Nope\"}";
        let mut wire = (garbage.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(garbage);
        assert!(read_frame(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn reasons_are_bounded_on_a_character_boundary() {
        let long = "あ".repeat(400);
        let bounded = bounded_reason(&long);
        assert!(bounded.len() <= MAX_REASON_BYTES);
        assert!(long.starts_with(&bounded));
    }
}
