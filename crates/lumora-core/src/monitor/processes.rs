//! The process list, like Task Manager's: every process from one kernel
//! query, with CPU, memory, disk and GPU use, grouped into apps, background
//! processes and Windows processes, plus the actions on them.

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent: u32,
    /// File name, e.g. "chrome.exe".
    pub name: String,
    /// The app's own name for itself, e.g. "Google Chrome".
    pub description: String,
    pub path: Option<String>,
    /// "app", "background" or "windows".
    pub group: &'static str,
    /// Percent of the whole CPU.
    pub cpu: f32,
    /// Private working set, bytes (what Task Manager calls Memory).
    pub memory: u64,
    /// Bytes per second read and written.
    pub disk: f64,
    /// Percent of the busiest GPU engine.
    pub gpu: f32,
    pub threads: u32,
    /// "running", "suspended" or "efficiency".
    pub status: &'static str,
}

/// One process as the kernel lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Raw {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub create_time: i64,
    /// Kernel + user time, 100 ns units.
    pub cpu_time: u64,
    pub private_ws: u64,
    /// Bytes read + written so far.
    pub io: u64,
    pub threads: u32,
    pub suspended: bool,
    pub base_priority: i32,
}

fn rd<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at + N)?.try_into().ok()
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    rd::<4>(b, at).map(u32::from_le_bytes)
}
fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    rd::<4>(b, at).map(i32::from_le_bytes)
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    rd::<8>(b, at).map(u64::from_le_bytes)
}

/// Size of a 64-bit `SYSTEM_PROCESS_INFORMATION` before its thread list.
pub const ENTRY: usize = 0x100;
/// Size of one 64-bit `SYSTEM_THREAD_INFORMATION`.
pub const THREAD: usize = 0x50;

/// Reads the kernel's process list (`SystemProcessInformation`, 64-bit
/// layout). `base` is the address the buffer lives at, so name pointers can
/// be turned back into offsets.
pub fn parse(buf: &[u8], base: usize) -> Vec<Raw> {
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let Some(next) = u32_at(buf, at) else { break };
        let threads = u32_at(buf, at + 0x04).unwrap_or(0);
        let name_len = rd::<2>(buf, at + 0x38).map(u16::from_le_bytes).unwrap_or(0) as usize;
        let name_ptr = u64_at(buf, at + 0x40).unwrap_or(0) as usize;
        let pid = u64_at(buf, at + 0x50).unwrap_or(0) as u32;
        let name = if name_ptr >= base && name_len > 0 {
            let off = name_ptr - base;
            buf.get(off..off + name_len)
                .map(|b| {
                    let w: Vec<u16> = b
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16_lossy(&w)
                })
                .unwrap_or_default()
        } else if pid == 0 {
            "System Idle Process".into()
        } else {
            String::new()
        };
        // A process is suspended when every thread waits for that reason.
        let mut suspended = threads > 0;
        for t in 0..threads as usize {
            let ta = at + ENTRY + t * THREAD;
            let state = u32_at(buf, ta + 0x44).unwrap_or(0);
            let reason = u32_at(buf, ta + 0x48).unwrap_or(0);
            if !(state == 5 && reason == 5) {
                suspended = false;
                break;
            }
        }
        let read = u64_at(buf, at + 0xE8).unwrap_or(0);
        let write = u64_at(buf, at + 0xF0).unwrap_or(0);
        out.push(Raw {
            pid,
            parent: u64_at(buf, at + 0x58).unwrap_or(0) as u32,
            name,
            create_time: u64_at(buf, at + 0x20).unwrap_or(0) as i64,
            cpu_time: u64_at(buf, at + 0x28).unwrap_or(0) + u64_at(buf, at + 0x30).unwrap_or(0),
            private_ws: u64_at(buf, at + 0x08).unwrap_or(0),
            io: read + write,
            threads,
            suspended,
            base_priority: i32_at(buf, at + 0x48).unwrap_or(8),
        });
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    out
}

/// Processes that belong to Windows itself when their path can't be read.
const SYSTEM_NAMES: &[&str] = &[
    "System Idle Process",
    "System",
    "Registry",
    "Secure System",
    "Memory Compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
    "winlogon.exe",
    "fontdrvhost.exe",
    "dwm.exe",
    "LsaIso.exe",
];

/// Which section a process goes in.
pub fn group_of(
    raw: &Raw,
    path: Option<&str>,
    has_window: bool,
    windows_dir: &str,
) -> &'static str {
    if has_window {
        return "app";
    }
    let in_windows = path.is_some_and(|p| {
        p.len() >= windows_dir.len() && p[..windows_dir.len()].eq_ignore_ascii_case(windows_dir)
    });
    if in_windows || raw.pid <= 4 || (path.is_none() && SYSTEM_NAMES.contains(&raw.name.as_str())) {
        "windows"
    } else {
        "background"
    }
}

/// The process ids under `root` (itself first), for "end process tree".
pub fn tree(rows: &[(u32, u32)], root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let p = out[i];
        for &(pid, parent) in rows {
            if parent == p && pid != p && !out.contains(&pid) {
                out.push(pid);
            }
        }
        i += 1;
    }
    out
}

pub use super::imp_processes::{
    end, open_location, properties, set_efficiency, set_priority, ProcessSampler,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a fake two-process buffer in the 64-bit kernel layout.
    fn fake(base: usize) -> Vec<u8> {
        let name = "app.exe"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect::<Vec<u8>>();
        let first = ENTRY + 2 * THREAD;
        let mut b = vec![0u8; first + ENTRY + 64];
        let put = |b: &mut Vec<u8>, at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
        // Process 1: two suspended threads, a name stored after process 2.
        put(&mut b, 0, &(first as u32).to_le_bytes());
        put(&mut b, 0x04, &2u32.to_le_bytes());
        put(&mut b, 0x08, &(4096u64 * 10).to_le_bytes());
        put(&mut b, 0x28, &100u64.to_le_bytes());
        put(&mut b, 0x30, &50u64.to_le_bytes());
        put(&mut b, 0x38, &(name.len() as u16).to_le_bytes());
        let name_at = first + ENTRY;
        put(&mut b, 0x40, &((base + name_at) as u64).to_le_bytes());
        put(&mut b, 0x48, &8i32.to_le_bytes());
        put(&mut b, 0x50, &1234u64.to_le_bytes());
        put(&mut b, 0x58, &4u64.to_le_bytes());
        put(&mut b, 0xE8, &1000u64.to_le_bytes());
        put(&mut b, 0xF0, &24u64.to_le_bytes());
        for t in 0..2 {
            let ta = ENTRY + t * THREAD;
            put(&mut b, ta + 0x44, &5u32.to_le_bytes());
            put(&mut b, ta + 0x48, &5u32.to_le_bytes());
        }
        // Process 2: the last entry, idle.
        put(&mut b, first + 0x50, &0u64.to_le_bytes());
        put(&mut b, name_at, &name);
        b
    }

    #[test]
    fn parses_the_kernel_list() {
        let base = 0x1000_0000;
        let rows = parse(&fake(base), base);
        assert_eq!(rows.len(), 2);
        let a = &rows[0];
        assert_eq!((a.pid, a.parent, a.name.as_str()), (1234, 4, "app.exe"));
        assert_eq!(a.cpu_time, 150);
        assert_eq!(a.io, 1024);
        assert_eq!(a.private_ws, 40960);
        assert!(a.suspended);
        assert_eq!(rows[1].name, "System Idle Process");
    }

    #[test]
    fn groups_like_task_manager() {
        let r = Raw {
            pid: 900,
            name: "svchost.exe".into(),
            ..Default::default()
        };
        assert_eq!(
            group_of(
                &r,
                Some("C:\\Windows\\System32\\svchost.exe"),
                false,
                "C:\\Windows"
            ),
            "windows"
        );
        assert_eq!(group_of(&r, None, false, "C:\\Windows"), "windows");
        let r = Raw {
            pid: 901,
            name: "steam.exe".into(),
            ..Default::default()
        };
        assert_eq!(
            group_of(
                &r,
                Some("C:\\Program Files\\Steam\\steam.exe"),
                false,
                "C:\\Windows"
            ),
            "background"
        );
        assert_eq!(
            group_of(
                &r,
                Some("C:\\Program Files\\Steam\\steam.exe"),
                true,
                "C:\\Windows"
            ),
            "app"
        );
    }

    #[test]
    fn trees_include_grandchildren() {
        let rows = [(10, 1), (11, 10), (12, 11), (13, 1), (14, 10)];
        assert_eq!(tree(&rows, 10), vec![10, 11, 14, 12]);
    }
}
