//! Apps that start with Windows, managed exactly as Task Manager does: the
//! `Run` registry keys and Startup folders list them, and the matching
//! `StartupApproved` value turns each on or off (the app itself stays put).

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StartupItem {
    /// Location and name, used to switch it on or off.
    pub id: String,
    pub name: String,
    /// What the app calls itself (its file description), when known.
    pub description: String,
    pub command: String,
    pub path: Option<String>,
    /// "Registry (you)", "Registry (all users)", "Startup folder", …
    pub location: String,
    pub enabled: bool,
    /// Changing it needs admin rights (Lumora's helper does it).
    pub admin: bool,
}

/// The program a startup command runs: the quoted path, or the text up to
/// the first ".exe".
pub fn exe_of(command: &str) -> Option<String> {
    let c = command.trim();
    if let Some(rest) = c.strip_prefix('"') {
        return rest
            .split('"')
            .next()
            .map(str::to_string)
            .filter(|s| !s.is_empty());
    }
    let lower = c.to_ascii_lowercase();
    lower
        .find(".exe")
        .map(|i| c[..i + 4].to_string())
        .or_else(|| c.split_whitespace().next().map(str::to_string))
}

/// Task Manager's on/off flag: the first byte of the approved value, even
/// for on (missing counts as on).
pub fn approved_on(data: Option<&[u8]>) -> bool {
    data.and_then(|d| d.first()).is_none_or(|b| b & 1 == 0)
}

/// The approved value that switches an item: on = 02, off = 03, followed by
/// eleven zero bytes.
pub fn approved_value(on: bool) -> [u8; 12] {
    let mut v = [0u8; 12];
    v[0] = if on { 0x02 } else { 0x03 };
    v
}

pub use imp::{list, set_enabled};

#[cfg(windows)]
mod imp {
    use super::{approved_on, approved_value, exe_of, StartupItem};
    use crate::monitor::win::{from_wide, wide};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ,
        KEY_SET_VALUE, KEY_WOW64_64KEY, REG_BINARY, REG_OPTION_NON_VOLATILE,
    };

    const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RUN32: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run";
    const APPROVED: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\";

    struct Loc {
        key: &'static str,
        label: &'static str,
        root: HKEY,
        approved: &'static str,
        admin: bool,
    }

    fn locations() -> [Loc; 5] {
        [
            Loc {
                key: "hkcu",
                label: "Registry (you)",
                root: HKEY_CURRENT_USER,
                approved: "Run",
                admin: false,
            },
            Loc {
                key: "hklm",
                label: "Registry (all users)",
                root: HKEY_LOCAL_MACHINE,
                approved: "Run",
                admin: true,
            },
            Loc {
                key: "hklm32",
                label: "Registry (all users, 32-bit)",
                root: HKEY_LOCAL_MACHINE,
                approved: "Run32",
                admin: true,
            },
            Loc {
                key: "folder",
                label: "Startup folder (you)",
                root: HKEY_CURRENT_USER,
                approved: "StartupFolder",
                admin: false,
            },
            Loc {
                key: "commonfolder",
                label: "Startup folder (all users)",
                root: HKEY_LOCAL_MACHINE,
                approved: "StartupFolder",
                admin: true,
            },
        ]
    }

    fn open(root: HKEY, path: &str, access: u32) -> Option<HKEY> {
        let p = wide(path);
        let mut k: HKEY = std::ptr::null_mut();
        // SAFETY: valid root and path; closed by the caller.
        (unsafe { RegOpenKeyExW(root, p.as_ptr(), 0, access | KEY_WOW64_64KEY, &mut k) }
            == ERROR_SUCCESS)
            .then_some(k)
    }

    /// Every (name, data) of a key's values.
    fn values(root: HKEY, path: &str) -> Vec<(String, Vec<u8>)> {
        let Some(k) = open(root, path, KEY_READ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0.. {
            let mut name = vec![0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; 4096];
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            // SAFETY: buffers as long as we say.
            let r = unsafe {
                RegEnumValueW(
                    k,
                    i,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            data.truncate(data_len as usize);
            out.push((String::from_utf16_lossy(&name[..name_len as usize]), data));
        }
        // SAFETY: our own key.
        unsafe { RegCloseKey(k) };
        out
    }

    fn approved(root: HKEY, sub: &str, name: &str) -> Option<Vec<u8>> {
        let k = open(root, &format!("{APPROVED}{sub}"), KEY_QUERY_VALUE)?;
        let n = wide(name);
        let mut data = vec![0u8; 64];
        let mut len = data.len() as u32;
        // SAFETY: a buffer as long as we say.
        let r = unsafe {
            RegQueryValueExW(
                k,
                n.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                data.as_mut_ptr(),
                &mut len,
            )
        };
        unsafe { RegCloseKey(k) };
        (r == ERROR_SUCCESS).then(|| {
            data.truncate(len as usize);
            data
        })
    }

    fn text(data: &[u8]) -> String {
        let w: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        from_wide(&w)
    }

    fn folder(loc: &str) -> Option<std::path::PathBuf> {
        let base = if loc == "folder" {
            std::env::var("APPDATA").ok()?
        } else {
            std::env::var("ProgramData").ok()?
        };
        Some(std::path::Path::new(&base).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup"))
    }

    pub fn list() -> Vec<StartupItem> {
        let mut out = Vec::new();
        for loc in locations() {
            let entries: Vec<(String, String)> = match loc.key {
                "folder" | "commonfolder" => folder(loc.key)
                    .and_then(|d| std::fs::read_dir(d).ok())
                    .map(|it| {
                        it.flatten()
                            .filter(|e| {
                                !e.file_name()
                                    .to_string_lossy()
                                    .eq_ignore_ascii_case("desktop.ini")
                            })
                            .map(|e| {
                                (
                                    e.file_name().to_string_lossy().to_string(),
                                    e.path().to_string_lossy().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                "hklm32" => values(loc.root, RUN32)
                    .into_iter()
                    .map(|(n, d)| (n, text(&d)))
                    .collect(),
                _ => values(loc.root, RUN)
                    .into_iter()
                    .map(|(n, d)| (n, text(&d)))
                    .collect(),
            };
            for (name, command) in entries {
                let path = if loc.key.ends_with("folder") {
                    Some(command.clone())
                } else {
                    exe_of(&command)
                };
                let description = path
                    .as_deref()
                    .filter(|p| p.to_ascii_lowercase().ends_with(".exe"))
                    .and_then(crate::monitor::procs_win::file_description)
                    .unwrap_or_default();
                let on = approved_on(approved(loc.root, loc.approved, &name).as_deref());
                out.push(StartupItem {
                    id: format!("{}|{}", loc.key, name),
                    name: name.trim_end_matches(".lnk").to_string(),
                    description,
                    command,
                    path,
                    location: loc.label.into(),
                    enabled: on,
                    admin: loc.admin,
                });
            }
        }
        out.sort_by_key(|i| i.name.to_lowercase());
        out
    }

    /// Switches an item. `Err("admin")` when it needs Lumora's helper.
    pub fn set_enabled(id: &str, on: bool) -> Result<(), String> {
        let (key, name) = id.split_once('|').ok_or("unknown startup item")?;
        let loc = locations()
            .into_iter()
            .find(|l| l.key == key)
            .ok_or("unknown startup item")?;
        let path = wide(&format!("{APPROVED}{}", loc.approved));
        let mut k: HKEY = std::ptr::null_mut();
        // SAFETY: valid root and path; closed below.
        let r = unsafe {
            RegCreateKeyExW(
                loc.root,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut k,
                std::ptr::null_mut(),
            )
        };
        if r == ERROR_ACCESS_DENIED {
            return Err("admin".into());
        }
        if r != ERROR_SUCCESS {
            return Err(format!("Windows didn't allow it (error {r})"));
        }
        let n = wide(name);
        let v = approved_value(on);
        // SAFETY: a 12-byte value on our own key.
        let r = unsafe { RegSetValueExW(k, n.as_ptr(), 0, REG_BINARY, v.as_ptr(), v.len() as u32) };
        unsafe { RegCloseKey(k) };
        match r {
            ERROR_SUCCESS => Ok(()),
            ERROR_ACCESS_DENIED => Err("admin".into()),
            e => Err(format!("Windows didn't allow it (error {e})")),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::StartupItem;
    pub fn list() -> Vec<StartupItem> {
        Vec::new()
    }
    pub fn set_enabled(_id: &str, _on: bool) -> Result<(), String> {
        Err("startup apps are a Windows feature".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_program_in_a_command() {
        assert_eq!(
            exe_of("\"C:\\Program Files\\App\\app.exe\" --tray").as_deref(),
            Some("C:\\Program Files\\App\\app.exe")
        );
        assert_eq!(
            exe_of("C:\\Tools\\thing.exe -m").as_deref(),
            Some("C:\\Tools\\thing.exe")
        );
        assert_eq!(
            exe_of("C:\\Program Files\\x\\y.EXE /s").as_deref(),
            Some("C:\\Program Files\\x\\y.EXE")
        );
    }

    #[test]
    fn approved_flags_match_task_manager() {
        assert!(approved_on(None));
        assert!(approved_on(Some(&[0x02, 0, 0])));
        assert!(approved_on(Some(&[0x06])));
        assert!(!approved_on(Some(&[0x03, 1, 2])));
        assert!(!approved_on(Some(&[0x07])));
        assert_eq!(approved_value(false)[0], 3);
        assert_eq!(approved_value(true)[0], 2);
    }
}
