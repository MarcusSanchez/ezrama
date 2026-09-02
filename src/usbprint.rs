//! Discovery of the display's USB printer-class interface through SetupAPI.

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
}

impl fmt::Display for WinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with Windows error {}", self.call, self.code)
    }
}

impl std::error::Error for WinError {}

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
}
