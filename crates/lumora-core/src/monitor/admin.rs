//! Jobs that need admin rights, done by Lumora's helper over its signed-in
//! connection (see [`crate::cooling::relay`]):
//!
//! ```text
//! → svc <start|stop|restart> <name>     ← ok | err <why>
//! → svcstart <name> <auto|delayed|manual|disabled>
//! → kill <pid>
//! → startup <0|1> <id>
//! → purge                               standby memory → free
//! → disks                               ← ok <JSON list of drive health>
//! ```

use std::io;

/// Handles one admin command in the helper; `None` when it isn't one.
pub fn serve_command(cmd: &str, arg: &str) -> Option<String> {
    let r: Result<String, String> = match cmd {
        "svc" => match arg.split_once(' ') {
            Some((action, name)) => {
                super::services::control(name.trim(), action).map(|_| String::new())
            }
            None => Err("expected an action and a service".into()),
        },
        "svcstart" => match arg.split_once(' ') {
            Some((name, start)) => {
                super::services::set_start(name, start.trim()).map(|_| String::new())
            }
            None => Err("expected a service and a startup type".into()),
        },
        "kill" => arg
            .trim()
            .parse::<u32>()
            .map_err(|_| "expected a process id".to_string())
            .and_then(|pid| {
                super::processes::end(pid).map_err(|_| "Windows didn't allow ending it".into())
            })
            .map(|_| String::new()),
        "startup" => match arg.split_once(' ') {
            Some((on, id)) => super::startup::set_enabled(id, on == "1").map(|_| String::new()),
            None => Err("expected on/off and an item".into()),
        },
        "purge" => purge_standby().map(|_| String::new()),
        "disks" => serde_json::to_string(&super::storage::disk_health()).map_err(|e| e.to_string()),
        _ => return None,
    };
    Some(match r {
        Ok(s) if s.is_empty() => "ok".into(),
        Ok(s) => format!("ok {s}"),
        Err(e) => format!("err {}", e.replace('\n', " ")),
    })
}

/// Asks Lumora's helper to do one admin job.
pub fn ask(relay_key: Option<&str>, line: &str) -> Result<String, String> {
    let key = relay_key.ok_or("Lumora's helper isn't set up")?;
    let mut c = crate::cooling::relay::Client::connect(crate::cooling::relay::PORT, key)
        .map_err(|_| "helper".to_string())?;
    c.command(line).map_err(|e: io::Error| {
        let t = e.to_string();
        if t.contains("unknown command") {
            "helper-old".to_string()
        } else {
            t
        }
    })
}

/// Clears Windows' standby list, like "Empty standby list" tools do.
#[cfg(windows)]
pub fn purge_standby() -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // The privilege the purge needs (admins have it, switched off).
    let name = super::win::wide("SeProfileSingleProcessPrivilege");
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: our own process token; closed below.
    unsafe {
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err("couldn't get admin rights".into());
        }
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
            CloseHandle(token);
            return Err("couldn't get admin rights".into());
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        CloseHandle(token);
    }
    type Set = unsafe extern "system" fn(i32, *const core::ffi::c_void, u32) -> i32;
    // SAFETY: ntdll is always loaded; the symbol has this signature.
    let lib = unsafe { libloading::Library::new("ntdll.dll") }.map_err(|e| e.to_string())?;
    let set: libloading::Symbol<Set> =
        unsafe { lib.get(b"NtSetSystemInformation\0") }.map_err(|e| e.to_string())?;
    // SystemMemoryListInformation = 80; MemoryPurgeStandbyList = 4.
    let cmd: u32 = 4;
    // SAFETY: a 4-byte command, as the call expects.
    let r = unsafe { set(80, (&cmd as *const u32).cast(), 4) };
    if r >= 0 {
        Ok(())
    } else {
        Err(format!("Windows refused (status {:#x})", r as u32))
    }
}

#[cfg(not(windows))]
pub fn purge_standby() -> Result<(), String> {
    Err("freeing memory is a Windows feature".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_admin_commands_are_taken() {
        assert!(serve_command("gpufan", "50").is_none());
        let r = serve_command("svc", "nonsense").unwrap();
        assert!(r.starts_with("err"));
        let r = serve_command("disks", "").unwrap();
        assert!(r.starts_with("ok"));
    }
}
