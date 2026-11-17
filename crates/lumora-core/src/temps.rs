//! Live temperatures that effects can follow.
//!
//! Sources: `"liquid"` (a cooler Lumora is running), `"gpu"` (NVIDIA cards,
//! read through the driver's own NVML library, no extra install needed) and
//! `"cpu"` (on Windows from Lumora's sensor helper, on Linux from hwmon).
//! Readings older than a few seconds count as missing.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

const STALE_AFTER: Duration = Duration::from_secs(8);

fn registry() -> &'static Mutex<HashMap<String, (f32, Instant)>> {
    static R: OnceLock<Mutex<HashMap<String, (f32, Instant)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn set(source: &str, celsius: Option<f32>) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    match celsius {
        Some(c) if c.is_finite() => {
            r.insert(source.to_string(), (c, Instant::now()));
        }
        _ => {
            r.remove(source);
        }
    }
}

pub fn get(source: &str) -> Option<f32> {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    r.get(source)
        .filter(|(_, at)| at.elapsed() < STALE_AFTER)
        .map(|(c, _)| *c)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TempReading {
    pub source: String,
    pub celsius: f32,
}

/// Every current reading, for the UI.
pub fn all() -> Vec<TempReading> {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<TempReading> = r
        .iter()
        .filter(|(_, (_, at))| at.elapsed() < STALE_AFTER)
        .map(|(s, (c, _))| TempReading {
            source: s.clone(),
            celsius: *c,
        })
        .collect();
    v.sort_by(|a, b| a.source.cmp(&b.source));
    v
}

/// NVIDIA GPU temperature through NVML, loaded at runtime.
pub mod gpu {
    use libloading::{Library, Symbol};

    type Init = unsafe extern "C" fn() -> i32;
    type Handle = unsafe extern "C" fn(u32, *mut *mut std::ffi::c_void) -> i32;
    type Temp = unsafe extern "C" fn(*mut std::ffi::c_void, i32, *mut u32) -> i32;
    type Count = unsafe extern "C" fn(*mut std::ffi::c_void, *mut u32) -> i32;
    type FanGet = unsafe extern "C" fn(*mut std::ffi::c_void, u32, *mut u32) -> i32;
    type FanSet = unsafe extern "C" fn(*mut std::ffi::c_void, u32, u32) -> i32;
    type FanDefault = unsafe extern "C" fn(*mut std::ffi::c_void, u32) -> i32;
    type MinMax = unsafe extern "C" fn(*mut std::ffi::c_void, *mut u32, *mut u32) -> i32;
    type Name = unsafe extern "C" fn(*mut std::ffi::c_void, *mut u8, u32) -> i32;
    type Pci = unsafe extern "C" fn(*mut std::ffi::c_void, *mut PciInfo) -> i32;

    /// `nvmlPciInfo_t`.
    #[repr(C)]
    pub struct PciInfo {
        bus_id_legacy: [u8; 16],
        domain: u32,
        bus: u32,
        device: u32,
        pci_device_id: u32,
        pci_sub_system_id: u32,
        bus_id: [u8; 32],
    }

    /// NVML's "not allowed" answer: setting fans needs admin rights.
    pub const NO_PERMISSION: i32 = 4;

    pub struct Nvml {
        lib: Library,
        device: *mut std::ffi::c_void,
    }

    // The handle is only used from the polling thread that owns it.
    unsafe impl Send for Nvml {}

    impl Nvml {
        pub fn open() -> Option<Nvml> {
            let names: &[&str] = if cfg!(windows) {
                &["nvml.dll"]
            } else {
                &["libnvidia-ml.so.1", "libnvidia-ml.so"]
            };
            for name in names {
                // SAFETY: loading the NVIDIA driver's own library; symbols are
                // checked for presence and called with the documented types.
                unsafe {
                    let Ok(lib) = Library::new(name) else {
                        continue;
                    };
                    let init: Symbol<Init> = lib.get(b"nvmlInit_v2\0").ok()?;
                    if init() != 0 {
                        return None;
                    }
                    let handle: Symbol<Handle> =
                        lib.get(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
                    let mut device = std::ptr::null_mut();
                    if handle(0, &mut device) != 0 {
                        return None;
                    }
                    return Some(Nvml { lib, device });
                }
            }
            None
        }

        /// Calls a symbol with the library's own types; `None` when the
        /// driver is too old to have it.
        fn sym<T>(&self, name: &[u8]) -> Option<Symbol<'_, T>> {
            // SAFETY: see `open`; each caller names the symbol's real type.
            unsafe { self.lib.get(name).ok() }
        }

        pub fn name(&self) -> Option<String> {
            let f = self.sym::<Name>(b"nvmlDeviceGetName\0")?;
            let mut buf = [0u8; 96];
            // SAFETY: the buffer is as long as we say.
            (unsafe { f(self.device, buf.as_mut_ptr(), buf.len() as u32) } == 0).then(|| {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                String::from_utf8_lossy(&buf[..end]).trim().to_string()
            })
        }

        /// The card's PCI device id (the top half of NVML's combined id).
        pub fn pci_device(&self) -> Option<u16> {
            let f = self.sym::<Pci>(b"nvmlDeviceGetPciInfo_v3\0")?;
            let mut info: PciInfo = unsafe { std::mem::zeroed() };
            // SAFETY: NVML fills the struct it's given.
            (unsafe { f(self.device, &mut info) } == 0).then_some((info.pci_device_id >> 16) as u16)
        }

        pub fn fan_count(&self) -> u32 {
            let Some(f) = self.sym::<Count>(b"nvmlDeviceGetNumFans\0") else {
                return 0;
            };
            let mut n = 0u32;
            // SAFETY: see `open`.
            if unsafe { f(self.device, &mut n) } == 0 {
                n.min(8)
            } else {
                0
            }
        }

        /// Each fan's current speed, in percent.
        pub fn fan_speeds(&self) -> Vec<u32> {
            let Some(f) = self.sym::<FanGet>(b"nvmlDeviceGetFanSpeed_v2\0") else {
                return Vec::new();
            };
            (0..self.fan_count())
                .filter_map(|i| {
                    let mut v = 0u32;
                    // SAFETY: see `open`.
                    (unsafe { f(self.device, i, &mut v) } == 0).then_some(v)
                })
                .collect()
        }

        /// The lowest and highest speed the card allows, in percent.
        pub fn fan_range(&self) -> (u32, u32) {
            let Some(f) = self.sym::<MinMax>(b"nvmlDeviceGetMinMaxFanSpeed\0") else {
                return (30, 100);
            };
            let (mut lo, mut hi) = (0u32, 0u32);
            // SAFETY: see `open`.
            if unsafe { f(self.device, &mut lo, &mut hi) } == 0 && hi > lo {
                (lo, hi.min(100))
            } else {
                (30, 100)
            }
        }

        /// Sets every fan to `percent`. Returns NVML's code (0 = done).
        pub fn set_fans(&self, percent: u32) -> i32 {
            let Some(f) = self.sym::<FanSet>(b"nvmlDeviceSetFanSpeed_v2\0") else {
                return 3; // NOT_SUPPORTED
            };
            let (lo, hi) = self.fan_range();
            let p = percent.clamp(lo, hi);
            let mut code = 0;
            for i in 0..self.fan_count() {
                // SAFETY: see `open`.
                let r = unsafe { f(self.device, i, p) };
                if r != 0 {
                    code = r;
                }
            }
            code
        }

        /// Hands every fan back to the driver's own automatic control.
        pub fn auto_fans(&self) -> i32 {
            let policy = self.sym::<FanSet>(b"nvmlDeviceSetFanControlPolicy\0");
            let default = self.sym::<FanDefault>(b"nvmlDeviceSetDefaultFanSpeed_v2\0");
            let mut code = 0;
            for i in 0..self.fan_count() {
                // SAFETY: see `open`. Policy 0 is the driver's temperature-based
                // control; setting it first works around drivers where
                // SetDefaultFanSpeed alone doesn't resume the automatic curve.
                unsafe {
                    if let Some(p) = &policy {
                        let _ = p(self.device, i, 0);
                    }
                    if let Some(d) = &default {
                        let r = d(self.device, i);
                        if r != 0 {
                            code = r;
                        }
                    }
                }
            }
            code
        }

        pub fn temperature(&self) -> Option<f32> {
            // SAFETY: see `open`. 0 = NVML_TEMPERATURE_GPU.
            unsafe {
                let temp: Symbol<Temp> = self.lib.get(b"nvmlDeviceGetTemperature\0").ok()?;
                let mut c = 0u32;
                (temp(self.device, 0, &mut c) == 0).then_some(c as f32)
            }
        }
    }

    /// Polls the GPU temperature into the registry every two seconds.
    pub fn start_polling() {
        std::thread::Builder::new()
            .name("lumora-gpu-temp".into())
            .spawn(|| {
                let Some(nvml) = Nvml::open() else { return };
                loop {
                    super::set("gpu", nvml.temperature());
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            })
            .ok();
    }
}

/// CPU temperature.
///
/// On Windows it comes from `lumora-sensors`, a small helper that runs with
/// admin rights (Windows keeps the sensor from everyone else) and reports on
/// `127.0.0.1:6744`. On Linux the kernel's hwmon drivers have it.
pub mod cpu {
    use std::sync::{Mutex, OnceLock};

    use serde::Serialize;

    /// Where the helper reports.
    pub const PORT: u16 = 6744;

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum State {
        /// Readings are coming in.
        Ok,
        /// The helper isn't installed or running (it needs a one-time setup).
        NeedsSetup,
        /// The helper runs but can't read the sensor; `message` says why.
        Error,
        /// This system has no way to read it.
        Unsupported,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct Status {
        pub state: State,
        pub message: String,
    }

    fn slot() -> &'static Mutex<Status> {
        static S: OnceLock<Mutex<Status>> = OnceLock::new();
        S.get_or_init(|| {
            Mutex::new(Status {
                state: State::NeedsSetup,
                message: String::new(),
            })
        })
    }

    fn set_status(state: State, message: impl Into<String>) {
        *slot().lock().unwrap_or_else(|e| e.into_inner()) = Status {
            state,
            message: message.into(),
        };
    }

    pub fn status() -> Status {
        slot().lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Handles one line from the helper.
    pub fn take_line(line: &str) {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("cpu ") {
            if let Ok(c) = v.trim().parse::<f32>() {
                super::set("cpu", Some(c));
                set_status(State::Ok, "");
            }
        } else if let Some(msg) = line.strip_prefix("error ") {
            super::set("cpu", None);
            set_status(State::Error, msg);
        }
    }

    /// Starts following the CPU temperature in the background.
    pub fn start_polling() {
        std::thread::Builder::new()
            .name("lumora-cpu-temp".into())
            .spawn(run)
            .ok();
    }

    #[cfg(not(target_os = "linux"))]
    fn run() {
        use std::io::{BufRead, BufReader};
        use std::net::{SocketAddr, TcpStream};
        use std::time::Duration;
        if !cfg!(windows) {
            set_status(
                State::Unsupported,
                "CPU temperature isn't available on this system yet",
            );
            return;
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
        loop {
            match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                Ok(stream) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    for line in BufReader::new(stream).lines() {
                        match line {
                            Ok(l) => take_line(&l),
                            Err(_) => break,
                        }
                    }
                    super::set("cpu", None);
                }
                Err(_) => {
                    if status().state != State::NeedsSetup {
                        super::set("cpu", None);
                    }
                    set_status(State::NeedsSetup, "");
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// The package temperature from the CPU's hwmon driver.
    #[cfg(target_os = "linux")]
    fn run() {
        use std::time::Duration;
        loop {
            match read_hwmon() {
                Some(c) => {
                    super::set("cpu", Some(c));
                    set_status(State::Ok, "");
                }
                None => {
                    super::set("cpu", None);
                    set_status(
                        State::Unsupported,
                        "no CPU sensor found (k10temp or coretemp)",
                    );
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    #[cfg(target_os = "linux")]
    fn read_hwmon() -> Option<f32> {
        for e in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
            let dir = e.path();
            let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
            let name = name.trim();
            if name != "k10temp" && name != "coretemp" && name != "zenpower" {
                continue;
            }
            // temp1 is Tctl on AMD and the package on Intel.
            let milli: f32 = std::fs::read_to_string(dir.join("temp1_input"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            return Some(milli / 1000.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cpu_helper_lines() {
        super::cpu::take_line("cpu 47.125");
        assert!((super::get("cpu").unwrap() - 47.125).abs() < 0.001);
        assert_eq!(super::cpu::status().state, super::cpu::State::Ok);
        super::cpu::take_line("error the PawnIO driver isn't installed");
        assert_eq!(super::get("cpu"), None);
        assert_eq!(
            super::cpu::status().message,
            "the PawnIO driver isn't installed"
        );
    }

    use super::*;

    #[test]
    fn readings_come_and_go() {
        set("test-a", Some(41.5));
        assert_eq!(get("test-a"), Some(41.5));
        assert!(all().iter().any(|r| r.source == "test-a"));
        set("test-a", None);
        assert_eq!(get("test-a"), None);
        set("test-b", Some(f32::NAN));
        assert_eq!(get("test-b"), None);
    }
}
