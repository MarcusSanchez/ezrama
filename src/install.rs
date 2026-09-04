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
    Ok(Some(registry_string(kind, &data[..size as usize])))
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

/// Every value under the Run key.
pub fn run_entries() -> Result<Vec<RunEntry>, WinError> {
    let key = open_run_key(KEY_QUERY_VALUE)?;
    let mut entries = Vec::new();
    let mut index: DWORD = 0;
    loop {
        let mut name = vec![0u16; 16_384];
        let mut name_len = name.len() as DWORD;
        let mut kind: DWORD = 0;
        let mut data = vec![0u8; 65_536];
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
