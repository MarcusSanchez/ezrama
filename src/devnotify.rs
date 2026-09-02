//! Device arrival and removal notifications through a message-only window.
//!
//! The message loop runs on whichever thread calls [`run_message_loop`] and
//! forwards events for the display's printer interface to a channel. It also
//! raises an interrupt flag so a blocking holding loop can notice promptly.

use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::usbprint::{is_panorama_path, wide, WinError};
use crate::watch::Event;
use crate::win::*;

/// Private message that asks the loop to quit.
const WM_STOP_WATCH: u32 = WM_APP + 1;

static SENDER: Mutex<Option<Sender<Event>>> = Mutex::new(None);
static INTERRUPT: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

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

/// Asks a running message loop to quit. Safe to call from any thread.
pub fn request_stop() -> bool {
    let window = WINDOW.load(Ordering::SeqCst) as HWND;
    if window.is_null() {
        return false;
    }
    unsafe { PostMessageW(window, WM_STOP_WATCH, 0, 0) != 0 }
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

    let class_name = wide("ezrama-watch");
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
        // A second loop in the same process finds the class already registered.
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
        clear_interrupt();
        assert!(!interrupted());
        interrupt();
        assert!(interrupted());
        clear_interrupt();
        assert!(!interrupted());
    }

    #[test]
    fn stop_without_a_loop_is_refused() {
        assert!(!request_stop());
    }
}
