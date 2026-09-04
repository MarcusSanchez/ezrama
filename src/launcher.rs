//! Starting other programs and waiting for them: the watcher at install
//! time, and KANALI on request, where the wait covers KANALI's habit of
//! handing over to a fresh process of its own.

use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::Duration;

use crate::install::read_string;
use crate::overlapped::wait_millis;
use crate::usbprint::{wide_path, WinError};
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

/// A process started by [`start_detached`]: its id and a handle to wait on.
pub struct Process {
    handle: HANDLE,
    id: u32,
}

// Process handles may be used from any thread.
unsafe impl Send for Process {}

impl Process {
    /// Opens a running process for waiting only.
    pub fn open(id: u32) -> Result<Self, WinError> {
        let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, id) };
        if handle.is_null() {
            return Err(WinError::last("OpenProcess"));
        }
        Ok(Self { handle, id })
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Blocks until the process exits.
    pub fn wait(&self) {
        unsafe {
            WaitForSingleObject(self.handle, INFINITE);
        }
    }

    /// Waits up to `timeout` for the process to exit; false on timeout.
    pub fn wait_for(&self, timeout: Duration) -> bool {
        unsafe { WaitForSingleObject(self.handle, wait_millis(timeout)) == WAIT_OBJECT_0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Starts `program` with `arguments`, in `directory` when given, detached
/// from any console and without this process's handles. A shell pipeline
/// that ran ezrama is therefore not held open by the new process.
pub fn start_detached(
    program: &Path,
    arguments: &str,
    directory: Option<&Path>,
) -> Result<Process, WinError> {
    let application = wide_path(program);
    let mut command_line: Vec<u16> = std::iter::once(u16::from(b'"'))
        .chain(program.as_os_str().encode_wide())
        .chain(format!("\" {arguments}").encode_utf16())
        .chain(std::iter::once(0))
        .collect();
    let directory = directory.map(wide_path);
    let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
    startup.cb = mem::size_of::<STARTUPINFOW>() as DWORD;
    let mut information: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            DETACHED_PROCESS,
            ptr::null_mut(),
            directory.as_ref().map_or(ptr::null(), |d| d.as_ptr()),
            &startup,
            &mut information,
        )
    };
    if created == 0 {
        return Err(WinError::last("CreateProcessW"));
    }
    unsafe {
        CloseHandle(information.hThread);
    }
    Ok(Process {
        handle: information.hProcess,
        id: information.dwProcessId,
    })
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

/// Starts the program at `path` in its own directory.
pub fn launch(path: &Path) -> Result<Process, WinError> {
    start_detached(path, "", path.parent())
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
/// before an empty result counts. Once a process has been seen, an empty
/// result counts at once: KANALI hands over exactly once, within a third
/// of a second, and repeating the grace after every wait would delay each
/// resume by the full grace for a second hand-over that does not happen.
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

    #[test]
    fn structure_layouts() {
        assert_eq!(mem::size_of::<STARTUPINFOW>(), 104);
        assert_eq!(mem::size_of::<PROCESS_INFORMATION>(), 24);
        assert_eq!(mem::size_of::<PROCESSENTRY32W>(), 568);
    }

    #[test]
    fn detached_processes_start_and_can_be_waited_for() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let process = start_detached(&system.join("cmd.exe"), "/c exit 0", Some(&system)).unwrap();
        assert_ne!(process.id(), 0);
        process.wait();
        assert!(process.wait_for(Duration::ZERO));
        let own = Process::open(std::process::id()).unwrap();
        assert!(!own.wait_for(Duration::ZERO));
        assert!(Process::open(0).is_err());
        let missing = start_detached(Path::new(r"C:\ezrama-no-such-program.exe"), "", None);
        assert_eq!(missing.err().map(|error| error.code), Some(ERROR_FILE_NOT_FOUND));
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
        }
    }
}
