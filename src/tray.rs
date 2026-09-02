//! The pieces of the notification-area icon that need Windows: an icon
//! handle built from pixels, another program's icon read back as pixels,
//! the pop-up menu, where KANALI is installed, and waiting for its
//! processes to exit.

use std::ffi::c_void;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::thread;
use std::time::Duration;

use crate::icon::Image;
use crate::install::read_string;
use crate::usbprint::{wide, WinError};
use crate::win::*;

/// File name of KANALI's executable.
pub const KANALI_EXE: &str = "KANALI.exe";
/// Uninstall key written by KANALI's installer; its `DisplayIcon` value
/// names the executable.
pub const KANALI_UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\KANALI";
/// After a started KANALI hands over to another process of its own, how
/// long to look for that process before concluding KANALI has closed.
pub const HANDOVER_GRACE: Duration = Duration::from_secs(1);
/// How many times to look.
pub const HANDOVER_CHECKS: u32 = 3;

/// An icon handle that is destroyed when dropped.
pub struct Icon {
    handle: HICON,
}

// Icon handles may be used from any thread.
unsafe impl Send for Icon {}

impl Icon {
    pub fn handle(&self) -> HICON {
        self.handle
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            DestroyIcon(self.handle);
        }
    }
}

/// Asks for per-monitor DPI scaling so the icon size reported by the system
/// is the size the notification area draws.
pub fn enable_dpi_awareness() -> bool {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}

/// The side of a small icon, as the notification area draws it.
pub fn small_icon_size() -> usize {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if size <= 0 {
        16
    } else {
        size as usize
    }
}

/// A top-down 32-bit header for a square bitmap.
fn bitmap_info(size: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as DWORD,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [0],
    }
}

/// Builds an icon from premultiplied pixels.
pub fn create_icon(image: &Image) -> Result<Icon, WinError> {
    let size = image.size as i32;
    let info = bitmap_info(size);
    let mut bits: *mut c_void = ptr::null_mut();
    let color = unsafe {
        CreateDIBSection(ptr::null_mut(), &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0)
    };
    if color.is_null() || bits.is_null() {
        return Err(WinError::last("CreateDIBSection"));
    }
    unsafe {
        ptr::copy_nonoverlapping(image.pixels.as_ptr(), bits as *mut u32, image.pixels.len());
    }
    let mask = unsafe { CreateBitmap(size, size, 1, 1, ptr::null()) };
    if mask.is_null() {
        unsafe { DeleteObject(color) };
        return Err(WinError::last("CreateBitmap"));
    }
    let info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let handle = unsafe { CreateIconIndirect(&info) };
    let error = WinError::last("CreateIconIndirect");
    unsafe {
        DeleteObject(mask);
        DeleteObject(color);
    }
    if handle.is_null() {
        return Err(error);
    }
    Ok(Icon { handle })
}

/// Reads a square bitmap back as pixels.
fn bitmap_pixels(dc: HDC, bitmap: HBITMAP) -> Option<Image> {
    let mut info = bitmap_info(0);
    info.bmiHeader.biBitCount = 0;
    let probed = unsafe { GetDIBits(dc, bitmap, 0, 0, ptr::null_mut(), &mut info, DIB_RGB_COLORS) };
    let (width, height) = (info.bmiHeader.biWidth, info.bmiHeader.biHeight.abs());
    if probed == 0 || width <= 0 || width != height {
        return None;
    }
    let mut info = bitmap_info(width);
    let mut pixels = vec![0u32; (width * height) as usize];
    let read = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if read == 0 {
        return None;
    }
    Image::from_pixels(width as usize, pixels)
}

/// The pixels of an icon handle. An icon without an alpha channel gets one
/// from its mask.
fn icon_pixels(icon: HICON) -> Option<Image> {
    let mut info: ICONINFO = unsafe { mem::zeroed() };
    if unsafe { GetIconInfo(icon, &mut info) } == 0 {
        return None;
    }
    let dc = unsafe { GetDC(ptr::null_mut()) };
    let color = if info.hbmColor.is_null() {
        None
    } else {
        bitmap_pixels(dc, info.hbmColor)
    };
    let image = color.map(|color| {
        if color.has_alpha() {
            return color;
        }
        match bitmap_pixels(dc, info.hbmMask) {
            Some(mask) if mask.size == color.size => color.with_mask(&mask),
            _ => color.with_mask(&Image::blank(color.size)),
        }
    });
    unsafe {
        ReleaseDC(ptr::null_mut(), dc);
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
    }
    image
}

/// The first icon of the program at `path`: whichever of the program's
/// small and large renderings is the smaller one still at least `size`
/// pixels, or the largest available.
pub fn program_icon(path: &Path, size: usize) -> Option<Image> {
    let file = wide(&path.to_string_lossy());
    let mut large: HICON = ptr::null_mut();
    let mut small: HICON = ptr::null_mut();
    let count = unsafe { ExtractIconExW(file.as_ptr(), 0, &mut large, &mut small, 1) };
    if count == 0 {
        return None;
    }
    let mut candidates: Vec<Image> = [small, large]
        .into_iter()
        .filter(|icon| !icon.is_null())
        .filter_map(|icon| {
            let image = icon_pixels(icon);
            unsafe {
                DestroyIcon(icon);
            }
            image
        })
        .collect();
    candidates.sort_by_key(|image| image.size);
    let fitting = candidates.iter().position(|image| image.size >= size);
    let chosen = fitting.unwrap_or(candidates.len().checked_sub(1)?);
    Some(candidates.swap_remove(chosen))
}

/// The notification-area entry for a window.
pub struct TrayIcon {
    data: Box<NOTIFYICONDATAW>,
    shown: bool,
}

// The structure only carries handles, which may be used from any thread.
unsafe impl Send for TrayIcon {}

impl TrayIcon {
    /// Describes an entry without showing it.
    pub fn new(window: HWND, callback: u32, icon: HICON, tip: &str) -> Self {
        let mut data: Box<NOTIFYICONDATAW> = Box::new(unsafe { mem::zeroed() });
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as DWORD;
        data.hWnd = window;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = callback;
        data.hIcon = icon;
        data.uVersion = NOTIFYICON_VERSION_4;
        let mut tray = Self { data, shown: false };
        tray.write_tip(tip);
        tray
    }

    fn write_tip(&mut self, tip: &str) {
        self.data.szTip = [0; 128];
        for (slot, unit) in self.data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *slot = unit;
        }
    }

    fn notify(&mut self, message: DWORD) -> bool {
        unsafe { Shell_NotifyIconW(message, &mut *self.data) != 0 }
    }

    /// Shows the entry, or shows it again after the taskbar was recreated.
    /// An add the shell reports as failed may still have taken effect, so
    /// a failed add is followed by a modify that claims the entry if so.
    pub fn show(&mut self) -> bool {
        if self.shown {
            self.notify(NIM_DELETE);
            self.shown = false;
        }
        if !self.notify(NIM_ADD) && !self.notify(NIM_MODIFY) {
            return false;
        }
        self.shown = true;
        self.notify(NIM_SETVERSION)
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Changes the tooltip of a shown entry.
    pub fn set_tip(&mut self, tip: &str) -> bool {
        self.write_tip(tip);
        self.shown && self.notify(NIM_MODIFY)
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        if self.shown {
            self.notify(NIM_DELETE);
        }
    }
}

/// A pop-up menu shown at the cursor.
pub struct Menu {
    handle: HMENU,
}

impl Menu {
    pub fn new() -> Option<Self> {
        let handle = unsafe { CreatePopupMenu() };
        (!handle.is_null()).then_some(Self { handle })
    }

    /// Appends an item that reports `id` when chosen.
    pub fn item(&self, id: usize, text: &str, enabled: bool) {
        let text = wide(text);
        let flags = MF_STRING | if enabled { 0 } else { MF_GRAYED };
        unsafe {
            AppendMenuW(self.handle, flags, id, text.as_ptr());
        }
    }

    pub fn separator(&self) {
        unsafe {
            AppendMenuW(self.handle, MF_SEPARATOR, 0, ptr::null());
        }
    }

    /// Shows the menu at the cursor for `window` and waits for a choice.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn show(&self, window: HWND) -> Option<usize> {
        let mut point = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut point);
            SetForegroundWindow(window);
        }
        let chosen = unsafe {
            TrackPopupMenu(
                self.handle,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                window,
                ptr::null(),
            )
        };
        unsafe {
            PostMessageW(window, WM_NULL, 0, 0);
        }
        (chosen > 0).then_some(chosen as usize)
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            DestroyMenu(self.handle);
        }
    }
}

/// A `DisplayIcon` value is a path, possibly quoted, possibly followed by
/// a comma and an icon index.
pub fn icon_source_path(value: &str) -> &str {
    let value = value.trim();
    let value = match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => match value.rsplit_once(',') {
            Some((path, index)) if index.trim().parse::<i32>().is_ok() => path,
            _ => value,
        },
    };
    value.trim()
}

/// Where KANALI's executable is, if it is installed.
pub fn kanali_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Ok(Some(value)) = read_string(root, KANALI_UNINSTALL_KEY, "DisplayIcon") {
            candidates.push(PathBuf::from(icon_source_path(&value)));
        }
    }
    if let Some(programs) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(programs).join("KANALI").join(KANALI_EXE));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Starts the program at `path` in its own directory with no console.
pub fn launch(path: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(path);
    if let Some(directory) = path.parent() {
        command.current_dir(directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Process ids whose executable is named `name`, other than this process.
pub fn processes_named(name: &str) -> Vec<u32> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let own = unsafe { GetCurrentProcessId() };
    let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as DWORD;
    let mut found = Vec::new();
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let len = entry.szExeFile.iter().position(|&u| u == 0).unwrap_or(entry.szExeFile.len());
        let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
        if entry.th32ProcessID != own && exe.eq_ignore_ascii_case(name) {
            found.push(entry.th32ProcessID);
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    found
}

/// Waits for every listed process to exit. Processes that cannot be opened
/// are ignored.
fn wait_all(pids: &[u32]) {
    let handles: Vec<HANDLE> = pids
        .iter()
        .take(64)
        .map(|&pid| unsafe { OpenProcess(SYNCHRONIZE, 0, pid) })
        .filter(|handle| !handle.is_null())
        .collect();
    if !handles.is_empty() {
        unsafe {
            WaitForMultipleObjects(handles.len() as DWORD, handles.as_ptr(), 1, INFINITE);
        }
    }
    for handle in handles {
        unsafe {
            CloseHandle(handle);
        }
    }
}

/// Blocks until no process named `name` is left. A program that starts and
/// hands over to a fresh process of its own may leave a gap; the first
/// look is repeated [`HANDOVER_CHECKS`] times, [`HANDOVER_GRACE`] apart,
/// before an empty result counts.
pub fn wait_for_processes_named(name: &str) {
    let mut checks = HANDOVER_CHECKS;
    loop {
        let pids = processes_named(name);
        if pids.is_empty() {
            if checks == 0 {
                return;
            }
            checks -= 1;
            thread::sleep(HANDOVER_GRACE);
            continue;
        }
        checks = 0;
        wait_all(&pids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icon;

    #[test]
    fn structure_layouts() {
        assert_eq!(mem::size_of::<NOTIFYICONDATAW>(), 976);
        assert_eq!(mem::size_of::<ICONINFO>(), 32);
        assert_eq!(mem::size_of::<BITMAPINFOHEADER>(), 40);
        assert_eq!(mem::size_of::<PROCESSENTRY32W>(), 568);
    }

    #[test]
    fn icon_source_paths_lose_quotes_and_indices() {
        assert_eq!(icon_source_path(r"C:\P\KANALI.exe"), r"C:\P\KANALI.exe");
        assert_eq!(icon_source_path(r"C:\P\KANALI.exe,0"), r"C:\P\KANALI.exe");
        assert_eq!(icon_source_path(r"C:\P\KANALI.exe, -1"), r"C:\P\KANALI.exe");
        assert_eq!(icon_source_path(r#""C:\P q\KANALI.exe",0"#), r"C:\P q\KANALI.exe");
        assert_eq!(icon_source_path(r"C:\a,b\KANALI.exe"), r"C:\a,b\KANALI.exe");
        assert_eq!(icon_source_path("  "), "");
    }

    #[test]
    fn an_icon_round_trips_through_a_handle() {
        let image = icon::boxed(16, None);
        let icon = create_icon(&image).unwrap();
        let back = icon_pixels(icon.handle()).unwrap();
        assert_eq!(back.size, 16);
        assert_eq!(back.pixels, image.pixels);
    }

    #[test]
    fn a_label_survives_the_round_trip_with_its_alpha() {
        let mut label = Image::blank(32);
        label.pixels[0] = icon::opaque(10, 20, 30);
        label.pixels[33] = 0x8040_2010;
        let image = icon::boxed(32, Some(&label));
        let icon = create_icon(&image).unwrap();
        let back = icon_pixels(icon.handle()).unwrap();
        assert_eq!(back.pixels, image.pixels);
    }

    #[test]
    fn the_own_process_is_not_listed_but_a_shell_is_findable() {
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_string();
        let own = std::process::id();
        assert!(!processes_named(&name).contains(&own));
        assert!(processes_named("no-such-program-ezrama.exe").is_empty());
    }

    #[test]
    fn kanali_path_when_present_is_a_file_named_kanali() {
        if let Some(path) = kanali_path() {
            assert!(path.is_file());
            assert!(path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .eq_ignore_ascii_case(KANALI_EXE));
            let small = program_icon(&path, 10).expect("the installed program has an icon");
            assert!(small.size >= 10);
            assert!(small.has_alpha());
            let large = program_icon(&path, small.size + 1).unwrap();
            assert!(large.size > small.size);
            let largest = program_icon(&path, 4096).unwrap();
            assert_eq!(largest.size, large.size);
        }
    }
}
