//! Wire framing: a `u32` little-endian length followed by that many bytes of
//! UTF-8 JSON. The length is checked before any buffer is allocated.

use std::io::{self, Read, Write};

/// The largest frame either side accepts once authenticated.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// The largest frame accepted before the token has been verified. A `hello`
/// is tiny, so an unauthenticated peer cannot make Hane allocate more.
pub const MAX_PREAUTH_FRAME_BYTES: usize = 4 * 1024;

#[derive(Debug)]
pub enum FrameError {
    /// The peer closed the connection between two frames.
    Closed,
    /// The peer closed or stalled inside a frame.
    Truncated,
    /// The announced length is zero or above the limit. Nothing was allocated.
    BadLength(usize),
    Io(io::Error),
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => Self::Truncated,
            _ => Self::Io(error),
        }
    }
}

/// Reads the four length bytes. `Closed` only when the stream ends before the
/// first byte, i.e. cleanly between frames.
pub fn read_length(reader: &mut impl Read, limit: usize) -> Result<usize, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > limit {
        return Err(FrameError::BadLength(length));
    }
    Ok(length)
}

/// Reads the body announced by [`read_length`].
pub fn read_body(reader: &mut impl Read, length: usize) -> Result<Vec<u8>, FrameError> {
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

pub fn read_frame(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, FrameError> {
    let length = read_length(reader, limit)?;
    read_body(reader, length)
}

pub fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame size out of range",
        ));
    }
    // One write so a reader never sees a header without its body.
    let mut buffer = Vec::with_capacity(4 + body.len());
    buffer.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buffer.extend_from_slice(body);
    writer.write_all(&buffer)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_frame() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"{\"a\":1}").unwrap();
        assert_eq!(&wire[..4], &7u32.to_le_bytes());
        let body = read_frame(&mut wire.as_slice(), MAX_FRAME_BYTES).unwrap();
        assert_eq!(body, b"{\"a\":1}");
    }

    #[test]
    fn oversized_length_is_refused_before_allocating() {
        // A body is never read: the reader holds the header only.
        let header = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
        let error = read_frame(&mut header.as_slice(), MAX_FRAME_BYTES).unwrap_err();
        assert!(matches!(error, FrameError::BadLength(len) if len == MAX_FRAME_BYTES + 1));
        let header = u32::MAX.to_le_bytes();
        assert!(matches!(
            read_frame(&mut header.as_slice(), MAX_PREAUTH_FRAME_BYTES),
            Err(FrameError::BadLength(_))
        ));
    }

    #[test]
    fn zero_length_and_truncation_are_distinguished_from_a_clean_close() {
        assert!(matches!(
            read_frame(&mut [0u8; 4].as_slice(), MAX_FRAME_BYTES),
            Err(FrameError::BadLength(0))
        ));
        assert!(matches!(
            read_frame(&mut [].as_slice(), MAX_FRAME_BYTES),
            Err(FrameError::Closed)
        ));
        assert!(matches!(
            read_frame(&mut [1u8, 0].as_slice(), MAX_FRAME_BYTES),
            Err(FrameError::Truncated)
        ));
        let mut short = 10u32.to_le_bytes().to_vec();
        short.extend_from_slice(b"abc");
        assert!(matches!(
            read_frame(&mut short.as_slice(), MAX_FRAME_BYTES),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn refuses_to_write_an_empty_or_oversized_frame() {
        assert!(write_frame(&mut Vec::new(), b"").is_err());
        assert!(write_frame(&mut Vec::new(), &vec![0; MAX_FRAME_BYTES + 1]).is_err());
    }
}
