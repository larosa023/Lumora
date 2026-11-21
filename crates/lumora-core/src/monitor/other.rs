//! Stand-in readers for systems other than Windows (Lumora's system pages
//! are Windows-only; this keeps the app and its tests building elsewhere).
//! Linux gets real CPU and memory numbers from /proc, so development there
//! sees something sensible.

use super::{busy, Cpu, DiskRate, Memory, NetRate};

pub struct Sampler {
    prev: Vec<(u64, u64)>,
}

impl Sampler {
    pub fn new() -> Self {
        Sampler { prev: Vec::new() }
    }

    pub fn cpu(&mut self, out: &mut Cpu) {
        out.name = "Processor".into();
        let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
            return;
        };
        let now: Vec<(u64, u64)> = stat
            .lines()
            .filter(|l| l.starts_with("cpu"))
            .map(|l| {
                let v: Vec<u64> = l
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|x| x.parse().ok())
                    .collect();
                let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
                (idle, v.iter().sum())
            })
            .collect();
        if self.prev.len() == now.len() && !now.is_empty() {
            out.usage = busy(self.prev[0], now[0]);
            out.cores = self.prev[1..]
                .iter()
                .zip(&now[1..])
                .map(|(a, b)| busy(*a, *b))
                .collect();
        }
        self.prev = now;
    }

    pub fn memory(&mut self, out: &mut Memory) {
        let Ok(info) = std::fs::read_to_string("/proc/meminfo") else {
            return;
        };
        let kb = |key: &str| {
            info.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024
        };
        out.total = kb("MemTotal:");
        out.available = kb("MemAvailable:");
        out.used = out.total.saturating_sub(out.available);
        out.cached = kb("Cached:");
        out.percent = if out.total > 0 {
            (out.used as f64 / out.total as f64 * 100.0) as f32
        } else {
            0.0
        };
    }

    pub fn network(&mut self) -> Vec<NetRate> {
        Vec::new()
    }

    pub fn disks(&mut self) -> Vec<DiskRate> {
        Vec::new()
    }
}

/// Processes on other systems: a short list from /proc (Linux), enough to
/// develop the page.
#[derive(Default)]
pub struct ProcessSampler;

impl ProcessSampler {
    pub fn new() -> Self {
        ProcessSampler
    }

    pub fn sample(&mut self) -> Vec<super::processes::ProcessRow> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
            .filter_map(|pid| {
                let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
                Some(super::processes::ProcessRow {
                    pid,
                    parent: 0,
                    name: name.trim().to_string(),
                    description: name.trim().to_string(),
                    path: None,
                    group: "background",
                    cpu: 0.0,
                    memory: 0,
                    disk: 0.0,
                    gpu: 0.0,
                    threads: 1,
                    status: "running",
                })
            })
            .collect()
    }
}

pub fn end(_pid: u32) -> Result<(), String> {
    Err("not available here".into())
}
pub fn set_priority(_pid: u32, _level: &str) -> Result<(), String> {
    Err("not available here".into())
}
pub fn set_efficiency(_pid: u32, _on: bool) -> Result<(), String> {
    Err("not available here".into())
}
pub fn open_location(_path: &str) -> Result<(), String> {
    Err("not available here".into())
}
pub fn properties(_path: &str) -> Result<(), String> {
    Err("not available here".into())
}
