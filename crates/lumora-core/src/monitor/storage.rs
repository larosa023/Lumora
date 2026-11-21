//! Drives: every drive letter with its free space (Lumora reads these), and
//! each physical disk's model, temperature, health and life left (asked
//! straight from the drive, which needs admin rights: the helper does it).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Volume {
    /// "C:".
    pub letter: String,
    pub label: String,
    pub filesystem: String,
    pub total: u64,
    pub free: u64,
    /// "fixed", "removable", "network" or "other".
    pub kind: &'static str,
    /// The physical disk it's on, when known.
    pub disk: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DiskHealth {
    pub number: u32,
    pub model: String,
    /// "NVMe", "SATA", "USB", …
    pub bus: String,
    pub size: u64,
    pub temp: Option<f32>,
    /// Percent of rated life left (NVMe drives report it).
    pub life_left: Option<u32>,
    /// "good", "warning" or "unknown".
    pub health: String,
    pub power_on_hours: Option<u64>,
    /// Data written over the drive's life, bytes (NVMe).
    pub written: Option<u64>,
}

/// Reads an NVMe health log (512 bytes) into `d`.
pub fn read_nvme_health(log: &[u8], d: &mut DiskHealth) {
    if log.len() < 144 {
        return;
    }
    let kelvin = u16::from_le_bytes([log[1], log[2]]);
    if kelvin > 200 {
        d.temp = Some(kelvin as f32 - 273.15);
    }
    d.life_left = Some(100u32.saturating_sub(log[5] as u32));
    d.health = if log[0] == 0 { "good" } else { "warning" }.into();
    let u64_at = |at: usize| u64::from_le_bytes(log[at..at + 8].try_into().unwrap_or_default());
    // Data units are 1000 sectors of 512 bytes.
    d.written = Some(u64_at(48).saturating_mul(512_000));
    d.power_on_hours = Some(u64_at(128));
}

pub fn bus_name(t: u32) -> &'static str {
    match t {
        0x11 => "NVMe",
        0x0B => "SATA",
        0x07 => "USB",
        0x03 => "ATA",
        0x01 => "SCSI",
        0x0A => "SAS",
        0x08 => "RAID",
        0x0C => "SD",
        0x0D => "MMC",
        0x0F => "Virtual",
        _ => "Disk",
    }
}

pub use imp::{disk_health, volumes};

#[cfg(windows)]
mod imp {
    use super::{bus_name, read_nvme_health, DiskHealth, Volume};
    use crate::monitor::win::{from_wide, wide};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW,
        GetVolumeInformationW, FILE_SHARE_READ, FILE_SHARE_WRITE,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;

    struct File(HANDLE);
    impl Drop for File {
        fn drop(&mut self) {
            // SAFETY: our own handle.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(path: &str, access: u32) -> Option<File> {
        let p = wide(path);
        // SAFETY: opening a device for queries only.
        let h = unsafe {
            CreateFileW(
                p.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(File(h))
    }

    fn ioctl(f: &File, code: u32, input: &[u8], out_len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; out_len];
        let mut got = 0u32;
        // SAFETY: buffers as long as we say.
        let ok = unsafe {
            DeviceIoControl(
                f.0,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        } != 0;
        ok.then(|| {
            out.truncate(got as usize);
            out
        })
    }

    fn disk_of(letter: &str) -> Option<u32> {
        let f = open(&format!("\\\\.\\{letter}"), 0)?;
        let out = ioctl(&f, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 256)?;
        // { count, pad, { DiskNumber, pad, start, length }… }
        (out.len() >= 12).then(|| u32::from_le_bytes([out[8], out[9], out[10], out[11]]))
    }

    pub fn volumes() -> Vec<Volume> {
        let mut buf = vec![0u16; 512];
        // SAFETY: a buffer as long as we say.
        let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let roots: Vec<String> = buf[..n.min(buf.len())]
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        roots
            .into_iter()
            .filter_map(|root| {
                let w = wide(&root);
                // SAFETY: valid root path.
                let kind = match unsafe { GetDriveTypeW(w.as_ptr()) } {
                    3 => "fixed",
                    2 => "removable",
                    4 => "network",
                    5 => return None, // CD/DVD
                    _ => "other",
                };
                let (mut free, mut total, mut _all_free) = (0u64, 0u64, 0u64);
                // SAFETY: valid root path.
                if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, &mut total, &mut _all_free) }
                    == 0
                {
                    return None;
                }
                let mut label = vec![0u16; 261];
                let mut fs = vec![0u16; 64];
                // SAFETY: buffers as long as we say.
                unsafe {
                    GetVolumeInformationW(
                        w.as_ptr(),
                        label.as_mut_ptr(),
                        label.len() as u32,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        fs.as_mut_ptr(),
                        fs.len() as u32,
                    )
                };
                let letter = root.trim_end_matches('\\').to_string();
                Some(Volume {
                    disk: if kind == "network" {
                        None
                    } else {
                        disk_of(&letter)
                    },
                    letter,
                    label: from_wide(&label),
                    filesystem: from_wide(&fs),
                    total,
                    free,
                    kind,
                })
            })
            .collect()
    }

    fn query(property: u32, extra: &[u8]) -> Vec<u8> {
        let mut q = Vec::with_capacity(8 + extra.len());
        q.extend_from_slice(&property.to_le_bytes());
        q.extend_from_slice(&0u32.to_le_bytes()); // PropertyStandardQuery
        q.extend_from_slice(extra);
        if extra.is_empty() {
            q.extend_from_slice(&[0u8; 4]);
        }
        q
    }

    fn cstr_at(b: &[u8], off: usize) -> String {
        if off == 0 || off >= b.len() {
            return String::new();
        }
        let end = b[off..]
            .iter()
            .position(|&c| c == 0)
            .map_or(b.len(), |e| off + e);
        String::from_utf8_lossy(&b[off..end]).trim().to_string()
    }

    /// Every physical disk's details (needs admin rights for health).
    pub fn disk_health() -> Vec<DiskHealth> {
        let mut out = Vec::new();
        for n in 0..16u32 {
            let path = format!("\\\\.\\PhysicalDrive{n}");
            let Some(f) = open(&path, GENERIC_READ | GENERIC_WRITE).or_else(|| open(&path, 0))
            else {
                continue;
            };
            let mut d = DiskHealth {
                number: n,
                health: "unknown".into(),
                ..Default::default()
            };
            // StorageDeviceProperty: model and bus.
            if let Some(b) = ioctl(&f, IOCTL_STORAGE_QUERY_PROPERTY, &query(0, &[]), 1024) {
                if b.len() >= 32 {
                    let at = |i: usize| {
                        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
                    };
                    let vendor = cstr_at(&b, at(12));
                    let product = cstr_at(&b, at(16));
                    d.model = if vendor.is_empty() || product.starts_with(&vendor) {
                        product
                    } else {
                        format!("{vendor} {product}")
                    };
                    d.bus = bus_name(at(28) as u32).into();
                }
            }
            if let Some(b) = ioctl(&f, IOCTL_DISK_GET_LENGTH_INFO, &[], 8) {
                if b.len() == 8 {
                    d.size = u64::from_le_bytes(b.try_into().unwrap_or_default());
                }
            }
            // StorageDeviceTemperatureProperty: any drive that reports one.
            if let Some(b) = ioctl(&f, IOCTL_STORAGE_QUERY_PROPERTY, &query(48, &[]), 256) {
                if b.len() >= 28 && u16::from_le_bytes([b[12], b[13]]) > 0 {
                    let t = i16::from_le_bytes([b[26], b[27]]);
                    if t > -40 && t < 150 {
                        d.temp = Some(t as f32);
                    }
                }
            }
            // StorageDeviceProtocolSpecificProperty: the NVMe health log.
            if d.bus == "NVMe" {
                let mut spec = Vec::new();
                for v in [3u32, 2, 2, 0, 40, 512, 0, 0, 0, 0] {
                    spec.extend_from_slice(&v.to_le_bytes());
                }
                let mut input = query(50, &spec);
                input.resize(8 + 40 + 512, 0);
                if let Some(b) = ioctl(&f, IOCTL_STORAGE_QUERY_PROPERTY, &input, 8 + 40 + 512) {
                    if b.len() >= 8 + 40 {
                        let off = u32::from_le_bytes([b[24], b[25], b[26], b[27]]) as usize;
                        if let Some(log) = b.get(8 + off..8 + off + 512) {
                            read_nvme_health(log, &mut d);
                        }
                    }
                }
            }
            if !d.model.is_empty() || d.size > 0 {
                out.push(d);
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{DiskHealth, Volume};
    pub fn volumes() -> Vec<Volume> {
        Vec::new()
    }
    pub fn disk_health() -> Vec<DiskHealth> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_nvme_health_log() {
        let mut log = vec![0u8; 512];
        log[1..3].copy_from_slice(&(273u16 + 41).to_le_bytes());
        log[5] = 7; // 7 % used
        log[48..56].copy_from_slice(&1000u64.to_le_bytes());
        log[128..136].copy_from_slice(&4321u64.to_le_bytes());
        let mut d = DiskHealth::default();
        read_nvme_health(&log, &mut d);
        assert_eq!(d.temp.map(|t| t.round()), Some(41.0));
        assert_eq!(d.life_left, Some(93));
        assert_eq!(d.health, "good");
        assert_eq!(d.power_on_hours, Some(4321));
        assert_eq!(d.written, Some(512_000_000));
        log[0] = 1;
        read_nvme_health(&log, &mut d);
        assert_eq!(d.health, "warning");
    }
}
