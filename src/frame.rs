//! TRYX framing: a four-byte magic, a little-endian `u32` payload length,
//! then the payload.

use std::fmt;

/// The bytes that begin every frame.
pub const MAGIC: [u8; 4] = *b"TRYX";
/// Magic plus length prefix.
pub const HEADER_LEN: usize = 8;
/// Largest payload accepted in either direction.
pub const MAX_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The buffer does not begin with the frame magic.
    BadMagic,
    /// A payload length exceeds [`MAX_PAYLOAD`].
    Oversize(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BadMagic => write!(f, "invalid frame magic"),
            FrameError::Oversize(len) => {
                write!(f, "frame payload of {len} bytes exceeds {MAX_PAYLOAD}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Wraps `payload` in a frame.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::Oversize(payload.len()));
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Accumulates bytes read from the device and splits them into frames.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes received from the device.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Number of bytes waiting to be decoded.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Discards everything buffered.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Removes and returns the next complete payload.
    ///
    /// Returns `Ok(None)` when more bytes are needed. A header that is not a
    /// valid frame empties the buffer and returns the error.
    pub fn take_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if self.buf.len() < MAGIC.len() {
            return Ok(None);
        }
        if self.buf[..MAGIC.len()] != MAGIC {
            self.buf.clear();
            return Err(FrameError::BadMagic);
        }
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = payload_len(&self.buf);
        if len > MAX_PAYLOAD {
            self.buf.clear();
            return Err(FrameError::Oversize(len));
        }
        let total = HEADER_LEN + len;
        if self.buf.len() < total {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..total].to_vec();
        self.buf.drain(..total);
        Ok(Some(payload))
    }
}

/// Reads the payload length from a buffer that holds at least a full header.
fn payload_len(header: &[u8]) -> usize {
    u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_writes_magic_and_little_endian_length() {
        let frame = encode(b"ab").unwrap();
        assert_eq!(frame, b"TRYX\x02\x00\x00\x00ab");
    }

    #[test]
    fn encode_accepts_empty_and_maximum_payloads() {
        assert_eq!(encode(&[]).unwrap(), b"TRYX\x00\x00\x00\x00");
        let max = vec![0x5a; MAX_PAYLOAD];
        let frame = encode(&max).unwrap();
        assert_eq!(frame.len(), HEADER_LEN + MAX_PAYLOAD);
        assert_eq!(&frame[4..8], &(MAX_PAYLOAD as u32).to_le_bytes());
    }

    #[test]
    fn encode_rejects_oversize_payload() {
        let too_big = vec![0; MAX_PAYLOAD + 1];
        assert_eq!(encode(&too_big), Err(FrameError::Oversize(MAX_PAYLOAD + 1)));
    }

    #[test]
    fn round_trip() {
        let payload = b"\x0a\x02\x08\x01\xa2\x06\x04\x0a\x02NA";
        let mut decoder = Decoder::new();
        decoder.push(&encode(payload).unwrap());
        assert_eq!(decoder.take_frame(), Ok(Some(payload.to_vec())));
        assert_eq!(decoder.take_frame(), Ok(None));
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn frame_split_across_reads() {
        let frame = encode(b"hello?").unwrap();
        let mut decoder = Decoder::new();
        decoder.push(&frame[..3]);
        assert_eq!(decoder.take_frame(), Ok(None));
        decoder.push(&frame[3..6]);
        assert_eq!(decoder.take_frame(), Ok(None));
        decoder.push(&frame[6..10]);
        assert_eq!(decoder.take_frame(), Ok(None));
        decoder.push(&frame[10..]);
        assert_eq!(decoder.take_frame(), Ok(Some(b"hello?".to_vec())));
    }

    #[test]
    fn two_frames_in_one_read() {
        let mut bytes = encode(b"first").unwrap();
        bytes.extend(encode(b"second").unwrap());
        let mut decoder = Decoder::new();
        decoder.push(&bytes);
        assert_eq!(decoder.take_frame(), Ok(Some(b"first".to_vec())));
        assert_eq!(decoder.take_frame(), Ok(Some(b"second".to_vec())));
        assert_eq!(decoder.take_frame(), Ok(None));
    }

    #[test]
    fn empty_payload_frame() {
        let mut decoder = Decoder::new();
        decoder.push(b"TRYX\x00\x00\x00\x00");
        assert_eq!(decoder.take_frame(), Ok(Some(Vec::new())));
    }

    #[test]
    fn oversize_length_is_malformed_and_clears_buffer() {
        let mut decoder = Decoder::new();
        let len = (MAX_PAYLOAD as u32 + 1).to_le_bytes();
        decoder.push(b"TRYX");
        decoder.push(&len);
        decoder.push(b"trailing");
        assert_eq!(decoder.take_frame(), Err(FrameError::Oversize(MAX_PAYLOAD + 1)));
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn wrong_magic_is_malformed_and_clears_buffer() {
        let mut decoder = Decoder::new();
        decoder.push(b"TRY");
        assert_eq!(decoder.take_frame(), Ok(None));
        decoder.push(b"Z\x01\x00\x00\x00x");
        assert_eq!(decoder.take_frame(), Err(FrameError::BadMagic));
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn clear_discards_partial_frame() {
        let mut decoder = Decoder::new();
        decoder.push(b"TRYX\x05\x00");
        decoder.clear();
        assert_eq!(decoder.buffered(), 0);
        assert_eq!(decoder.take_frame(), Ok(None));
    }
}
