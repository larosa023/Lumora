//! Windows readers for [`super::Sampler`], and small helpers the other
//! monitor files share (wide strings, the registry, performance counters).

use std::collections::HashMap;
use std::time::Instant;

use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessorPerformanceInformation,
};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows_sys::Win32::System::Power::{
    CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows_sys::Win32::System::SystemInformation::{
    GetActiveProcessorCount, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

use super::{busy, Cpu, DiskRate, Memory, NetRate};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A NUL-terminated (or full) UTF-16 buffer as a string.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// A NUL-terminated UTF-16 pointer as a string.
///
/// # Safety
/// `p` is null or points at a NUL-terminated UTF-16 string.
pub unsafe fn from_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: the caller promises a terminator.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` characters were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// A string value from the registry.
pub fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (path, name) = (wide(path), wide(name));
    let mut buf = vec![0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer is as long as we say.
    let r = unsafe {
        RegGetValueW(
            root,
            path.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    (r == ERROR_SUCCESS)
        .then(|| from_wide(&buf).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn reg_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let (path, name) = (wide(path), wide(name));
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: a DWORD-sized buffer.
    let r = unsafe {
        RegGetValueW(
            root,
            path.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut v as *mut u32).cast(),
            &mut len,
        )
    };
    (r == ERROR_SUCCESS).then_some(v)
}

/// A Windows performance-counter query (PDH).
pub struct Pdh {
    query: PDH_HQUERY,
    counters: Vec<PDH_HCOUNTER>,
}

// SAFETY: PDH handles may be used from any thread; each `Pdh` is only used
// from one at a time (it lives inside a sampler).
unsafe impl Send for Pdh {}

impl Pdh {
    pub fn open() -> Option<Pdh> {
        let mut query: PDH_HQUERY = std::ptr::null_mut();
        // SAFETY: a fresh query handle.
        (unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } == 0).then_some(Pdh {
            query,
            counters: Vec::new(),
        })
    }

    /// Adds a counter (English path, wildcards allowed); its index.
    pub fn add(&mut self, path: &str) -> Option<usize> {
        let p = wide(path);
        let mut c: PDH_HCOUNTER = std::ptr::null_mut();
        // SAFETY: valid query and path.
        if unsafe { PdhAddEnglishCounterW(self.query, p.as_ptr(), 0, &mut c) } != 0 {
            return None;
        }
        self.counters.push(c);
        Some(self.counters.len() - 1)
    }

    pub fn collect(&mut self) -> bool {
        // SAFETY: valid query.
        unsafe { PdhCollectQueryData(self.query) == 0 }
    }

    /// One counter's value (`format` extra flags, e.g. no 100 % cap).
    pub fn value(&self, i: usize, extra: u32) -> Option<f64> {
        let c = *self.counters.get(i)?;
        // SAFETY: zeroed plain data, filled by PDH.
        let mut v: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
        // SAFETY: valid counter.
        let r = unsafe {
            PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE | extra, std::ptr::null_mut(), &mut v)
        };
        // SAFETY: a double was asked for.
        (r == 0 && v.CStatus <= 1).then(|| unsafe { v.Anonymous.doubleValue })
    }

    /// A wildcard counter's values, by instance name.
    pub fn array(&self, i: usize) -> Vec<(String, f64)> {
        let Some(&c) = self.counters.get(i) else {
            return Vec::new();
        };
        let mut bytes = 0u32;
        let mut count = 0u32;
        // SAFETY: asking for the size first.
        let r = unsafe {
            PdhGetFormattedCounterArrayW(
                c,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if r != PDH_MORE_DATA || bytes == 0 {
            return Vec::new();
        }
        // u64s keep the buffer aligned for the items it holds.
        let mut buf = vec![0u64; (bytes as usize).div_ceil(8) + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        // SAFETY: a buffer of the size PDH asked for.
        let r = unsafe {
            PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut bytes, &mut count, items)
        };
        if r != 0 {
            return Vec::new();
        }
        (0..count as usize)
            .filter_map(|k| {
                // SAFETY: PDH wrote `count` items, names pointing into the buffer.
                let it = unsafe { &*items.add(k) };
                (it.FmtValue.CStatus <= 1).then(|| {
                    // SAFETY: as above; a double was asked for.
                    (unsafe { from_ptr(it.szName) }, unsafe {
                        it.FmtValue.Anonymous.doubleValue
                    })
                })
            })
            .collect()
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        // SAFETY: our own query.
        unsafe { PdhCloseQuery(self.query) };
    }
}

const PDH_FMT_NOCAP100: u32 = 0x8000;

pub struct Sampler {
    cores: Vec<(u64, u64)>,
    name: Option<String>,
    base_mhz: Option<u32>,
    pdh: Option<Pdh>,
    perf: Option<usize>,
    disk: Option<(usize, usize, usize)>,
    net: HashMap<u64, (u64, u64)>,
    net_at: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Sampler {
            cores: Vec::new(),
            name: None,
            base_mhz: None,
            pdh: None,
            perf: None,
            disk: None,
            net: HashMap::new(),
            net_at: None,
        }
    }

    fn pdh(&mut self) -> Option<&mut Pdh> {
        if self.pdh.is_none() {
            let mut p = Pdh::open()?;
            self.perf = p.add("\\Processor Information(_Total)\\% Processor Performance");
            self.pdh = Some(p);
        }
        self.pdh.as_mut()
    }

    pub fn cpu(&mut self, out: &mut Cpu) {
        if self.name.is_none() {
            self.name = reg_string(
                windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE,
                "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
                "ProcessorNameString",
            )
            .or(Some("Processor".into()));
            self.base_mhz = base_mhz();
        }
        out.name = self.name.clone().unwrap_or_default();
        out.base_mhz = self.base_mhz;

        // Busy per core from the kernel's own idle/kernel/user times.
        // SAFETY: plain query for the number of active logical processors.
        let n = unsafe { GetActiveProcessorCount(0xFFFF) }.clamp(1, 512) as usize;
        // SAFETY: zeroed plain data, filled by the kernel.
        let mut buf: Vec<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION> =
            vec![unsafe { std::mem::zeroed() }; n];
        let mut got = 0u32;
        // SAFETY: a buffer of `n` entries.
        let r = unsafe {
            NtQuerySystemInformation(
                SystemProcessorPerformanceInformation,
                buf.as_mut_ptr().cast(),
                (n * std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>()) as u32,
                &mut got,
            )
        };
        if r >= 0 {
            let k = (got as usize
                / std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>())
            .min(n);
            let now: Vec<(u64, u64)> = buf[..k]
                .iter()
                .map(|c| (c.IdleTime as u64, (c.KernelTime + c.UserTime) as u64))
                .collect();
            if self.cores.len() == now.len() {
                out.cores = self
                    .cores
                    .iter()
                    .zip(&now)
                    .map(|(a, b)| busy(*a, *b))
                    .collect();
                let (pi, pt) = self
                    .cores
                    .iter()
                    .fold((0, 0), |s, c| (s.0 + c.0, s.1 + c.1));
                let (ni, nt) = now.iter().fold((0, 0), |s, c| (s.0 + c.0, s.1 + c.1));
                out.usage = busy((pi, pt), (ni, nt));
            }
            self.cores = now;
        }

        // SAFETY: zeroed plain data, sized as passed.
        let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            K32GetPerformanceInfo(
                &mut pi,
                std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
            )
        } != 0
        {
            out.processes = pi.ProcessCount;
            out.threads = pi.ThreadCount;
            out.handles = pi.HandleCount;
        }
        // SAFETY: no arguments.
        out.uptime_s = unsafe { GetTickCount64() } / 1000;

        let base = self.base_mhz;
        let perf_idx = self.perf;
        if let Some(p) = self.pdh() {
            p.collect();
            if let (Some(i), Some(b)) = (perf_idx, base) {
                out.clock_mhz = p
                    .value(i, PDH_FMT_NOCAP100)
                    .map(|pct| (b as f64 * pct / 100.0).round() as u32)
                    .filter(|m| *m > 0);
            }
        }
    }

    pub fn memory(&mut self, out: &mut Memory) {
        // SAFETY: zeroed plain data with its length set.
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
            out.total = m.ullTotalPhys;
            out.available = m.ullAvailPhys;
            out.used = m.ullTotalPhys.saturating_sub(m.ullAvailPhys);
            out.percent = if m.ullTotalPhys > 0 {
                (out.used as f64 / m.ullTotalPhys as f64 * 100.0) as f32
            } else {
                0.0
            };
        }
        // SAFETY: as above.
        let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            K32GetPerformanceInfo(
                &mut pi,
                std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
            )
        } != 0
        {
            let page = pi.PageSize as u64;
            out.committed = pi.CommitTotal as u64 * page;
            out.commit_limit = pi.CommitLimit as u64 * page;
            out.cached = pi.SystemCache as u64 * page;
        }
    }

    pub fn network(&mut self) -> Vec<NetRate> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: the API allocates the table; freed below.
        if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
            return Vec::new();
        }
        let now = Instant::now();
        let secs = self
            .net_at
            .map(|t| now.duration_since(t).as_secs_f64())
            .filter(|s| *s > 0.05);
        let mut out = Vec::new();
        let mut seen = HashMap::new();
        // SAFETY: `NumEntries` rows follow the header.
        let rows = unsafe {
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        };
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            let hardware = flags & 0x01 != 0;
            let filter = flags & 0x02 != 0;
            // Physical adapters that are up (not filters, tunnels or loopback).
            if !hardware || filter || r.OperStatus != 1 {
                continue;
            }
            // SAFETY: the LUID is a plain 64-bit value.
            let key = unsafe { r.InterfaceLuid.Value };
            seen.insert(key, (r.InOctets, r.OutOctets));
            let (down, up) = match (self.net.get(&key), secs) {
                (Some(&(pi, po)), Some(s)) => (
                    r.InOctets.saturating_sub(pi) as f64 / s,
                    r.OutOctets.saturating_sub(po) as f64 / s,
                ),
                _ => (0.0, 0.0),
            };
            out.push(NetRate {
                name: from_wide(&r.Alias),
                down,
                up,
                link: r.ReceiveLinkSpeed.max(r.TransmitLinkSpeed),
            });
        }
        // SAFETY: the table from GetIfTable2.
        unsafe { FreeMibTable(table as *const _) };
        self.net = seen;
        self.net_at = Some(now);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn disks(&mut self) -> Vec<DiskRate> {
        if self.disk.is_none() {
            let Some(p) = self.pdh() else {
                return Vec::new();
            };
            let idle = p.add("\\PhysicalDisk(*)\\% Idle Time");
            let read = p.add("\\PhysicalDisk(*)\\Disk Read Bytes/sec");
            let write = p.add("\\PhysicalDisk(*)\\Disk Write Bytes/sec");
            if let (Some(a), Some(b), Some(c)) = (idle, read, write) {
                self.disk = Some((a, b, c));
                p.collect();
            }
        }
        let (Some((ia, ib, ic)), Some(p)) = (self.disk, self.pdh.as_ref()) else {
            return Vec::new();
        };
        // `cpu()` collected the query this round already.
        let reads: HashMap<String, f64> = p.array(ib).into_iter().collect();
        let writes: HashMap<String, f64> = p.array(ic).into_iter().collect();
        let mut out: Vec<DiskRate> = p
            .array(ia)
            .into_iter()
            .filter(|(n, _)| n != "_Total")
            .map(|(n, idle)| DiskRate {
                active: (100.0 - idle).clamp(0.0, 100.0) as f32,
                read: reads.get(&n).copied().unwrap_or(0.0),
                write: writes.get(&n).copied().unwrap_or(0.0),
                name: n,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

fn base_mhz() -> Option<u32> {
    // SAFETY: plain query for the logical processor count.
    let n = unsafe { GetActiveProcessorCount(0xFFFF) }.clamp(1, 512) as usize;
    // SAFETY: zeroed plain data, filled by the power API.
    let mut info: Vec<PROCESSOR_POWER_INFORMATION> = vec![unsafe { std::mem::zeroed() }; n];
    let bytes = (n * std::mem::size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    // SAFETY: a buffer of `n` entries.
    let r = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            std::ptr::null(),
            0,
            info.as_mut_ptr().cast(),
            bytes,
        )
    };
    (r == 0).then(|| info[0].MaxMhz).filter(|m| *m > 0)
}
