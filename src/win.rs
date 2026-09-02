//! Hand-declared Win32 bindings for the calls the program needs: device
//! discovery and I/O, the watcher's window, the notification-area icon,
//! the registry, and process waits.

#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type HDEVINFO = *mut c_void;
pub type BOOL = i32;
pub type DWORD = u32;

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

pub const ERROR_FILE_NOT_FOUND: DWORD = 2;
pub const ERROR_PATH_NOT_FOUND: DWORD = 3;
pub const ERROR_ACCESS_DENIED: DWORD = 5;
pub const ERROR_INVALID_HANDLE: DWORD = 6;
pub const ERROR_BAD_UNIT: DWORD = 20;
pub const ERROR_GEN_FAILURE: DWORD = 31;
pub const ERROR_SHARING_VIOLATION: DWORD = 32;
pub const ERROR_INSUFFICIENT_BUFFER: DWORD = 122;
pub const ERROR_NO_MORE_ITEMS: DWORD = 259;
pub const ERROR_NO_SUCH_DEVICE: DWORD = 433;
pub const ERROR_OPERATION_ABORTED: DWORD = 995;
pub const ERROR_IO_INCOMPLETE: DWORD = 996;
pub const ERROR_IO_PENDING: DWORD = 997;
pub const ERROR_DEVICE_NOT_CONNECTED: DWORD = 1167;
pub const ERROR_NOT_FOUND: DWORD = 1168;
pub const ERROR_DEVICE_REMOVED: DWORD = 1617;

pub const WAIT_OBJECT_0: DWORD = 0;
pub const WAIT_TIMEOUT: DWORD = 258;
pub const WAIT_FAILED: DWORD = 0xffff_ffff;

pub const CTRL_C_EVENT: DWORD = 0;
pub const CTRL_BREAK_EVENT: DWORD = 1;
pub const CTRL_CLOSE_EVENT: DWORD = 2;

pub type PHANDLER_ROUTINE = Option<unsafe extern "system" fn(CtrlType: DWORD) -> BOOL>;

pub type HWND = *mut c_void;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type WNDPROC = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

/// Parent handle that makes a window message-only.
pub const HWND_MESSAGE: HWND = -3isize as HWND;

pub const WM_DESTROY: u32 = 0x0002;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUERYENDSESSION: u32 = 0x0011;
pub const WM_ENDSESSION: u32 = 0x0016;
pub const WM_DEVICECHANGE: u32 = 0x0219;
pub const WM_APP: u32 = 0x8000;

pub const DBT_DEVICEARRIVAL: WPARAM = 0x8000;
pub const DBT_DEVICEREMOVECOMPLETE: WPARAM = 0x8004;
pub const DBT_DEVTYP_DEVICEINTERFACE: DWORD = 5;
pub const DEVICE_NOTIFY_WINDOW_HANDLE: DWORD = 0;

pub const EVENT_MODIFY_STATE: DWORD = 0x0002;
pub const SYNCHRONIZE: DWORD = 0x0010_0000;
pub const ERROR_ALREADY_EXISTS: DWORD = 183;

pub type HKEY = *mut c_void;
pub type LSTATUS = i32;

pub const HKEY_CURRENT_USER: HKEY = 0x8000_0001usize as HKEY;
pub const HKEY_LOCAL_MACHINE: HKEY = 0x8000_0002usize as HKEY;
pub const KEY_QUERY_VALUE: DWORD = 0x0001;
pub const KEY_SET_VALUE: DWORD = 0x0002;
pub const REG_SZ: DWORD = 1;
pub const ERROR_SUCCESS: LSTATUS = 0;
pub const ERROR_MORE_DATA: LSTATUS = 234;

#[link(name = "advapi32")]
extern "system" {
    pub fn RegOpenKeyExW(
        hKey: HKEY,
        lpSubKey: *const u16,
        ulOptions: DWORD,
        samDesired: DWORD,
        phkResult: *mut HKEY,
    ) -> LSTATUS;
    pub fn RegCloseKey(hKey: HKEY) -> LSTATUS;
    pub fn RegQueryValueExW(
        hKey: HKEY,
        lpValueName: *const u16,
        lpReserved: *mut DWORD,
        lpType: *mut DWORD,
        lpData: *mut u8,
        lpcbData: *mut DWORD,
    ) -> LSTATUS;
    pub fn RegSetValueExW(
        hKey: HKEY,
        lpValueName: *const u16,
        Reserved: DWORD,
        dwType: DWORD,
        lpData: *const u8,
        cbData: DWORD,
    ) -> LSTATUS;
    pub fn RegDeleteValueW(hKey: HKEY, lpValueName: *const u16) -> LSTATUS;
    pub fn RegEnumValueW(
        hKey: HKEY,
        dwIndex: DWORD,
        lpValueName: *mut u16,
        lpcchValueName: *mut DWORD,
        lpReserved: *mut DWORD,
        lpType: *mut DWORD,
        lpData: *mut u8,
        lpcbData: *mut DWORD,
    ) -> LSTATUS;
}

#[repr(C)]
pub struct WNDCLASSW {
    pub style: u32,
    pub lpfnWndProc: WNDPROC,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HANDLE,
    pub hIcon: HANDLE,
    pub hCursor: HANDLE,
    pub hbrBackground: HANDLE,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: u32,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

#[repr(C)]
pub struct DEV_BROADCAST_HDR {
    pub dbch_size: DWORD,
    pub dbch_devicetype: DWORD,
    pub dbch_reserved: DWORD,
}

#[repr(C)]
pub struct DEV_BROADCAST_DEVICEINTERFACE_W {
    pub dbcc_size: DWORD,
    pub dbcc_devicetype: DWORD,
    pub dbcc_reserved: DWORD,
    pub dbcc_classguid: GUID,
    pub dbcc_name: [u16; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEMTIME {
    pub wYear: u16,
    pub wMonth: u16,
    pub wDayOfWeek: u16,
    pub wDay: u16,
    pub wHour: u16,
    pub wMinute: u16,
    pub wSecond: u16,
    pub wMilliseconds: u16,
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
    pub fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: DWORD,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HANDLE,
        hInstance: HANDLE,
        lpParam: *mut c_void,
    ) -> HWND;
    pub fn DestroyWindow(hWnd: HWND) -> BOOL;
    pub fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32) -> BOOL;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn RegisterDeviceNotificationW(
        hRecipient: HANDLE,
        NotificationFilter: *mut c_void,
        Flags: DWORD,
    ) -> HANDLE;
    pub fn UnregisterDeviceNotification(Handle: HANDLE) -> BOOL;
    pub fn FindWindowExW(
        hWndParent: HWND,
        hWndChildAfter: HWND,
        lpszClass: *const u16,
        lpszWindow: *const u16,
    ) -> HWND;
}

pub const DIGCF_PRESENT: DWORD = 0x0000_0002;
pub const DIGCF_DEVICEINTERFACE: DWORD = 0x0000_0010;

pub const GENERIC_READ: DWORD = 0x8000_0000;
pub const GENERIC_WRITE: DWORD = 0x4000_0000;
pub const OPEN_EXISTING: DWORD = 3;
pub const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;

pub const FORMAT_MESSAGE_IGNORE_INSERTS: DWORD = 0x0000_0200;
pub const FORMAT_MESSAGE_FROM_SYSTEM: DWORD = 0x0000_1000;

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

#[repr(C)]
pub struct OVERLAPPED {
    pub Internal: usize,
    pub InternalHigh: usize,
    pub Offset: DWORD,
    pub OffsetHigh: DWORD,
    pub hEvent: HANDLE,
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
    pub fn FormatMessageW(
        dwFlags: DWORD,
        lpSource: *const c_void,
        dwMessageId: DWORD,
        dwLanguageId: DWORD,
        lpBuffer: *mut u16,
        nSize: DWORD,
        Arguments: *mut c_void,
    ) -> DWORD;
    pub fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *mut c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn CreateEventW(
        lpEventAttributes: *mut c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
    pub fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut c_void,
        nNumberOfBytesToRead: DWORD,
        lpNumberOfBytesRead: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    pub fn WriteFile(
        hFile: HANDLE,
        lpBuffer: *const c_void,
        nNumberOfBytesToWrite: DWORD,
        lpNumberOfBytesWritten: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    pub fn CancelIoEx(hFile: HANDLE, lpOverlapped: *mut OVERLAPPED) -> BOOL;
    pub fn GetOverlappedResult(
        hFile: HANDLE,
        lpOverlapped: *mut OVERLAPPED,
        lpNumberOfBytesTransferred: *mut DWORD,
        bWait: BOOL,
    ) -> BOOL;
    pub fn SetConsoleCtrlHandler(HandlerRoutine: PHANDLER_ROUTINE, Add: BOOL) -> BOOL;
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HANDLE;
    pub fn GetLocalTime(lpSystemTime: *mut SYSTEMTIME);
    pub fn OpenEventW(dwDesiredAccess: DWORD, bInheritHandle: BOOL, lpName: *const u16) -> HANDLE;
    pub fn SetEvent(hEvent: HANDLE) -> BOOL;
    pub fn ResetEvent(hEvent: HANDLE) -> BOOL;
}

pub type HICON = HANDLE;
pub type HBITMAP = HANDLE;
pub type HDC = HANDLE;
pub type HMENU = HANDLE;

pub const SM_CXSMICON: i32 = 49;
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: HANDLE = -4isize as HANDLE;

pub const WM_NULL: u32 = 0x0000;
pub const WM_CONTEXTMENU: u32 = 0x007b;
pub const NIN_SELECT: u32 = 0x0400;
pub const NIN_KEYSELECT: u32 = 0x0401;

pub const NIM_ADD: DWORD = 0;
pub const NIM_MODIFY: DWORD = 1;
pub const NIM_DELETE: DWORD = 2;
pub const NIM_SETVERSION: DWORD = 4;
pub const NIF_MESSAGE: DWORD = 0x01;
pub const NIF_ICON: DWORD = 0x02;
pub const NIF_TIP: DWORD = 0x04;
pub const NIF_SHOWTIP: DWORD = 0x80;
pub const NOTIFYICON_VERSION_4: DWORD = 4;

pub const MF_STRING: DWORD = 0x0000;
pub const MF_GRAYED: DWORD = 0x0001;
pub const MF_SEPARATOR: DWORD = 0x0800;
pub const TPM_RIGHTBUTTON: DWORD = 0x0002;
pub const TPM_NONOTIFY: DWORD = 0x0080;
pub const TPM_RETURNCMD: DWORD = 0x0100;

pub const SMTO_ABORTIFHUNG: DWORD = 0x0002;

pub const DIB_RGB_COLORS: DWORD = 0;
pub const BI_RGB: DWORD = 0;

pub const TH32CS_SNAPPROCESS: DWORD = 0x0002;
pub const INFINITE: DWORD = 0xffff_ffff;

#[repr(C)]
pub struct NOTIFYICONDATAW {
    pub cbSize: DWORD,
    pub hWnd: HWND,
    pub uID: u32,
    pub uFlags: DWORD,
    pub uCallbackMessage: u32,
    pub hIcon: HICON,
    pub szTip: [u16; 128],
    pub dwState: DWORD,
    pub dwStateMask: DWORD,
    pub szInfo: [u16; 256],
    pub uVersion: u32,
    pub szInfoTitle: [u16; 64],
    pub dwInfoFlags: DWORD,
    pub guidItem: GUID,
    pub hBalloonIcon: HICON,
}

#[repr(C)]
pub struct ICONINFO {
    pub fIcon: BOOL,
    pub xHotspot: DWORD,
    pub yHotspot: DWORD,
    pub hbmMask: HBITMAP,
    pub hbmColor: HBITMAP,
}

#[repr(C)]
pub struct BITMAPINFOHEADER {
    pub biSize: DWORD,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: DWORD,
    pub biSizeImage: DWORD,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: DWORD,
    pub biClrImportant: DWORD,
}

#[repr(C)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [u32; 1],
}

#[repr(C)]
pub struct PROCESSENTRY32W {
    pub dwSize: DWORD,
    pub cntUsage: DWORD,
    pub th32ProcessID: DWORD,
    pub th32DefaultHeapID: usize,
    pub th32ModuleID: DWORD,
    pub cntThreads: DWORD,
    pub th32ParentProcessID: DWORD,
    pub pcPriClassBase: i32,
    pub dwFlags: DWORD,
    pub szExeFile: [u16; 260],
}

#[link(name = "shell32")]
extern "system" {
    pub fn Shell_NotifyIconW(dwMessage: DWORD, lpData: *mut NOTIFYICONDATAW) -> BOOL;
    pub fn ExtractIconExW(
        lpszFile: *const u16,
        nIconIndex: i32,
        phiconLarge: *mut HICON,
        phiconSmall: *mut HICON,
        nIcons: u32,
    ) -> u32;
}

#[link(name = "user32")]
extern "system" {
    pub fn GetSystemMetrics(nIndex: i32) -> i32;
    pub fn SetProcessDpiAwarenessContext(value: HANDLE) -> BOOL;
    pub fn RegisterWindowMessageW(lpString: *const u16) -> u32;
    pub fn SendMessageTimeoutW(
        hWnd: HWND,
        Msg: u32,
        wParam: WPARAM,
        lParam: LPARAM,
        fuFlags: DWORD,
        uTimeout: u32,
        lpdwResult: *mut usize,
    ) -> LRESULT;
    pub fn SetForegroundWindow(hWnd: HWND) -> BOOL;
    pub fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
    pub fn CreatePopupMenu() -> HMENU;
    pub fn AppendMenuW(hMenu: HMENU, uFlags: DWORD, uIDNewItem: usize, lpNewItem: *const u16) -> BOOL;
    pub fn TrackPopupMenu(
        hMenu: HMENU,
        uFlags: DWORD,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HWND,
        prcRect: *const c_void,
    ) -> BOOL;
    pub fn DestroyMenu(hMenu: HMENU) -> BOOL;
    pub fn GetIconInfo(hIcon: HICON, piconinfo: *mut ICONINFO) -> BOOL;
    pub fn CreateIconIndirect(piconinfo: *const ICONINFO) -> HICON;
    pub fn DestroyIcon(hIcon: HICON) -> BOOL;
    pub fn GetDC(hWnd: HWND) -> HDC;
    pub fn ReleaseDC(hWnd: HWND, hDC: HDC) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn GetDIBits(
        hdc: HDC,
        hbm: HBITMAP,
        start: u32,
        cLines: u32,
        lpvBits: *mut c_void,
        lpbmi: *mut BITMAPINFO,
        usage: DWORD,
    ) -> i32;
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: DWORD,
        ppvBits: *mut *mut c_void,
        hSection: HANDLE,
        offset: DWORD,
    ) -> HBITMAP;
    pub fn CreateBitmap(nWidth: i32, nHeight: i32, nPlanes: u32, nBitCount: u32, lpBits: *const c_void) -> HBITMAP;
    pub fn DeleteObject(ho: HANDLE) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateToolhelp32Snapshot(dwFlags: DWORD, th32ProcessID: DWORD) -> HANDLE;
    pub fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Process32NextW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn OpenProcess(dwDesiredAccess: DWORD, bInheritHandle: BOOL, dwProcessId: DWORD) -> HANDLE;
    pub fn WaitForMultipleObjects(nCount: DWORD, lpHandles: *const HANDLE, bWaitAll: BOOL, dwMilliseconds: DWORD) -> DWORD;
    pub fn GetCurrentProcessId() -> DWORD;
}

pub const DETACHED_PROCESS: DWORD = 0x0000_0008;

#[repr(C)]
pub struct STARTUPINFOW {
    pub cb: DWORD,
    pub lpReserved: *mut u16,
    pub lpDesktop: *mut u16,
    pub lpTitle: *mut u16,
    pub dwX: DWORD,
    pub dwY: DWORD,
    pub dwXSize: DWORD,
    pub dwYSize: DWORD,
    pub dwXCountChars: DWORD,
    pub dwYCountChars: DWORD,
    pub dwFillAttribute: DWORD,
    pub dwFlags: DWORD,
    pub wShowWindow: u16,
    pub cbReserved2: u16,
    pub lpReserved2: *mut u8,
    pub hStdInput: HANDLE,
    pub hStdOutput: HANDLE,
    pub hStdError: HANDLE,
}

#[repr(C)]
pub struct PROCESS_INFORMATION {
    pub hProcess: HANDLE,
    pub hThread: HANDLE,
    pub dwProcessId: DWORD,
    pub dwThreadId: DWORD,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateProcessW(
        lpApplicationName: *const u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *mut c_void,
        lpThreadAttributes: *mut c_void,
        bInheritHandles: BOOL,
        dwCreationFlags: DWORD,
        lpEnvironment: *mut c_void,
        lpCurrentDirectory: *const u16,
        lpStartupInfo: *const STARTUPINFOW,
        lpProcessInformation: *mut PROCESS_INFORMATION,
    ) -> BOOL;
}
