//! Discovery and opening of the display's USB printer-class interface.

use std::ffi::c_void;
use std::fmt;
use std::mem;
use std::ptr;

use crate::win::*;

/// USB vendor id of the display.
pub const VENDOR_ID: u16 = 0x391a;
/// USB product id of the Panorama SE.
pub const PRODUCT_ID: u16 = 0x1021;

/// Substring that identifies the display in an interface path.
const PANORAMA_MARKER: &str = "vid_391a&pid_1021";

/// A failed Win32 call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinError {
    pub call: &'static str,
    pub code: DWORD,
}

impl WinError {
    fn last(call: &'static str) -> Self {
        let code = unsafe { GetLastError() };
        Self { call, code }
    }

    /// The system's description of the error code, or empty if it has none.
    pub fn message(&self) -> String {
        let mut buffer = [0u16; 512];
        let written = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                ptr::null(),
                self.code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as DWORD,
                ptr::null_mut(),
            )
        };
        String::from_utf16_lossy(&buffer[..written as usize])
            .trim()
            .to_string()
    }
}

impl fmt::Display for WinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self.message();
        if message.is_empty() {
            write!(f, "{} failed with Windows error {}", self.call, self.code)
        } else {
            write!(f, "{} failed: {} (Windows error {})", self.call, message, self.code)
        }
    }
}

impl std::error::Error for WinError {}

/// Why the device could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Another program holds the device.
    Busy(WinError),
    /// The interface path no longer refers to a working device.
    Gone(WinError),
    Other(WinError),
}

impl OpenError {
    pub fn classify(error: WinError) -> Self {
        match error.code {
            ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION => OpenError::Busy(error),
            ERROR_FILE_NOT_FOUND
            | ERROR_PATH_NOT_FOUND
            | ERROR_GEN_FAILURE
            | ERROR_DEVICE_NOT_CONNECTED => OpenError::Gone(error),
            _ => OpenError::Other(error),
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Busy(error) => write!(f, "another program holds the device ({error})"),
            OpenError::Gone(error) => write!(f, "the device is not available ({error})"),
            OpenError::Other(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// An open handle on the display's printer interface.
///
/// The handle is opened for exclusive use with overlapped I/O and is closed
/// when the value is dropped.
#[derive(Debug)]
pub struct Device {
    handle: HANDLE,
    path: String,
}

// A file handle may be used from any thread.
unsafe impl Send for Device {}

impl Device {
    pub fn open(path: &str) -> Result<Self, OpenError> {
        let wide_path = wide(path);
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(OpenError::classify(WinError::last("CreateFileW")));
        }
        Ok(Device {
            handle,
            path: path.to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// NUL-terminated UTF-16 for Win32 wide-string parameters.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Outcome of looking for the display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Absent,
    One(String),
    Several(Vec<String>),
}

/// Paths of every present printer-class interface.
pub fn printer_interfaces() -> Result<Vec<String>, WinError> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_USBPRINT,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE {
        return Err(WinError::last("SetupDiGetClassDevsW"));
    }
    let result = enumerate(set);
    unsafe {
        SetupDiDestroyDeviceInfoList(set);
    }
    result
}

fn enumerate(set: HDEVINFO) -> Result<Vec<String>, WinError> {
    let mut paths = Vec::new();
    for index in 0.. {
        let mut data = SP_DEVICE_INTERFACE_DATA {
            cbSize: mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as DWORD,
            InterfaceClassGuid: GUID_DEVINTERFACE_USBPRINT,
            Flags: 0,
            Reserved: 0,
        };
        let found = unsafe {
            SetupDiEnumDeviceInterfaces(
                set,
                ptr::null(),
                &GUID_DEVINTERFACE_USBPRINT,
                index,
                &mut data,
            )
        };
        if found == 0 {
            let error = WinError::last("SetupDiEnumDeviceInterfaces");
            if error.code == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(error);
        }

        let mut required: DWORD = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut data,
                ptr::null_mut(),
                0,
                &mut required,
                ptr::null_mut(),
            );
        }
        let error = WinError::last("SetupDiGetDeviceInterfaceDetailW");
        if error.code != ERROR_INSUFFICIENT_BUFFER {
            return Err(error);
        }

        let words = (required as usize).div_ceil(mem::size_of::<u32>()).max(2);
        let mut buffer = vec![0u32; words];
        buffer[0] = DEVICE_INTERFACE_DETAIL_CB_SIZE;
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut data,
                buffer.as_mut_ptr() as *mut c_void,
                required,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(WinError::last("SetupDiGetDeviceInterfaceDetailW"));
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr() as *const u8, required as usize)
        };
        paths.push(decode_detail_path(bytes));
    }
    Ok(paths)
}

/// Extracts the NUL-terminated UTF-16 path from a filled
/// `SP_DEVICE_INTERFACE_DETAIL_DATA_W` buffer.
pub fn decode_detail_path(detail: &[u8]) -> String {
    let path_bytes = detail.get(DEVICE_INTERFACE_DETAIL_PATH_OFFSET..).unwrap_or(&[]);
    let units: Vec<u16> = path_bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Whether an interface path belongs to a Panorama SE.
pub fn is_panorama_path(path: &str) -> bool {
    path.to_ascii_lowercase().contains(PANORAMA_MARKER)
}

/// Narrows a list of printer-class interface paths to the display.
pub fn select_panorama(paths: Vec<String>) -> Discovery {
    let mut matches: Vec<String> = paths.into_iter().filter(|p| is_panorama_path(p)).collect();
    match matches.len() {
        0 => Discovery::Absent,
        1 => Discovery::One(matches.remove(0)),
        _ => Discovery::Several(matches),
    }
}

/// Looks for the display among the present printer-class interfaces.
pub fn find_panorama() -> Result<Discovery, WinError> {
    printer_interfaces().map(select_panorama)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_buffer(path: &str, trailing: &[u8]) -> Vec<u8> {
        let mut bytes = DEVICE_INTERFACE_DETAIL_CB_SIZE.to_le_bytes().to_vec();
        for unit in path.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(trailing);
        bytes
    }

    #[test]
    fn decodes_path_after_the_size_prefix() {
        let path = r"\\?\usb#vid_391a&pid_1021#9358baa5e4ed588d#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}";
        let buffer = detail_buffer(path, b"junk after the terminator");
        assert_eq!(decode_detail_path(&buffer), path);
    }

    #[test]
    fn decodes_empty_and_short_buffers() {
        assert_eq!(decode_detail_path(&[]), "");
        assert_eq!(decode_detail_path(&[8, 0, 0, 0]), "");
        assert_eq!(decode_detail_path(&[8, 0, 0, 0, b'a']), "");
    }

    #[test]
    fn marker_match_is_case_insensitive() {
        assert!(is_panorama_path(r"\\?\USB#VID_391A&PID_1021#abc#{guid}"));
        assert!(is_panorama_path(r"\\?\usb#vid_391a&pid_1021#abc#{guid}"));
        assert!(!is_panorama_path(r"\\?\usb#vid_391a&pid_1011#abc#{guid}"));
        assert!(!is_panorama_path(r"\\?\usb#vid_04b8&pid_0005#printer#{guid}"));
    }

    #[test]
    fn selection_distinguishes_none_one_and_several() {
        let other = r"\\?\usb#vid_04b8&pid_0005#printer#{guid}".to_string();
        let panel = r"\\?\usb#vid_391a&pid_1021#one#{guid}".to_string();
        let second = r"\\?\usb#vid_391a&pid_1021#two#{guid}".to_string();

        assert_eq!(select_panorama(vec![]), Discovery::Absent);
        assert_eq!(select_panorama(vec![other.clone()]), Discovery::Absent);
        assert_eq!(
            select_panorama(vec![other.clone(), panel.clone()]),
            Discovery::One(panel.clone())
        );
        assert_eq!(
            select_panorama(vec![panel.clone(), other, second.clone()]),
            Discovery::Several(vec![panel, second])
        );
    }

    #[test]
    fn interface_data_layout_matches_the_platform() {
        assert_eq!(mem::size_of::<SP_DEVICE_INTERFACE_DATA>(), 32);
        assert_eq!(mem::size_of::<GUID>(), 16);
    }

    fn error(code: DWORD) -> WinError {
        WinError {
            call: "CreateFileW",
            code,
        }
    }

    #[test]
    fn open_errors_are_classified() {
        assert_eq!(OpenError::classify(error(5)), OpenError::Busy(error(5)));
        assert_eq!(OpenError::classify(error(32)), OpenError::Busy(error(32)));
        for code in [2, 3, 31, 1167] {
            assert_eq!(OpenError::classify(error(code)), OpenError::Gone(error(code)));
        }
        assert_eq!(OpenError::classify(error(87)), OpenError::Other(error(87)));
    }

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide(""), [0]);
        assert_eq!(wide("ab"), [b'a' as u16, b'b' as u16, 0]);
        assert_eq!(wide("\u{1F600}").len(), 3);
    }

    #[test]
    fn system_messages_are_available_for_known_codes() {
        assert!(!error(ERROR_ACCESS_DENIED).message().is_empty());
        assert!(error(0xffff_ffff).message().is_empty());
    }

    #[test]
    fn opening_a_missing_path_is_gone() {
        let result = Device::open(r"\\?\usb#vid_0000&pid_0000#none#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}");
        assert!(matches!(result, Err(OpenError::Gone(_))), "{result:?}");
    }
}
