//! Small operating-system helpers: running processes, installed services and
//! the Windows admin prompt.
//!
//! These call the OS directly instead of shelling out to PowerShell or
//! command-line tools. Hidden PowerShell windows are a hallmark of malware, and
//! antivirus heuristics rightly treat them that way.

use std::collections::HashSet;
use std::path::PathBuf;

/// A running process.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    /// Executable name, lowercased (`icue.exe`).
    pub name: String,
}

/// Names of running processes, lowercased.
pub fn running_process_names() -> HashSet<String> {
    processes().into_iter().map(|p| p.name).collect()
}

#[cfg(windows)]
mod imp {
    use super::Process;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, INFINITE,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn processes() -> Vec<Process> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap.is_null() {
                return out;
            }
            let _guard = Handle(snap);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut entry) == 0 {
                return out;
            }
            loop {
                out.push(Process { pid: entry.th32ProcessID, name: from_wide(&entry.szExeFile).to_lowercase() });
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        out
    }

    pub fn process_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let _guard = Handle(h);
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0 {
                return None;
            }
            Some(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
        }
    }

    pub fn kill(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return false;
            }
            let _guard = Handle(h);
            TerminateProcess(h, 1) != 0
        }
    }

    pub fn service_exists(name: &str) -> bool {
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() {
                return false;
            }
            let name = wide(std::ffi::OsStr::new(name));
            let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
            let found = !svc.is_null();
            if found {
                CloseServiceHandle(svc);
            }
            CloseServiceHandle(scm);
            found
        }
    }

    pub fn run_as_admin(exe: &Path, args: &str) -> Result<u32, super::ElevateError> {
        let verb = wide(std::ffi::OsStr::new("runas"));
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(args));
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = 0; // SW_HIDE: the helper has no window anyway
            if ShellExecuteExW(&mut info) == 0 {
                let err = GetLastError();
                return Err(if err == 1223 { super::ElevateError::Declined } else { super::ElevateError::Failed(err) });
            }
            if info.hProcess.is_null() {
                return Err(super::ElevateError::Failed(0));
            }
            let _guard = Handle(info.hProcess);
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            Ok(code)
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Process;
    use std::path::{Path, PathBuf};

    pub fn processes() -> Vec<Process> {
        let mut out = Vec::new();
        if let Ok(dir) = std::fs::read_dir("/proc") {
            for e in dir.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse().ok()) else { continue };
                if let Ok(name) = std::fs::read_to_string(e.path().join("comm")) {
                    out.push(Process { pid, name: name.trim().to_lowercase() });
                }
            }
        }
        out
    }

    pub fn process_path(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    pub fn kill(_pid: u32) -> bool {
        false
    }

    pub fn service_exists(_name: &str) -> bool {
        false
    }

    pub fn run_as_admin(_exe: &Path, _args: &str) -> Result<u32, super::ElevateError> {
        Err(super::ElevateError::Failed(0))
    }
}

/// Why the admin prompt didn't run the helper.
#[derive(Debug, PartialEq)]
pub enum ElevateError {
    /// The user said no.
    Declined,
    /// Windows error code.
    Failed(u32),
}

pub fn processes() -> Vec<Process> {
    imp::processes()
}

/// Full path of a process's executable, if it's readable.
pub fn process_path(pid: u32) -> Option<PathBuf> {
    imp::process_path(pid)
}

/// Ends a process. Returns whether it worked.
pub fn kill(pid: u32) -> bool {
    imp::kill(pid)
}

/// A Windows service with this name is installed.
pub fn service_exists(name: &str) -> bool {
    imp::service_exists(name)
}

/// Runs `exe args` as administrator after the Windows permission prompt, waits
/// for it and returns its exit code.
pub fn run_as_admin(exe: &std::path::Path, args: &str) -> Result<u32, ElevateError> {
    imp::run_as_admin(exe, args)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_this_process() {
        let me = std::process::id();
        assert!(super::processes().iter().any(|p| p.pid == me));
        assert!(super::process_path(me).is_some());
    }
}
