//! Per-user registry access: a key opened around a few calls and closed
//! when dropped, string values read as stored or with their environment
//! references expanded, and values written, deleted, and enumerated.

use std::path::Path;
use std::ptr;

use crate::usbprint::{wide, wide_path, WinError};
use crate::win::*;

fn status_error(call: &'static str, status: LSTATUS) -> WinError {
    WinError {
        call,
        code: status as DWORD,
    }
}

/// A predefined root key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    CurrentUser,
    LocalMachine,
}

impl Root {
    fn handle(self) -> HKEY {
        match self {
            Root::CurrentUser => HKEY_CURRENT_USER,
            Root::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

/// Decodes a NUL-terminated little-endian UTF-16 value.
pub fn utf16_string(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A string value as stored: expandable strings have their `%NAME%`
/// references replaced from the environment.
pub fn registry_string(kind: DWORD, bytes: &[u8]) -> String {
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

/// A value under a key: its name, type, and text (empty for non-string
/// types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub name: String,
    pub kind: DWORD,
    pub text: String,
}

/// An open key, closed when dropped.
pub struct Key {
    handle: HKEY,
}

impl Key {
    /// Opens `subkey` under `root` with `access`. A missing key is the
    /// error `ERROR_FILE_NOT_FOUND`.
    pub fn open(root: Root, subkey: &str, access: DWORD) -> Result<Self, WinError> {
        let subkey = wide(subkey);
        let mut handle: HKEY = ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(root.handle(), subkey.as_ptr(), 0, access, &mut handle) };
        if status != ERROR_SUCCESS {
            return Err(status_error("RegOpenKeyExW", status));
        }
        Ok(Self { handle })
    }

    /// Opens `subkey` under `root` for reading and writing, creating it
    /// if needed.
    pub fn create(root: Root, subkey: &str) -> Result<Self, WinError> {
        let subkey = wide(subkey);
        let mut handle: HKEY = ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                root.handle(),
                subkey.as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                ptr::null_mut(),
                &mut handle,
                ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(status_error("RegCreateKeyExW", status));
        }
        Ok(Self { handle })
    }

    fn set(&self, name: &str, kind: DWORD, data: &[u8]) -> Result<(), WinError> {
        let name = wide(name);
        let status = unsafe {
            RegSetValueExW(self.handle, name.as_ptr(), 0, kind, data.as_ptr(), data.len() as DWORD)
        };
        if status != ERROR_SUCCESS {
            return Err(status_error("RegSetValueExW", status));
        }
        Ok(())
    }

    fn set_units(&self, name: &str, kind: DWORD, units: &[u16]) -> Result<(), WinError> {
        let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        self.set(name, kind, &bytes)
    }

    /// Writes a plain string value.
    pub fn set_string(&self, name: &str, value: &str) -> Result<(), WinError> {
        self.set_units(name, REG_SZ, &wide(value))
    }

    /// Writes a string value of the given type, for values that must keep
    /// their `%NAME%` references.
    pub fn set_string_of_kind(&self, name: &str, value: &str, kind: DWORD) -> Result<(), WinError> {
        self.set_units(name, kind, &wide(value))
    }

    /// Writes a path as a plain string value, unit for unit.
    pub fn set_path(&self, name: &str, path: &Path) -> Result<(), WinError> {
        self.set_units(name, REG_SZ, &wide_path(path))
    }

    /// Writes a 32-bit value.
    pub fn set_dword(&self, name: &str, value: u32) -> Result<(), WinError> {
        self.set(name, REG_DWORD, &value.to_le_bytes())
    }

    /// Deletes the value `name`. Returns whether it existed.
    pub fn delete_value(&self, name: &str) -> Result<bool, WinError> {
        let name = wide(name);
        let status = unsafe { RegDeleteValueW(self.handle, name.as_ptr()) };
        if status as DWORD == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        if status != ERROR_SUCCESS {
            return Err(status_error("RegDeleteValueW", status));
        }
        Ok(true)
    }

    /// Reads the value `name` as stored: its type and its bytes, or `None`
    /// when there is no such value.
    pub fn read_raw(&self, name: &str) -> Result<Option<(DWORD, Vec<u8>)>, WinError> {
        let value_name = wide(name);
        let mut kind: DWORD = 0;
        let mut size: DWORD = 0;
        let status = unsafe {
            RegQueryValueExW(
                self.handle,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut kind,
                ptr::null_mut(),
                &mut size,
            )
        };
        if status as DWORD == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(status_error("RegQueryValueExW", status));
        }
        let mut data = vec![0u8; size as usize + 2];
        let mut size = data.len() as DWORD;
        let status = unsafe {
            RegQueryValueExW(
                self.handle,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut kind,
                data.as_mut_ptr(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(status_error("RegQueryValueExW", status));
        }
        data.truncate(size as usize);
        Ok(Some((kind, data)))
    }

    /// Every value under the key, string values decoded and expanded.
    pub fn values(&self) -> Result<Vec<Value>, WinError> {
        let mut values = Vec::new();
        let mut name = vec![0u16; 16_384];
        let mut data = vec![0u8; 65_536];
        let mut index: DWORD = 0;
        loop {
            let mut name_len = name.len() as DWORD;
            let mut kind: DWORD = 0;
            let mut data_len = data.len() as DWORD;
            let status = unsafe {
                RegEnumValueW(
                    self.handle,
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
                return Err(status_error("RegEnumValueW", status));
            }
            let text = if kind == REG_SZ || kind == REG_EXPAND_SZ {
                registry_string(kind, &data[..(data_len as usize).min(data.len())])
            } else {
                String::new()
            };
            values.push(Value {
                name: String::from_utf16_lossy(&name[..name_len as usize]),
                kind,
                text,
            });
            index += 1;
        }
        Ok(values)
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.handle);
        }
    }
}

/// Opens `subkey` for reading, treating a missing key as no key.
fn open_for_reading(root: Root, subkey: &str) -> Result<Option<Key>, WinError> {
    match Key::open(root, subkey, KEY_QUERY_VALUE) {
        Ok(key) => Ok(Some(key)),
        Err(error) if error.code == ERROR_FILE_NOT_FOUND => Ok(None),
        Err(error) => Err(error),
    }
}

/// Reads the value `name` under `subkey` of `root` as stored. A missing
/// key or value is `None`.
pub fn read_raw(root: Root, subkey: &str, name: &str) -> Result<Option<(DWORD, Vec<u8>)>, WinError> {
    match open_for_reading(root, subkey)? {
        Some(key) => key.read_raw(name),
        None => Ok(None),
    }
}

/// Reads the string value `name` under `subkey` of `root`, expanded when
/// it is an expandable string. A missing key or value, or a value of
/// another type, is `None`.
pub fn read_string(root: Root, subkey: &str, name: &str) -> Result<Option<String>, WinError> {
    Ok(read_raw(root, subkey, name)?
        .filter(|(kind, _)| *kind == REG_SZ || *kind == REG_EXPAND_SZ)
        .map(|(kind, bytes)| registry_string(kind, &bytes)))
}

/// Deletes `subkey` under `root`. Returns whether it existed.
pub fn delete_key(root: Root, subkey: &str) -> Result<bool, WinError> {
    let subkey = wide(subkey);
    let status = unsafe { RegDeleteKeyW(root.handle(), subkey.as_ptr()) };
    if status as DWORD == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(status_error("RegDeleteKeyW", status));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(read_string(Root::CurrentUser, r"Software\ezrama-no-such-key", "x"), Ok(None));
        assert_eq!(delete_key(Root::CurrentUser, r"Software\ezrama-no-such-key"), Ok(false));
    }

    #[test]
    fn values_of_every_kind_round_trip_under_a_test_key() {
        let subkey = format!(r"Software\ezrama-registry-test-{}", std::process::id());
        let key = Key::create(Root::CurrentUser, &subkey).unwrap();
        key.set_string("plain", "one").unwrap();
        key.set_string_of_kind("expandable", "%SystemRoot%", REG_EXPAND_SZ).unwrap();
        key.set_path("path", Path::new(r"C:\ezrama test\x")).unwrap();
        key.set_dword("number", 7).unwrap();
        let mut values = key.values().unwrap();
        values.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = values.iter().map(|value| value.name.as_str()).collect();
        assert_eq!(names, ["expandable", "number", "path", "plain"]);
        assert_eq!(values[0].text, std::env::var("SystemRoot").unwrap());
        assert_eq!(values[1].kind, REG_DWORD);
        assert_eq!(values[1].text, "");
        assert_eq!(values[2].text, r"C:\ezrama test\x");
        assert_eq!(values[3].text, "one");
        let (kind, bytes) = key.read_raw("expandable").unwrap().unwrap();
        assert_eq!(kind, REG_EXPAND_SZ);
        assert_eq!(utf16_string(&bytes), "%SystemRoot%");
        assert_eq!(read_string(Root::CurrentUser, &subkey, "number"), Ok(None), "not a string");
        assert_eq!(key.delete_value("plain"), Ok(true));
        assert_eq!(key.delete_value("plain"), Ok(false));
        drop(key);
        assert_eq!(delete_key(Root::CurrentUser, &subkey), Ok(true));
        assert_eq!(read_string(Root::CurrentUser, &subkey, "path"), Ok(None));
    }
}
