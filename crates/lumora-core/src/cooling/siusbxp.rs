//! Corsair Hydro Pro coolers through their own Windows driver.
//!
//! iCUE installs Silicon Labs' USBXpress driver (`SIUSBXP`) for these
//! coolers, plus `SiUSBXp.dll`, the library apps use to talk through it. With
//! that driver in place neither libusb nor UsbDk can reach the cooler, so
//! Lumora loads the copy of `SiUSBXp.dll` already on the PC (it doesn't ship
//! one) and talks through Corsair's own driver: nothing to install.
//!
//! The library's interface is Silicon Labs' published USBXpress API:
//! `SI_GetNumDevices`, `SI_GetProductString`, `SI_Open`, `SI_Read`,
//! `SI_Write`, `SI_CheckRXQueue`, `SI_SetTimeouts`, `SI_Close`.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use libloading::Library;

use super::Link;

type Handle = *mut c_void;
const SI_SUCCESS: u8 = 0;
const SI_DEVICE_NOT_FOUND: u8 = 0xFF;
/// `SI_GetProductString` selectors.
const SI_RETURN_VID: u32 = 0x03;
const SI_RETURN_PID: u32 = 0x04;

type GetNumDevices = unsafe extern "system" fn(*mut u32) -> u8;
type GetProductString = unsafe extern "system" fn(u32, *mut c_void, u32) -> u8;
type Open = unsafe extern "system" fn(u32, *mut Handle) -> u8;
type Close = unsafe extern "system" fn(Handle) -> u8;
type Read = unsafe extern "system" fn(Handle, *mut c_void, u32, *mut u32, *mut c_void) -> u8;
type Write = unsafe extern "system" fn(Handle, *const c_void, u32, *mut u32, *mut c_void) -> u8;
type CheckRxQueue = unsafe extern "system" fn(Handle, *mut u32, *mut u32) -> u8;
type SetTimeouts = unsafe extern "system" fn(u32, u32) -> u8;

pub struct Api {
    lib: Library,
    pub path: PathBuf,
}

fn code(c: u8) -> String {
    match c {
        0x01 => "invalid handle".into(),
        0x02 => "read error".into(),
        0x04 => "write error".into(),
        0x08 => "device I/O failed".into(),
        0x0D => "read timed out".into(),
        0x0E => "write timed out".into(),
        0xFF => "the cooler is already open in another program. iCUE's background \"Corsair Service\" keeps it open even with iCUE closed: stop it (Win+R, services.msc, Corsair Service, Stop; set Startup type to Manual to keep it stopped) and Lumora connects within seconds".into(),
        c => format!("USBXpress error 0x{c:02x}"),
    }
}

fn err(what: &str, c: u8) -> io::Error {
    let kind = if c == SI_DEVICE_NOT_FOUND {
        io::ErrorKind::ResourceBusy
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, format!("{what}: {}", code(c)))
}

/// Folders iCUE (and the driver package) put `SiUSBXp.dll` in.
fn candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p).join("Corsair"));
        }
    }
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, out);
                }
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("SiUSBXp.dll"))
            {
                out.push(p);
            }
        }
    }
    for r in &roots {
        walk(r, 3, &mut out);
    }
    if let Some(win) = std::env::var_os("SystemRoot") {
        let win = PathBuf::from(win);
        out.push(win.join("System32").join("SiUSBXp.dll"));
        walk(
            &win.join("System32")
                .join("DriverStore")
                .join("FileRepository"),
            1,
            &mut out,
        );
    }
    out
}

/// The USBXpress library, loaded once (`Err` says why not).
pub fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut tried = Vec::new();
        for path in candidates() {
            if !path.is_file() {
                continue;
            }
            // SAFETY: loading Silicon Labs' USBXpress library that iCUE
            // installed; its functions are called with their published types.
            match unsafe { Library::new(&path) } {
                Ok(lib) => {
                    let api = Api { lib, path };
                    let _ = api.set_timeouts(1000, 1000);
                    return Ok(api);
                }
                Err(e) => tried.push(format!("{}: {e}", path.display())),
            }
        }
        Err(if tried.is_empty() {
            "SiUSBXp.dll (installed with iCUE) wasn't found".to_string()
        } else {
            format!("SiUSBXp.dll wouldn't load ({})", tried.join("; "))
        })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

impl Api {
    fn sym<T>(&self, name: &[u8]) -> io::Result<libloading::Symbol<'_, T>> {
        // SAFETY: see `api`.
        unsafe { self.lib.get(name) }.map_err(|e| io::Error::other(format!("SiUSBXp.dll: {e}")))
    }

    fn set_timeouts(&self, read_ms: u32, write_ms: u32) -> io::Result<()> {
        let f = self.sym::<SetTimeouts>(b"SI_SetTimeouts\0")?;
        // SAFETY: published signature.
        match unsafe { f(read_ms, write_ms) } {
            SI_SUCCESS => Ok(()),
            c => Err(err("SI_SetTimeouts", c)),
        }
    }

    fn product_string(&self, index: u32, what: u32) -> Option<String> {
        let f = self
            .sym::<GetProductString>(b"SI_GetProductString\0")
            .ok()?;
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is larger than the library's maximum string.
        let c = unsafe { f(index, buf.as_mut_ptr().cast(), what) };
        (c == SI_SUCCESS).then(|| {
            String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)])
                .trim()
                .to_string()
        })
    }

    /// USBXpress devices: (index, vendor id, product id).
    pub fn devices(&self) -> io::Result<Vec<(u32, u16, u16)>> {
        let f = self.sym::<GetNumDevices>(b"SI_GetNumDevices\0")?;
        let mut n = 0u32;
        // SAFETY: published signature.
        match unsafe { f(&mut n) } {
            SI_SUCCESS => {}
            SI_DEVICE_NOT_FOUND => return Ok(Vec::new()),
            c => return Err(err("SI_GetNumDevices", c)),
        }
        let hex = |s: Option<String>| {
            s.and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0)
        };
        Ok((0..n.min(16))
            .map(|i| {
                (
                    i,
                    hex(self.product_string(i, SI_RETURN_VID)),
                    hex(self.product_string(i, SI_RETURN_PID)),
                )
            })
            .collect())
    }

    pub fn open(&'static self, index: u32) -> io::Result<SiLink> {
        let f = self.sym::<Open>(b"SI_Open\0")?;
        let mut h: Handle = std::ptr::null_mut();
        // SAFETY: published signature.
        match unsafe { f(index, &mut h) } {
            SI_SUCCESS => Ok(SiLink {
                api: self,
                handle: h,
            }),
            c => Err(err("SI_Open", c)),
        }
    }
}

/// One open cooler through USBXpress.
pub struct SiLink {
    api: &'static Api,
    handle: Handle,
}

// The handle is used from one thread at a time (the cooling thread).
unsafe impl Send for SiLink {}

impl Link for SiLink {
    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        let f = self.api.sym::<Write>(b"SI_Write\0")?;
        let mut written = 0u32;
        // SAFETY: published signature; the buffer outlives the call.
        match unsafe {
            f(
                self.handle,
                msg.as_ptr().cast(),
                msg.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        } {
            SI_SUCCESS if written as usize == msg.len() => Ok(()),
            SI_SUCCESS => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the cooler took part of a message",
            )),
            c => Err(err("SI_Write", c)),
        }
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let check = self.api.sym::<CheckRxQueue>(b"SI_CheckRXQueue\0")?;
        let read = self.api.sym::<Read>(b"SI_Read\0")?;
        // Wait for bytes, then take what's there (a reply arrives whole).
        let end = Instant::now() + timeout;
        loop {
            let (mut queued, mut status) = (0u32, 0u32);
            // SAFETY: published signature.
            let c = unsafe { check(self.handle, &mut queued, &mut status) };
            if c != SI_SUCCESS {
                return Err(err("SI_CheckRXQueue", c));
            }
            if queued > 0 {
                std::thread::sleep(Duration::from_millis(3));
                let mut buf = [0u8; 64];
                let want = (queued as usize).clamp(1, buf.len()) as u32;
                let mut got = 0u32;
                // SAFETY: the buffer holds `want` bytes.
                let c = unsafe {
                    read(
                        self.handle,
                        buf.as_mut_ptr().cast(),
                        want,
                        &mut got,
                        std::ptr::null_mut(),
                    )
                };
                return match c {
                    SI_SUCCESS | 0x0D if got > 0 => Ok(Some(buf[..got as usize].to_vec())),
                    SI_SUCCESS | 0x0D => Ok(None),
                    c => Err(err("SI_Read", c)),
                };
            }
            if Instant::now() >= end {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for SiLink {
    fn drop(&mut self) {
        if let Ok(f) = self.api.sym::<Close>(b"SI_Close\0") {
            // SAFETY: closing our own handle once.
            unsafe { f(self.handle) };
        }
    }
}

/// For the troubleshooting report.
pub fn diagnose() -> String {
    match api() {
        Err(e) => format!("USBXpress (Corsair's driver): {e}"),
        Ok(a) => match a.devices() {
            Err(e) => format!(
                "USBXpress: {} loaded, but listing failed: {e}",
                a.path.display()
            ),
            Ok(d) => format!(
                "USBXpress: {} | devices: {}",
                a.path.display(),
                if d.is_empty() {
                    "none".to_string()
                } else {
                    d.iter()
                        .map(|(i, v, p)| format!("#{i} {v:04x}:{p:04x}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        },
    }
}
