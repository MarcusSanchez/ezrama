//! Overlapped I/O on the printer interface handle.
//!
//! One input transfer is armed before every output transfer and stays armed
//! while the output is in flight. It is never re-armed while an output is
//! pending. A timed-out output is cancelled and the cancellation is waited
//! for before its buffer is released; if the cancellation never completes
//! the buffer is leaked on purpose and the transport is marked lost, since
//! the kernel may still write into it.

use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use crate::transport::{ReadError, Transport, WriteError};
use crate::usbprint::{Device, WinError};
use crate::win::*;

/// Size of each input transfer.
pub const READ_BUFFER_SIZE: usize = 64 * 1024;
/// Input transfer errors tolerated before the transport latches as lost.
pub const MAX_READ_ERROR_RETRIES: u32 = 10;
/// Longest wait for a cancelled transfer to complete before its buffer is
/// abandoned.
pub const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_millis(2000);

const READ_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(5);
const READ_RETRY_MAX_BACKOFF: Duration = Duration::from_millis(100);
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// An in-flight transfer. Boxed so its address is stable for the kernel,
/// and only dropped once the transfer has completed or been cancelled.
struct Pending {
    overlapped: OVERLAPPED,
    buffer: Vec<u8>,
    event: HANDLE,
}

impl Pending {
    fn new(buffer: Vec<u8>) -> Result<Box<Self>, WinError> {
        let event = unsafe { CreateEventW(ptr::null_mut(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(WinError::last("CreateEventW"));
        }
        Ok(Box::new(Pending {
            overlapped: OVERLAPPED {
                Internal: 0,
                InternalHigh: 0,
                Offset: 0,
                OffsetHigh: 0,
                hEvent: event,
            },
            buffer,
            event,
        }))
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
        }
    }
}

/// Result of a transfer whose event has been signalled.
#[derive(Debug)]
enum Completion {
    /// Completed; `usize` bytes were transferred.
    Done(usize),
    /// Cancelled; `usize` bytes were transferred before that.
    Aborted(usize),
    /// Failed; `usize` bytes were transferred before that.
    Error(WinError, usize),
}

/// How a wait for the armed read ended.
enum Waited {
    Timeout,
    Interrupted,
    Completed(Completion),
}

impl Completion {
    fn transferred(&self) -> usize {
        match self {
            Completion::Done(n) | Completion::Aborted(n) | Completion::Error(_, n) => *n,
        }
    }
}

/// Whether an error code means the device has gone away.
pub fn is_device_lost(code: DWORD) -> bool {
    matches!(
        code,
        ERROR_FILE_NOT_FOUND
            | ERROR_INVALID_HANDLE
            | ERROR_BAD_UNIT
            | ERROR_NO_SUCH_DEVICE
            | ERROR_DEVICE_NOT_CONNECTED
            | ERROR_DEVICE_REMOVED
    )
}

/// Backoff before re-arming input after its `attempt`th consecutive error.
pub fn read_retry_backoff(attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(8);
    (READ_RETRY_INITIAL_BACKOFF * factor).min(READ_RETRY_MAX_BACKOFF)
}

/// Milliseconds for a wait call, never the infinite sentinel.
pub fn wait_millis(duration: Duration) -> DWORD {
    duration
        .as_millis()
        .min(u128::from(WAIT_FAILED - 1)) as DWORD
}

/// Outcome of taking down an armed read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disarm {
    /// No read was armed.
    NotArmed,
    /// The read had already completed with this many bytes, now queued.
    Completed(usize),
    /// The read was cancelled and reaped within the given time.
    Cancelled(Duration),
    /// The cancellation did not complete; the buffer was abandoned and the
    /// transport is now lost.
    Abandoned,
}

/// Overlapped transport over an open device handle.
pub struct UsbprintTransport {
    read: Option<Box<Pending>>,
    queue: Vec<u8>,
    read_errors: u32,
    lost: Option<String>,
    device: Device,
    interrupt: Option<HANDLE>,
}

// The transport is used from one thread at a time and may move between
// threads with its handle.
unsafe impl Send for UsbprintTransport {}

impl UsbprintTransport {
    pub fn new(device: Device) -> Self {
        Self {
            read: None,
            queue: Vec::new(),
            read_errors: 0,
            lost: None,
            device,
            interrupt: None,
        }
    }

    /// Makes every read wait end early, with [`ReadError::Interrupted`],
    /// while the manual-reset event `interrupt` is set. The handle stays
    /// owned by the caller.
    pub fn with_interrupt(mut self, interrupt: HANDLE) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Why the transport is unusable, once it has latched.
    pub fn lost(&self) -> Option<&str> {
        self.lost.as_deref()
    }

    pub fn read_armed(&self) -> bool {
        self.read.is_some()
    }

    fn handle(&self) -> HANDLE {
        self.device.handle()
    }

    fn latch_lost(&mut self, reason: String) -> String {
        if self.lost.is_none() {
            self.lost = Some(reason.clone());
        }
        self.lost.clone().unwrap_or(reason)
    }

    fn check_lost_for_read(&self) -> Result<(), ReadError> {
        match &self.lost {
            Some(reason) => Err(ReadError::Lost(reason.clone())),
            None => Ok(()),
        }
    }

    /// Arms an input transfer if none is pending. Applies the bounded retry
    /// policy when the submission itself fails.
    pub fn arm_read(&mut self) -> Result<(), ReadError> {
        self.check_lost_for_read()?;
        while self.read.is_none() {
            let mut pending = Pending::new(vec![0u8; READ_BUFFER_SIZE])
                .map_err(|error| ReadError::Failed(error.to_string()))?;
            let submitted = unsafe {
                ReadFile(
                    self.handle(),
                    pending.buffer.as_mut_ptr() as *mut c_void,
                    READ_BUFFER_SIZE as DWORD,
                    ptr::null_mut(),
                    &mut pending.overlapped,
                )
            };
            if submitted != 0 {
                self.read = Some(pending);
                break;
            }
            let error = WinError::last("ReadFile");
            if error.code == ERROR_IO_PENDING {
                self.read = Some(pending);
                break;
            }
            drop(pending);
            self.read_failure(error)?;
        }
        Ok(())
    }

    /// Records an input error. Returns `Ok` after backing off when another
    /// attempt is allowed, or the latched error when the budget is spent.
    fn read_failure(&mut self, error: WinError) -> Result<(), ReadError> {
        if is_device_lost(error.code) {
            return Err(ReadError::Lost(self.latch_lost(error.to_string())));
        }
        self.read_errors += 1;
        if self.read_errors > MAX_READ_ERROR_RETRIES {
            let reason = format!(
                "input failed {} times in a row, last: {error}",
                self.read_errors
            );
            return Err(ReadError::Lost(self.latch_lost(reason)));
        }
        thread::sleep(read_retry_backoff(self.read_errors));
        Ok(())
    }

    /// Reads the result of a transfer whose event has been signalled.
    fn completion(&self, pending: &mut Pending) -> Completion {
        let mut transferred: DWORD = 0;
        let ok = unsafe {
            GetOverlappedResult(
                self.handle(),
                &mut pending.overlapped,
                &mut transferred,
                0,
            )
        };
        let transferred = transferred as usize;
        if ok != 0 {
            return Completion::Done(transferred);
        }
        let error = WinError::last("GetOverlappedResult");
        if error.code == ERROR_OPERATION_ABORTED {
            Completion::Aborted(transferred)
        } else {
            Completion::Error(error, transferred)
        }
    }

    /// Waits up to `timeout` for the armed read, or for the interrupt if
    /// one is set. On completion the transfer is reaped and its buffer
    /// released; the read is not re-armed. An interrupt leaves it armed.
    fn wait_read(&mut self, timeout: Duration) -> Waited {
        let Some(pending) = self.read.as_ref() else {
            return Waited::Timeout;
        };
        let event = pending.event;
        let waited = match self.interrupt {
            Some(interrupt) => {
                let handles = [event, interrupt];
                unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, wait_millis(timeout)) }
            }
            None => unsafe { WaitForSingleObject(event, wait_millis(timeout)) },
        };
        if self.interrupt.is_some() && waited == WAIT_OBJECT_0 + 1 {
            return Waited::Interrupted;
        }
        if waited != WAIT_OBJECT_0 {
            return Waited::Timeout;
        }
        let Some(mut pending) = self.read.take() else {
            return Waited::Timeout;
        };
        let completion = self.completion(&mut pending);
        if let Completion::Done(n) | Completion::Aborted(n) = completion {
            self.queue.extend_from_slice(&pending.buffer[..n.min(pending.buffer.len())]);
        }
        Waited::Completed(completion)
    }

    /// Applies a read completion: counts bytes, resets or advances the error
    /// budget. Returns how many bytes were queued.
    fn apply_read_completion(&mut self, completion: Completion) -> Result<usize, ReadError> {
        match completion {
            Completion::Done(n) | Completion::Aborted(n) => {
                if n > 0 {
                    self.read_errors = 0;
                }
                Ok(n)
            }
            Completion::Error(error, n) => {
                self.read_failure(error)?;
                Ok(n)
            }
        }
    }

    /// Cancels an armed read and waits for the cancellation to land.
    pub fn disarm_read(&mut self) -> Disarm {
        let Some(mut pending) = self.read.take() else {
            return Disarm::NotArmed;
        };
        let started = Instant::now();
        let already_done = unsafe { WaitForSingleObject(pending.event, 0) } == WAIT_OBJECT_0;
        if !already_done {
            let cancelled = unsafe { CancelIoEx(self.handle(), &mut pending.overlapped) };
            if cancelled == 0 {
                let error = WinError::last("CancelIoEx");
                if error.code != ERROR_NOT_FOUND {
                    mem::forget(pending);
                    self.latch_lost(format!("could not cancel the input transfer: {error}"));
                    return Disarm::Abandoned;
                }
            }
            let landed = unsafe {
                WaitForSingleObject(pending.event, wait_millis(CANCEL_DRAIN_TIMEOUT))
            };
            if landed != WAIT_OBJECT_0 {
                mem::forget(pending);
                self.latch_lost("input cancellation did not complete".to_string());
                return Disarm::Abandoned;
            }
        }
        let completion = self.completion(&mut pending);
        let transferred = completion.transferred();
        if transferred > 0 {
            self.queue
                .extend_from_slice(&pending.buffer[..transferred.min(pending.buffer.len())]);
        }
        drop(pending);
        if already_done {
            Disarm::Completed(transferred)
        } else {
            Disarm::Cancelled(started.elapsed())
        }
    }
}

impl Transport for UsbprintTransport {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, ReadError> {
        self.check_lost_for_read()?;
        if !self.queue.is_empty() {
            return Ok(mem::take(&mut self.queue));
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.arm_read()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.wait_read(remaining) {
                Waited::Timeout => return Ok(Vec::new()),
                Waited::Interrupted => return Err(ReadError::Interrupted),
                Waited::Completed(completion) => {
                    let queued = self.apply_read_completion(completion)?;
                    if queued > 0 {
                        return Ok(mem::take(&mut self.queue));
                    }
                    if Instant::now() >= deadline {
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }

    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), WriteError> {
        if let Some(reason) = &self.lost {
            return Err(WriteError::Lost(reason.clone()));
        }
        self.arm_read().map_err(|error| match error {
            ReadError::Lost(reason) => WriteError::Lost(reason),
            ReadError::Failed(reason) => WriteError::Failed { written: 0, reason },
            ReadError::Interrupted => WriteError::Failed {
                written: 0,
                reason: "interrupted".to_string(),
            },
        })?;

        let mut pending = Pending::new(data.to_vec()).map_err(|error| WriteError::Failed {
            written: 0,
            reason: error.to_string(),
        })?;
        let submitted = unsafe {
            WriteFile(
                self.handle(),
                pending.buffer.as_ptr() as *const c_void,
                pending.buffer.len() as DWORD,
                ptr::null_mut(),
                &mut pending.overlapped,
            )
        };
        if submitted == 0 {
            let error = WinError::last("WriteFile");
            if error.code != ERROR_IO_PENDING {
                drop(pending);
                return Err(self.write_failure(error, 0));
            }
        }

        let deadline = Instant::now() + timeout;
        let signalled = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = remaining.min(WAIT_SLICE);
            let waited = unsafe { WaitForSingleObject(pending.event, wait_millis(slice)) };
            if waited == WAIT_OBJECT_0 {
                break true;
            }
            if waited != WAIT_TIMEOUT || remaining.is_zero() {
                break false;
            }
        };

        let result = if signalled {
            match self.completion(&mut pending) {
                Completion::Done(n) if n == data.len() => Ok(()),
                Completion::Done(n) => Err(WriteError::Partial { written: n }),
                Completion::Aborted(n) => Err(WriteError::Failed {
                    written: n,
                    reason: "transfer was aborted".to_string(),
                }),
                Completion::Error(error, n) => Err(self.write_failure(error, n)),
            }
        } else {
            let cancelled = unsafe { CancelIoEx(self.handle(), &mut pending.overlapped) };
            if cancelled == 0 {
                let error = WinError::last("CancelIoEx");
                if error.code != ERROR_NOT_FOUND {
                    mem::forget(pending);
                    self.latch_lost(format!("could not cancel the output transfer: {error}"));
                    return Err(WriteError::Unknown);
                }
            }
            let landed = unsafe {
                WaitForSingleObject(pending.event, wait_millis(CANCEL_DRAIN_TIMEOUT))
            };
            if landed != WAIT_OBJECT_0 {
                mem::forget(pending);
                self.latch_lost("output cancellation did not complete".to_string());
                return Err(WriteError::Unknown);
            }
            match self.completion(&mut pending) {
                Completion::Done(n) if n == data.len() => Ok(()),
                Completion::Aborted(0) => Err(WriteError::TimedOut),
                Completion::Done(n) | Completion::Aborted(n) => {
                    Err(WriteError::Partial { written: n })
                }
                Completion::Error(error, n) => Err(self.write_failure(error, n)),
            }
        };
        drop(pending);

        if let Waited::Completed(completion) = self.wait_read(Duration::ZERO) {
            let _ = self.apply_read_completion(completion);
        }
        result
    }
}

impl UsbprintTransport {
    fn write_failure(&mut self, error: WinError, written: usize) -> WriteError {
        if is_device_lost(error.code) {
            WriteError::Lost(self.latch_lost(error.to_string()))
        } else {
            WriteError::Failed {
                written,
                reason: error.to_string(),
            }
        }
    }
}

impl Drop for UsbprintTransport {
    fn drop(&mut self) {
        self.disarm_read();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbprint::{self, Discovery};

    /// With the display present: an interrupt that is already set ends a
    /// read wait at once and leaves the read armed; once cleared, the
    /// same read times out normally and is reaped. Nothing is written.
    #[test]
    fn an_interrupt_ends_a_read_wait_early() {
        let Ok(Discovery::One(path)) = usbprint::find_panorama() else {
            return;
        };
        let Ok(device) = Device::open(&path) else {
            return;
        };
        let interrupt = unsafe { CreateEventW(ptr::null_mut(), 1, 1, ptr::null()) };
        let mut transport = UsbprintTransport::new(device).with_interrupt(interrupt);
        let started = Instant::now();
        assert_eq!(transport.read(Duration::from_secs(5)), Err(ReadError::Interrupted));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(transport.read_armed());
        assert!(transport.lost().is_none());
        unsafe {
            ResetEvent(interrupt);
        }
        let read = transport.read(Duration::from_millis(100));
        assert!(read.is_ok(), "after the interrupt clears, reads work: {read:?}");
        assert!(!matches!(transport.disarm_read(), Disarm::Abandoned));
        unsafe {
            CloseHandle(interrupt);
        }
    }

    #[test]
    fn overlapped_layout_matches_the_platform() {
        assert_eq!(mem::size_of::<OVERLAPPED>(), 32);
    }

    #[test]
    fn device_loss_codes() {
        for code in [2, 6, 20, 433, 1167, 1617] {
            assert!(is_device_lost(code), "{code}");
        }
        for code in [5, 31, 995, 996, 997, 1168] {
            assert!(!is_device_lost(code), "{code}");
        }
    }

    #[test]
    fn read_retry_backoff_doubles_and_caps() {
        let millis: Vec<u64> = (0..=8).map(|n| read_retry_backoff(n).as_millis() as u64).collect();
        assert_eq!(millis, [5, 5, 10, 20, 40, 80, 100, 100, 100]);
        assert_eq!(read_retry_backoff(u32::MAX), READ_RETRY_MAX_BACKOFF);
    }

    #[test]
    fn wait_millis_never_produces_the_infinite_sentinel() {
        assert_eq!(wait_millis(Duration::ZERO), 0);
        assert_eq!(wait_millis(Duration::from_millis(300)), 300);
        assert_eq!(wait_millis(Duration::from_secs(u64::MAX)), WAIT_FAILED - 1);
    }
}
