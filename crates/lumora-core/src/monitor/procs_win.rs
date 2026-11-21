//! Windows process sampling and actions (see [`super::processes`]).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessInformation,
};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::Threading::GetActiveProcessorCount;
use windows_sys::Win32::System::Threading::{
    GetProcessInformation, OpenProcess, ProcessPowerThrottling, QueryFullProcessImageNameW,
    SetPriorityClass, SetProcessInformation, TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS,
};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
    GW_OWNER, SW_SHOW,
};

use super::processes::{group_of, parse, ProcessRow, Raw};
use super::win::{from_wide, wide, Pdh};

struct Seen {
    cpu_time: u64,
    io: u64,
}

pub struct ProcessSampler {
    prev: HashMap<(u32, i64), Seen>,
    at: Option<Instant>,
    paths: HashMap<(u32, i64), Option<String>>,
    descriptions: HashMap<String, String>,
    buf: Vec<u64>,
    gpu: Option<(Pdh, usize)>,
    gpu_tried: bool,
    windows_dir: String,
    cpus: f64,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler {
    pub fn new() -> Self {
        ProcessSampler {
            prev: HashMap::new(),
            at: None,
            paths: HashMap::new(),
            descriptions: HashMap::new(),
            buf: vec![0u64; 128 * 1024],
            gpu: None,
            gpu_tried: false,
            windows_dir: std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
            // SAFETY: plain query.
            cpus: unsafe { GetActiveProcessorCount(0xFFFF) }.max(1) as f64,
        }
    }

    fn raw(&mut self) -> Vec<Raw> {
        loop {
            let bytes = (self.buf.len() * 8) as u32;
            let mut need = 0u32;
            // SAFETY: a buffer of `bytes` bytes.
            let r = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    self.buf.as_mut_ptr().cast(),
                    bytes,
                    &mut need,
                )
            };
            if r >= 0 {
                let base = self.buf.as_ptr() as usize;
                // SAFETY: the kernel filled `need` bytes (at most the buffer).
                let b = unsafe {
                    std::slice::from_raw_parts(
                        self.buf.as_ptr() as *const u8,
                        (need.min(bytes)) as usize,
                    )
                };
                return parse(b, base);
            }
            if self.buf.len() > 8 * 1024 * 1024 {
                return Vec::new();
            }
            let grow = (need as usize / 8 + 16 * 1024).max(self.buf.len() * 2);
            self.buf = vec![0u64; grow];
        }
    }

    fn gpu_by_pid(&mut self) -> HashMap<u32, f32> {
        if !self.gpu_tried {
            self.gpu_tried = true;
            if let Some(mut p) = Pdh::open() {
                if let Some(i) = p.add("\\GPU Engine(*)\\Utilization Percentage") {
                    p.collect();
                    self.gpu = Some((p, i));
                }
            }
        }
        let Some((p, i)) = self.gpu.as_mut() else {
            return HashMap::new();
        };
        p.collect();
        // Instances look like "pid_1234_luid_..._eng_0_engtype_3D": add up
        // each engine type per process, then take the busiest type.
        let mut per: HashMap<(u32, String), f64> = HashMap::new();
        for (name, v) in p.array(*i) {
            let mut parts = name.split('_');
            let (Some("pid"), Some(pid)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            let kind = name.rsplit("engtype_").next().unwrap_or("").to_string();
            *per.entry((pid, kind)).or_default() += v;
        }
        let mut out: HashMap<u32, f32> = HashMap::new();
        for ((pid, _), v) in per {
            let e = out.entry(pid).or_default();
            *e = e.max(v.min(100.0) as f32);
        }
        out
    }

    pub fn sample(&mut self) -> Vec<ProcessRow> {
        let now = Instant::now();
        let secs = self
            .at
            .map(|t| now.duration_since(t).as_secs_f64())
            .filter(|s| *s > 0.05);
        let raw = self.raw();
        let windowed = windowed_pids();
        let gpu = self.gpu_by_pid();
        let mut seen = HashMap::new();
        let mut keep = HashSet::new();
        let mut rows = Vec::with_capacity(raw.len());
        for r in raw {
            let key = (r.pid, r.create_time);
            keep.insert(key);
            let (cpu, disk) = match (self.prev.get(&key), secs) {
                (Some(p), Some(s)) => (
                    // 100 ns units over wall time, across every core.
                    (r.cpu_time.saturating_sub(p.cpu_time) as f64 / 1e7 / s / self.cpus * 100.0)
                        .clamp(0.0, 100.0) as f32,
                    r.io.saturating_sub(p.io) as f64 / s,
                ),
                _ => (0.0, 0.0),
            };
            seen.insert(
                key,
                Seen {
                    cpu_time: r.cpu_time,
                    io: r.io,
                },
            );
            if r.pid == 0 {
                continue; // Idle isn't a real process.
            }
            let path = self
                .paths
                .entry(key)
                .or_insert_with(|| image_path(r.pid))
                .clone();
            let description = match &path {
                Some(p) => self
                    .descriptions
                    .entry(p.clone())
                    .or_insert_with(|| file_description(p).unwrap_or_default())
                    .clone(),
                None => String::new(),
            };
            let status = if r.suspended {
                "suspended"
            } else if r.base_priority <= 4 && efficiency_on(r.pid) {
                "efficiency"
            } else {
                "running"
            };
            rows.push(ProcessRow {
                group: group_of(
                    &r,
                    path.as_deref(),
                    windowed.contains(&r.pid),
                    &self.windows_dir,
                ),
                pid: r.pid,
                parent: r.parent,
                description: if description.is_empty() {
                    r.name.clone()
                } else {
                    description
                },
                name: r.name,
                path,
                cpu,
                memory: r.private_ws,
                disk,
                gpu: gpu.get(&key.0).copied().unwrap_or(0.0),
                threads: r.threads,
                status,
            });
        }
        self.paths.retain(|k, _| keep.contains(k));
        self.prev = seen;
        self.at = Some(now);
        rows
    }
}

/// Processes with a visible, titled, unowned top-level window: "apps".
fn windowed_pids() -> HashSet<u32> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: called by EnumWindows with our set's address.
        let set = unsafe { &mut *(lparam as *mut HashSet<u32>) };
        // SAFETY: plain window queries.
        unsafe {
            if IsWindowVisible(hwnd) != 0
                && GetWindow(hwnd, GW_OWNER).is_null()
                && GetWindowTextLengthW(hwnd) > 0
            {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                set.insert(pid);
            }
        }
        1
    }
    let mut set = HashSet::new();
    // SAFETY: the callback only touches `set` during this call.
    unsafe { EnumWindows(Some(each), &mut set as *mut _ as LPARAM) };
    set
}

fn image_path(pid: u32) -> Option<String> {
    // SAFETY: limited query access; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer is as long as we say.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: our own handle.
    unsafe { CloseHandle(h) };
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

fn efficiency_on(pid: u32) -> bool {
    // SAFETY: limited query access; closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let mut st = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    // SAFETY: a struct of the size passed.
    let ok = unsafe {
        GetProcessInformation(
            h,
            ProcessPowerThrottling,
            (&mut st as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    } != 0;
    // SAFETY: our own handle.
    unsafe { CloseHandle(h) };
    ok && st.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
}

/// The "File description" from a program's version info.
pub fn file_description(path: &str) -> Option<String> {
    let w = wide(path);
    let mut handle = 0u32;
    // SAFETY: valid path.
    let size = unsafe { GetFileVersionInfoSizeW(w.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: a buffer of the size asked for.
    if unsafe { GetFileVersionInfoW(w.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let tr = wide("\\VarFileInfo\\Translation");
    // SAFETY: querying inside the block just read.
    let (lang, cp) =
        if unsafe { VerQueryValueW(data.as_ptr().cast(), tr.as_ptr(), &mut p, &mut len) } != 0
            && len >= 4
        {
            // SAFETY: at least two u16s.
            let v = unsafe { std::slice::from_raw_parts(p as *const u16, 2) };
            (v[0], v[1])
        } else {
            (0x0409, 0x04B0)
        };
    let key = wide(&format!(
        "\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"
    ));
    // SAFETY: as above.
    if unsafe { VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut p, &mut len) } == 0
        || len == 0
    {
        return None;
    }
    // SAFETY: `len` UTF-16 characters, including the terminator.
    let s = from_wide(unsafe { std::slice::from_raw_parts(p as *const u16, len as usize) });
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Ends a process. `Err("denied")` when it needs admin rights.
pub fn end(pid: u32) -> Result<(), String> {
    // SAFETY: terminate access; closed below.
    let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if h.is_null() {
        return Err("denied".into());
    }
    // SAFETY: our own handle.
    let ok = unsafe { TerminateProcess(h, 1) } != 0;
    unsafe { CloseHandle(h) };
    if ok {
        Ok(())
    } else {
        Err("denied".into())
    }
}

pub fn set_priority(pid: u32, level: &str) -> Result<(), String> {
    let class = match level {
        "idle" | "low" => IDLE_PRIORITY_CLASS,
        "below" => BELOW_NORMAL_PRIORITY_CLASS,
        "normal" => NORMAL_PRIORITY_CLASS,
        "above" => ABOVE_NORMAL_PRIORITY_CLASS,
        "high" => HIGH_PRIORITY_CLASS,
        "realtime" => REALTIME_PRIORITY_CLASS,
        _ => return Err("unknown priority".into()),
    };
    // SAFETY: set-information access; closed below.
    let h = unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err("Windows didn't allow changing this process".into());
    }
    // SAFETY: our own handle.
    let ok = unsafe { SetPriorityClass(h, class) } != 0;
    unsafe { CloseHandle(h) };
    ok.then_some(())
        .ok_or_else(|| "Windows didn't allow changing this process".into())
}

/// Efficiency mode, like Task Manager's: low priority plus power throttling.
pub fn set_efficiency(pid: u32, on: bool) -> Result<(), String> {
    // SAFETY: set-information access; closed below.
    let h = unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err("Windows didn't allow changing this process".into());
    }
    let st = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    // SAFETY: a struct of the size passed; our own handle.
    let ok = unsafe {
        SetProcessInformation(
            h,
            ProcessPowerThrottling,
            (&st as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0
            && SetPriorityClass(
                h,
                if on {
                    IDLE_PRIORITY_CLASS
                } else {
                    NORMAL_PRIORITY_CLASS
                },
            ) != 0
    };
    unsafe { CloseHandle(h) };
    ok.then_some(())
        .ok_or_else(|| "Windows didn't allow changing this process".into())
}

pub fn open_location(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{path}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The file's Properties window.
pub fn properties(path: &str) -> Result<(), String> {
    let (verb, file) = (wide("properties"), wide(path));
    // SAFETY: zeroed plain data, filled in below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_INVOKEIDLIST;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOW;
    // SAFETY: the strings outlive the call.
    (unsafe { ShellExecuteExW(&mut info) } != 0)
        .then_some(())
        .ok_or_else(|| "couldn't open its properties".into())
}
