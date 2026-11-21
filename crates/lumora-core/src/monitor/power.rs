//! Windows power plans: list them, see the active one, switch.

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PowerPlan {
    /// The plan's GUID, as text.
    pub id: String,
    pub name: String,
    pub active: bool,
}

pub use imp::{plans, set_plan};

#[cfg(windows)]
mod imp {
    use super::PowerPlan;
    use crate::monitor::win::from_wide;
    use windows_sys::core::GUID;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::System::Power::{
        PowerEnumerate, PowerGetActiveScheme, PowerReadFriendlyName, PowerSetActiveScheme,
        ACCESS_SCHEME,
    };

    fn text(g: &GUID) -> String {
        format!(
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g.data1,
            g.data2,
            g.data3,
            g.data4[0],
            g.data4[1],
            g.data4[2],
            g.data4[3],
            g.data4[4],
            g.data4[5],
            g.data4[6],
            g.data4[7]
        )
    }

    fn parse(s: &str) -> Option<GUID> {
        let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if h.len() != 32 {
            return None;
        }
        let n = |a: usize, b: usize| u64::from_str_radix(&h[a..b], 16).ok();
        let mut d4 = [0u8; 8];
        for (i, b) in d4.iter_mut().enumerate() {
            *b = n(16 + i * 2, 18 + i * 2)? as u8;
        }
        Some(GUID {
            data1: n(0, 8)? as u32,
            data2: n(8, 12)? as u16,
            data3: n(12, 16)? as u16,
            data4: d4,
        })
    }

    fn active() -> Option<String> {
        let mut p: *mut GUID = std::ptr::null_mut();
        // SAFETY: Windows allocates the GUID; freed below.
        if unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut p) } != ERROR_SUCCESS
            || p.is_null()
        {
            return None;
        }
        // SAFETY: a valid GUID from the call above.
        let t = text(unsafe { &*p });
        unsafe { LocalFree(p as _) };
        Some(t)
    }

    fn name(g: &GUID) -> String {
        let mut buf = vec![0u16; 256];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: a buffer as long as we say.
        let r = unsafe {
            PowerReadFriendlyName(
                std::ptr::null_mut(),
                g,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r == ERROR_SUCCESS {
            from_wide(&buf)
        } else {
            "Power plan".into()
        }
    }

    pub fn plans() -> Vec<PowerPlan> {
        let current = active();
        let mut out = Vec::new();
        for i in 0..32u32 {
            let mut g: GUID = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<GUID>() as u32;
            // SAFETY: a GUID-sized buffer.
            let r = unsafe {
                PowerEnumerate(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    ACCESS_SCHEME,
                    i,
                    (&mut g as *mut GUID).cast(),
                    &mut len,
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            let id = text(&g);
            out.push(PowerPlan {
                active: current.as_deref() == Some(id.as_str()),
                name: name(&g),
                id,
            });
        }
        out
    }

    pub fn set_plan(id: &str) -> Result<(), String> {
        let g = parse(id).ok_or("unknown power plan")?;
        // SAFETY: a valid GUID.
        match unsafe { PowerSetActiveScheme(std::ptr::null_mut(), &g) } {
            ERROR_SUCCESS => Ok(()),
            e => Err(format!("Windows didn't switch the power plan (error {e})")),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PowerPlan;
    pub fn plans() -> Vec<PowerPlan> {
        Vec::new()
    }
    pub fn set_plan(_id: &str) -> Result<(), String> {
        Err("power plans are a Windows feature".into())
    }
}
