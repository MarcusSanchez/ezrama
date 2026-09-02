//! The byte transport the session runs over, and a scripted stand-in for
//! tests.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

/// How a write ended when it did not transfer every byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// The deadline passed with nothing transferred and the write was
    /// cancelled cleanly. Sending the same bytes again is safe.
    TimedOut,
    /// The transfer stopped after `written` bytes.
    Partial { written: usize },
    /// The fate of the write could not be determined; the transport is no
    /// longer usable.
    Unknown,
    /// The device is gone or the transport has latched as lost.
    Lost(String),
    /// The write failed with a known result.
    Failed { written: usize, reason: String },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::TimedOut => write!(f, "write timed out before any byte was transferred"),
            WriteError::Partial { written } => {
                write!(f, "write stopped after {written} bytes")
            }
            WriteError::Unknown => write!(f, "write outcome is unknown"),
            WriteError::Lost(reason) => write!(f, "device lost: {reason}"),
            WriteError::Failed { written, reason } => {
                write!(f, "write failed after {written} bytes: {reason}")
            }
        }
    }
}

impl std::error::Error for WriteError {}

/// Why a read could not be serviced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The device is gone or the transport has latched as lost.
    Lost(String),
    /// A call failed in a way that does not condemn the transport.
    Failed(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Lost(reason) => write!(f, "device lost: {reason}"),
            ReadError::Failed(reason) => write!(f, "read failed: {reason}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// A duplex byte stream to the device.
pub trait Transport {
    /// Writes all of `data`, waiting at most `timeout` for it to transfer.
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), WriteError>;

    /// Returns whatever bytes arrive within `timeout`. An empty result means
    /// the deadline passed with nothing received.
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, ReadError>;
}

/// One scripted answer to a `read` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadStep {
    Data(Vec<u8>),
    Timeout,
    Error(ReadError),
    /// Reads time out until a write has happened; the next write consumes
    /// this step and unblocks whatever follows it.
    WaitForWrite,
}

/// Records writes and serves reads from a script.
#[derive(Debug, Default)]
pub struct MockTransport {
    pub writes: Vec<Vec<u8>>,
    write_results: VecDeque<Result<(), WriteError>>,
    reads: VecDeque<ReadStep>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `bytes` on a later read.
    pub fn queue_read(&mut self, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.reads.push_back(ReadStep::Data(bytes.into()));
        self
    }

    /// Makes a later read time out with nothing received.
    pub fn queue_timeout(&mut self) -> &mut Self {
        self.reads.push_back(ReadStep::Timeout);
        self
    }

    /// Makes a later read fail.
    pub fn queue_read_error(&mut self, error: ReadError) -> &mut Self {
        self.reads.push_back(ReadStep::Error(error));
        self
    }

    /// Holds later reads at a timeout until the next write.
    pub fn queue_wait_for_write(&mut self) -> &mut Self {
        self.reads.push_back(ReadStep::WaitForWrite);
        self
    }

    /// Serves `bytes` only after the next write, as a device replying to a
    /// request would.
    pub fn queue_after_write(&mut self, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.queue_wait_for_write().queue_read(bytes)
    }

    /// Makes a later write return `result`. Writes without a queued result
    /// succeed.
    pub fn queue_write_result(&mut self, result: Result<(), WriteError>) -> &mut Self {
        self.write_results.push_back(result);
        self
    }

    /// Scripted reads not yet consumed.
    pub fn pending_reads(&self) -> usize {
        self.reads.len()
    }
}

impl Transport for MockTransport {
    fn write(&mut self, data: &[u8], _timeout: Duration) -> Result<(), WriteError> {
        self.writes.push(data.to_vec());
        if self.reads.front() == Some(&ReadStep::WaitForWrite) {
            self.reads.pop_front();
        }
        self.write_results.pop_front().unwrap_or(Ok(()))
    }

    fn read(&mut self, _timeout: Duration) -> Result<Vec<u8>, ReadError> {
        if self.reads.front() == Some(&ReadStep::WaitForWrite) {
            return Ok(Vec::new());
        }
        match self.reads.pop_front() {
            Some(ReadStep::Data(bytes)) => Ok(bytes),
            Some(ReadStep::Timeout) | None => Ok(Vec::new()),
            Some(ReadStep::Error(error)) => Err(error),
            Some(ReadStep::WaitForWrite) => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(10);

    #[test]
    fn mock_records_writes_and_succeeds_by_default() {
        let mut mock = MockTransport::new();
        assert_eq!(mock.write(b"one", T), Ok(()));
        assert_eq!(mock.write(b"two", T), Ok(()));
        assert_eq!(mock.writes, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn mock_serves_scripted_write_results_in_order() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Ok(()));
        assert_eq!(mock.write(b"a", T), Err(WriteError::TimedOut));
        assert_eq!(mock.write(b"b", T), Ok(()));
        assert_eq!(mock.write(b"c", T), Ok(()));
    }

    #[test]
    fn mock_serves_reads_timeouts_and_errors() {
        let mut mock = MockTransport::new();
        mock.queue_read(b"TRYX".to_vec())
            .queue_timeout()
            .queue_read_error(ReadError::Lost("unplugged".into()));
        assert_eq!(mock.pending_reads(), 3);
        assert_eq!(mock.read(T), Ok(b"TRYX".to_vec()));
        assert_eq!(mock.read(T), Ok(Vec::new()));
        assert_eq!(mock.read(T), Err(ReadError::Lost("unplugged".into())));
        assert_eq!(mock.read(T), Ok(Vec::new()));
        assert_eq!(mock.pending_reads(), 0);
    }

    #[test]
    fn mock_holds_reads_until_a_write_happens() {
        let mut mock = MockTransport::new();
        mock.queue_read(b"stale".to_vec())
            .queue_after_write(b"reply".to_vec());
        assert_eq!(mock.read(T), Ok(b"stale".to_vec()));
        assert_eq!(mock.read(T), Ok(Vec::new()));
        assert_eq!(mock.read(T), Ok(Vec::new()));
        assert_eq!(mock.pending_reads(), 2);
        assert_eq!(mock.write(b"request", T), Ok(()));
        assert_eq!(mock.read(T), Ok(b"reply".to_vec()));
        assert_eq!(mock.pending_reads(), 0);
    }

    #[test]
    fn errors_render() {
        assert_eq!(
            WriteError::Partial { written: 3 }.to_string(),
            "write stopped after 3 bytes"
        );
        assert_eq!(
            WriteError::Failed {
                written: 0,
                reason: "x".into()
            }
            .to_string(),
            "write failed after 0 bytes: x"
        );
        assert_eq!(ReadError::Failed("y".into()).to_string(), "read failed: y");
    }
}
