//! The PC's specs: Windows, CPU, motherboard and BIOS, each RAM stick (from
//! the firmware's SMBIOS table), the graphics card and the drives.

use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct RamStick {
    pub slot: String,
    pub size: u64,
    /// "DDR5", "DDR4", …
    pub kind: String,
    /// Rated speed, MT/s.
    pub speed: u32,
    /// Speed it actually runs at, MT/s.
    pub configured: u32,
    pub maker: String,
    pub part: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Specs {
    pub os: String,
    pub cpu: String,
    pub cores: u32,
    pub threads: u32,
    pub board: String,
    pub bios: String,
    pub ram_total: u64,
    pub ram: Vec<RamStick>,
    pub gpu: Option<String>,
    pub gpu_driver: Option<String>,
    pub gpu_vram: Option<u64>,
    pub drives: Vec<String>,
}

/// What the firmware says about the board, BIOS and RAM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Smbios {
    pub board: String,
    pub bios: String,
    pub ram: Vec<RamStick>,
}

fn memory_kind(t: u8) -> &'static str {
    match t {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    }
}

/// Reads a raw SMBIOS table (as `GetSystemFirmwareTable("RSMB")` returns
/// it: an 8-byte header, then the structures).
pub fn parse_smbios(raw: &[u8]) -> Smbios {
    let mut out = Smbios::default();
    let data = raw.get(8..).unwrap_or(&[]);
    let mut at = 0usize;
    let (mut board_maker, mut board_product) = (String::new(), String::new());
    while at + 4 <= data.len() {
        let kind = data[at];
        let len = data[at + 1] as usize;
        if len < 4 || at + len > data.len() {
            break;
        }
        let f = &data[at..at + len];
        // The strings follow the formatted part, each NUL-terminated,
        // the list ending with an extra NUL.
        let mut strings = Vec::new();
        let mut p = at + len;
        loop {
            let end = data[p..]
                .iter()
                .position(|&c| c == 0)
                .map_or(data.len(), |e| p + e);
            if end == p {
                p += 1;
                break;
            }
            strings.push(String::from_utf8_lossy(&data[p..end]).trim().to_string());
            p = end + 1;
            if p >= data.len() {
                break;
            }
        }
        if strings.is_empty() {
            p += 1; // two NULs when there are no strings
        }
        let s = |i: usize| -> String {
            f.get(i)
                .and_then(|&n| (n as usize).checked_sub(1))
                .and_then(|n| strings.get(n))
                .cloned()
                .unwrap_or_default()
        };
        let u16_at = |i: usize| f.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |i: usize| {
            f.get(i..i + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        match kind {
            0 => {
                let (vendor, version, date) = (s(0x04), s(0x05), s(0x08));
                out.bios = [vendor, version, date]
                    .into_iter()
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            2 => {
                board_maker = s(0x04);
                board_product = s(0x05);
            }
            17 => {
                let size = match u16_at(0x0C).unwrap_or(0) {
                    0 | 0xFFFF => 0,
                    0x7FFF => u32_at(0x1C).unwrap_or(0) as u64 * 1024 * 1024,
                    v if v & 0x8000 != 0 => (v & 0x7FFF) as u64 * 1024,
                    v => v as u64 * 1024 * 1024,
                };
                if size > 0 {
                    let speed = match u16_at(0x15).unwrap_or(0) {
                        0xFFFF => u32_at(0x54).unwrap_or(0),
                        v => v as u32,
                    };
                    let configured = match u16_at(0x20).unwrap_or(0) {
                        0xFFFF => u32_at(0x58).unwrap_or(0),
                        v => v as u32,
                    };
                    out.ram.push(RamStick {
                        slot: s(0x10),
                        size,
                        kind: memory_kind(f.get(0x12).copied().unwrap_or(0)).into(),
                        speed,
                        configured,
                        maker: s(0x17),
                        part: s(0x1A),
                    });
                }
            }
            127 => break,
            _ => {}
        }
        at = p;
    }
    out.board = format!("{board_maker} {board_product}").trim().to_string();
    out
}

/// Plain-text specs, for "Copy specs".
pub fn as_text(s: &Specs) -> String {
    let gb = |b: u64| format!("{:.0} GB", b as f64 / 1_073_741_824.0);
    let mut t = String::new();
    t.push_str(&format!("OS: {}\n", s.os));
    t.push_str(&format!(
        "CPU: {} ({} cores, {} threads)\n",
        s.cpu, s.cores, s.threads
    ));
    if !s.board.is_empty() {
        t.push_str(&format!("Motherboard: {}\n", s.board));
    }
    if !s.bios.is_empty() {
        t.push_str(&format!("BIOS: {}\n", s.bios));
    }
    t.push_str(&format!("RAM: {}", gb(s.ram_total)));
    if let Some(r) = s.ram.first() {
        t.push_str(&format!(
            " ({}x {} {}{}{})",
            s.ram.len(),
            gb(r.size),
            r.kind,
            if r.configured > 0 {
                format!("-{}", r.configured)
            } else {
                String::new()
            },
            if r.part.is_empty() {
                String::new()
            } else {
                format!(", {}", r.part)
            },
        ));
    }
    t.push('\n');
    if let Some(g) = &s.gpu {
        t.push_str(&format!("GPU: {g}"));
        if let Some(v) = s.gpu_vram {
            t.push_str(&format!(" ({})", gb(v)));
        }
        if let Some(d) = &s.gpu_driver {
            t.push_str(&format!(", driver {d}"));
        }
        t.push('\n');
    }
    for d in &s.drives {
        t.push_str(&format!("Drive: {d}\n"));
    }
    t
}

pub use imp::gather;

#[cfg(windows)]
mod imp {
    use super::{parse_smbios, Specs};
    use crate::monitor::win::{reg_dword, reg_string};
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, GetSystemFirmwareTable, GlobalMemoryStatusEx,
        RelationProcessorCore, MEMORYSTATUSEX, RSMB,
    };
    use windows_sys::Win32::System::Threading::GetActiveProcessorCount;

    fn os() -> String {
        let k = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
        let mut name =
            reg_string(HKEY_LOCAL_MACHINE, k, "ProductName").unwrap_or_else(|| "Windows".into());
        let build: u32 = reg_string(HKEY_LOCAL_MACHINE, k, "CurrentBuild")
            .and_then(|b| b.parse().ok())
            .unwrap_or(0);
        // Windows 11 still calls itself "Windows 10" here.
        if build >= 22000 {
            name = name.replace("Windows 10", "Windows 11");
        }
        let version = reg_string(HKEY_LOCAL_MACHINE, k, "DisplayVersion").unwrap_or_default();
        let ubr = reg_dword(HKEY_LOCAL_MACHINE, k, "UBR").unwrap_or(0);
        format!("{name} {version} (build {build}.{ubr})").replace("  ", " ")
    }

    fn cores() -> u32 {
        let mut len = 0u32;
        // SAFETY: asking for the size first.
        unsafe {
            GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut len)
        };
        if len == 0 {
            return 0;
        }
        let mut buf = vec![0u64; len as usize / 8 + 1];
        // SAFETY: a buffer of the size asked for.
        if unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorCore,
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        } == 0
        {
            return 0;
        }
        // SAFETY: `len` bytes were written.
        let b = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, len as usize) };
        let mut at = 0;
        let mut n = 0;
        while at + 8 <= b.len() {
            let size = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            if size == 0 {
                break;
            }
            n += 1;
            at += size;
        }
        n
    }

    fn smbios() -> Vec<u8> {
        // SAFETY: asking for the size first.
        let n = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
        if n == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; n as usize];
        // SAFETY: a buffer of the size asked for.
        let got = unsafe { GetSystemFirmwareTable(RSMB, 0, buf.as_mut_ptr(), n) };
        buf.truncate(got as usize);
        buf
    }

    pub fn gather() -> Specs {
        let fw = parse_smbios(&smbios());
        let cpu = reg_string(
            HKEY_LOCAL_MACHINE,
            "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
            "ProcessorNameString",
        )
        .unwrap_or_default();
        let bios_key = "HARDWARE\\DESCRIPTION\\System\\BIOS";
        let board = if fw.board.is_empty() {
            format!(
                "{} {}",
                reg_string(HKEY_LOCAL_MACHINE, bios_key, "BaseBoardManufacturer")
                    .unwrap_or_default(),
                reg_string(HKEY_LOCAL_MACHINE, bios_key, "BaseBoardProduct").unwrap_or_default()
            )
            .trim()
            .to_string()
        } else {
            fw.board
        };
        // SAFETY: zeroed plain data with its length set.
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe { GlobalMemoryStatusEx(&mut m) };
        let ram_sum: u64 = fw.ram.iter().map(|r| r.size).sum();
        let nvml = crate::temps::gpu::Nvml::open();
        let drives = crate::monitor::storage::disk_health()
            .into_iter()
            .map(|d| format!("{} ({}, {:.0} GB)", d.model, d.bus, d.size as f64 / 1e9))
            .collect();
        Specs {
            os: os(),
            cpu: cpu.trim().to_string(),
            cores: cores(),
            // SAFETY: plain query.
            threads: unsafe { GetActiveProcessorCount(0xFFFF) },
            board,
            bios: fw.bios,
            ram_total: if ram_sum > 0 { ram_sum } else { m.ullTotalPhys },
            ram: fw.ram,
            gpu: nvml.as_ref().and_then(|n| n.name()),
            gpu_driver: nvml.as_ref().and_then(|n| n.driver_version()),
            gpu_vram: nvml.as_ref().and_then(|n| n.memory()).map(|m| m.0),
            drives,
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Specs;
    pub fn gather() -> Specs {
        Specs {
            os: std::env::consts::OS.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(kind: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, (formatted.len() + 4) as u8, 0, 0];
        v.extend_from_slice(formatted);
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        if strings.is_empty() {
            v.push(0);
        }
        v.push(0);
        v
    }

    #[test]
    fn reads_board_bios_and_ram_sticks() {
        let mut raw = vec![0u8; 8];
        // BIOS: vendor 1, version 2, date at 0x08 = 3.
        let mut f0 = vec![0u8; 0x14];
        f0[0] = 1;
        f0[1] = 2;
        f0[4] = 3;
        raw.extend(structure(
            0,
            &f0,
            &["American Megatrends", "3287", "10/10/2025"],
        ));
        let mut f2 = vec![0u8; 0x0B];
        f2[0] = 1;
        f2[1] = 2;
        raw.extend(structure(
            2,
            &f2,
            &["ASUSTeK COMPUTER INC.", "TUF GAMING B650E-PLUS WIFI"],
        ));
        // A 32 GB DDR5 stick at 6000 MT/s.
        let mut f17 = vec![0u8; 0x24];
        f17[0x0C - 4..0x0E - 4].copy_from_slice(&0x7FFFu16.to_le_bytes());
        f17[0x1C - 4..0x20 - 4].copy_from_slice(&32768u32.to_le_bytes());
        f17[0x10 - 4] = 1;
        f17[0x12 - 4] = 0x22;
        f17[0x15 - 4..0x17 - 4].copy_from_slice(&6000u16.to_le_bytes());
        f17[0x17 - 4] = 2;
        f17[0x1A - 4] = 3;
        f17[0x20 - 4..0x22 - 4].copy_from_slice(&6000u16.to_le_bytes());
        raw.extend(structure(17, &f17, &["DIMM_A2", "Team Group", "UD5-6000"]));
        // An empty slot.
        raw.extend(structure(17, &vec![0u8; 0x24], &[]));
        raw.extend(structure(127, &[], &[]));
        let s = parse_smbios(&raw);
        assert_eq!(s.bios, "American Megatrends 3287 10/10/2025");
        assert_eq!(s.board, "ASUSTeK COMPUTER INC. TUF GAMING B650E-PLUS WIFI");
        assert_eq!(s.ram.len(), 1);
        let r = &s.ram[0];
        assert_eq!(r.size, 32 * 1024 * 1024 * 1024);
        assert_eq!(
            (r.kind.as_str(), r.speed, r.configured),
            ("DDR5", 6000, 6000)
        );
        assert_eq!(
            (r.slot.as_str(), r.maker.as_str(), r.part.as_str()),
            ("DIMM_A2", "Team Group", "UD5-6000")
        );
    }
}
