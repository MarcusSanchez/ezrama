//! Start Menu shortcuts through the shell's link object.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::usbprint::{wide, wide_path, WinError};
use crate::win::*;

/// File name of the Start Menu entry.
pub const SHORTCUT_FILE: &str = "ezrama.lnk";
/// Longest path the link object hands back.
const MAX_PATH: usize = 260;

/// What a shortcut points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub target: PathBuf,
    pub arguments: String,
}

/// Where the current user's Start Menu entry lives:
/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\ezrama.lnk`.
pub fn start_menu_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(base)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join(SHORTCUT_FILE),
    )
}

fn check(call: &'static str, result: HRESULT) -> Result<(), WinError> {
    if result < 0 {
        return Err(WinError {
            call,
            code: result as DWORD,
        });
    }
    Ok(())
}

/// COM initialised for the calling thread, and released when dropped.
struct Com {
    owned: bool,
}

impl Com {
    fn start() -> Result<Self, WinError> {
        let result = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self { owned: false });
        }
        check("CoInitializeEx", result)?;
        Ok(Self { owned: true })
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.owned {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// A shell link object with its file interface, released when dropped.
struct Link {
    link: *mut c_void,
    file: *mut c_void,
}

impl Link {
    fn create() -> Result<Self, WinError> {
        let mut link: *mut c_void = ptr::null_mut();
        let created = unsafe {
            CoCreateInstance(
                &CLSID_SHELL_LINK,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut link,
            )
        };
        check("CoCreateInstance", created)?;
        let mut file: *mut c_void = ptr::null_mut();
        let queried = unsafe { ((*Self::link_vtbl(link)).base.QueryInterface)(link, &IID_IPERSIST_FILE, &mut file) };
        if let Err(error) = check("QueryInterface", queried) {
            unsafe {
                ((*Self::link_vtbl(link)).base.Release)(link);
            }
            return Err(error);
        }
        Ok(Self { link, file })
    }

    unsafe fn link_vtbl(link: *mut c_void) -> *const IShellLinkWVtbl {
        *(link as *mut *const IShellLinkWVtbl)
    }

    fn vtbl(&self) -> &IShellLinkWVtbl {
        unsafe { &*Self::link_vtbl(self.link) }
    }

    fn file_vtbl(&self) -> &IPersistFileVtbl {
        unsafe { &**(self.file as *mut *const IPersistFileVtbl) }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe {
            (self.file_vtbl().base.Release)(self.file);
            (self.vtbl().base.Release)(self.link);
        }
    }
}

/// Writes a shortcut at `path` that runs `target` with `arguments` in the
/// target's directory, showing `icon` and `description`.
pub fn write(
    path: &Path,
    target: &Path,
    arguments: &str,
    icon: &Path,
    description: &str,
) -> Result<(), WinError> {
    let _com = Com::start()?;
    let link = Link::create()?;
    let this = link.link;
    let vtbl = link.vtbl();
    let target_units = wide_path(target);
    check("SetPath", unsafe { (vtbl.SetPath)(this, target_units.as_ptr()) })?;
    let argument_units = wide(arguments);
    check("SetArguments", unsafe { (vtbl.SetArguments)(this, argument_units.as_ptr()) })?;
    if let Some(directory) = target.parent() {
        let directory_units = wide_path(directory);
        check("SetWorkingDirectory", unsafe {
            (vtbl.SetWorkingDirectory)(this, directory_units.as_ptr())
        })?;
    }
    let icon_units = wide_path(icon);
    check("SetIconLocation", unsafe { (vtbl.SetIconLocation)(this, icon_units.as_ptr(), 0) })?;
    let description_units = wide(description);
    check("SetDescription", unsafe {
        (vtbl.SetDescription)(this, description_units.as_ptr())
    })?;
    let path_units = wide_path(path);
    check("Save", unsafe { (link.file_vtbl().Save)(link.file, path_units.as_ptr(), 1) })
}

/// Reads the target and arguments of the shortcut at `path`.
pub fn read(path: &Path) -> Result<Shortcut, WinError> {
    let _com = Com::start()?;
    let link = Link::create()?;
    let path_units = wide_path(path);
    check("Load", unsafe { (link.file_vtbl().Load)(link.file, path_units.as_ptr(), STGM_READ) })?;
    let this = link.link;
    let vtbl = link.vtbl();
    let mut buffer = vec![0u16; MAX_PATH];
    check("GetPath", unsafe {
        (vtbl.GetPath)(this, buffer.as_mut_ptr(), MAX_PATH as i32, ptr::null_mut(), 0)
    })?;
    let target = PathBuf::from(units_to_string(&buffer));
    check("GetArguments", unsafe { (vtbl.GetArguments)(this, buffer.as_mut_ptr(), MAX_PATH as i32) })?;
    Ok(Shortcut {
        target,
        arguments: units_to_string(&buffer),
    })
}

fn units_to_string(units: &[u16]) -> String {
    let end = units.iter().position(|&unit| unit == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_menu_path_ends_with_the_entry() {
        if let Some(path) = start_menu_path() {
            assert!(path.ends_with(Path::new("Programs").join(SHORTCUT_FILE)));
        }
    }

    #[test]
    fn a_shortcut_round_trips_through_the_shell() {
        let directory = std::env::temp_dir().join(format!("ezrama-shortcut-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("target.exe");
        std::fs::write(&target, b"not a program").unwrap();
        let link = directory.join("entry.lnk");
        write(&link, &target, "watch --interval 4", &directory.join("icon.ico"), "A test entry").unwrap();
        let back = read(&link).unwrap();
        assert_eq!(back.target, target);
        assert_eq!(back.arguments, "watch --interval 4");
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(read(&link).is_err());
    }
}
