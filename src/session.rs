//! The device session: framed exchange over a transport.
//!
//! The session fails closed. Once a malformed stream, an exhausted budget,
//! or a lost transport has been seen, every further call returns
//! [`SessionError::Closed`] and a new session must be started on a fresh
//! transport.

use std::fmt;
use std::time::{Duration, Instant};

use crate::frame::{Decoder, FrameError};
use crate::transport::{ReadError, Transport, WriteError};

/// Unrelated complete frames tolerated while waiting for a match.
pub const MAX_SKIPPED_FRAMES: usize = 256;
/// Bytes of unrelated complete frames tolerated while waiting for a match.
pub const MAX_SKIPPED_BYTES: usize = 4 * 1024 * 1024;
/// How long a drain waits for stragglers.
pub const DRAIN_WINDOW: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The session has been closed by an earlier failure.
    Closed,
    /// The transport reported a read failure.
    Read(ReadError),
    /// The transport reported a write failure.
    Write(WriteError),
    /// The byte stream could not be framed.
    Frame(FrameError),
    /// Too many unrelated frames arrived while waiting for a match.
    TooManySkipped,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Closed => write!(f, "the session is closed"),
            SessionError::Read(error) => write!(f, "{error}"),
            SessionError::Write(error) => write!(f, "{error}"),
            SessionError::Frame(error) => write!(f, "{error}"),
            SessionError::TooManySkipped => {
                write!(f, "too many unrelated response frames")
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// Counts frames set aside while waiting for the one that matters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SkipBudget {
    pub frames: usize,
    pub bytes: usize,
}

impl SkipBudget {
    /// Records a skipped frame with `payload_len` bytes of payload. Returns
    /// whether the budget still allows waiting.
    pub fn skip(&mut self, payload_len: usize) -> bool {
        self.frames += 1;
        self.bytes += payload_len + crate::frame::HEADER_LEN;
        self.frames <= MAX_SKIPPED_FRAMES && self.bytes <= MAX_SKIPPED_BYTES
    }
}

/// A framed session over one transport.
pub struct Session<T: Transport> {
    transport: T,
    decoder: Decoder,
    closed: Option<SessionError>,
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            decoder: Decoder::new(),
            closed: None,
        }
    }

    /// Whether the session can still be used.
    pub fn is_open(&self) -> bool {
        self.closed.is_none()
    }

    /// The failure that closed the session, if any.
    pub fn closed_by(&self) -> Option<&SessionError> {
        self.closed.as_ref()
    }

    /// Gives the transport back once the session is finished with it.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Closes the session, recording `error` as the reason, and returns it.
    pub(crate) fn close(&mut self, error: SessionError) -> SessionError {
        if self.closed.is_none() {
            self.closed = Some(error.clone());
            self.decoder.clear();
        }
        error
    }

    fn ensure_open(&self) -> Result<(), SessionError> {
        match &self.closed {
            Some(_) => Err(SessionError::Closed),
            None => Ok(()),
        }
    }

    /// Returns the next complete frame payload that arrives before
    /// `deadline`, or `None` when the deadline passes first.
    ///
    /// `discarded` accumulates bytes dropped during resynchronisation for
    /// the current operation; the caller keeps it across calls that belong
    /// to one exchange. A stream that cannot be resynchronised or a lost
    /// transport closes the session.
    pub fn read_frame(
        &mut self,
        deadline: Instant,
        discarded: &mut usize,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        self.ensure_open()?;
        loop {
            match self.decoder.resync_and_take_frame(discarded) {
                Ok(Some(payload)) => return Ok(Some(payload)),
                Ok(None) => {}
                Err(error) => return Err(self.close(SessionError::Frame(error))),
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let bytes = match self.transport.read(deadline - now) {
                Ok(bytes) => bytes,
                Err(error) => return Err(self.close(SessionError::Read(error))),
            };
            if bytes.is_empty() {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                continue;
            }
            self.decoder.push(&bytes);
        }
    }

    /// Writes one frame's bytes. A lost transport closes the session; other
    /// write failures are returned for the caller to classify.
    pub fn write_bytes(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), SessionError> {
        self.ensure_open()?;
        match self.transport.write(bytes, timeout) {
            Ok(()) => Ok(()),
            Err(WriteError::Lost(reason)) => {
                Err(self.close(SessionError::Write(WriteError::Lost(reason))))
            }
            Err(WriteError::Unknown) => Err(self.close(SessionError::Write(WriteError::Unknown))),
            Err(error) => Err(SessionError::Write(error)),
        }
    }

    /// Collects every complete frame that arrives within `window` and then
    /// discards any partial frame left over, so a later exchange starts
    /// from a clean stream. The skip budget bounds how much may be drained.
    pub fn drain(&mut self, window: Duration) -> Result<Vec<Vec<u8>>, SessionError> {
        self.ensure_open()?;
        let deadline = Instant::now() + window;
        let mut discarded = 0;
        let mut budget = SkipBudget::default();
        let mut drained = Vec::new();
        while let Some(payload) = self.read_frame(deadline, &mut discarded)? {
            if !budget.skip(payload.len()) {
                return Err(self.close(SessionError::TooManySkipped));
            }
            drained.push(payload);
        }
        self.decoder.clear();
        Ok(drained)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode, MAX_RESYNC_BYTES};
    use crate::transport::MockTransport;

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(200)
    }

    fn session(mock: MockTransport) -> Session<MockTransport> {
        Session::new(mock)
    }

    #[test]
    fn frame_split_across_three_reads() {
        let frame = encode(b"payload").unwrap();
        let mut mock = MockTransport::new();
        mock.queue_read(frame[..3].to_vec())
            .queue_read(frame[3..9].to_vec())
            .queue_read(frame[9..].to_vec());
        let mut session = session(mock);
        let mut discarded = 0;
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Ok(Some(b"payload".to_vec()))
        );
        assert_eq!(discarded, 0);
        assert!(session.is_open());
    }

    #[test]
    fn two_frames_in_one_read_are_returned_in_order() {
        let mut bytes = encode(b"first").unwrap();
        bytes.extend(encode(b"second").unwrap());
        let mut mock = MockTransport::new();
        mock.queue_read(bytes);
        let mut session = session(mock);
        let mut discarded = 0;
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Ok(Some(b"first".to_vec()))
        );
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Ok(Some(b"second".to_vec()))
        );
        assert_eq!(session.into_transport().pending_reads(), 0);
    }

    #[test]
    fn garbage_before_a_frame_is_discarded_and_counted() {
        let mut bytes = b"stale tail".to_vec();
        bytes.extend(encode(b"ok").unwrap());
        let mut mock = MockTransport::new();
        mock.queue_read(bytes);
        let mut session = session(mock);
        let mut discarded = 0;
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Ok(Some(b"ok".to_vec()))
        );
        assert_eq!(discarded, 10);
        assert!(session.is_open());
    }

    #[test]
    fn deadline_without_a_frame_returns_none_and_keeps_partial_bytes() {
        let frame = encode(b"late").unwrap();
        let mut mock = MockTransport::new();
        mock.queue_read(frame[..5].to_vec()).queue_timeout();
        let mut session = session(mock);
        let mut discarded = 0;
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(session.read_frame(deadline, &mut discarded), Ok(None));
        assert!(session.is_open());
        assert_eq!(session.decoder.buffered(), 5);

        session.transport.queue_read(frame[5..].to_vec());
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Ok(Some(b"late".to_vec()))
        );
    }

    #[test]
    fn exhausted_resync_closes_the_session() {
        let mut mock = MockTransport::new();
        mock.queue_read(vec![0u8; MAX_RESYNC_BYTES + 1]);
        let mut session = session(mock);
        let mut discarded = 0;
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Err(SessionError::Frame(FrameError::ResyncExhausted))
        );
        assert!(!session.is_open());
        assert_eq!(
            session.closed_by(),
            Some(&SessionError::Frame(FrameError::ResyncExhausted))
        );
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Err(SessionError::Closed)
        );
        assert_eq!(
            session.write_bytes(b"x", Duration::from_millis(10)),
            Err(SessionError::Closed)
        );
    }

    #[test]
    fn lost_transport_closes_the_session() {
        let mut mock = MockTransport::new();
        mock.queue_read_error(ReadError::Lost("gone".into()));
        let mut session = session(mock);
        let mut discarded = 0;
        assert_eq!(
            session.read_frame(soon(), &mut discarded),
            Err(SessionError::Read(ReadError::Lost("gone".into())))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn write_passes_bytes_through_and_classifies_failures() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Ok(()))
            .queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Err(WriteError::Partial { written: 2 }))
            .queue_write_result(Err(WriteError::Unknown));
        let mut session = session(mock);
        let timeout = Duration::from_millis(10);
        assert_eq!(session.write_bytes(b"a", timeout), Ok(()));
        assert_eq!(
            session.write_bytes(b"b", timeout),
            Err(SessionError::Write(WriteError::TimedOut))
        );
        assert!(session.is_open());
        assert_eq!(
            session.write_bytes(b"c", timeout),
            Err(SessionError::Write(WriteError::Partial { written: 2 }))
        );
        assert!(session.is_open());
        assert_eq!(
            session.write_bytes(b"d", timeout),
            Err(SessionError::Write(WriteError::Unknown))
        );
        assert!(!session.is_open());
        let mock = session.into_transport();
        assert_eq!(mock.writes, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]);
    }

    #[test]
    fn write_on_a_lost_transport_closes_the_session() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::Lost("unplugged".into())));
        let mut session = session(mock);
        assert_eq!(
            session.write_bytes(b"a", Duration::from_millis(10)),
            Err(SessionError::Write(WriteError::Lost("unplugged".into())))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn drain_collects_complete_frames_and_drops_the_partial_tail() {
        let mut bytes = encode(b"one").unwrap();
        bytes.extend(encode(b"two").unwrap());
        bytes.extend(&encode(b"three").unwrap()[..6]);
        let mut mock = MockTransport::new();
        mock.queue_read(bytes).queue_timeout();
        let mut session = session(mock);
        let drained = session.drain(Duration::from_millis(30)).unwrap();
        assert_eq!(drained, vec![b"one".to_vec(), b"two".to_vec()]);
        assert!(session.is_open());
        assert_eq!(session.decoder.buffered(), 0);
    }

    #[test]
    fn drain_with_nothing_queued_is_empty() {
        let mut session = session(MockTransport::new());
        assert_eq!(session.drain(Duration::from_millis(5)), Ok(Vec::new()));
        assert!(session.is_open());
    }

    #[test]
    fn drain_beyond_the_skip_budget_closes_the_session() {
        let one = encode(b"x").unwrap();
        let mut bytes = Vec::new();
        for _ in 0..=MAX_SKIPPED_FRAMES {
            bytes.extend(&one);
        }
        let mut mock = MockTransport::new();
        mock.queue_read(bytes);
        let mut session = session(mock);
        assert_eq!(
            session.drain(Duration::from_millis(500)),
            Err(SessionError::TooManySkipped)
        );
        assert!(!session.is_open());
    }

    #[test]
    fn skip_budget_limits_frames_and_bytes() {
        let mut budget = SkipBudget::default();
        for _ in 0..MAX_SKIPPED_FRAMES {
            assert!(budget.skip(0));
        }
        assert!(!budget.skip(0));

        let mut budget = SkipBudget::default();
        assert!(budget.skip(MAX_SKIPPED_BYTES - 8));
        assert!(!budget.skip(0));
    }
}
