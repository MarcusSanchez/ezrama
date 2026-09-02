//! TRYX framing: a four-byte magic, a little-endian `u32` payload length,
//! then the payload.

use std::fmt;

/// The bytes that begin every frame.
pub const MAGIC: [u8; 4] = *b"TRYX";
/// Magic plus length prefix.
pub const HEADER_LEN: usize = 8;
/// Largest payload accepted in either direction.
pub const MAX_PAYLOAD: usize = 1024 * 1024;
/// Most bytes a single read operation may discard while searching for a
/// frame boundary before the stream is declared unrecoverable.
pub const MAX_RESYNC_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The buffer does not begin with the frame magic.
    BadMagic,
    /// A payload length exceeds [`MAX_PAYLOAD`].
    Oversize(usize),
    /// More than [`MAX_RESYNC_BYTES`] were discarded without finding a frame.
    ResyncExhausted,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BadMagic => write!(f, "invalid frame magic"),
            FrameError::Oversize(len) => {
                write!(f, "frame payload of {len} bytes exceeds {MAX_PAYLOAD}")
            }
            FrameError::ResyncExhausted => write!(
                f,
                "response stream could not be resynchronised within {MAX_RESYNC_BYTES} bytes"
            ),
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

    /// Discards bytes until the buffer starts with a plausible frame header,
    /// is shorter than a header, or is empty. Returns how many bytes were
    /// dropped.
    ///
    /// Protobuf payloads may contain the magic bytes, so a magic followed by
    /// an implausible length is not trusted: one byte is dropped and the
    /// search continues.
    pub fn resync(&mut self) -> usize {
        let mut discarded = 0;
        while !self.buf.is_empty() {
            discarded += discard_before_magic(&mut self.buf);
            if self.buf.len() < HEADER_LEN {
                break;
            }
            if payload_len(&self.buf) <= MAX_PAYLOAD {
                break;
            }
            self.buf.drain(..1);
            discarded += 1;
        }
        discarded
    }

    /// Resynchronises, adds the dropped bytes to `discarded`, and then takes
    /// the next frame. Once `discarded` passes [`MAX_RESYNC_BYTES`] the
    /// buffer is emptied and [`FrameError::ResyncExhausted`] is returned.
    ///
    /// The caller keeps `discarded` for the lifetime of one read operation.
    pub fn resync_and_take_frame(
        &mut self,
        discarded: &mut usize,
    ) -> Result<Option<Vec<u8>>, FrameError> {
        *discarded += self.resync();
        if *discarded > MAX_RESYNC_BYTES {
            self.buf.clear();
            return Err(FrameError::ResyncExhausted);
        }
        self.take_frame()
    }
}

/// Reads the payload length from a buffer that holds at least a full header.
fn payload_len(header: &[u8]) -> usize {
    u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize
}

/// Drops bytes before the first magic. When no magic is present, keeps only
/// a trailing partial magic so a boundary split across reads still matches.
fn discard_before_magic(buf: &mut Vec<u8>) -> usize {
    if buf.is_empty() || buf.starts_with(&MAGIC) {
        return 0;
    }
    if let Some(index) = buf.windows(MAGIC.len()).position(|w| w == MAGIC) {
        buf.drain(..index);
        return index;
    }
    let mut keep = (MAGIC.len() - 1).min(buf.len());
    while keep > 0 && buf[buf.len() - keep..] != MAGIC[..keep] {
        keep -= 1;
    }
    let discarded = buf.len() - keep;
    buf.drain(..discarded);
    discarded
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

    #[test]
    fn resync_leaves_clean_input_alone() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.resync(), 0);
        decoder.push(&encode(b"ok").unwrap());
        assert_eq!(decoder.resync(), 0);
        assert_eq!(decoder.take_frame(), Ok(Some(b"ok".to_vec())));
    }

    #[test]
    fn resync_drops_garbage_before_magic() {
        let mut decoder = Decoder::new();
        decoder.push(b"junk");
        decoder.push(&encode(b"ab").unwrap());
        assert_eq!(decoder.resync(), 4);
        assert_eq!(decoder.take_frame(), Ok(Some(b"ab".to_vec())));
    }

    #[test]
    fn resync_keeps_partial_magic_at_end_of_read() {
        let mut decoder = Decoder::new();
        decoder.push(b"abcTR");
        assert_eq!(decoder.resync(), 3);
        assert_eq!(decoder.buffered(), 2);
        decoder.push(b"YX\x01\x00\x00\x00z");
        assert_eq!(decoder.resync(), 0);
        assert_eq!(decoder.take_frame(), Ok(Some(b"z".to_vec())));
    }

    #[test]
    fn resync_drops_everything_when_no_magic_prefix_remains() {
        let mut decoder = Decoder::new();
        decoder.push(b"hello");
        assert_eq!(decoder.resync(), 5);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn resync_skips_magic_with_implausible_length() {
        let mut decoder = Decoder::new();
        decoder.push(b"TRYX\xff\xff\xff\xff");
        decoder.push(&encode(b"q").unwrap());
        assert_eq!(decoder.resync(), 8);
        assert_eq!(decoder.take_frame(), Ok(Some(b"q".to_vec())));
    }

    #[test]
    fn resync_waits_for_a_full_header_before_judging_length() {
        let mut decoder = Decoder::new();
        decoder.push(b"xTRYX\xff\xff");
        assert_eq!(decoder.resync(), 1);
        assert_eq!(decoder.buffered(), 6);
        assert_eq!(decoder.take_frame(), Ok(None));
    }

    #[test]
    fn resync_and_take_frame_counts_across_calls() {
        let mut decoder = Decoder::new();
        let mut discarded = 0;
        decoder.push(b"ab");
        assert_eq!(decoder.resync_and_take_frame(&mut discarded), Ok(None));
        assert_eq!(discarded, 2);
        decoder.push(b"cd");
        decoder.push(&encode(b"p").unwrap());
        assert_eq!(
            decoder.resync_and_take_frame(&mut discarded),
            Ok(Some(b"p".to_vec()))
        );
        assert_eq!(discarded, 4);
    }

    #[test]
    fn resync_budget_boundary() {
        let mut decoder = Decoder::new();
        let mut discarded = 0;
        decoder.push(&vec![b'z'; MAX_RESYNC_BYTES]);
        assert_eq!(decoder.resync_and_take_frame(&mut discarded), Ok(None));
        assert_eq!(discarded, MAX_RESYNC_BYTES);
        decoder.push(b"z");
        decoder.push(&encode(b"late").unwrap());
        assert_eq!(
            decoder.resync_and_take_frame(&mut discarded),
            Err(FrameError::ResyncExhausted)
        );
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn resync_budget_exceeded_in_one_read() {
        let mut decoder = Decoder::new();
        let mut discarded = 0;
        decoder.push(&vec![0u8; MAX_RESYNC_BYTES + 1]);
        assert_eq!(
            decoder.resync_and_take_frame(&mut discarded),
            Err(FrameError::ResyncExhausted)
        );
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn take_frame_never_sees_bad_magic_after_resync() {
        let inputs: [&[u8]; 4] = [
            b"garbage",
            b"TRYX\xff\xff\xff\xff\x00",
            b"..TRYX\x00\x00\x10\x00TRYX",
            b"TRY",
        ];
        for input in inputs {
            let mut decoder = Decoder::new();
            decoder.push(input);
            decoder.resync();
            assert_ne!(decoder.take_frame(), Err(FrameError::BadMagic));
        }
    }
}
