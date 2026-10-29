//! Live temperatures that effects can follow.
//!
//! Sources: `"liquid"` (a cooler Lumora is running) and `"gpu"` (NVIDIA cards,
//! read through the driver's own NVML library, no extra install needed).
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
    r.get(source).filter(|(_, at)| at.elapsed() < STALE_AFTER).map(|(c, _)| *c)
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
        .map(|(s, (c, _))| TempReading { source: s.clone(), celsius: *c })
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

    pub struct Nvml {
        lib: Library,
        device: *mut std::ffi::c_void,
    }

    // The handle is only used from the polling thread that owns it.
    unsafe impl Send for Nvml {}

    impl Nvml {
        pub fn open() -> Option<Nvml> {
            let names: &[&str] = if cfg!(windows) { &["nvml.dll"] } else { &["libnvidia-ml.so.1", "libnvidia-ml.so"] };
            for name in names {
                // SAFETY: loading the NVIDIA driver's own library; symbols are
                // checked for presence and called with the documented types.
                unsafe {
                    let Ok(lib) = Library::new(name) else { continue };
                    let init: Symbol<Init> = lib.get(b"nvmlInit_v2\0").ok()?;
                    if init() != 0 {
                        return None;
                    }
                    let handle: Symbol<Handle> = lib.get(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
                    let mut device = std::ptr::null_mut();
                    if handle(0, &mut device) != 0 {
                        return None;
                    }
                    return Some(Nvml { lib, device });
                }
            }
            None
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

#[cfg(test)]
mod tests {
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
