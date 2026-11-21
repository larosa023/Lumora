//! System monitoring: CPU, memory, GPU, network and disks, the process list,
//! startup apps, Windows services, drives, specs, power plans, the reading
//! history and alerts.
//!
//! Everything here is sampled on demand by the app, at a rate that depends
//! on what's on screen: a light reading every few seconds in the background
//! (for the history, alerts and the tray icon) and a full one each second
//! only while a page or the overlay shows it.

pub mod admin;
pub mod alerts;
pub mod history;
pub mod power;
pub mod processes;
pub mod services;
pub mod specs;
pub mod startup;
pub mod storage;
pub mod tray_icon;

#[cfg(windows)]
pub(crate) mod win;
#[cfg(windows)]
pub(crate) use win as imp;
#[cfg(windows)]
pub(crate) mod procs_win;
#[cfg(windows)]
pub(crate) use procs_win as imp_processes;
#[cfg(not(windows))]
pub(crate) mod other;
#[cfg(not(windows))]
pub(crate) use other as imp;
#[cfg(not(windows))]
pub(crate) use other as imp_processes;

use serde::Serialize;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Cpu {
    pub name: String,
    /// Busy, percent of all cores.
    pub usage: f32,
    /// Busy per logical core, percent.
    pub cores: Vec<f32>,
    /// Current speed, MHz (Windows' own estimate, like Task Manager's).
    pub clock_mhz: Option<u32>,
    /// Base speed, MHz.
    pub base_mhz: Option<u32>,
    pub temp: Option<f32>,
    /// Package power, watts (from Lumora's admin helper).
    pub power: Option<f32>,
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
    pub uptime_s: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub percent: f32,
    pub committed: u64,
    pub commit_limit: u64,
    /// Cached by Windows (standby and file cache).
    pub cached: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Gpu {
    pub name: String,
    pub usage: Option<f32>,
    pub temp: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub core_mhz: Option<u32>,
    pub mem_mhz: Option<u32>,
    pub power: Option<f32>,
    pub fan: Option<u32>,
    pub driver: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct NetRate {
    pub name: String,
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
    /// Link speed, bits per second.
    pub link: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DiskRate {
    /// Like Task Manager: "0 C:" (disk number and its letters).
    pub name: String,
    /// Busy, percent of the time.
    pub active: f32,
    /// Bytes per second.
    pub read: f64,
    pub write: f64,
}

/// One reading of the whole system.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Sample {
    /// Unix time, milliseconds.
    pub at: u64,
    pub cpu: Cpu,
    pub memory: Memory,
    pub gpu: Option<Gpu>,
    pub net: Vec<NetRate>,
    pub disks: Vec<DiskRate>,
}

impl Sample {
    pub fn net_down(&self) -> f64 {
        self.net.iter().map(|n| n.down).sum()
    }
    pub fn net_up(&self) -> f64 {
        self.net.iter().map(|n| n.up).sum()
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Reads the system. Keep one alive: rates (CPU busy, network) are worked
/// out from the change since the last reading.
pub struct Sampler {
    imp: imp::Sampler,
    nvml: Option<crate::temps::gpu::Nvml>,
    nvml_tried: bool,
    gpu_name: Option<String>,
    gpu_driver: Option<String>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Sampler {
            imp: imp::Sampler::new(),
            nvml: None,
            nvml_tried: false,
            gpu_name: None,
            gpu_driver: None,
        }
    }

    /// `disks`: also read per-disk activity (only the storage page needs it).
    pub fn sample(&mut self, disks: bool) -> Sample {
        let mut s = Sample {
            at: now_ms(),
            ..Default::default()
        };
        self.imp.cpu(&mut s.cpu);
        s.cpu.temp = crate::temps::get("cpu");
        s.cpu.power = cpu_power();
        self.imp.memory(&mut s.memory);
        s.net = self.imp.network();
        if disks {
            s.disks = self.imp.disks();
        }
        s.gpu = self.gpu();
        s
    }

    fn gpu(&mut self) -> Option<Gpu> {
        if !self.nvml_tried {
            self.nvml_tried = true;
            self.nvml = crate::temps::gpu::Nvml::open();
            if let Some(n) = &self.nvml {
                self.gpu_name = n.name();
                self.gpu_driver = n.driver_version();
            }
        }
        let n = self.nvml.as_ref()?;
        let mem = n.memory();
        let fans = n.fan_speeds();
        Some(Gpu {
            name: self
                .gpu_name
                .clone()
                .unwrap_or_else(|| "NVIDIA graphics card".into()),
            usage: n.utilization().map(|u| u.0 as f32),
            temp: n.temperature(),
            vram_total: mem.map(|m| m.0),
            vram_used: mem.map(|m| m.1),
            core_mhz: n.clock(0),
            mem_mhz: n.clock(2),
            power: n.power(),
            fan: (!fans.is_empty()).then(|| fans.iter().sum::<u32>() / fans.len() as u32),
            driver: self.gpu_driver.clone(),
        })
    }
}

fn power_slot() -> &'static Mutex<Option<(f32, std::time::Instant)>> {
    static P: OnceLock<Mutex<Option<(f32, std::time::Instant)>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(None))
}

/// CPU package power as the admin helper last reported it.
pub fn set_cpu_power(watts: Option<f32>) {
    *power_slot().lock().unwrap_or_else(|e| e.into_inner()) =
        watts.map(|w| (w, std::time::Instant::now()));
}

pub fn cpu_power() -> Option<f32> {
    power_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(10))
        .map(|(w, _)| w)
}

/// Busy percentage from two (idle, total) time readings.
pub fn busy(prev: (u64, u64), now: (u64, u64)) -> f32 {
    let idle = now.0.saturating_sub(prev.0) as f64;
    let total = now.1.saturating_sub(prev.1) as f64;
    if total <= 0.0 {
        return 0.0;
    }
    ((1.0 - idle / total) * 100.0).clamp(0.0, 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_from_time_deltas() {
        assert_eq!(busy((100, 1000), (150, 1100)), 50.0);
        assert_eq!(busy((100, 1000), (100, 1000)), 0.0);
        assert_eq!(busy((0, 0), (0, 100)), 100.0);
    }

    #[test]
    fn sampler_runs_anywhere() {
        let mut s = Sampler::new();
        let a = s.sample(true);
        assert!(a.at > 0);
        let b = s.sample(false);
        assert!(b.cpu.usage >= 0.0 && b.cpu.usage <= 100.0);
    }
}
