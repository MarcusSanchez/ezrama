//! Installing the watcher for the current user: a copy of the binaries
//! under local app data and a Run entry that starts it at logon.

use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::usbprint::{wide, WinError};
use crate::win::*;

/// Registry key whose values start programs at logon for the current user.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Name of ezrama's value under [`RUN_KEY`].
pub const RUN_VALUE: &str = "ezrama";
/// File name of the console binary.
pub const CONSOLE_BINARY: &str = "ezrama.exe";
/// File name of the windowless binary that the Run entry starts.
pub const WATCHER_BINARY: &str = "ezramaw.exe";
/// File name of the icon written next to the binaries for the shortcut
/// and the installed-apps entry.
pub const ICON_FILE: &str = "ezrama.ico";

/// The per-user installation directory, `%LOCALAPPDATA%\ezrama`.
pub fn install_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("ezrama"))
}

/// The command line the Run entry executes for the watcher at `watcher`.
pub fn run_command(watcher: &Path) -> String {
    format!("\"{}\" watch", watcher.display())
}

/// A value under the Run key: its name and the command it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEntry {
    pub name: String,
    pub command: String,
}

fn open_key(root: HKEY, subkey: &str, access: DWORD) -> Result<HKEY, WinError> {
    let subkey = wide(subkey);
    let mut key: HKEY = ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, access, &mut key) };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegOpenKeyExW",
            code: status as DWORD,
        });
    }
    Ok(key)
}

fn open_run_key(access: DWORD) -> Result<HKEY, WinError> {
    open_key(HKEY_CURRENT_USER, RUN_KEY, access)
}

fn utf16_string(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Key where Task Manager records which Run entries it has disabled.
pub const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Whether Task Manager has disabled the Run entry `name`. An entry with
/// no record is enabled.
pub fn startup_disabled(name: &str) -> Result<bool, WinError> {
    let subkey = wide(STARTUP_APPROVED_KEY);
    let mut key: HKEY = ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_QUERY_VALUE, &mut key)
    };
    if status as DWORD == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegOpenKeyExW",
            code: status as DWORD,
        });
    }
    let value_name = wide(name);
    let mut kind: DWORD = 0;
    let mut data = [0u8; 16];
    let mut size = data.len() as DWORD;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            data.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if status as DWORD == ERROR_FILE_NOT_FOUND || status == ERROR_MORE_DATA {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegQueryValueExW",
            code: status as DWORD,
        });
    }
    // The first byte is even when enabled and odd when disabled.
    Ok(size > 0 && data[0] % 2 == 1)
}

/// Reads the string value `name` under the Run key, if it exists.
pub fn read_run_value(name: &str) -> Result<Option<String>, WinError> {
    read_string(HKEY_CURRENT_USER, RUN_KEY, name)
}

/// Reads the string value `name` under `subkey` of `root`. A missing key
/// or value is `None`.
pub fn read_string(root: HKEY, subkey: &str, name: &str) -> Result<Option<String>, WinError> {
    Ok(read_string_raw(root, subkey, name)?.map(|(kind, bytes)| registry_string(kind, &bytes)))
}

/// Reads the value `name` under `subkey` of `root` as stored: its type and
/// its bytes. A missing key or value is `None`.
fn read_string_raw(root: HKEY, subkey: &str, name: &str) -> Result<Option<(DWORD, Vec<u8>)>, WinError> {
    let key = match open_key(root, subkey, KEY_QUERY_VALUE) {
        Ok(key) => key,
        Err(error) if error.code == ERROR_FILE_NOT_FOUND => return Ok(None),
        Err(error) => return Err(error),
    };
    let value_name = wide(name);
    let mut kind: DWORD = 0;
    let mut size: DWORD = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            ptr::null_mut(),
            &mut size,
        )
    };
    if status as DWORD == ERROR_FILE_NOT_FOUND {
        unsafe { RegCloseKey(key) };
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        unsafe { RegCloseKey(key) };
        return Err(WinError {
            call: "RegQueryValueExW",
            code: status as DWORD,
        });
    }
    let mut data = vec![0u8; size as usize + 2];
    let mut size = data.len() as DWORD;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            data.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegQueryValueExW",
            code: status as DWORD,
        });
    }
    data.truncate(size as usize);
    Ok(Some((kind, data)))
}

/// A string value as stored: expandable strings have their `%NAME%`
/// references replaced from the environment.
fn registry_string(kind: DWORD, bytes: &[u8]) -> String {
    let text = utf16_string(bytes);
    if kind == REG_EXPAND_SZ {
        expand_environment(&text)
    } else {
        text
    }
}

/// Replaces `%NAME%` references with the environment's values, as the
/// shell does for expandable registry strings. Unknown names stay as they
/// are.
pub fn expand_environment(text: &str) -> String {
    let source = wide(text);
    let needed = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), ptr::null_mut(), 0) };
    if needed == 0 {
        return text.to_string();
    }
    let mut buffer = vec![0u16; needed as usize];
    let written =
        unsafe { ExpandEnvironmentStringsW(source.as_ptr(), buffer.as_mut_ptr(), needed) };
    if written == 0 || written > needed {
        return text.to_string();
    }
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Registry key holding the current user's environment variables.
pub const ENVIRONMENT_KEY: &str = "Environment";
/// The user's search path value under [`ENVIRONMENT_KEY`].
pub const PATH_VALUE: &str = "Path";

/// Whether two search path entries name the same directory: case does not
/// matter and neither does a trailing separator.
fn same_directory(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    trim(a) == trim(b)
}

/// `current` with `directory` appended, or `None` when it is already
/// there.
pub fn path_with(current: &str, directory: &str) -> Option<String> {
    if current.split(';').any(|entry| same_directory(entry, directory)) {
        return None;
    }
    let trimmed = current.trim_end_matches(';');
    if trimmed.is_empty() {
        Some(directory.to_string())
    } else {
        Some(format!("{trimmed};{directory}"))
    }
}

/// `current` without `directory`, or `None` when it was not there.
pub fn path_without(current: &str, directory: &str) -> Option<String> {
    let kept: Vec<&str> = current
        .split(';')
        .filter(|entry| !entry.trim().is_empty() && !same_directory(entry, directory))
        .collect();
    let had = current.split(';').any(|entry| same_directory(entry, directory));
    had.then(|| kept.join(";"))
}

fn set_string_of_kind(key: HKEY, name: &str, value: &str, kind: DWORD) -> Result<(), WinError> {
    let name = wide(name);
    let data = wide(value);
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            kind,
            data.as_ptr() as *const u8,
            (data.len() * 2) as DWORD,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegSetValueExW",
            code: status as DWORD,
        });
    }
    Ok(())
}

/// Tells open programs the environment changed, so a new terminal sees
/// the new search path.
fn announce_environment_change() {
    let what = wide(ENVIRONMENT_KEY);
    let mut result: usize = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            what.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            1000,
            &mut result,
        );
    }
}

/// Rewrites the user's search path value `name` through `edit`, keeping
/// the value's type and leaving `%NAME%` references unexpanded. Returns
/// whether anything changed.
pub fn edit_user_path(name: &str, edit: impl FnOnce(&str) -> Option<String>) -> Result<bool, WinError> {
    let (kind, current) = match read_string_raw(HKEY_CURRENT_USER, ENVIRONMENT_KEY, name)? {
        Some((kind, bytes)) => (kind, utf16_string(&bytes)),
        None => (REG_EXPAND_SZ, String::new()),
    };
    let Some(updated) = edit(&current) else {
        return Ok(false);
    };
    let key = open_key(HKEY_CURRENT_USER, ENVIRONMENT_KEY, KEY_SET_VALUE)?;
    let written = if updated.is_empty() {
        let value_name = wide(name);
        let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        if status != ERROR_SUCCESS && status as DWORD != ERROR_FILE_NOT_FOUND {
            Err(WinError {
                call: "RegDeleteValueW",
                code: status as DWORD,
            })
        } else {
            Ok(())
        }
    } else {
        set_string_of_kind(key, name, &updated, kind)
    };
    unsafe { RegCloseKey(key) };
    written?;
    announce_environment_change();
    Ok(true)
}

/// Adds `directory` to the user's search path. Returns whether it was
/// added rather than already there.
pub fn add_to_user_path(directory: &Path) -> Result<bool, WinError> {
    let directory = directory.to_string_lossy();
    edit_user_path(PATH_VALUE, |current| path_with(current, &directory))
}

/// Removes `directory` from the user's search path. Returns whether it
/// was there.
pub fn remove_from_user_path(directory: &Path) -> Result<bool, WinError> {
    let directory = directory.to_string_lossy();
    edit_user_path(PATH_VALUE, |current| path_without(current, &directory))
}

/// Whether the user's search path lists `directory`.
pub fn user_path_has(directory: &Path) -> Result<bool, WinError> {
    let current = read_string_raw(HKEY_CURRENT_USER, ENVIRONMENT_KEY, PATH_VALUE)?
        .map(|(_, bytes)| utf16_string(&bytes))
        .unwrap_or_default();
    let directory = directory.to_string_lossy();
    Ok(path_with(&current, &directory).is_none())
}

/// Writes the string value `name` under the Run key.
pub fn set_run_value(name: &str, command: &str) -> Result<(), WinError> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let value_name = wide(name);
    let data = wide(command);
    let bytes = data.len() * 2;
    let status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            bytes as DWORD,
        )
    };
    unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegSetValueExW",
            code: status as DWORD,
        });
    }
    Ok(())
}

/// Deletes the value `name` under the Run key. Returns whether it existed.
pub fn delete_run_value(name: &str) -> Result<bool, WinError> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let value_name = wide(name);
    let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    unsafe { RegCloseKey(key) };
    if status as DWORD == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegDeleteValueW",
            code: status as DWORD,
        });
    }
    Ok(true)
}

/// Whether ezrama's logon entry exists.
pub fn startup_enabled() -> bool {
    matches!(read_run_value(RUN_VALUE), Ok(Some(_)))
}

/// Adds or removes the logon entry `name` for the watcher next to this
/// executable. Returns whether anything changed.
pub fn set_startup_value(name: &str, enabled: bool) -> Result<bool, WinError> {
    if !enabled {
        return delete_run_value(name);
    }
    let watcher = std::env::current_exe()
        .ok()
        .and_then(|own| own.parent().map(|directory| directory.join(WATCHER_BINARY)))
        .ok_or(WinError {
            call: "GetModuleFileNameW",
            code: ERROR_FILE_NOT_FOUND,
        })?;
    let command = run_command(&watcher);
    if read_string(HKEY_CURRENT_USER, RUN_KEY, name)?.as_deref() == Some(command.as_str()) {
        return Ok(false);
    }
    set_run_value(name, &command)?;
    Ok(true)
}

/// Adds or removes ezrama's logon entry. Returns whether anything changed.
pub fn set_startup(enabled: bool) -> Result<bool, WinError> {
    set_startup_value(RUN_VALUE, enabled)
}

/// Every value under the Run key.
pub fn run_entries() -> Result<Vec<RunEntry>, WinError> {
    let key = open_run_key(KEY_QUERY_VALUE)?;
    let mut entries = Vec::new();
    let mut name = vec![0u16; 16_384];
    let mut data = vec![0u8; 65_536];
    let mut index: DWORD = 0;
    loop {
        let mut name_len = name.len() as DWORD;
        let mut kind: DWORD = 0;
        let mut data_len = data.len() as DWORD;
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                ptr::null_mut(),
                &mut kind,
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        if status as DWORD == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            unsafe { RegCloseKey(key) };
            return Err(WinError {
                call: "RegEnumValueW",
                code: status as DWORD,
            });
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let command = if kind == REG_SZ || kind == REG_EXPAND_SZ {
            registry_string(kind, &data[..(data_len as usize).min(data.len())])
        } else {
            String::new()
        };
        entries.push(RunEntry { name, command });
        index += 1;
    }
    unsafe { RegCloseKey(key) };
    Ok(entries)
}

/// Run entries other than ezrama's whose command mentions KANALI and that
/// Task Manager has not disabled.
pub fn kanali_run_entries() -> Result<Vec<RunEntry>, WinError> {
    let mut enabled = Vec::new();
    for entry in run_entries()? {
        if entry.name == RUN_VALUE || !entry.command.to_ascii_lowercase().contains("kanali") {
            continue;
        }
        if !startup_disabled(&entry.name)? {
            enabled.push(entry);
        }
    }
    Ok(enabled)
}

/// Copies `source` over `destination`, creating the directory.
pub fn copy_binary(source: &Path, destination: &Path) -> std::io::Result<u64> {
    if let Some(directory) = destination.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::copy(source, destination)
}

/// Registry key whose subkeys are the current user's installed programs,
/// as Settings lists them.
pub const UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
/// ezrama's subkey under [`UNINSTALL_ROOT`].
pub const UNINSTALL_SUBKEY: &str = "ezrama";

/// What the installed-apps entry says about the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallEntry {
    pub display_name: String,
    pub version: String,
    pub icon: PathBuf,
    pub location: PathBuf,
    /// The command Settings runs to remove the program.
    pub uninstall_command: String,
    pub size_kb: u32,
}

/// The installed-apps key for `subkey` under [`UNINSTALL_ROOT`].
fn uninstall_key(subkey: &str) -> String {
    format!(r"{UNINSTALL_ROOT}\{subkey}")
}

fn set_string(key: HKEY, name: &str, value: &str) -> Result<(), WinError> {
    let name = wide(name);
    let data = wide(value);
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            (data.len() * 2) as DWORD,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegSetValueExW",
            code: status as DWORD,
        });
    }
    Ok(())
}

fn set_dword(key: HKEY, name: &str, value: u32) -> Result<(), WinError> {
    let name = wide(name);
    let data = value.to_le_bytes();
    let status = unsafe {
        RegSetValueExW(key, name.as_ptr(), 0, REG_DWORD, data.as_ptr(), data.len() as DWORD)
    };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegSetValueExW",
            code: status as DWORD,
        });
    }
    Ok(())
}

/// Creates or replaces the installed-apps entry `subkey`, so Settings
/// lists the program with a working Uninstall.
pub fn write_uninstall_entry(subkey: &str, entry: &UninstallEntry) -> Result<(), WinError> {
    let path = wide(&uninstall_key(subkey));
    let mut key: HKEY = ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            ptr::null_mut(),
            &mut key,
            ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegCreateKeyExW",
            code: status as DWORD,
        });
    }
    let written = set_string(key, "DisplayName", &entry.display_name)
        .and_then(|()| set_string(key, "DisplayVersion", &entry.version))
        .and_then(|()| set_string(key, "DisplayIcon", &entry.icon.to_string_lossy()))
        .and_then(|()| set_string(key, "InstallLocation", &entry.location.to_string_lossy()))
        .and_then(|()| set_string(key, "UninstallString", &entry.uninstall_command))
        .and_then(|()| set_dword(key, "NoModify", 1))
        .and_then(|()| set_dword(key, "NoRepair", 1))
        .and_then(|()| set_dword(key, "EstimatedSize", entry.size_kb));
    unsafe { RegCloseKey(key) };
    written
}

/// Removes the installed-apps entry `subkey`. Returns whether it existed.
pub fn delete_uninstall_entry(subkey: &str) -> Result<bool, WinError> {
    let path = wide(&uninstall_key(subkey));
    let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr()) };
    if status as DWORD == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(WinError {
            call: "RegDeleteKeyW",
            code: status as DWORD,
        });
    }
    Ok(true)
}

/// The command Settings runs for the installed-apps entry `subkey`, if
/// the entry exists.
pub fn uninstall_command(subkey: &str) -> Result<Option<String>, WinError> {
    read_string(HKEY_CURRENT_USER, &uninstall_key(subkey), "UninstallString")
}

/// Writes the program's icon as [`ICON_FILE`] in `directory`, creating the
/// directory, and returns the file's path.
pub fn write_icon(directory: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let path = directory.join(ICON_FILE);
    fs::write(&path, crate::icon::ico_bytes(&crate::icon::Image::embedded()))?;
    Ok(path)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_quotes_the_path() {
        let command = run_command(Path::new(r"C:\Users\me\AppData\Local\ezrama\ezramaw.exe"));
        assert_eq!(command, r#""C:\Users\me\AppData\Local\ezrama\ezramaw.exe" watch"#);
    }

    #[test]
    fn install_dir_is_under_local_app_data() {
        if let Some(dir) = install_dir() {
            assert!(dir.ends_with("ezrama"));
            assert!(dir.parent().is_some());
        }
    }

    #[test]
    fn utf16_strings_stop_at_the_terminator() {
        let mut bytes = Vec::new();
        for unit in "ab".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0, b'z', 0]);
        assert_eq!(utf16_string(&bytes), "ab");
        assert_eq!(utf16_string(&[]), "");
    }

    #[test]
    fn expandable_strings_take_values_from_the_environment() {
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand_environment(r"%SystemRoot%\sub"), format!(r"{root}\sub"));
        assert_eq!(expand_environment("plain"), "plain");
        assert_eq!(expand_environment("%ezrama_no_such_name%"), "%ezrama_no_such_name%");
        assert_eq!(expand_environment(""), "");
        let bytes: Vec<u8> = "%SystemRoot%".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(registry_string(REG_EXPAND_SZ, &bytes), root);
        assert_eq!(registry_string(REG_SZ, &bytes), "%SystemRoot%");
    }

    #[test]
    fn search_path_edits_are_case_blind_and_separator_tolerant() {
        assert_eq!(path_with("", r"C:\a"), Some(r"C:\a".to_string()));
        assert_eq!(path_with(r"C:\x;", r"C:\a"), Some(r"C:\x;C:\a".to_string()));
        assert_eq!(path_with(r"C:\x;c:\A\", r"C:\a"), None);
        assert_eq!(path_with(r"%SystemRoot%;C:\x", r"C:\a"), Some(r"%SystemRoot%;C:\x;C:\a".to_string()));
        assert_eq!(path_without(r"C:\x;C:\a;C:\y", r"c:\a\"), Some(r"C:\x;C:\y".to_string()));
        assert_eq!(path_without(r"C:\a", r"C:\a"), Some(String::new()));
        assert_eq!(path_without(r"C:\x;;C:\y", r"C:\a"), None);
        assert_eq!(path_without("", r"C:\a"), None);
    }

    #[test]
    fn a_search_path_value_round_trips_with_its_type_unexpanded() {
        let name = format!("ezrama-path-test-{}", std::process::id());
        let directory = Path::new(r"C:\ezrama-test\bin");
        let value = |name: &str| read_string_raw(HKEY_CURRENT_USER, ENVIRONMENT_KEY, name).unwrap();
        assert_eq!(value(&name), None);
        assert_eq!(edit_user_path(&name, |current| path_with(current, r"%SystemRoot%\x")), Ok(true));
        let (kind, bytes) = value(&name).unwrap();
        assert_eq!(kind, REG_EXPAND_SZ);
        assert_eq!(utf16_string(&bytes), r"%SystemRoot%\x");
        let text = directory.to_string_lossy();
        assert_eq!(edit_user_path(&name, |current| path_with(current, &text)), Ok(true));
        assert_eq!(edit_user_path(&name, |current| path_with(current, &text)), Ok(false));
        assert_eq!(utf16_string(&value(&name).unwrap().1), format!(r"%SystemRoot%\x;{text}"));
        assert_eq!(edit_user_path(&name, |current| path_without(current, &text)), Ok(true));
        assert_eq!(utf16_string(&value(&name).unwrap().1), r"%SystemRoot%\x");
        assert_eq!(edit_user_path(&name, |current| path_without(current, r"%SystemRoot%\x")), Ok(true));
        assert_eq!(value(&name), None, "an emptied value is deleted");
    }

    #[test]
    fn a_logon_entry_toggles_on_and_off() {
        let name = format!("ezrama-test-{}", std::process::id());
        assert_eq!(set_startup_value(&name, false), Ok(false));
        assert_eq!(set_startup_value(&name, true), Ok(true));
        let command = read_run_value(&name).unwrap().unwrap();
        assert!(command.ends_with(&format!("{WATCHER_BINARY}\" watch")));
        assert_eq!(set_startup_value(&name, true), Ok(false), "already set");
        assert_eq!(set_startup_value(&name, false), Ok(true));
        assert_eq!(read_run_value(&name), Ok(None));
    }

    #[test]
    fn an_installed_apps_entry_round_trips_and_deletes() {
        let subkey = format!("ezrama-test-{}", std::process::id());
        let entry = UninstallEntry {
            display_name: "ezrama test".to_string(),
            version: "0.0.0".to_string(),
            icon: PathBuf::from(r"C:\ezrama-test\ezrama.ico"),
            location: PathBuf::from(r"C:\ezrama-test"),
            uninstall_command: r#""C:\ezrama-test\ezrama.exe" uninstall"#.to_string(),
            size_kb: 840,
        };
        assert_eq!(uninstall_command(&subkey), Ok(None));
        write_uninstall_entry(&subkey, &entry).unwrap();
        assert_eq!(uninstall_command(&subkey), Ok(Some(entry.uninstall_command.clone())));
        assert_eq!(
            read_string(HKEY_CURRENT_USER, &uninstall_key(&subkey), "DisplayName"),
            Ok(Some("ezrama test".to_string()))
        );
        assert_eq!(delete_uninstall_entry(&subkey), Ok(true));
        assert_eq!(delete_uninstall_entry(&subkey), Ok(false));
        assert_eq!(uninstall_command(&subkey), Ok(None));
    }

    #[test]
    fn a_missing_key_reads_as_no_value() {
        let value = read_string(HKEY_CURRENT_USER, r"Software\ezrama-no-such-key", "x");
        assert_eq!(value, Ok(None));
    }

    #[test]
    fn startup_approval_of_an_unknown_entry_is_enabled() {
        assert_eq!(startup_disabled("ezrama-no-such-entry"), Ok(false));
    }

    #[test]
    fn run_key_is_readable_and_the_ezrama_value_is_a_string_or_absent() {
        let value = read_run_value(RUN_VALUE).unwrap();
        if let Some(command) = value {
            assert!(command.contains("watch"));
        }
        let entries = run_entries().unwrap();
        assert!(entries.iter().all(|entry| !entry.name.is_empty()));
    }
}
