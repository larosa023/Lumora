//! Lumora's CPU temperature helper.
//!
//! Windows only lets programs with admin rights read the CPU's temperature
//! sensor. This tiny helper runs elevated (started at sign-in by the
//! `Lumora\Sensors` scheduled task, from a folder only admins can change) and
//! does exactly one thing: reads the CPU temperature through the signed PawnIO
//! driver and tells Lumora on `127.0.0.1:6744`, one line per second:
//!
//! ```text
//! cpu 45.250
//! error <what went wrong>
//! ```
//!
//! Reading is done through PawnIO's own signed modules (`AMDFamily17.bin`
//! for Ryzen, `IntelMSR.bin` for Intel), which only allow reads of a fixed
//! set of harmless registers.
//!
//! It also sets NVIDIA graphics card fan speeds for Lumora (NVML only lets
//! admins do that; see `lumora_core::cooling::gpu`), handing them back to the
//! driver whenever Lumora goes quiet.
//!
//! It also sets motherboard fan header speeds for Lumora, through PawnIO's
//! signed `LpcIO` module (see `lumora_core::cooling::board`), and hands every
//! header back to the BIOS whenever Lumora goes quiet.
//!
//! It also relays Corsair Hydro Pro coolers for Lumora on `127.0.0.1:6745`
//! (see `lumora_core::cooling::relay`): the UsbDk driver those coolers need
//! only answers programs with admin rights. The relay only signs in Lumora
//! (by a random key Lumora writes in the user's settings folder) and only
//! reaches supported coolers.

#![cfg_attr(windows, windows_subsystem = "windows")]

/// Where Lumora listens for readings.
pub const PORT: u16 = 6744;

fn main() {
    #[cfg(windows)]
    win::main();
}

/// Celsius from AMD's THM_TCON_CUR_TMP register (SMN 0x59800): bits 31..21 in
/// steps of 1/8 °C, and bit 19 selects the -49 °C range.
pub fn amd_tctl(reg: u32) -> f32 {
    let t = ((reg >> 21) & 0x7FF) as f32 * 0.125;
    if reg & 0x80000 != 0 {
        t - 49.0
    } else {
        t
    }
}

/// Celsius from Intel's package thermal status (MSR 0x1B1) and temperature
/// target (MSR 0x1A2): the sensor reports degrees below TjMax.
pub fn intel_package(status: u64, target: u64) -> f32 {
    let tjmax = ((target >> 16) & 0xFF) as f32;
    let tjmax = if tjmax == 0.0 { 100.0 } else { tjmax };
    tjmax - ((status >> 16) & 0x7F) as f32
}

#[cfg(windows)]
mod win {
    use std::ffi::CString;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use libloading::{Library, Symbol};
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    type Open = unsafe extern "system" fn(*mut HANDLE) -> i32;
    type Load = unsafe extern "system" fn(HANDLE, *const u8, usize) -> i32;
    type Execute = unsafe extern "system" fn(
        HANDLE,
        *const i8,
        *const u64,
        usize,
        *mut u64,
        usize,
        *mut usize,
    ) -> i32;

    /// One PawnIO handle with one module loaded.
    struct PawnIo {
        lib: Library,
        handle: HANDLE,
    }

    // The handle is only used from the reading thread.
    unsafe impl Send for PawnIo {}

    fn library() -> Result<Library, String> {
        let mut tried = Vec::new();
        let base = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        for path in [
            base.join("PawnIO").join("PawnIOLib.dll"),
            PathBuf::from("PawnIOLib.dll"),
        ] {
            // SAFETY: loading the PawnIO driver's own user-mode library.
            match unsafe { Library::new(&path) } {
                Ok(l) => return Ok(l),
                Err(e) => tried.push(format!("{}: {e}", path.display())),
            }
        }
        Err(format!(
            "the PawnIO driver isn't installed ({})",
            tried.join("; ")
        ))
    }

    impl PawnIo {
        fn open(module: &[u8]) -> Result<PawnIo, String> {
            let lib = library()?;
            // SAFETY: documented PawnIOLib entry points, called with their
            // declared types; the module blob outlives the call.
            unsafe {
                let open: Symbol<Open> = lib.get(b"pawnio_open\0").map_err(|e| e.to_string())?;
                let load: Symbol<Load> = lib.get(b"pawnio_load\0").map_err(|e| e.to_string())?;
                let mut handle: HANDLE = std::ptr::null_mut();
                let hr = open(&mut handle);
                if hr < 0 {
                    return Err(format!("couldn't open the PawnIO driver (0x{hr:08x})"));
                }
                let hr = load(handle, module.as_ptr(), module.len());
                if hr < 0 {
                    return Err(format!(
                        "the PawnIO driver refused the sensor module (0x{hr:08x})"
                    ));
                }
                Ok(PawnIo { lib, handle })
            }
        }

        /// Runs a module function that may return nothing (`want` = 0).
        fn exec(&self, name: &str, input: &[u64], want: usize) -> Result<u64, String> {
            let cname = CString::new(name).map_err(|e| e.to_string())?;
            let mut out = [0u64; 1];
            let mut got = 0usize;
            // SAFETY: see `open`; buffers are sized as passed.
            let hr = unsafe {
                let exec: Symbol<Execute> = self
                    .lib
                    .get(b"pawnio_execute\0")
                    .map_err(|e| e.to_string())?;
                exec(
                    self.handle,
                    cname.as_ptr(),
                    input.as_ptr(),
                    input.len(),
                    out.as_mut_ptr(),
                    want,
                    &mut got,
                )
            };
            if hr < 0 || got < want {
                return Err(format!("{name} failed (0x{hr:08x})"));
            }
            Ok(out[0])
        }

        fn call(&self, name: &str, input: &[u64]) -> Result<u64, String> {
            let cname = CString::new(name).map_err(|e| e.to_string())?;
            let mut out = [0u64; 1];
            let mut got = 0usize;
            // SAFETY: see `open`; buffers are sized as passed.
            let hr = unsafe {
                let exec: Symbol<Execute> = self
                    .lib
                    .get(b"pawnio_execute\0")
                    .map_err(|e| e.to_string())?;
                exec(
                    self.handle,
                    cname.as_ptr(),
                    input.as_ptr(),
                    input.len(),
                    out.as_mut_ptr(),
                    out.len(),
                    &mut got,
                )
            };
            if hr < 0 || got == 0 {
                return Err(format!("{name} failed (0x{hr:08x})"));
            }
            Ok(out[0])
        }
    }

    enum Vendor {
        Amd,
        Intel,
    }

    fn cpu_vendor() -> Option<Vendor> {
        // SAFETY: CPUID is always available on x86-64.
        let (v, f) = unsafe {
            (
                core::arch::x86_64::__cpuid(0),
                core::arch::x86_64::__cpuid(1),
            )
        };
        let mut id = Vec::new();
        for r in [v.ebx, v.edx, v.ecx] {
            id.extend_from_slice(&r.to_le_bytes());
        }
        let base = (f.eax >> 8) & 0xF;
        let family = base + if base == 0xF { (f.eax >> 20) & 0xFF } else { 0 };
        match &id[..] {
            b"AuthenticAMD" if family >= 0x17 => Some(Vendor::Amd),
            b"GenuineIntel" => Some(Vendor::Intel),
            _ => None,
        }
    }

    /// Reads the CPU temperature, one way per CPU maker.
    struct Reader {
        io: PawnIo,
        vendor: Vendor,
        /// AMD: PCI access is shared with other tools (and Lumora's engine),
        /// which all take this named mutex first.
        pci_lock: HANDLE,
    }

    unsafe impl Send for Reader {}

    impl Reader {
        fn new() -> Result<Reader, String> {
            let vendor = cpu_vendor()
                .ok_or("this CPU isn't supported yet (AMD Ryzen and Intel Core are)")?;
            let dir = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default();
            let file = match vendor {
                Vendor::Amd => "AMDFamily17.bin",
                Vendor::Intel => "IntelMSR.bin",
            };
            let module =
                std::fs::read(dir.join(file)).map_err(|e| format!("{file} is missing ({e})"))?;
            let io = PawnIo::open(&module)?;
            let name: Vec<u16> = "Global\\Access_PCI\0".encode_utf16().collect();
            // SAFETY: creating (or opening) a named mutex.
            let pci_lock = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            Ok(Reader {
                io,
                vendor,
                pci_lock,
            })
        }

        fn read(&self) -> Result<f32, String> {
            match self.vendor {
                Vendor::Amd => {
                    // SAFETY: waiting on and releasing our own mutex handle.
                    let locked = !self.pci_lock.is_null() && {
                        let w = unsafe { WaitForSingleObject(self.pci_lock, 200) };
                        w == WAIT_OBJECT_0 || w == WAIT_ABANDONED
                    };
                    let r = self.io.call("ioctl_read_smn", &[0x0005_9800]);
                    if locked {
                        unsafe { ReleaseMutex(self.pci_lock) };
                    }
                    Ok(super::amd_tctl(r? as u32))
                }
                Vendor::Intel => {
                    let target = self.io.call("ioctl_read_msr", &[0x1A2])?;
                    let status = self.io.call("ioctl_read_msr", &[0x1B1])?;
                    Ok(super::intel_package(status, target))
                }
            }
        }
    }

    impl Reader {
        /// CPU package power from the energy counter (AMD and Intel RAPL),
        /// worked out from the change since the last reading.
        fn power(&self, last: &mut Option<(u64, std::time::Instant)>) -> Option<f32> {
            let (unit_msr, energy_msr) = match self.vendor {
                Vendor::Amd => (0xC001_0299u64, 0xC001_029Bu64),
                Vendor::Intel => (0x606, 0x611),
            };
            let unit = (self.io.call("ioctl_read_msr", &[unit_msr]).ok()? >> 8) & 0x1F;
            let raw = self.io.call("ioctl_read_msr", &[energy_msr]).ok()? & 0xFFFF_FFFF;
            let now = std::time::Instant::now();
            let prev = last.replace((raw, now));
            let (p, at) = prev?;
            let secs = now.duration_since(at).as_secs_f64();
            if secs < 0.2 {
                return None;
            }
            // The counter is 32 bits and wraps.
            let delta = if raw >= p {
                raw - p
            } else {
                raw + (1u64 << 32) - p
            };
            let watts = delta as f64 / (1u64 << unit) as f64 / secs;
            (0.0..1000.0).contains(&watts).then_some(watts as f32)
        }
    }

    /// The motherboard's Super I/O chip through PawnIO's `LpcIO` module,
    /// under the ISA-bus lock other monitoring tools share.
    struct Lpc {
        io: PawnIo,
        isa: HANDLE,
        locked: bool,
    }

    unsafe impl Send for Lpc {}

    impl Lpc {
        fn open() -> Result<Lpc, String> {
            let dir = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default();
            let module = std::fs::read(dir.join("LpcIO.bin"))
                .map_err(|e| format!("LpcIO.bin is missing ({e})"))?;
            let io = PawnIo::open(&module)?;
            let name: Vec<u16> = "Global\\Access_ISABUS.HTP.Method\0"
                .encode_utf16()
                .collect();
            // SAFETY: creating (or opening) a named mutex.
            let isa = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            Ok(Lpc {
                io,
                isa,
                locked: false,
            })
        }
    }

    impl lumora_core::cooling::board::SuperIo for Lpc {
        fn select_slot(&mut self, slot: u8) -> Result<(), String> {
            self.io
                .exec("ioctl_select_slot", &[slot as u64], 0)
                .map(|_| ())
        }
        fn find_bars(&mut self) -> Result<(), String> {
            self.io.exec("ioctl_find_bars", &[], 0).map(|_| ())
        }
        fn inb(&mut self, port: u16) -> Result<u8, String> {
            self.io
                .exec("ioctl_pio_inb", &[port as u64], 1)
                .map(|v| v as u8)
        }
        fn outb(&mut self, port: u16, value: u8) -> Result<(), String> {
            self.io
                .exec("ioctl_pio_outb", &[port as u64, value as u64], 0)
                .map(|_| ())
        }
        fn sio_inb(&mut self, reg: u8) -> Result<u8, String> {
            self.io
                .exec("ioctl_superio_inb", &[reg as u64], 1)
                .map(|v| v as u8)
        }
        fn sio_outb(&mut self, reg: u8, value: u8) -> Result<(), String> {
            self.io
                .exec("ioctl_superio_outb", &[reg as u64, value as u64], 0)
                .map(|_| ())
        }
        fn lock(&mut self) -> bool {
            if self.isa.is_null() {
                return true;
            }
            // SAFETY: waiting on our own mutex handle.
            let w = unsafe { WaitForSingleObject(self.isa, 200) };
            self.locked = w == WAIT_OBJECT_0 || w == WAIT_ABANDONED;
            self.locked
        }
        fn unlock(&mut self) {
            if self.locked {
                // SAFETY: releasing the mutex this thread holds.
                unsafe { ReleaseMutex(self.isa) };
                self.locked = false;
            }
        }
    }

    pub fn main() {
        // One copy at a time: a second one can't take the port and leaves.
        let Ok(listener) = TcpListener::bind(("127.0.0.1", super::PORT)) else {
            return;
        };

        // The cooler relay, beside the temperature.
        // Graphics card fans go back to the driver whenever Lumora goes quiet.
        lumora_core::cooling::gpu::start_watchdog();
        // Motherboard fan headers too, through PawnIO's LpcIO module.
        lumora_core::cooling::board::set_provider(Box::new(|| {
            Lpc::open().map(|l| Box::new(l) as Box<dyn lumora_core::cooling::board::SuperIo>)
        }));
        lumora_core::cooling::board::start_watchdog();
        if let Some(key_file) = lumora_core::cooling::relay::default_key_file() {
            std::thread::spawn(move || {
                let backend = lumora_core::cooling::usb::UsbBackend::new(None);
                lumora_core::cooling::relay::run(Box::new(backend), key_file);
            });
        }
        let line = Arc::new(Mutex::new(String::from("error starting\n")));

        let reading = line.clone();
        std::thread::spawn(move || loop {
            match Reader::new() {
                Ok(reader) => {
                    let mut energy: Option<(u64, std::time::Instant)> = None;
                    loop {
                        let mut text = match reader.read() {
                            Ok(t) if (-20.0..150.0).contains(&t) => format!("cpu {t:.3}\n"),
                            Ok(t) => format!("error the sensor gave an odd reading ({t})\n"),
                            Err(e) => format!("error {e}\n"),
                        };
                        if let Some(w) = reader.power(&mut energy) {
                            text.push_str(&format!("power {w:.1}\n"));
                        }
                        *reading.lock().unwrap_or_else(|e| e.into_inner()) = text;
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                Err(e) => {
                    // The driver may still be starting right after sign-in.
                    *reading.lock().unwrap_or_else(|e| e.into_inner()) = format!("error {e}\n");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        });

        for stream in listener.incoming().flatten() {
            let line = line.clone();
            std::thread::spawn(move || serve(stream, line));
        }
    }

    fn serve(mut stream: TcpStream, line: Arc<Mutex<String>>) {
        loop {
            let text = line.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if stream.write_all(text.as_bytes()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amd_register_to_celsius() {
        // 45.25 °C: 362 eighths in bits 31..21.
        assert_eq!(amd_tctl(362 << 21), 45.25);
        // The -49 °C range.
        assert_eq!(amd_tctl((800 << 21) | 0x80000), 51.0);
    }

    #[test]
    fn intel_degrees_below_tjmax() {
        assert_eq!(intel_package(30 << 16, 100 << 16), 70.0);
        assert_eq!(intel_package(30 << 16, 0), 70.0, "missing TjMax means 100");
    }
}
