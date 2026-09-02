//! Hand-declared Win32 bindings for the handful of calls the transport
//! needs.

#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type HDEVINFO = *mut c_void;
pub type BOOL = i32;
pub type DWORD = u32;

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

pub const ERROR_INSUFFICIENT_BUFFER: DWORD = 122;
pub const ERROR_NO_MORE_ITEMS: DWORD = 259;

pub const DIGCF_PRESENT: DWORD = 0x0000_0002;
pub const DIGCF_DEVICEINTERFACE: DWORD = 0x0000_0010;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

/// Interface class exposed by `usbprint.sys` for USB printer-class devices.
pub const GUID_DEVINTERFACE_USBPRINT: GUID = GUID {
    Data1: 0x28d7_8fad,
    Data2: 0x5a12,
    Data3: 0x11d1,
    Data4: [0xae, 0x5b, 0x00, 0x00, 0xf8, 0x03, 0xa8, 0xc2],
};

#[repr(C)]
pub struct SP_DEVICE_INTERFACE_DATA {
    pub cbSize: DWORD,
    pub InterfaceClassGuid: GUID,
    pub Flags: DWORD,
    pub Reserved: usize,
}

/// `cbSize` of `SP_DEVICE_INTERFACE_DETAIL_DATA_W` on 64-bit Windows: the
/// `DWORD` plus one `WCHAR`, padded to the structure's alignment.
pub const DEVICE_INTERFACE_DETAIL_CB_SIZE: DWORD = 8;
/// Byte offset of `DevicePath` inside `SP_DEVICE_INTERFACE_DETAIL_DATA_W`.
pub const DEVICE_INTERFACE_DETAIL_PATH_OFFSET: usize = 4;

#[link(name = "setupapi")]
extern "system" {
    pub fn SetupDiGetClassDevsW(
        ClassGuid: *const GUID,
        Enumerator: *const u16,
        hwndParent: *mut c_void,
        Flags: DWORD,
    ) -> HDEVINFO;
    pub fn SetupDiEnumDeviceInterfaces(
        DeviceInfoSet: HDEVINFO,
        DeviceInfoData: *const c_void,
        InterfaceClassGuid: *const GUID,
        MemberIndex: DWORD,
        DeviceInterfaceData: *mut SP_DEVICE_INTERFACE_DATA,
    ) -> BOOL;
    pub fn SetupDiGetDeviceInterfaceDetailW(
        DeviceInfoSet: HDEVINFO,
        DeviceInterfaceData: *mut SP_DEVICE_INTERFACE_DATA,
        DeviceInterfaceDetailData: *mut c_void,
        DeviceInterfaceDetailDataSize: DWORD,
        RequiredSize: *mut DWORD,
        DeviceInfoData: *mut c_void,
    ) -> BOOL;
    pub fn SetupDiDestroyDeviceInfoList(DeviceInfoSet: HDEVINFO) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetLastError() -> DWORD;
}
