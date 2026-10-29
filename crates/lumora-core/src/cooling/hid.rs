//! Real cooler access through the operating system's HID stack.

use std::collections::HashSet;
use std::io;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use super::kraken::{self, NZXT_VID};
use super::{Backend, FoundCooler, HidIo};

pub struct HidBackend {
    api: Option<HidApi>,
}

impl HidBackend {
    pub fn new() -> Self {
        HidBackend { api: HidApi::new().ok() }
    }
}

impl Default for HidBackend {
    fn default() -> Self {
        Self::new()
    }
}

struct Device(HidDevice);

impl HidIo for Device {
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        self.0.write(report).map(|_| ()).map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut buf = [0u8; 64];
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        match self.0.read_timeout(&mut buf, ms) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(buf[..n].to_vec())),
            Err(e) => Err(io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())),
        }
    }
}

impl Backend for HidBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        let Some(api) = self.api.as_mut() else { return Vec::new() };
        let _ = api.refresh_devices();
        let mut seen = HashSet::new();
        api.device_list()
            .filter(|d| d.vendor_id() == NZXT_VID && kraken::model(d.product_id()).is_some())
            .filter_map(|d| {
                let serial = d.serial_number().unwrap_or("").to_string();
                let id = format!("hid:{:04x}:{:04x}:{}", d.vendor_id(), d.product_id(), serial);
                // Composite devices list one entry per interface.
                seen.insert(id.clone()).then(|| FoundCooler { id, vid: d.vendor_id(), pid: d.product_id(), serial })
            })
            .collect()
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn HidIo>> {
        let api = self.api.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "HID isn't available"))?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == cooler.vid && d.product_id() == cooler.pid && d.serial_number().unwrap_or("") == cooler.serial)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler was unplugged"))?;
        let dev = info.open_device(api).map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        Ok(Box::new(Device(dev)))
    }
}

/// Running processes, lowercased, for spotting vendor apps.
pub fn running_processes() -> HashSet<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(0x0800_0000)
            .output();
        if let Ok(o) = out {
            return String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split("\",\"").next())
                .map(|n| n.trim_matches('"').to_lowercase())
                .collect();
        }
        HashSet::new()
    }
    #[cfg(not(windows))]
    {
        let mut set = HashSet::new();
        if let Ok(dir) = std::fs::read_dir("/proc") {
            for e in dir.flatten() {
                if let Ok(name) = std::fs::read_to_string(e.path().join("comm")) {
                    set.insert(name.trim().to_lowercase());
                }
            }
        }
        set
    }
}
