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

/// A Windows service's (or kernel driver's) state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceState {
    pub running: bool,
    /// Why it stopped, as a Windows error code (0 when none). 1275 means
    /// Windows blocked the driver; 577 means its signature was rejected.
    pub exit_code: u32,
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
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        OsString::from_wide(&buf[..end])
            .to_string_lossy()
            .into_owned()
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
                out.push(Process {
                    pid: entry.th32ProcessID,
                    name: from_wide(&entry.szExeFile).to_lowercase(),
                });
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

    /// A registry value as text (REG_SZ, or REG_MULTI_SZ joined by commas).
    fn reg_text(key: &str, value: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
        };
        let k = wide(std::ffi::OsStr::new(key));
        let v = wide(std::ffi::OsStr::new(value));
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                k.as_ptr(),
                v.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_MULTI_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r != 0 {
            return None;
        }
        let words = &buf[..(len as usize / 2).min(buf.len())];
        let text: Vec<String> = words
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| OsString::from_wide(s).to_string_lossy().into_owned())
            .collect();
        Some(text.join(","))
    }

    /// Subkey names under an HKLM key.
    fn reg_subkeys(key: &str) -> Vec<String> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
        };
        let k = wide(std::ffi::OsStr::new(key));
        let mut out = Vec::new();
        unsafe {
            let mut h: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, k.as_ptr(), 0, KEY_READ, &mut h) != 0 {
                return out;
            }
            for i in 0..64u32 {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(
                    h,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) != 0
                {
                    break;
                }
                out.push(
                    OsString::from_wide(&name[..len as usize])
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            RegCloseKey(h);
        }
        out
    }

    pub fn usb_driver_info(vid: u16, pid: u16) -> Vec<String> {
        let base = format!(r"SYSTEM\CurrentControlSet\Enum\USB\VID_{vid:04X}&PID_{pid:04X}");
        reg_subkeys(&base)
            .into_iter()
            .map(|inst| {
                let key = format!(r"{base}\{inst}");
                let get = |v: &str| reg_text(&key, v).unwrap_or_else(|| "-".into());
                format!(
                    "{inst}: driver {} | class {} | {} | filters {}",
                    get("Service"),
                    get("Class"),
                    get("DeviceDesc"),
                    get("UpperFilters")
                )
            })
            .collect()
    }

    pub fn usb_class_filters() -> Option<String> {
        reg_text(
            r"SYSTEM\CurrentControlSet\Control\Class\{36FC9E60-C465-11CF-8056-444553540000}",
            "UpperFilters",
        )
    }

    pub fn service_state(name: &str) -> Option<super::ServiceState> {
        use windows_sys::Win32::System::Services::{
            QueryServiceStatus, SERVICE_RUNNING, SERVICE_STATUS,
        };
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() {
                return None;
            }
            let wname = wide(std::ffi::OsStr::new(name));
            let svc = OpenServiceW(scm, wname.as_ptr(), SERVICE_QUERY_STATUS);
            let mut out = None;
            if !svc.is_null() {
                let mut st: SERVICE_STATUS = std::mem::zeroed();
                if QueryServiceStatus(svc, &mut st) != 0 {
                    out = Some(super::ServiceState {
                        running: st.dwCurrentState == SERVICE_RUNNING,
                        exit_code: st.dwWin32ExitCode,
                    });
                }
                CloseServiceHandle(svc);
            }
            CloseServiceHandle(scm);
            out
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
                return Err(if err == 1223 {
                    super::ElevateError::Declined
                } else {
                    super::ElevateError::Failed(err)
                });
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
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse().ok()) else {
                    continue;
                };
                if let Ok(name) = std::fs::read_to_string(e.path().join("comm")) {
                    out.push(Process {
                        pid,
                        name: name.trim().to_lowercase(),
                    });
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

    pub fn service_state(_name: &str) -> Option<super::ServiceState> {
        None
    }

    pub fn usb_driver_info(_vid: u16, _pid: u16) -> Vec<String> {
        Vec::new()
    }

    pub fn usb_class_filters() -> Option<String> {
        None
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

/// For each plugged-in instance of a USB device: its Windows driver, class
/// and filters (from the registry), for the troubleshooting report.
pub fn usb_driver_info(vid: u16, pid: u16) -> Vec<String> {
    imp::usb_driver_info(vid, pid)
}

/// Filters Windows adds to every USB hub and controller (UsbDk belongs here).
pub fn usb_class_filters() -> Option<String> {
    imp::usb_class_filters()
}

/// A service's state, or `None` when it isn't installed.
pub fn service_state(name: &str) -> Option<ServiceState> {
    imp::service_state(name)
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
