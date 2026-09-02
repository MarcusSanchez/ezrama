//! The device session: framed exchange over a transport.
//!
//! The session fails closed. Once a malformed stream, an exhausted budget,
//! a rejected exchange, or a lost transport has been seen, every further
//! call returns [`SessionError::Closed`] and a new session must be started
//! on a fresh transport.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::num::NonZeroU64;
use std::thread;
use std::time::{Duration, Instant};

use crate::frame::{self, Decoder, FrameError};
use crate::pb::DecodeError;
use crate::transport::{ReadError, Transport, WriteError};
use crate::wire::{self, field, DeviceInformation, ProtocolError, Response, UserConfiguration};

/// Unrelated complete frames tolerated while waiting for a match.
pub const MAX_SKIPPED_FRAMES: usize = 256;
/// Bytes of unrelated complete frames tolerated while waiting for a match.
pub const MAX_SKIPPED_BYTES: usize = 4 * 1024 * 1024;
/// Bound on a drain sweep, and how long to wait for one optional reply.
pub const DRAIN_WINDOW: Duration = Duration::from_millis(250);
/// How long a keepalive Ping may take to transfer.
pub const KEEPALIVE_WRITE_TIMEOUT: Duration = Duration::from_millis(2000);
/// How long a matching response may take.
pub const TRANSACTION_TIMEOUT: Duration = Duration::from_millis(3000);
/// How long a bootstrap request may take to transfer.
pub const BOOTSTRAP_WRITE_TIMEOUT: Duration = Duration::from_millis(2000);
/// How long the device may take to answer its first DeviceInfo request
/// after enumeration.
pub const READINESS_DEADLINE: Duration = Duration::from_millis(20_000);
/// First pause after a DeviceInfo write that transferred nothing.
pub const READINESS_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Longest pause between DeviceInfo attempts.
pub const READINESS_MAX_BACKOFF: Duration = Duration::from_millis(2000);
/// Attempts allowed for a read-only query whose reply times out cleanly.
pub const IDEMPOTENT_QUERY_ATTEMPTS: u32 = 2;
/// Pause before the second attempt of a read-only query.
pub const IDEMPOTENT_QUERY_BACKOFF: Duration = Duration::from_millis(250);

/// Time source for deadlines and backoff, replaceable in tests.
pub trait Clock {
    fn now(&mut self) -> Instant;
    fn sleep(&mut self, duration: Duration);
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

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
    /// A response payload was not a valid message.
    Decode(DecodeError),
    /// Too many unrelated frames arrived while waiting for a match.
    TooManySkipped,
    /// No matching response arrived before the deadline.
    Timeout,
    /// The deadline passed with part of a frame received.
    IncompleteResponse,
    /// A response carried a header the exchange does not accept.
    UnexpectedHeader,
    /// The device answered with an error.
    Rejected(ProtocolError),
    /// The matching response carried the wrong body.
    UnexpectedBody { expected: u32, actual: Option<u32> },
    /// The device never accepted a DeviceInfo request within the readiness
    /// deadline.
    NotReady { attempts: u32 },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Closed => write!(f, "the session is closed"),
            SessionError::Read(error) => write!(f, "{error}"),
            SessionError::Write(error) => write!(f, "{error}"),
            SessionError::Frame(error) => write!(f, "{error}"),
            SessionError::Decode(error) => write!(f, "{error}"),
            SessionError::TooManySkipped => {
                write!(f, "too many unrelated response frames")
            }
            SessionError::Timeout => write!(f, "timed out waiting for the matching response"),
            SessionError::IncompleteResponse => {
                write!(f, "the response was cut off before the deadline")
            }
            SessionError::UnexpectedHeader => write!(f, "response has an unexpected header"),
            SessionError::Rejected(error) => {
                if error.why.is_empty() {
                    write!(f, "device rejected the request with error {}", error.code)
                } else {
                    write!(f, "device rejected the request: {}", error.why)
                }
            }
            SessionError::UnexpectedBody { expected, actual } => match actual {
                Some(actual) => write!(f, "response body {actual} does not match expected {expected}"),
                None => write!(f, "response has no body; expected {expected}"),
            },
            SessionError::NotReady { attempts } => {
                write!(f, "device did not become ready after {attempts} attempts")
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
        self.bytes += payload_len + frame::HEADER_LEN;
        self.frames <= MAX_SKIPPED_FRAMES && self.bytes <= MAX_SKIPPED_BYTES
    }
}

/// What arrived after a write-only request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalReply {
    /// Nothing arrived within the window.
    None,
    /// The device acknowledged the request by track id.
    Acknowledged,
    /// An unrelated frame arrived and was consumed.
    Drained,
}

/// How a keepalive Ping ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepaliveOutcome {
    /// The Ping transferred; the reply, if any, was consumed.
    Sent(OptionalReply),
    /// The write transferred nothing before its deadline; the session is
    /// still open and the Ping may be sent again.
    Retryable(SessionError),
    /// The session has been closed.
    Fatal(SessionError),
}

/// What the bootstrap learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    pub device: DeviceInformation,
    pub auth: String,
    /// DeviceInfo writes made before the device answered.
    pub readiness_attempts: u32,
}

/// A framed session over one transport.
pub struct Session<T: Transport> {
    transport: T,
    decoder: Decoder,
    closed: Option<SessionError>,
    clock: Box<dyn Clock + Send>,
    next_track: u64,
}

/// A random starting point for track ids, so a new session's ids do not
/// collide with responses left over from an earlier one.
fn random_track_seed() -> u64 {
    RandomState::new().hash_one(0u8)
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self::with_clock(transport, Box::new(SystemClock))
    }

    pub fn with_clock(transport: T, clock: Box<dyn Clock + Send>) -> Self {
        Self {
            transport,
            decoder: Decoder::new(),
            closed: None,
            clock,
            next_track: random_track_seed(),
        }
    }

    /// Sets where track ids continue from, for reproducible sessions.
    pub fn seed_track_ids(&mut self, seed: u64) {
        self.next_track = seed;
    }

    /// The next track id, never zero.
    pub fn allocate_track(&mut self) -> NonZeroU64 {
        let mut id = self.next_track;
        self.next_track = self.next_track.wrapping_add(1);
        if id == 0 {
            id = self.next_track;
            self.next_track = self.next_track.wrapping_add(1);
        }
        if self.next_track == 0 {
            self.next_track = 1;
        }
        NonZeroU64::new(id).unwrap_or(NonZeroU64::MIN)
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

    /// The session's idea of now.
    pub fn now(&mut self) -> Instant {
        self.clock.now()
    }

    /// Pauses on the session's clock.
    pub fn sleep(&mut self, duration: Duration) {
        self.clock.sleep(duration);
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
            let now = self.clock.now();
            if now >= deadline {
                return Ok(None);
            }
            let bytes = match self.transport.read(deadline - now) {
                Ok(bytes) => bytes,
                Err(error) => return Err(self.close(SessionError::Read(error))),
            };
            if bytes.is_empty() {
                return Ok(None);
            }
            self.decoder.push(&bytes);
        }
    }

    /// Writes one frame's bytes. A lost transport or an unknown outcome
    /// closes the session; other write failures are returned for the caller
    /// to classify.
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

    /// Sweeps up every complete frame the device has already sent, without
    /// waiting for more, then discards any partial frame left over so the
    /// next exchange starts from a clean stream. Bounded by the skip budget
    /// and by [`DRAIN_WINDOW`] of processing time.
    pub fn drain_queued(&mut self) -> Result<Vec<Vec<u8>>, SessionError> {
        self.ensure_open()?;
        let started = self.clock.now();
        let mut discarded = 0;
        let mut budget = SkipBudget::default();
        let mut drained = Vec::new();
        loop {
            loop {
                match self.decoder.resync_and_take_frame(&mut discarded) {
                    Ok(Some(payload)) => {
                        if !budget.skip(payload.len()) {
                            return Err(self.close(SessionError::TooManySkipped));
                        }
                        drained.push(payload);
                    }
                    Ok(None) => break,
                    Err(error) => return Err(self.close(SessionError::Frame(error))),
                }
            }
            if self.clock.now().saturating_duration_since(started) >= DRAIN_WINDOW {
                break;
            }
            let bytes = match self.transport.read(Duration::ZERO) {
                Ok(bytes) => bytes,
                Err(error) => return Err(self.close(SessionError::Read(error))),
            };
            if bytes.is_empty() {
                break;
            }
            self.decoder.push(&bytes);
        }
        self.decoder.clear();
        Ok(drained)
    }

    /// Waits up to [`DRAIN_WINDOW`] for at most one frame after a
    /// write-only request. A frame for `track` without an error is an
    /// acknowledgement; any frame carrying a device error is a rejection,
    /// returned without closing; any other frame is consumed. A partial
    /// frame left at the deadline is discarded.
    pub fn await_optional_reply(
        &mut self,
        track: Option<NonZeroU64>,
    ) -> Result<OptionalReply, SessionError> {
        self.ensure_open()?;
        let deadline = self.clock.now() + DRAIN_WINDOW;
        let mut discarded = 0;
        let Some(payload) = self.read_frame(deadline, &mut discarded)? else {
            self.decoder.clear();
            return Ok(OptionalReply::None);
        };
        if let Ok(response) = Response::parse(&payload) {
            if let Some(rejection) = response.rejection() {
                return Err(SessionError::Rejected(rejection.clone()));
            }
            let matches = track.is_some_and(|track| {
                matches!(
                    response.header,
                    Some(header) if header.version == 1 && header.track_id == track.get()
                )
            });
            if matches {
                return Ok(OptionalReply::Acknowledged);
            }
        }
        Ok(OptionalReply::Drained)
    }

    /// Writes a tracked request whose reply is optional: drain, write, then
    /// wait for at most one reply. A write failure closes the session; a
    /// rejection is returned without closing.
    pub fn write_tracked_only(
        &mut self,
        request: impl FnOnce(NonZeroU64) -> Vec<u8>,
    ) -> Result<OptionalReply, SessionError> {
        self.ensure_open()?;
        let track = self.allocate_track();
        let frame = framed(&request(track))?;
        self.drain_queued()?;
        if let Err(error) = self.write_bytes(&frame, TRANSACTION_TIMEOUT) {
            return Err(self.close(error));
        }
        self.await_optional_reply(Some(track))
    }

    /// Sends the empty overlay layout that switches the panel to its stored
    /// work configuration. Any failure, including a rejection, closes the
    /// session.
    pub fn activate(&mut self) -> Result<OptionalReply, SessionError> {
        match self.write_tracked_only(wire::activation_trigger) {
            Ok(reply) => Ok(reply),
            Err(error) => Err(self.close(error)),
        }
    }

    /// Sends one keepalive Ping. The session stays open after a Ping write
    /// that timed out with nothing transferred; every other failure closes
    /// it.
    pub fn ping(&mut self) -> KeepaliveOutcome {
        if let Err(error) = self.ensure_open() {
            return KeepaliveOutcome::Fatal(error);
        }
        if let Err(error) = self.drain_queued() {
            return KeepaliveOutcome::Fatal(error);
        }
        let frame = match framed(&wire::keepalive_ping()) {
            Ok(frame) => frame,
            Err(error) => return KeepaliveOutcome::Fatal(self.close(error)),
        };
        match self.write_bytes(&frame, TRANSACTION_TIMEOUT.min(KEEPALIVE_WRITE_TIMEOUT)) {
            Ok(()) => {}
            Err(error @ SessionError::Write(WriteError::TimedOut)) => {
                return KeepaliveOutcome::Retryable(error);
            }
            Err(error) => return KeepaliveOutcome::Fatal(self.close(error)),
        }
        match self.await_optional_reply(None) {
            Ok(reply) => KeepaliveOutcome::Sent(reply),
            Err(error) => KeepaliveOutcome::Fatal(self.close(error)),
        }
    }

    /// One tracked exchange: drain, write the request built for a fresh
    /// track id, and wait for the response carrying that id and
    /// `expected_body`.
    ///
    /// Frames for other track ids, header-less Pong frames, and events are
    /// skipped within the budget. A response without a header, a wrong
    /// body, a malformed frame, or a write failure closes the session. A
    /// device error is returned without closing. A clean timeout closes the
    /// session unless `keep_open_on_timeout` is set; a timeout with part of
    /// a frame received always closes.
    pub fn execute(
        &mut self,
        request: impl FnOnce(NonZeroU64) -> Vec<u8>,
        expected_body: u32,
        keep_open_on_timeout: bool,
    ) -> Result<Vec<u8>, SessionError> {
        self.ensure_open()?;
        let track = self.allocate_track();
        let frame = framed(&request(track))?;
        self.drain_queued()?;
        if let Err(error) = self.write_bytes(&frame, TRANSACTION_TIMEOUT) {
            return Err(self.close(error));
        }

        let deadline = self.clock.now() + TRANSACTION_TIMEOUT;
        let mut discarded = 0;
        let mut budget = SkipBudget::default();
        loop {
            let Some(payload) = self.read_frame(deadline, &mut discarded)? else {
                if self.decoder.buffered() > 0 {
                    return Err(self.close(SessionError::IncompleteResponse));
                }
                if keep_open_on_timeout {
                    return Err(SessionError::Timeout);
                }
                return Err(self.close(SessionError::Timeout));
            };
            let response = match Response::parse(&payload) {
                Ok(response) => response,
                Err(error) => return Err(self.close(SessionError::Decode(error))),
            };
            let body_number = response.body_number();
            let unsolicited = body_number == Some(field::ASYNCHRONOUS_EVENT)
                || (body_number == Some(field::PONG) && expected_body != field::PONG);
            if unsolicited {
                if !budget.skip(payload.len()) {
                    return Err(self.close(SessionError::TooManySkipped));
                }
                continue;
            }
            let Some(header) = response.header else {
                return Err(self.close(SessionError::UnexpectedHeader));
            };
            if header.track_id != track.get() {
                if !budget.skip(payload.len()) {
                    return Err(self.close(SessionError::TooManySkipped));
                }
                continue;
            }
            if let Some(rejection) = response.rejection() {
                return Err(SessionError::Rejected(rejection.clone()));
            }
            return match response.body {
                Some((number, body)) if number == expected_body => Ok(body.to_vec()),
                other => Err(self.close(SessionError::UnexpectedBody {
                    expected: expected_body,
                    actual: other.map(|(number, _)| number),
                })),
            };
        }
    }

    /// Reads the device's user configuration. The query is idempotent, so a
    /// clean timeout on the first attempt is followed by one more after a
    /// short pause.
    pub fn query_user_configuration(&mut self) -> Result<UserConfiguration, SessionError> {
        let mut attempt = 1;
        loop {
            let retry_available = attempt < IDEMPOTENT_QUERY_ATTEMPTS;
            match self.execute(
                wire::user_configuration_query,
                field::USER_CONFIGURATION,
                retry_available,
            ) {
                Ok(body) => {
                    return UserConfiguration::parse(&body)
                        .map_err(|error| self.close(SessionError::Decode(error)));
                }
                Err(SessionError::Timeout) if retry_available => {
                    self.clock.sleep(IDEMPOTENT_QUERY_BACKOFF);
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Runs the three untracked exchanges that open a display session:
    /// DeviceInfo, SystemConfiguration, and DeviceAuth.
    ///
    /// Only the DeviceInfo write is retried, and only when it transferred
    /// nothing before its timeout, with backoff inside the readiness
    /// deadline. Any other failure closes the session.
    pub fn bootstrap(&mut self) -> Result<Bootstrap, SessionError> {
        self.ensure_open()?;
        self.drain_queued()?;

        let probe = framed(&wire::device_information_query())?;
        let started = self.clock.now();
        let mut attempts = 0u32;
        let mut backoff = READINESS_INITIAL_BACKOFF;
        let mut device_body = None;
        loop {
            let elapsed = self.clock.now().saturating_duration_since(started);
            if elapsed >= READINESS_DEADLINE {
                break;
            }
            attempts += 1;
            let remaining = READINESS_DEADLINE - elapsed;
            let write_timeout = TRANSACTION_TIMEOUT
                .min(BOOTSTRAP_WRITE_TIMEOUT)
                .min(remaining.max(Duration::from_millis(1)));
            match self.write_bytes(&probe, write_timeout) {
                Ok(()) => {}
                Err(SessionError::Write(WriteError::TimedOut)) => {
                    let elapsed = self.clock.now().saturating_duration_since(started);
                    let remaining = READINESS_DEADLINE.saturating_sub(elapsed);
                    if remaining.is_zero() {
                        break;
                    }
                    self.clock.sleep(backoff.min(remaining));
                    backoff = (backoff * 2).min(READINESS_MAX_BACKOFF);
                    continue;
                }
                Err(error) => return Err(self.close(error)),
            }

            let elapsed = self.clock.now().saturating_duration_since(started);
            let budget = READINESS_DEADLINE.saturating_sub(elapsed);
            if budget.is_zero() {
                return Err(self.close(SessionError::Timeout));
            }
            let deadline = self.clock.now() + budget;
            device_body = Some(self.read_bootstrap_response(field::DEVICE_INFORMATION, deadline)?);
            break;
        }
        let Some(device_body) = device_body else {
            return Err(self.close(SessionError::NotReady { attempts }));
        };
        let device = match DeviceInformation::parse(&device_body) {
            Ok(device) => device,
            Err(error) => return Err(self.close(SessionError::Decode(error))),
        };

        self.bootstrap_exchange(
            &wire::system_configuration_query(),
            field::SYSTEM_CONFIGURATION,
        )?;
        let auth_body = self.bootstrap_exchange(
            &wire::device_authentication_query(),
            field::DEVICE_AUTHENTICATION,
        )?;
        let auth = match wire::parse_device_authentication(&auth_body) {
            Ok(auth) => auth,
            Err(error) => return Err(self.close(SessionError::Decode(error))),
        };

        Ok(Bootstrap {
            device,
            auth,
            readiness_attempts: attempts,
        })
    }

    /// One untracked exchange sent exactly once: drain, write, and wait for
    /// the exact response body. Any failure closes the session.
    fn bootstrap_exchange(&mut self, payload: &[u8], expected_body: u32) -> Result<Vec<u8>, SessionError> {
        self.drain_queued()?;
        let frame = framed(payload)?;
        if let Err(error) = self.write_bytes(&frame, TRANSACTION_TIMEOUT.min(BOOTSTRAP_WRITE_TIMEOUT)) {
            return Err(self.close(error));
        }
        let deadline = self.clock.now() + TRANSACTION_TIMEOUT;
        self.read_bootstrap_response(expected_body, deadline)
    }

    /// Waits for a bootstrap response carrying `expected_body`.
    ///
    /// Accepted headers carry version 1, track id 0, and crc 0. Frames with
    /// a stale tracked header, and header-less Pong or event frames, are
    /// skipped within the budget. Anything else, a device error, or a
    /// different body closes the session.
    fn read_bootstrap_response(&mut self, expected_body: u32, deadline: Instant) -> Result<Vec<u8>, SessionError> {
        let mut discarded = 0;
        let mut budget = SkipBudget::default();
        loop {
            let Some(payload) = self.read_frame(deadline, &mut discarded)? else {
                return Err(self.close(SessionError::Timeout));
            };
            let response = match Response::parse(&payload) {
                Ok(response) => response,
                Err(error) => return Err(self.close(SessionError::Decode(error))),
            };
            let expected_header = matches!(
                response.header,
                Some(header) if header.version == 1 && header.track_id == 0 && header.payload_crc32 == 0
            );
            let stale_tracked = matches!(
                response.header,
                Some(header) if header.version == 1 && header.track_id != 0 && header.payload_crc32 == 0
            );
            let headerless_async = response.header.is_none()
                && matches!(
                    response.body_number(),
                    Some(field::PONG) | Some(field::ASYNCHRONOUS_EVENT)
                );
            if stale_tracked || headerless_async {
                if !budget.skip(payload.len()) {
                    return Err(self.close(SessionError::TooManySkipped));
                }
                continue;
            }
            if !expected_header {
                return Err(self.close(SessionError::UnexpectedHeader));
            }
            if let Some(rejection) = response.rejection() {
                let rejection = rejection.clone();
                return Err(self.close(SessionError::Rejected(rejection)));
            }
            return match response.body {
                Some((number, body)) if number == expected_body => Ok(body.to_vec()),
                other => Err(self.close(SessionError::UnexpectedBody {
                    expected: expected_body,
                    actual: other.map(|(number, _)| number),
                })),
            };
        }
    }
}

fn framed(payload: &[u8]) -> Result<Vec<u8>, SessionError> {
    frame::encode(payload).map_err(SessionError::Frame)
}

/// Test doubles shared by the session and holding tests.
#[cfg(test)]
pub mod testing {
    use super::Clock;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A clock that only moves when the session sleeps.
    #[derive(Clone)]
    pub struct FakeClock {
        base: Instant,
        offset: Arc<Mutex<Duration>>,
        sleeps: Arc<Mutex<Vec<Duration>>>,
    }

    impl Default for FakeClock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FakeClock {
        pub fn new() -> Self {
            Self {
                base: Instant::now(),
                offset: Arc::new(Mutex::new(Duration::ZERO)),
                sleeps: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn sleeps_ms(&self) -> Vec<u64> {
            self.sleeps
                .lock()
                .unwrap()
                .iter()
                .map(|d| d.as_millis() as u64)
                .collect()
        }

        pub fn elapsed(&self) -> Duration {
            *self.offset.lock().unwrap()
        }
    }

    impl Clock for FakeClock {
        fn now(&mut self) -> Instant {
            self.base + *self.offset.lock().unwrap()
        }

        fn sleep(&mut self, duration: Duration) {
            self.sleeps.lock().unwrap().push(duration);
            *self.offset.lock().unwrap() += duration;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeClock;
    use super::*;
    use crate::frame::{encode, MAX_RESYNC_BYTES};
    use crate::pb::Message;
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
        assert_eq!(session.read_frame(soon(), &mut discarded), Ok(None));
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
        assert_eq!(session.bootstrap(), Err(SessionError::Closed));
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
        let drained = session.drain_queued().unwrap();
        assert_eq!(drained, vec![b"one".to_vec(), b"two".to_vec()]);
        assert!(session.is_open());
        assert_eq!(session.decoder.buffered(), 0);
    }

    #[test]
    fn drain_with_nothing_queued_is_empty() {
        let mut session = session(MockTransport::new());
        assert_eq!(session.drain_queued(), Ok(Vec::new()));
        assert!(session.is_open());
    }

    #[test]
    fn drain_does_not_wait_for_a_pending_reply() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(encode(b"reply").unwrap());
        let mut session = session(mock);
        assert_eq!(session.drain_queued(), Ok(Vec::new()));
        assert_eq!(session.into_transport().pending_reads(), 2);
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
        assert_eq!(session.drain_queued(), Err(SessionError::TooManySkipped));
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

    fn bootstrap_session(mock: MockTransport) -> (Session<MockTransport>, FakeClock) {
        let clock = FakeClock::new();
        (Session::with_clock(mock, Box::new(clock.clone())), clock)
    }

    fn response(header: Option<Message>, error: Option<Message>, body: Option<(u32, Message)>) -> Vec<u8> {
        let mut message = Message::new();
        if let Some(header) = header {
            message = message.message(field::HEADER, &header);
        }
        if let Some(error) = error {
            message = message.message(field::ERROR, &error);
        }
        if let Some((number, body)) = body {
            message = message.message(number, &body);
        }
        encode(message.as_bytes()).unwrap()
    }

    fn bootstrap_header() -> Message {
        Message::new().uint(1, 1)
    }

    fn device_info_body() -> Message {
        Message::new()
            .bytes(1, b"Linux")
            .bytes(3, b"1.2.3")
            .bytes(4, b"PASE")
            .bytes(8, b"SN42")
    }

    fn device_info_response() -> Vec<u8> {
        response(
            Some(bootstrap_header()),
            None,
            Some((field::DEVICE_INFORMATION, device_info_body())),
        )
    }

    fn system_configuration_response() -> Vec<u8> {
        response(
            Some(bootstrap_header()),
            None,
            Some((field::SYSTEM_CONFIGURATION, Message::new())),
        )
    }

    fn auth_response() -> Vec<u8> {
        response(
            Some(bootstrap_header()),
            None,
            Some((field::DEVICE_AUTHENTICATION, Message::new().bytes(1, b"token"))),
        )
    }

    fn queue_remaining_exchanges(mock: &mut MockTransport) {
        mock.queue_after_write(system_configuration_response())
            .queue_after_write(auth_response());
    }

    fn expected_writes() -> Vec<Vec<u8>> {
        vec![
            encode(&wire::device_information_query()).unwrap(),
            encode(&wire::system_configuration_query()).unwrap(),
            encode(&wire::device_authentication_query()).unwrap(),
        ]
    }

    #[test]
    fn bootstrap_happy_path() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, clock) = bootstrap_session(mock);

        let bootstrap = session.bootstrap().unwrap();
        assert_eq!(bootstrap.device.os_name, "Linux");
        assert_eq!(bootstrap.device.firmware_version, "1.2.3");
        assert_eq!(bootstrap.device.product_name, "PASE");
        assert_eq!(bootstrap.device.serial_number, "SN42");
        assert_eq!(bootstrap.auth, "token");
        assert_eq!(bootstrap.readiness_attempts, 1);
        assert!(session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        let mock = session.into_transport();
        assert_eq!(mock.writes, expected_writes());
        assert_eq!(mock.pending_reads(), 0);
    }

    #[test]
    fn bootstrap_drains_stale_frames_before_the_first_probe() {
        let stale = response(
            Some(Message::new().uint(1, 1).uint(2, 77)),
            None,
            Some((field::ACKNOWLEDGEMENT, Message::new())),
        );
        let mut mock = MockTransport::new();
        mock.queue_read(stale).queue_after_write(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, _) = bootstrap_session(mock);
        assert!(session.bootstrap().is_ok());
    }

    #[test]
    fn bootstrap_skips_stale_tracked_responses() {
        let stale = response(
            Some(Message::new().uint(1, 1).uint(2, 9)),
            None,
            Some((field::DEVICE_INFORMATION, device_info_body())),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(stale).queue_read(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(session.bootstrap().unwrap().readiness_attempts, 1);
    }

    #[test]
    fn bootstrap_skips_headerless_pong_and_events() {
        let pong = response(None, None, Some((field::PONG, Message::new().bytes(1, b"hello?"))));
        let event = response(None, None, Some((field::ASYNCHRONOUS_EVENT, Message::new().uint(1, 1))));
        let mut mock = MockTransport::new();
        mock.queue_after_write(pong)
            .queue_read(event)
            .queue_read(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, _) = bootstrap_session(mock);
        assert!(session.bootstrap().is_ok());
    }

    #[test]
    fn bootstrap_rejects_unexpected_headers() {
        let wrong_version = response(
            Some(Message::new().uint(1, 2)),
            None,
            Some((field::DEVICE_INFORMATION, device_info_body())),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(wrong_version);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(session.bootstrap(), Err(SessionError::UnexpectedHeader));
        assert!(!session.is_open());

        let missing = response(None, None, Some((field::DEVICE_INFORMATION, device_info_body())));
        let mut mock = MockTransport::new();
        mock.queue_after_write(missing);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(session.bootstrap(), Err(SessionError::UnexpectedHeader));
    }

    #[test]
    fn bootstrap_fails_on_a_device_error() {
        let rejected = response(
            Some(bootstrap_header()),
            Some(Message::new().uint(1, 1).bytes(2, b"busy")),
            Some((field::DEVICE_INFORMATION, device_info_body())),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(rejected);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::Rejected(ProtocolError {
                code: 1,
                why: "busy".into()
            }))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn bootstrap_fails_on_the_wrong_body() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(system_configuration_response());
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::UnexpectedBody {
                expected: field::DEVICE_INFORMATION,
                actual: Some(field::SYSTEM_CONFIGURATION)
            })
        );

        let header_only = response(Some(bootstrap_header()), None, None);
        let mut mock = MockTransport::new();
        mock.queue_after_write(header_only);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::UnexpectedBody {
                expected: field::DEVICE_INFORMATION,
                actual: None
            })
        );
    }

    #[test]
    fn bootstrap_fails_on_a_malformed_response() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(encode(&[0x0a, 0x09, 0x08]).unwrap());
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::Decode(DecodeError::Truncated))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn bootstrap_retries_only_zero_byte_writes_with_backoff() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Ok(()));
        mock.queue_wait_for_write()
            .queue_wait_for_write()
            .queue_after_write(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, clock) = bootstrap_session(mock);

        let bootstrap = session.bootstrap().unwrap();
        assert_eq!(bootstrap.readiness_attempts, 3);
        assert_eq!(clock.sleeps_ms(), [500, 1000]);
        let mock = session.into_transport();
        assert_eq!(mock.writes.len(), 5);
        assert_eq!(mock.writes[0], mock.writes[2]);
    }

    #[test]
    fn bootstrap_backoff_caps_at_two_seconds() {
        let mut mock = MockTransport::new();
        for _ in 0..4 {
            mock.queue_write_result(Err(WriteError::TimedOut));
            mock.queue_wait_for_write();
        }
        mock.queue_write_result(Ok(()));
        mock.queue_after_write(device_info_response());
        queue_remaining_exchanges(&mut mock);
        let (mut session, clock) = bootstrap_session(mock);

        assert_eq!(session.bootstrap().unwrap().readiness_attempts, 5);
        assert_eq!(clock.sleeps_ms(), [500, 1000, 2000, 2000]);
    }

    #[test]
    fn bootstrap_partial_write_is_terminal() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::Partial { written: 3 }));
        let (mut session, clock) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::Write(WriteError::Partial { written: 3 }))
        );
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        assert_eq!(session.into_transport().writes.len(), 1);
    }

    #[test]
    fn bootstrap_gives_up_at_the_readiness_deadline() {
        let mut mock = MockTransport::new();
        for _ in 0..20 {
            mock.queue_write_result(Err(WriteError::TimedOut));
        }
        let (mut session, clock) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::NotReady { attempts: 12 })
        );
        assert!(!session.is_open());
        let sleeps = clock.sleeps_ms();
        assert_eq!(sleeps.len(), 12);
        assert_eq!(&sleeps[..3], [500, 1000, 2000]);
        assert_eq!(sleeps[11], 500);
        assert_eq!(sleeps.iter().sum::<u64>(), 20_000);
        assert_eq!(session.into_transport().writes.len(), 12);
    }

    #[test]
    fn bootstrap_does_not_resend_after_a_complete_write_without_reply() {
        let mut mock = MockTransport::new();
        mock.queue_wait_for_write();
        let (mut session, clock) = bootstrap_session(mock);
        assert_eq!(session.bootstrap(), Err(SessionError::Timeout));
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        assert_eq!(session.into_transport().writes.len(), 1);
    }

    #[test]
    fn later_exchanges_are_sent_once_and_any_write_failure_is_terminal() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Ok(()))
            .queue_write_result(Err(WriteError::TimedOut));
        mock.queue_after_write(device_info_response())
            .queue_after_write(system_configuration_response());
        let (mut session, clock) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::Write(WriteError::TimedOut))
        );
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        assert_eq!(session.into_transport().writes.len(), 2);
    }

    #[test]
    fn later_exchange_wrong_body_is_terminal() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(device_info_response())
            .queue_after_write(auth_response());
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(
            session.bootstrap(),
            Err(SessionError::UnexpectedBody {
                expected: field::SYSTEM_CONFIGURATION,
                actual: Some(field::DEVICE_AUTHENTICATION)
            })
        );
    }

    #[test]
    fn bootstrap_stops_after_too_many_stale_frames() {
        let stale = response(
            Some(Message::new().uint(1, 1).uint(2, 5)),
            None,
            Some((field::PONG, Message::new())),
        );
        let mut bytes = Vec::new();
        for _ in 0..=MAX_SKIPPED_FRAMES {
            bytes.extend(&stale);
        }
        let mut mock = MockTransport::new();
        mock.queue_after_write(bytes);
        let (mut session, _) = bootstrap_session(mock);
        assert_eq!(session.bootstrap(), Err(SessionError::TooManySkipped));
        assert!(!session.is_open());
    }

    fn tracked_header(track: u64) -> Message {
        Message::new().uint(1, 1).uint(2, track)
    }

    fn config_body() -> Message {
        let work = Message::new().uint(2, 1).bytes(3, b"wall.mp4");
        let display = Message::new().uint(1, 1).uint(2, 75);
        Message::new()
            .message(1, &Message::new().bytes(1, b"boot.mp4"))
            .message(2, &Message::new().uint(1, 1).bytes(2, b"idle.mp4"))
            .message(3, &work)
            .message(5, &display)
    }

    fn config_response(track: u64) -> Vec<u8> {
        response(
            Some(tracked_header(track)),
            None,
            Some((field::USER_CONFIGURATION, config_body())),
        )
    }

    fn tracked_session(mock: MockTransport, seed: u64) -> (Session<MockTransport>, FakeClock) {
        let (mut session, clock) = bootstrap_session(mock);
        session.seed_track_ids(seed);
        (session, clock)
    }

    #[test]
    fn track_ids_increment_and_skip_zero() {
        let (mut session, _) = tracked_session(MockTransport::new(), u64::MAX - 1);
        assert_eq!(session.allocate_track().get(), u64::MAX - 1);
        assert_eq!(session.allocate_track().get(), u64::MAX);
        assert_eq!(session.allocate_track().get(), 1);
        assert_eq!(session.allocate_track().get(), 2);
        session.seed_track_ids(0);
        assert_eq!(session.allocate_track().get(), 1);
    }

    #[test]
    fn track_ids_start_from_a_random_non_zero_seed() {
        let first = Session::new(MockTransport::new()).allocate_track().get();
        let second = Session::new(MockTransport::new()).allocate_track().get();
        assert_ne!(first, 0);
        assert_ne!(first, second);
    }

    #[test]
    fn query_returns_the_configuration_and_writes_the_tracked_request() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(config_response(41));
        let (mut session, clock) = tracked_session(mock, 41);

        let config = session.query_user_configuration().unwrap();
        assert_eq!(config.work.as_ref().unwrap().single_mode_media_file, "wall.mp4");
        assert_eq!(config.display.as_ref().unwrap().backlight_brightness, 75);
        assert_eq!(config.standby.as_ref().unwrap().media_file, "idle.mp4");
        assert!(session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        let mock = session.into_transport();
        assert_eq!(
            mock.writes,
            vec![encode(&wire::user_configuration_query(NonZeroU64::new(41).unwrap())).unwrap()]
        );
    }

    #[test]
    fn execute_drains_stale_frames_before_writing() {
        let stale = config_response(40);
        let mut mock = MockTransport::new();
        mock.queue_read(stale).queue_after_write(config_response(41));
        let (mut session, _) = tracked_session(mock, 41);
        assert!(session.query_user_configuration().is_ok());
    }

    #[test]
    fn execute_skips_other_tracks_pongs_and_events() {
        let other_track = config_response(99);
        let pong = response(None, None, Some((field::PONG, Message::new().bytes(1, b"hello?"))));
        let event = response(
            Some(tracked_header(41)),
            None,
            Some((field::ASYNCHRONOUS_EVENT, Message::new().uint(1, 1))),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(other_track)
            .queue_read(pong)
            .queue_read(event)
            .queue_read(config_response(41));
        let (mut session, _) = tracked_session(mock, 41);
        assert!(session.query_user_configuration().is_ok());
        assert!(session.is_open());
    }

    #[test]
    fn execute_rejects_a_headerless_response() {
        let missing = response(None, None, Some((field::USER_CONFIGURATION, config_body())));
        let mut mock = MockTransport::new();
        mock.queue_after_write(missing);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.query_user_configuration(), Err(SessionError::UnexpectedHeader));
        assert!(!session.is_open());
    }

    #[test]
    fn execute_returns_a_device_error_without_closing() {
        let rejected = response(
            Some(tracked_header(41)),
            Some(Message::new().uint(1, 2).bytes(2, b"unsupported")),
            None,
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(rejected);
        let (mut session, clock) = tracked_session(mock, 41);
        assert_eq!(
            session.query_user_configuration(),
            Err(SessionError::Rejected(ProtocolError {
                code: 2,
                why: "unsupported".into()
            }))
        );
        assert!(session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
    }

    #[test]
    fn execute_closes_on_the_wrong_body() {
        let wrong = response(
            Some(tracked_header(41)),
            None,
            Some((field::ACKNOWLEDGEMENT, Message::new())),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(wrong);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.query_user_configuration(),
            Err(SessionError::UnexpectedBody {
                expected: field::USER_CONFIGURATION,
                actual: Some(field::ACKNOWLEDGEMENT)
            })
        );
        assert!(!session.is_open());
    }

    #[test]
    fn execute_closes_on_a_malformed_response() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(encode(&[0x0a, 0x09, 0x08]).unwrap());
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.query_user_configuration(),
            Err(SessionError::Decode(DecodeError::Truncated))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn query_retries_once_after_a_clean_timeout() {
        let mut mock = MockTransport::new();
        mock.queue_wait_for_write().queue_after_write(config_response(42));
        let (mut session, clock) = tracked_session(mock, 41);

        assert!(session.query_user_configuration().is_ok());
        assert!(session.is_open());
        assert_eq!(clock.sleeps_ms(), [250]);
        let mock = session.into_transport();
        assert_eq!(mock.writes.len(), 2);
        assert_eq!(
            mock.writes[0],
            encode(&wire::user_configuration_query(NonZeroU64::new(41).unwrap())).unwrap()
        );
        assert_eq!(
            mock.writes[1],
            encode(&wire::user_configuration_query(NonZeroU64::new(42).unwrap())).unwrap()
        );
    }

    #[test]
    fn query_gives_up_after_the_second_clean_timeout() {
        let mut mock = MockTransport::new();
        mock.queue_wait_for_write().queue_wait_for_write();
        let (mut session, clock) = tracked_session(mock, 41);
        assert_eq!(session.query_user_configuration(), Err(SessionError::Timeout));
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), [250]);
        assert_eq!(session.into_transport().writes.len(), 2);
    }

    #[test]
    fn a_partial_frame_at_the_deadline_is_not_retried() {
        let partial = config_response(41)[..5].to_vec();
        let mut mock = MockTransport::new();
        mock.queue_after_write(partial);
        let (mut session, clock) = tracked_session(mock, 41);
        assert_eq!(
            session.query_user_configuration(),
            Err(SessionError::IncompleteResponse)
        );
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        assert_eq!(session.into_transport().writes.len(), 1);
    }

    #[test]
    fn a_tracked_write_failure_closes_without_retry() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut));
        let (mut session, clock) = tracked_session(mock, 41);
        assert_eq!(
            session.query_user_configuration(),
            Err(SessionError::Write(WriteError::TimedOut))
        );
        assert!(!session.is_open());
        assert_eq!(clock.sleeps_ms(), Vec::<u64>::new());
        assert_eq!(session.into_transport().writes.len(), 1);
    }

    #[test]
    fn execute_stops_after_too_many_unrelated_tracks() {
        let unrelated = config_response(7);
        let mut bytes = Vec::new();
        for _ in 0..=MAX_SKIPPED_FRAMES {
            bytes.extend(&unrelated);
        }
        let mut mock = MockTransport::new();
        mock.queue_after_write(bytes);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.query_user_configuration(), Err(SessionError::TooManySkipped));
        assert!(!session.is_open());
    }

    #[test]
    fn execute_on_a_closed_session_is_refused() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::Unknown));
        let (mut session, _) = tracked_session(mock, 41);
        assert!(session.query_user_configuration().is_err());
        assert_eq!(session.query_user_configuration(), Err(SessionError::Closed));
    }

    fn trigger_frame(track: u64) -> Vec<u8> {
        encode(&wire::activation_trigger(NonZeroU64::new(track).unwrap())).unwrap()
    }

    fn acknowledgement(track: u64) -> Vec<u8> {
        response(
            Some(tracked_header(track)),
            None,
            Some((field::ACKNOWLEDGEMENT, Message::new())),
        )
    }

    #[test]
    fn activate_writes_the_trigger_and_takes_the_acknowledgement() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(acknowledgement(41));
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.activate(), Ok(OptionalReply::Acknowledged));
        assert!(session.is_open());
        let mock = session.into_transport();
        assert_eq!(mock.writes, vec![trigger_frame(41)]);
        assert_eq!(mock.pending_reads(), 0);
    }

    #[test]
    fn activate_accepts_no_reply() {
        let mut mock = MockTransport::new();
        mock.queue_wait_for_write();
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.activate(), Ok(OptionalReply::None));
        assert!(session.is_open());
    }

    #[test]
    fn activate_consumes_one_unrelated_frame() {
        let pong = response(None, None, Some((field::PONG, Message::new().bytes(1, b"hello?"))));
        let mut mock = MockTransport::new();
        mock.queue_after_write(pong).queue_read(acknowledgement(41));
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.activate(), Ok(OptionalReply::Drained));
        assert!(session.is_open());
        assert_eq!(session.into_transport().pending_reads(), 1);
    }

    #[test]
    fn activate_discards_a_partial_frame_at_the_deadline() {
        let partial = acknowledgement(41)[..5].to_vec();
        let mut mock = MockTransport::new();
        mock.queue_after_write(partial);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.activate(), Ok(OptionalReply::None));
        assert!(session.is_open());
        assert_eq!(session.decoder.buffered(), 0);
    }

    #[test]
    fn activate_treats_a_malformed_reply_as_drained() {
        let mut mock = MockTransport::new();
        mock.queue_after_write(encode(&[0x0a, 0x09, 0x08]).unwrap());
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.activate(), Ok(OptionalReply::Drained));
        assert!(session.is_open());
    }

    #[test]
    fn activate_closes_on_a_matching_rejection() {
        let rejected = response(
            Some(tracked_header(41)),
            Some(Message::new().uint(1, 1).bytes(2, b"no")),
            None,
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(rejected);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.activate(),
            Err(SessionError::Rejected(ProtocolError {
                code: 1,
                why: "no".into()
            }))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn activate_closes_on_an_unrelated_error_frame() {
        let rejected = response(
            Some(tracked_header(9)),
            Some(Message::new().uint(1, 2)),
            Some((field::ACKNOWLEDGEMENT, Message::new())),
        );
        let mut mock = MockTransport::new();
        mock.queue_after_write(rejected);
        let (mut session, _) = tracked_session(mock, 41);
        assert!(matches!(session.activate(), Err(SessionError::Rejected(_))));
        assert!(!session.is_open());
    }

    #[test]
    fn activate_closes_on_a_write_failure() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut));
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.activate(),
            Err(SessionError::Write(WriteError::TimedOut))
        );
        assert!(!session.is_open());
    }

    #[test]
    fn ping_writes_the_keepalive_and_accepts_silence() {
        let mut mock = MockTransport::new();
        mock.queue_wait_for_write();
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.ping(), KeepaliveOutcome::Sent(OptionalReply::None));
        assert!(session.is_open());
        let mock = session.into_transport();
        assert_eq!(mock.writes, vec![encode(&wire::keepalive_ping()).unwrap()]);
    }

    #[test]
    fn ping_consumes_an_optional_pong() {
        let pong = response(None, None, Some((field::PONG, Message::new().bytes(1, b"hello?"))));
        let mut mock = MockTransport::new();
        mock.queue_after_write(pong);
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(session.ping(), KeepaliveOutcome::Sent(OptionalReply::Drained));
        assert!(session.is_open());
    }

    #[test]
    fn ping_zero_byte_timeout_is_retryable_and_keeps_the_session() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut));
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.ping(),
            KeepaliveOutcome::Retryable(SessionError::Write(WriteError::TimedOut))
        );
        assert!(session.is_open());
        assert_eq!(session.ping(), KeepaliveOutcome::Sent(OptionalReply::None));
    }

    #[test]
    fn ping_partial_write_is_fatal() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::Partial { written: 4 }));
        let (mut session, _) = tracked_session(mock, 41);
        assert_eq!(
            session.ping(),
            KeepaliveOutcome::Fatal(SessionError::Write(WriteError::Partial { written: 4 }))
        );
        assert!(!session.is_open());
        assert_eq!(session.ping(), KeepaliveOutcome::Fatal(SessionError::Closed));
    }

    #[test]
    fn ping_error_frame_is_fatal() {
        let rejected = response(None, Some(Message::new().uint(1, 1)), None);
        let mut mock = MockTransport::new();
        mock.queue_after_write(rejected);
        let (mut session, _) = tracked_session(mock, 41);
        assert!(matches!(session.ping(), KeepaliveOutcome::Fatal(SessionError::Rejected(_))));
        assert!(!session.is_open());
    }
}
