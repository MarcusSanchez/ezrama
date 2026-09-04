//! The notification-area icon: an icon handle built from pixels, the entry
//! itself, and its pop-up menu.

use std::ffi::c_void;
use std::mem;
use std::ptr;

use crate::icon::Image;
use crate::usbprint::{wide, WinError};
use crate::watch::State;
use crate::win::*;

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
    let handle = unsafe {
        GdiFlush();
        CreateIconIndirect(&info)
    };
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

/// The program's icon at the size the notification area draws.
pub fn app_icon() -> Result<Icon, WinError> {
    create_icon(&Image::embedded().resample(small_icon_size()))
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
    pub fn new() -> Result<Self, WinError> {
        let handle = unsafe { CreatePopupMenu() };
        if handle.is_null() {
            return Err(WinError::last("CreatePopupMenu"));
        }
        Ok(Self { handle })
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

/// Something the menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Pause,
    Resume,
    OpenKanali,
    Quit,
}

/// One line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A disabled line that only shows text.
    Label(String),
    Separator,
    Item {
        choice: MenuChoice,
        text: &'static str,
        enabled: bool,
    },
}

/// The menu for a watcher in `state`; `kanali` says whether KANALI is
/// installed and can be started.
pub fn menu_entries(state: State, kanali: bool) -> Vec<MenuEntry> {
    let toggle = if state.released() {
        MenuEntry::Item {
            choice: MenuChoice::Resume,
            text: "Resume",
            enabled: true,
        }
    } else {
        MenuEntry::Item {
            choice: MenuChoice::Pause,
            text: "Pause",
            enabled: true,
        }
    };
    vec![
        MenuEntry::Label(state.label().to_string()),
        MenuEntry::Separator,
        toggle,
        MenuEntry::Item {
            choice: MenuChoice::OpenKanali,
            text: if kanali { "Open KANALI" } else { "KANALI is not installed" },
            enabled: kanali && state != State::WaitingForKanali,
        },
        MenuEntry::Separator,
        MenuEntry::Item {
            choice: MenuChoice::Quit,
            text: "Quit",
            enabled: true,
        },
    ]
}

/// Shows the menu for `state` at the cursor and returns what was chosen.
pub fn show_menu(window: HWND, state: State, kanali: bool) -> Option<MenuChoice> {
    let menu = Menu::new().ok()?;
    let entries = menu_entries(state, kanali);
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            MenuEntry::Label(text) => menu.item(0, text, false),
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Item { text, enabled, .. } => menu.item(index + 1, text, *enabled),
        }
    }
    let chosen = menu.show(window)?;
    match entries.get(chosen - 1) {
        Some(MenuEntry::Item { choice, .. }) => Some(*choice),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icon;

    fn items(entries: &[MenuEntry]) -> Vec<(MenuChoice, &'static str, bool)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item { choice, text, enabled } => Some((*choice, *text, *enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_follows_the_state() {
        let active = menu_entries(State::Active, true);
        assert_eq!(active[0], MenuEntry::Label("Active".to_string()));
        assert_eq!(
            items(&active),
            [
                (MenuChoice::Pause, "Pause", true),
                (MenuChoice::OpenKanali, "Open KANALI", true),
                (MenuChoice::Quit, "Quit", true)
            ]
        );
        assert_eq!(items(&menu_entries(State::Paused, true))[0], (MenuChoice::Resume, "Resume", true));
        let waiting = items(&menu_entries(State::WaitingForKanali, true));
        assert_eq!(waiting[0].0, MenuChoice::Resume);
        assert_eq!(waiting[1], (MenuChoice::OpenKanali, "Open KANALI", false));
        let missing = items(&menu_entries(State::Connecting, false));
        assert_eq!(missing[1], (MenuChoice::OpenKanali, "KANALI is not installed", false));
        assert_eq!(menu_entries(State::NoDisplay, true).len(), 6);
    }

    /// Reads a square bitmap back as pixels.
    fn bitmap_pixels(dc: HDC, bitmap: HBITMAP) -> Option<Image> {
        let mut info = bitmap_info(0);
        info.bmiHeader.biBitCount = 0;
        let probed =
            unsafe { GetDIBits(dc, bitmap, 0, 0, ptr::null_mut(), &mut info, DIB_RGB_COLORS) };
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

    /// The colour pixels of an icon handle.
    fn icon_pixels(icon: HICON) -> Option<Image> {
        let mut info: ICONINFO = unsafe { mem::zeroed() };
        if unsafe { GetIconInfo(icon, &mut info) } == 0 {
            return None;
        }
        let dc = unsafe { GetDC(ptr::null_mut()) };
        let image = bitmap_pixels(dc, info.hbmColor);
        unsafe {
            ReleaseDC(ptr::null_mut(), dc);
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
        }
        image
    }

    #[test]
    fn structure_layouts() {
        assert_eq!(mem::size_of::<NOTIFYICONDATAW>(), 976);
        assert_eq!(mem::size_of::<ICONINFO>(), 32);
        assert_eq!(mem::size_of::<BITMAPINFOHEADER>(), 40);
    }

    #[test]
    fn an_icon_round_trips_through_a_handle_with_its_alpha() {
        let mut image = Image::blank(32);
        image.pixels[0] = icon::opaque(10, 20, 30);
        image.pixels[33] = 0x8040_2010;
        image.pixels[1023] = icon::opaque(255, 255, 255);
        let icon = create_icon(&image).unwrap();
        let back = icon_pixels(icon.handle()).unwrap();
        assert_eq!(back.size, 32);
        assert_eq!(back.pixels, image.pixels);
    }

    #[test]
    fn the_app_icon_is_the_artwork_at_the_system_size() {
        let icon = app_icon().unwrap();
        let back = icon_pixels(icon.handle()).unwrap();
        assert_eq!(back.size, small_icon_size());
        assert_eq!(back.pixels, Image::embedded().resample(small_icon_size()).pixels);
    }

    /// The icon file written for the shortcut loads back through the shell
    /// with its alpha intact; opaque pixels survive exactly.
    #[test]
    fn the_icon_file_loads_back_through_the_shell() {
        let image = Image::embedded();
        let path = std::env::temp_dir().join(format!("ezrama-icon-test-{}.ico", std::process::id()));
        std::fs::write(&path, icon::ico_bytes(&image)).unwrap();
        let file = crate::usbprint::wide_path(&path);
        let side = image.size as i32;
        let loaded = unsafe {
            LoadImageW(ptr::null_mut(), file.as_ptr(), IMAGE_ICON, side, side, LR_LOADFROMFILE)
        };
        std::fs::remove_file(&path).unwrap();
        assert!(!loaded.is_null());
        let back = icon_pixels(loaded).unwrap();
        unsafe {
            DestroyIcon(loaded);
        }
        assert_eq!(back.size, image.size);
        for (index, (&loaded, &original)) in back.pixels.iter().zip(&image.pixels).enumerate() {
            assert_eq!(icon::alpha(loaded), icon::alpha(original), "alpha at {index}");
            if icon::alpha(original) == 255 {
                assert_eq!(loaded, original, "opaque pixel at {index}");
            }
        }
    }

    #[test]
    fn menus_are_created_and_destroyed() {
        let menu = Menu::new().unwrap();
        menu.item(1, "One", true);
        menu.separator();
        menu.item(2, "Two", false);
        drop(menu);
    }
}
