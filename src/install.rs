//! Installing for the current user: the program under local app data, the
//! windowless copy derived from it, the icon file, the logon entry, the
//! installed-apps entry, and the search path.

use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::{self, read_string, utf16_string, Key, Root};
use crate::usbprint::WinError;
use crate::win::*;

/// Registry key whose values start programs at logon for the current user.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Name of ezrama's value under [`RUN_KEY`].
pub const RUN_VALUE: &str = "ezrama";
/// Key where Task Manager records which Run entries it has disabled.
pub const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// Registry key whose subkeys are the current user's installed programs,
/// as Settings lists them.
pub const UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
/// ezrama's subkey under [`UNINSTALL_ROOT`].
pub const UNINSTALL_SUBKEY: &str = "ezrama";
/// Registry key holding the current user's environment variables.
pub const ENVIRONMENT_KEY: &str = "Environment";
/// The user's search path value under [`ENVIRONMENT_KEY`].
pub const PATH_VALUE: &str = "Path";
/// File name of the console binary.
pub const CONSOLE_BINARY: &str = "ezrama.exe";
/// File name of the windowless copy that the logon entry starts.
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

/// Copies `source` over `destination`, creating the directory.
pub fn copy_binary(source: &Path, destination: &Path) -> std::io::Result<u64> {
    if let Some(directory) = destination.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::copy(source, destination)
}

/// Subsystem value of a program that runs without a console.
const SUBSYSTEM_WINDOWS_GUI: u16 = 2;
/// Offset of the subsystem field within a 64-bit optional header.
const OPTIONAL_HEADER_SUBSYSTEM: usize = 68;
/// Offset of the checksum field within a 64-bit optional header.
const OPTIONAL_HEADER_CHECKSUM: usize = 64;
/// Magic number of a 64-bit optional header.
const OPTIONAL_HEADER_MAGIC_PE32_PLUS: u16 = 0x20b;

/// The console program `image` rewritten to run without a console: the
/// same code and data with the subsystem field set to the windowed value
/// and the checksum cleared, which is what the windowless twin of a
/// console build differs in.
pub fn windowless_image(image: &[u8]) -> Result<Vec<u8>, String> {
    let field = |offset: usize| -> Result<u16, String> {
        let bytes = image
            .get(offset..offset + 2)
            .ok_or_else(|| "the image is too short".to_string())?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    if image.get(..2) != Some(b"MZ") {
        return Err("not a Windows executable".to_string());
    }
    let header = u32::from_le_bytes(
        image
            .get(60..64)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "the image is too short".to_string())?,
    ) as usize;
    if image.get(header..header + 4) != Some(b"PE\0\0") {
        return Err("no executable header".to_string());
    }
    let optional = header + 24;
    if field(optional)? != OPTIONAL_HEADER_MAGIC_PE32_PLUS {
        return Err("not a 64-bit executable".to_string());
    }
    let subsystem = optional + OPTIONAL_HEADER_SUBSYSTEM;
    let checksum = optional + OPTIONAL_HEADER_CHECKSUM;
    if image.len() < subsystem + 2 {
        return Err("the image is too short".to_string());
    }
    let mut out = image.to_vec();
    out[subsystem..subsystem + 2].copy_from_slice(&SUBSYSTEM_WINDOWS_GUI.to_le_bytes());
    out[checksum..checksum + 4].copy_from_slice(&0u32.to_le_bytes());
    Ok(out)
}

/// Writes the windowless twin of the program at `console` as
/// [`WATCHER_BINARY`] next to it.
pub fn write_watcher(console: &Path) -> std::io::Result<PathBuf> {
    let image = fs::read(console)?;
    let windowless = windowless_image(&image).map_err(std::io::Error::other)?;
    let path = console.with_file_name(WATCHER_BINARY);
    fs::write(&path, windowless)?;
    Ok(path)
}

/// Writes the program's icon as [`ICON_FILE`] in `directory`, creating the
/// directory, and returns the file's path.
pub fn write_icon(directory: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let path = directory.join(ICON_FILE);
    fs::write(&path, crate::icon::ico_bytes(&crate::icon::Image::embedded()))?;
    Ok(path)
}

/// A value under the Run key: its name and the command it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEntry {
    pub name: String,
    pub command: String,
}

/// Whether Task Manager has disabled the Run entry `name`. An entry with
/// no record is enabled; the record's first byte is even when enabled and
/// odd when disabled.
pub fn startup_disabled(name: &str) -> Result<bool, WinError> {
    let record = registry::read_raw(Root::CurrentUser, STARTUP_APPROVED_KEY, name)?;
    Ok(record.is_some_and(|(_, bytes)| bytes.first().is_some_and(|byte| byte % 2 == 1)))
}

/// Reads the string value `name` under the Run key, if it exists.
pub fn read_run_value(name: &str) -> Result<Option<String>, WinError> {
    read_string(Root::CurrentUser, RUN_KEY, name)
}

/// Writes the string value `name` under the Run key.
pub fn set_run_value(name: &str, command: &str) -> Result<(), WinError> {
    Key::open(Root::CurrentUser, RUN_KEY, KEY_SET_VALUE)?.set_string(name, command)
}

/// Deletes the value `name` under the Run key. Returns whether it existed.
pub fn delete_run_value(name: &str) -> Result<bool, WinError> {
    Key::open(Root::CurrentUser, RUN_KEY, KEY_SET_VALUE)?.delete_value(name)
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
    if read_run_value(name)?.as_deref() == Some(command.as_str()) {
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
    let values = Key::open(Root::CurrentUser, RUN_KEY, KEY_QUERY_VALUE)?.values()?;
    Ok(values
        .into_iter()
        .map(|value| RunEntry {
            name: value.name,
            command: value.text,
        })
        .collect())
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

/// The entry for an installation in `directory`: the console binary's
/// `uninstall` command, the icon file, and the size of what is there.
pub fn uninstall_entry_for(directory: &Path, name: &str, version: &str) -> UninstallEntry {
    let size_kb = [CONSOLE_BINARY, WATCHER_BINARY, ICON_FILE]
        .iter()
        .filter_map(|file| fs::metadata(directory.join(file)).ok())
        .map(|metadata| metadata.len())
        .sum::<u64>()
        .div_ceil(1024) as u32;
    UninstallEntry {
        display_name: name.to_string(),
        version: version.to_string(),
        icon: directory.join(ICON_FILE),
        location: directory.to_path_buf(),
        uninstall_command: format!("\"{}\" uninstall", directory.join(CONSOLE_BINARY).display()),
        size_kb,
    }
}

/// The installed-apps key for `subkey` under [`UNINSTALL_ROOT`].
fn uninstall_key(subkey: &str) -> String {
    format!(r"{UNINSTALL_ROOT}\{subkey}")
}

/// Creates or replaces the installed-apps entry `subkey`, so Settings
/// lists the program with a working Uninstall.
pub fn write_uninstall_entry(subkey: &str, entry: &UninstallEntry) -> Result<(), WinError> {
    let key = Key::create(Root::CurrentUser, &uninstall_key(subkey))?;
    key.set_string("DisplayName", &entry.display_name)?;
    key.set_string("DisplayVersion", &entry.version)?;
    key.set_path("DisplayIcon", &entry.icon)?;
    key.set_path("InstallLocation", &entry.location)?;
    key.set_string("UninstallString", &entry.uninstall_command)?;
    key.set_dword("NoModify", 1)?;
    key.set_dword("NoRepair", 1)?;
    key.set_dword("EstimatedSize", entry.size_kb)
}

/// Removes the installed-apps entry `subkey`. Returns whether it existed.
pub fn delete_uninstall_entry(subkey: &str) -> Result<bool, WinError> {
    registry::delete_key(Root::CurrentUser, &uninstall_key(subkey))
}

/// The command Settings runs for the installed-apps entry `subkey`, if
/// the entry exists.
pub fn uninstall_command(subkey: &str) -> Result<Option<String>, WinError> {
    read_string(Root::CurrentUser, &uninstall_key(subkey), "UninstallString")
}

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

/// Tells open programs the environment changed, so a new terminal sees
/// the new search path.
fn announce_environment_change() {
    let what = crate::usbprint::wide(ENVIRONMENT_KEY);
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
    let (kind, current) = match registry::read_raw(Root::CurrentUser, ENVIRONMENT_KEY, name)? {
        Some((kind, bytes)) => (kind, utf16_string(&bytes)),
        None => (REG_EXPAND_SZ, String::new()),
    };
    let Some(updated) = edit(&current) else {
        return Ok(false);
    };
    let key = Key::open(Root::CurrentUser, ENVIRONMENT_KEY, KEY_SET_VALUE)?;
    if updated.is_empty() {
        key.delete_value(name)?;
    } else {
        key.set_string_of_kind(name, &updated, kind)?;
    }
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
    let current = registry::read_raw(Root::CurrentUser, ENVIRONMENT_KEY, PATH_VALUE)?
        .map(|(_, bytes)| utf16_string(&bytes))
        .unwrap_or_default();
    let directory = directory.to_string_lossy();
    Ok(path_with(&current, &directory).is_none())
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

    /// A minimal 64-bit image: DOS stub, PE signature, COFF header, and an
    /// optional header with the console subsystem and a checksum.
    fn console_image() -> Vec<u8> {
        let header = 128usize;
        let mut image = vec![0u8; header + 4 + 20 + 240];
        image[..2].copy_from_slice(b"MZ");
        image[60..64].copy_from_slice(&(header as u32).to_le_bytes());
        image[header..header + 4].copy_from_slice(b"PE\0\0");
        let optional = header + 24;
        image[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        image[optional + 64..optional + 68].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        image[optional + 68..optional + 70].copy_from_slice(&3u16.to_le_bytes());
        image
    }

    #[test]
    fn a_console_image_becomes_windowless_by_two_fields() {
        let image = console_image();
        let windowless = windowless_image(&image).unwrap();
        assert_eq!(windowless.len(), image.len());
        let optional = 128 + 24;
        assert_eq!(&windowless[optional + 68..optional + 70], &2u16.to_le_bytes());
        assert_eq!(&windowless[optional + 64..optional + 68], &0u32.to_le_bytes());
        let differing = windowless.iter().zip(&image).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 5, "only the subsystem byte and the four checksum bytes change");
        assert!(windowless_image(b"MZ").is_err());
        assert!(windowless_image(b"not an image at all, but long enough to read a header from").is_err());
        let mut wrong_magic = image.clone();
        wrong_magic[optional..optional + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        assert_eq!(windowless_image(&wrong_magic), Err("not a 64-bit executable".to_string()));
    }

    #[test]
    fn this_program_can_be_made_windowless() {
        let own = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let windowless = windowless_image(&own).unwrap();
        assert_eq!(windowless.len(), own.len());
        let header = u32::from_le_bytes(own[60..64].try_into().unwrap()) as usize;
        let optional = header + 24;
        assert_eq!(&own[optional + 68..optional + 70], &3u16.to_le_bytes(), "tests run as a console program");
        assert_eq!(&windowless[optional + 68..optional + 70], &2u16.to_le_bytes());
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
        let value = |name: &str| registry::read_raw(Root::CurrentUser, ENVIRONMENT_KEY, name).unwrap();
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
            read_string(Root::CurrentUser, &uninstall_key(&subkey), "DisplayIcon"),
            Ok(Some(r"C:\ezrama-test\ezrama.ico".to_string()))
        );
        assert_eq!(delete_uninstall_entry(&subkey), Ok(true));
        assert_eq!(delete_uninstall_entry(&subkey), Ok(false));
        assert_eq!(uninstall_command(&subkey), Ok(None));
    }

    #[test]
    fn the_entry_for_a_directory_names_its_files() {
        let entry = uninstall_entry_for(Path::new(r"C:\ezrama-test"), "ezrama", "0.0.0");
        assert_eq!(entry.icon, PathBuf::from(r"C:\ezrama-test\ezrama.ico"));
        assert_eq!(entry.uninstall_command, r#""C:\ezrama-test\ezrama.exe" uninstall"#);
        assert_eq!(entry.size_kb, 0, "nothing is there");
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
