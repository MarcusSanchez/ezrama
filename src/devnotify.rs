//! Device arrival and removal notifications, and control requests, through
//! a message-only window.
//!
//! The message loop runs on whichever thread calls [`run_message_loop`] and
//! forwards events for the display's printer interface, and control
//! requests posted to the window, to a channel. It also raises an interrupt
//! flag so a blocking holding loop can notice promptly. Another process
//! reaches a running watcher through [`control_watcher`], which finds the
//! window by class name.

use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

use crate::overlapped::wait_millis;
use crate::usbprint::{is_panorama_path, wide, WinError};
use crate::watch::Event;
use crate::win::*;

/// Window class of the watcher's message-only window.
pub const WINDOW_CLASS: &str = "ezrama-watch";
/// Session-local event that is set while the watcher has released the
/// display on request.
pub const PAUSED_EVENT_NAME: &str = "Local\\ezrama-paused";

const WM_STOP_WATCH: u32 = WM_APP + 1;
const WM_PAUSE_WATCH: u32 = WM_APP + 2;
const WM_RESUME_WATCH: u32 = WM_APP + 3;

static SENDER: Mutex<Option<Sender<Event>>> = Mutex::new(None);
static INTERRUPT: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// A request sent to a running watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Pause,
    Resume,
    Stop,
}

impl Request {
    fn message(self) -> u32 {
        match self {
            Request::Pause => WM_PAUSE_WATCH,
            Request::Resume => WM_RESUME_WATCH,
            Request::Stop => WM_STOP_WATCH,
        }
    }
}

/// Whether an event has arrived since the flag was last cleared.
pub fn interrupted() -> bool {
    INTERRUPT.load(Ordering::SeqCst)
}

/// Clears the interrupt flag once the pending events have been handled.
pub fn clear_interrupt() {
    INTERRUPT.store(false, Ordering::SeqCst);
}

/// Raises the interrupt flag without an event, for a stop from elsewhere.
pub fn interrupt() {
    INTERRUPT.store(true, Ordering::SeqCst);
}

fn local_window() -> HWND {
    WINDOW.load(Ordering::SeqCst) as HWND
}

fn post(window: HWND, request: Request) -> bool {
    if window.is_null() {
        return false;
    }
    unsafe { PostMessageW(window, request.message(), 0, 0) != 0 }
}

/// Sends a request to the message loop running in this process.
pub fn request(request: Request) -> bool {
    post(local_window(), request)
}

/// Asks this process's message loop to quit.
pub fn request_stop() -> bool {
    request(Request::Stop)
}

/// Finds the window of a watcher running anywhere in this desktop session.
pub fn find_watcher() -> Option<HWND> {
    let class = wide(WINDOW_CLASS);
    let window = unsafe {
        FindWindowExW(HWND_MESSAGE, ptr::null_mut(), class.as_ptr(), ptr::null())
    };
    if window.is_null() {
        None
    } else {
        Some(window)
    }
}

/// Sends a request to whichever watcher is running in this desktop
/// session. Returns false when there is none.
pub fn control_watcher(request: Request) -> bool {
    match find_watcher() {
        Some(window) => post(window, request),
        None => false,
    }
}

/// The event a watcher sets while it has released the display on request.
pub struct PausedSignal {
    handle: HANDLE,
}

// Event handles may be used from any thread.
unsafe impl Send for PausedSignal {}

impl PausedSignal {
    /// Creates or opens the watcher's confirmation event, initially clear.
    pub fn create() -> Result<Self, WinError> {
        Self::named(PAUSED_EVENT_NAME)
    }

    /// Creates or opens an event by name, initially clear.
    pub fn named(name: &str) -> Result<Self, WinError> {
        let name = wide(name);
        let handle = unsafe { CreateEventW(ptr::null_mut(), 1, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(WinError::last("CreateEventW"));
        }
        let signal = Self { handle };
        signal.clear();
        Ok(signal)
    }

    /// Marks the display as released.
    pub fn set(&self) {
        unsafe {
            SetEvent(self.handle);
        }
    }

    /// Marks the display as held or about to be.
    pub fn clear(&self) {
        unsafe {
            ResetEvent(self.handle);
        }
    }
}

impl Drop for PausedSignal {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Waits up to `timeout` for a watcher to confirm it has released the
/// display. `Ok(false)` is a timeout; an error means no watcher has created
/// the confirmation event.
pub fn wait_paused(timeout: Duration) -> Result<bool, WinError> {
    wait_named(PAUSED_EVENT_NAME, timeout)
}

/// Waits up to `timeout` for the named event to be set.
pub fn wait_named(name: &str, timeout: Duration) -> Result<bool, WinError> {
    let name = wide(name);
    let handle = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(WinError::last("OpenEventW"));
    }
    let waited = unsafe { WaitForSingleObject(handle, wait_millis(timeout)) };
    unsafe {
        CloseHandle(handle);
    }
    Ok(waited == WAIT_OBJECT_0)
}

fn send(event: Event) {
    INTERRUPT.store(true, Ordering::SeqCst);
    if let Ok(sender) = SENDER.lock() {
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(event);
        }
    }
}

/// Reads the NUL-terminated interface path from a device-interface
/// broadcast.
unsafe fn broadcast_path(lparam: LPARAM) -> Option<String> {
    let header = lparam as *const DEV_BROADCAST_HDR;
    if header.is_null() || (*header).dbch_devicetype != DBT_DEVTYP_DEVICEINTERFACE {
        return None;
    }
    let broadcast = lparam as *const DEV_BROADCAST_DEVICEINTERFACE_W;
    let name = ptr::addr_of!((*broadcast).dbcc_name) as *const u16;
    let mut units = Vec::new();
    let mut index = 0;
    loop {
        let unit = *name.add(index);
        if unit == 0 {
            break;
        }
        units.push(unit);
        index += 1;
    }
    Some(String::from_utf16_lossy(&units))
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVICEARRIVAL || wparam == DBT_DEVICEREMOVECOMPLETE {
                if let Some(path) = broadcast_path(lparam) {
                    if is_panorama_path(&path) {
                        if wparam == DBT_DEVICEARRIVAL {
                            send(Event::Arrived(path));
                        } else {
                            send(Event::Removed(path));
                        }
                    }
                }
            }
            1
        }
        WM_PAUSE_WATCH => {
            send(Event::Pause);
            0
        }
        WM_RESUME_WATCH => {
            send(Event::Resume);
            0
        }
        WM_STOP_WATCH | WM_CLOSE => {
            PostQuitMessage(0);
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                PostQuitMessage(0);
            }
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// Creates the message-only window, registers for printer-interface
/// notifications, and pumps messages until asked to quit. Sends
/// [`Event::Quit`] on the channel before returning.
pub fn run_message_loop(sender: Sender<Event>) -> Result<(), WinError> {
    if let Ok(mut slot) = SENDER.lock() {
        *slot = Some(sender.clone());
    }

    let class_name = wide(WINDOW_CLASS);
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_procedure),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        let error = WinError::last("RegisterClassW");
        if error.code != 1410 {
            return Err(error);
        }
    }

    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            instance,
            ptr::null_mut(),
        )
    };
    if window.is_null() {
        return Err(WinError::last("CreateWindowExW"));
    }
    WINDOW.store(window as isize, Ordering::SeqCst);

    let mut filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as DWORD,
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE,
        dbcc_reserved: 0,
        dbcc_classguid: GUID_DEVINTERFACE_USBPRINT,
        dbcc_name: [0],
    };
    let registration = unsafe {
        RegisterDeviceNotificationW(
            window,
            &mut filter as *mut _ as *mut c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    };
    if registration.is_null() {
        let error = WinError::last("RegisterDeviceNotificationW");
        unsafe { DestroyWindow(window) };
        WINDOW.store(0, Ordering::SeqCst);
        return Err(error);
    }

    let mut message = MSG {
        hwnd: ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: POINT { x: 0, y: 0 },
    };
    loop {
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    unsafe {
        UnregisterDeviceNotification(registration);
        DestroyWindow(window);
    }
    WINDOW.store(0, Ordering::SeqCst);
    send(Event::Quit);
    if let Ok(mut slot) = SENDER.lock() {
        *slot = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    /// Serialises the tests that touch the process-wide statics.
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn broadcast_filter_layout() {
        assert_eq!(mem::size_of::<DEV_BROADCAST_HDR>(), 12);
        assert_eq!(mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>(), 32);
        assert_eq!(mem::size_of::<MSG>(), 48);
    }

    /// A broadcast structure in word-aligned storage, as the system delivers it.
    fn broadcast(device_type: u32, name: &str) -> Vec<u32> {
        let units: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = 28 + units.len() * 2;
        let mut words = vec![0u32; byte_len.div_ceil(4)];
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, words.len() * 4)
        };
        bytes[..4].copy_from_slice(&(byte_len as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&device_type.to_le_bytes());
        for (index, unit) in units.iter().enumerate() {
            bytes[28 + index * 2..30 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        words
    }

    #[test]
    fn broadcast_path_is_decoded_from_the_structure() {
        let path = r"\\?\USB#VID_391A&PID_1021#abc#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}";
        let words = broadcast(DBT_DEVTYP_DEVICEINTERFACE, path);
        let decoded = unsafe { broadcast_path(words.as_ptr() as LPARAM) };
        assert_eq!(decoded.as_deref(), Some(path));
        assert!(is_panorama_path(decoded.as_deref().unwrap()));
    }

    #[test]
    fn non_interface_broadcasts_are_ignored() {
        let words = broadcast(2, "ignored");
        assert_eq!(unsafe { broadcast_path(words.as_ptr() as LPARAM) }, None);
        assert_eq!(unsafe { broadcast_path(0) }, None);
    }

    #[test]
    fn interrupt_flag_round_trips() {
        let _guard = LOCK.lock().unwrap();
        clear_interrupt();
        assert!(!interrupted());
        interrupt();
        assert!(interrupted());
        clear_interrupt();
        assert!(!interrupted());
    }

    #[test]
    fn message_loop_delivers_control_requests_in_order() {
        let _guard = LOCK.lock().unwrap();
        assert!(!request_stop());

        let (sender, events) = mpsc::channel();
        let pump = thread::spawn(move || run_message_loop(sender));
        let deadline = Instant::now() + Duration::from_secs(5);
        while local_window().is_null() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!local_window().is_null(), "the message loop did not create its window");

        clear_interrupt();
        assert!(request(Request::Pause));
        assert!(request(Request::Resume));
        assert!(request_stop());
        let wait = Duration::from_secs(5);
        assert_eq!(events.recv_timeout(wait), Ok(Event::Pause));
        assert_eq!(events.recv_timeout(wait), Ok(Event::Resume));
        assert_eq!(events.recv_timeout(wait), Ok(Event::Quit));
        assert!(interrupted());
        pump.join().unwrap().unwrap();
        assert!(local_window().is_null());
        assert!(!request_stop());
    }

    #[test]
    fn paused_signal_round_trips_through_a_named_event() {
        let name = format!("Local\\ezrama-paused-test-{}", std::process::id());
        let short = Duration::from_millis(50);
        assert!(wait_named(&name, short).is_err());
        let signal = PausedSignal::named(&name).unwrap();
        assert_eq!(wait_named(&name, short), Ok(false));
        signal.set();
        assert_eq!(wait_named(&name, short), Ok(true));
        signal.clear();
        assert_eq!(wait_named(&name, short), Ok(false));
        drop(signal);
        assert!(wait_named(&name, short).is_err());
    }
}
