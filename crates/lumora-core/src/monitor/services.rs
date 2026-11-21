//! Windows services: listed by Lumora itself, started, stopped and changed
//! by its admin helper (Windows only lets admins do that).

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub display: String,
    pub description: String,
    /// "running", "stopped", "starting", "stopping" or "paused".
    pub state: &'static str,
    /// "auto", "delayed", "manual", "disabled", "boot" or "system".
    pub start: &'static str,
    pub pid: u32,
    pub can_stop: bool,
}

pub fn state_name(s: u32) -> &'static str {
    match s {
        4 => "running",
        2 => "starting",
        3 => "stopping",
        7 | 6 | 5 => "paused",
        _ => "stopped",
    }
}

pub fn start_name(start: u32, delayed: bool) -> &'static str {
    match start {
        2 if delayed => "delayed",
        2 => "auto",
        3 => "manual",
        4 => "disabled",
        0 => "boot",
        1 => "system",
        _ => "manual",
    }
}

pub use imp::{control, list, set_start};

#[cfg(windows)]
mod imp {
    use super::{start_name, state_name, Service};
    use crate::monitor::win::{from_ptr, wide};
    use windows_sys::Win32::System::Services::{
        ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
        EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfig2W,
        QueryServiceConfigW, QueryServiceStatusEx, StartServiceW, ENUM_SERVICE_STATUS_PROCESSW,
        QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT,
        SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_STOP,
        SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
        SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO,
        SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_DISABLED, SERVICE_NO_CHANGE,
        SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STATE_ALL,
        SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_WIN32,
    };

    struct H(SC_HANDLE);
    impl Drop for H {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: our own handle.
                unsafe { CloseServiceHandle(self.0) };
            }
        }
    }

    fn manager(access: u32) -> Result<H, String> {
        // SAFETY: the local service manager.
        let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
        if h.is_null() {
            Err("couldn't open the Windows service list".into())
        } else {
            Ok(H(h))
        }
    }

    fn service(m: &H, name: &str, access: u32) -> Result<H, String> {
        let n = wide(name);
        // SAFETY: valid manager and name.
        let h = unsafe { OpenServiceW(m.0, n.as_ptr(), access) };
        if h.is_null() {
            Err(format!("Windows didn't allow changing {name}"))
        } else {
            Ok(H(h))
        }
    }

    /// (start type, delayed, description) of one service.
    fn config(m: &H, name: &str) -> (u32, bool, String) {
        let Ok(s) = service(m, name, SERVICE_QUERY_CONFIG) else {
            return (3, false, String::new());
        };
        let mut buf = vec![0u64; 1024];
        let mut need = 0u32;
        // SAFETY: a buffer as long as we say.
        let start = if unsafe {
            QueryServiceConfigW(
                s.0,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut need,
            )
        } != 0
        {
            // SAFETY: filled by the call above.
            unsafe { (*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW)).dwStartType }
        } else {
            3
        };
        let mut delayed = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: 0,
        };
        // SAFETY: a struct of the size passed.
        unsafe {
            QueryServiceConfig2W(
                s.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                (&mut delayed as *mut SERVICE_DELAYED_AUTO_START_INFO).cast(),
                std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>() as u32,
                &mut need,
            )
        };
        // SAFETY: as above.
        let description = if unsafe {
            QueryServiceConfig2W(
                s.0,
                SERVICE_CONFIG_DESCRIPTION,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut need,
            )
        } != 0
        {
            // SAFETY: filled above; the text points into the buffer.
            let d = unsafe { &*(buf.as_ptr() as *const SERVICE_DESCRIPTIONW) };
            let s = unsafe { from_ptr(d.lpDescription) };
            // Some descriptions are resource references ("@%SystemRoot%\…").
            if s.starts_with('@') {
                String::new()
            } else {
                s
            }
        } else {
            String::new()
        };
        (start, delayed.fDelayedAutostart != 0, description)
    }

    pub fn list() -> Vec<Service> {
        let Ok(m) = manager(SC_MANAGER_ENUMERATE_SERVICE | SC_MANAGER_CONNECT) else {
            return Vec::new();
        };
        let mut need = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        let mut buf = vec![0u64; 64 * 1024];
        // SAFETY: a buffer as long as we say.
        let ok = unsafe {
            EnumServicesStatusExW(
                m.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut need,
                &mut count,
                &mut resume,
                std::ptr::null(),
            )
        } != 0;
        if !ok && count == 0 {
            return Vec::new();
        }
        let items = buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW;
        let mut out: Vec<Service> = (0..count as usize)
            .map(|i| {
                // SAFETY: `count` entries were written; strings point into the buffer.
                let e = unsafe { &*items.add(i) };
                let name = unsafe { from_ptr(e.lpServiceName) };
                let st = &e.ServiceStatusProcess;
                let (start, delayed, description) = config(&m, &name);
                Service {
                    display: unsafe { from_ptr(e.lpDisplayName) },
                    description,
                    state: state_name(st.dwCurrentState),
                    start: start_name(start, delayed),
                    pid: st.dwProcessId,
                    can_stop: st.dwControlsAccepted & SERVICE_ACCEPT_STOP != 0,
                    name,
                }
            })
            .collect();
        out.sort_by_key(|s| s.display.to_lowercase());
        out
    }

    fn wait_for(s: &H, want: u32) {
        for _ in 0..60 {
            let mut st: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
            let mut need = 0u32;
            // SAFETY: a struct of the size passed.
            let ok = unsafe {
                QueryServiceStatusEx(
                    s.0,
                    SC_STATUS_PROCESS_INFO,
                    (&mut st as *mut SERVICE_STATUS_PROCESS).cast(),
                    std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                    &mut need,
                )
            } != 0;
            if !ok || st.dwCurrentState == want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Starts, stops or restarts a service (needs admin rights).
    pub fn control(name: &str, action: &str) -> Result<(), String> {
        let m = manager(SC_MANAGER_CONNECT)?;
        let s = service(
            &m,
            name,
            SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS,
        )?;
        let stop = || {
            let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
            // SAFETY: our own handle.
            if unsafe { ControlService(s.0, SERVICE_CONTROL_STOP, &mut st) } == 0 {
                return Err(format!("{name} didn't stop"));
            }
            wait_for(&s, 1);
            Ok(())
        };
        let start = || {
            // SAFETY: our own handle, no arguments.
            if unsafe { StartServiceW(s.0, 0, std::ptr::null()) } == 0 {
                return Err(format!("{name} didn't start"));
            }
            wait_for(&s, 4);
            Ok(())
        };
        match action {
            "start" => start(),
            "stop" => stop(),
            "restart" => {
                stop()?;
                start()
            }
            _ => Err("unknown action".into()),
        }
    }

    /// Changes when a service starts (needs admin rights).
    pub fn set_start(name: &str, start: &str) -> Result<(), String> {
        let m = manager(SC_MANAGER_CONNECT)?;
        let s = service(&m, name, SERVICE_CHANGE_CONFIG)?;
        let kind = match start {
            "auto" | "delayed" => SERVICE_AUTO_START,
            "manual" => SERVICE_DEMAND_START,
            "disabled" => SERVICE_DISABLED,
            _ => return Err("unknown startup type".into()),
        };
        // SAFETY: only the start type changes.
        let ok = unsafe {
            ChangeServiceConfigW(
                s.0,
                SERVICE_NO_CHANGE,
                kind,
                SERVICE_NO_CHANGE,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0;
        if !ok {
            return Err(format!("Windows didn't allow changing {name}"));
        }
        if kind == SERVICE_AUTO_START {
            let info = SERVICE_DELAYED_AUTO_START_INFO {
                fDelayedAutostart: (start == "delayed") as i32,
            };
            // SAFETY: a struct of the right type.
            unsafe {
                ChangeServiceConfig2W(
                    s.0,
                    SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                    (&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast(),
                )
            };
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Service;
    pub fn list() -> Vec<Service> {
        Vec::new()
    }
    pub fn control(_name: &str, _action: &str) -> Result<(), String> {
        Err("services are a Windows feature".into())
    }
    pub fn set_start(_name: &str, _start: &str) -> Result<(), String> {
        Err("services are a Windows feature".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_states_and_start_types() {
        assert_eq!(state_name(4), "running");
        assert_eq!(state_name(1), "stopped");
        assert_eq!(start_name(2, true), "delayed");
        assert_eq!(start_name(2, false), "auto");
        assert_eq!(start_name(4, false), "disabled");
    }
}
