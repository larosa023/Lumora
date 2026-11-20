//! The graphics card's fans, run like one more cooler.
//!
//! NVIDIA cards are read through the driver's own NVML library. Reading needs
//! nothing special, but setting a fan speed needs admin rights, so on Windows
//! Lumora's admin helper does the setting (over the same signed-in connection
//! as the cooler relay, see [`super::relay`]):
//!
//! ```text
//! → gpufan <percent>   ← ok | err <why>    every fan to this speed
//! → gpuauto            ← ok | err <why>    back to the driver's own control
//! → gpukeep            ← ok                "Lumora is still here"
//! ```
//!
//! The helper hands the fans back to the driver as soon as the connection
//! that set them closes, or when it hears nothing for a few seconds, so a
//! closed or crashed Lumora never leaves the card stuck at one speed.

use std::io;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::curve::SpeedSetting;
use super::family::Driver;
use super::{Backend, CoolerStatus, FoundCooler, Link};
use crate::temps::gpu::{Nvml, NO_PERMISSION};

pub const NVIDIA_VID: u16 = 0x10DE;

/// What the card is, found once per run.
#[derive(Debug, PartialEq)]
pub struct GpuModel {
    pub pid: u16,
    pub name: &'static str,
    pub fans: u8,
    /// The card's lowest allowed speed, in percent.
    pub min_duty: u8,
}

fn registered() -> &'static Mutex<Option<&'static GpuModel>> {
    static M: OnceLock<Mutex<Option<&'static GpuModel>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}

/// The card found on this PC (one per run, so leaking it is fine).
pub fn model(pid: u16) -> Option<&'static GpuModel> {
    registered()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .filter(|m| m.pid == pid)
}

fn register(m: GpuModel) -> &'static GpuModel {
    let mut slot = registered().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = *slot {
        if *old == m {
            return old;
        }
    }
    let leaked: &'static GpuModel = Box::leak(Box::new(m));
    *slot = Some(leaked);
    leaked
}

/// A cooler id for the card.
fn id_for(pid: u16) -> String {
    format!("gpu:{NVIDIA_VID:04x}:{pid:04x}:0")
}

// --- Helper side ------------------------------------------------------------

/// How long the helper keeps a speed without hearing from Lumora.
const KEEP_FOR: Duration = Duration::from_secs(10);

struct Service {
    nvml: Option<Nvml>,
    /// The connection that set the fans, and when it last spoke.
    owner: Option<(u64, Instant)>,
}

fn service() -> &'static Mutex<Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Service {
            nvml: None,
            owner: None,
        })
    })
}

fn with_nvml<T>(s: &mut Service, f: impl FnOnce(&Nvml) -> T) -> Result<T, String> {
    if s.nvml.is_none() {
        s.nvml = Nvml::open();
    }
    s.nvml
        .as_ref()
        .map(f)
        .ok_or_else(|| "no NVIDIA graphics card or driver found".to_string())
}

fn nvml_error(code: i32) -> String {
    match code {
        NO_PERMISSION => "the helper isn't allowed to set the fans".into(),
        3 => "this card or driver doesn't let other apps set its fans".into(),
        c => format!("the NVIDIA driver said no (code {c})"),
    }
}

/// Handles one helper command for connection `conn`; `None` when it isn't a
/// GPU command.
pub fn serve_command(conn: u64, cmd: &str, arg: &str) -> Option<String> {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    let answer = match cmd {
        "gpufan" => {
            let pct = arg.trim().parse::<u32>().unwrap_or(100).min(100);
            match with_nvml(&mut s, |n| n.set_fans(pct)) {
                Ok(0) => {
                    s.owner = Some((conn, Instant::now()));
                    "ok".to_string()
                }
                Ok(code) => format!("err {}", nvml_error(code)),
                Err(e) => format!("err {e}"),
            }
        }
        "gpuauto" => {
            s.owner = None;
            match with_nvml(&mut s, |n| n.auto_fans()) {
                Ok(0) => "ok".to_string(),
                Ok(code) => format!("err {}", nvml_error(code)),
                Err(e) => format!("err {e}"),
            }
        }
        "gpukeep" => {
            if let Some((c, at)) = s.owner.as_mut() {
                if *c == conn {
                    *at = Instant::now();
                }
            }
            "ok".to_string()
        }
        _ => return None,
    };
    Some(answer)
}

/// A connection closed: if it was running the fans, give them back.
pub fn connection_closed(conn: u64) {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    if s.owner.is_some_and(|(c, _)| c == conn) {
        s.owner = None;
        let _ = with_nvml(&mut s, |n| n.auto_fans());
    }
}

/// Gives the fans back to the driver when Lumora goes quiet. Runs in the
/// helper for as long as it lives.
pub fn start_watchdog() {
    std::thread::Builder::new()
        .name("lumora-gpu-fans".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
            if s.owner.is_some_and(|(_, at)| at.elapsed() > KEEP_FOR) {
                s.owner = None;
                let _ = with_nvml(&mut s, |n| n.auto_fans());
            }
        })
        .ok();
}

// --- Lumora side ------------------------------------------------------------

/// How Lumora sets the fans: through the admin helper (Windows) or itself.
enum Setter {
    Helper(super::relay::Client),
    Direct,
}

/// Finds the NVIDIA card and opens it as a cooler with one "fan" channel.
pub struct GpuBackend {
    nvml: Option<Nvml>,
    tried: bool,
    relay_key: Option<String>,
}

impl GpuBackend {
    /// `relay_key`: the key for Lumora's admin helper (Windows).
    pub fn new(relay_key: Option<String>) -> Self {
        GpuBackend {
            nvml: None,
            tried: false,
            relay_key,
        }
    }

    fn helper(&self) -> io::Result<super::relay::Client> {
        let key = self
            .relay_key
            .as_deref()
            .ok_or_else(|| io::Error::other("no helper key"))?;
        let mut c = super::relay::Client::connect(super::relay::PORT, key)?;
        // An older helper doesn't know the GPU commands yet.
        c.command("gpukeep").map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the helper is from an older Lumora",
            )
        })?;
        Ok(c)
    }
}

impl Backend for GpuBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        if !self.tried {
            self.tried = true;
            self.nvml = Nvml::open();
        }
        let Some(n) = self.nvml.as_ref() else {
            return Vec::new();
        };
        let fans = n.fan_count();
        if fans == 0 {
            return Vec::new();
        }
        let pid = n.pci_device().unwrap_or(0);
        let name = n.name().unwrap_or_else(|| "NVIDIA graphics card".into());
        let name = name.trim_start_matches("NVIDIA ").to_string();
        let (lo, _) = n.fan_range();
        register(GpuModel {
            pid,
            name: Box::leak(name.into_boxed_str()),
            fans: fans as u8,
            min_duty: lo.min(100) as u8,
        });
        let (needs_setup, problem) = if cfg!(windows) {
            match self.helper() {
                Ok(_) => (false, None),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => (
                    true,
                    Some("Lumora's helper is from an older version. Run the setup once to update it (one Windows prompt).".into()),
                ),
                Err(_) => (
                    true,
                    Some("Setting graphics card fan speeds needs Lumora's small helper with admin rights. One Windows prompt sets it up (the same helper that reads the CPU temperature).".into()),
                ),
            }
        } else {
            (false, None)
        };
        vec![FoundCooler {
            id: id_for(pid),
            vid: NVIDIA_VID,
            pid,
            serial: String::new(),
            needs_setup,
            needs_restart: false,
            problem,
        }]
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        if cooler.vid != NVIDIA_VID {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "not a graphics card",
            ));
        }
        Ok(Box::new(GpuLink {
            setter: Some(if cfg!(windows) {
                Setter::Helper(self.helper()?)
            } else {
                Setter::Direct
            }),
        }))
    }

    fn diagnose(&mut self) -> String {
        match self.nvml.as_ref() {
            None => "graphics card fans: no NVIDIA driver library".into(),
            Some(n) => format!(
                "graphics card fans: {} fan(s), {:?} % allowed, now {:?} %",
                n.fan_count(),
                n.fan_range(),
                n.fan_speeds()
            ),
        }
    }
}

/// Carries the way to set the fans from `open` to the driver.
pub struct GpuLink {
    setter: Option<Setter>,
}

impl Link for GpuLink {
    fn write(&mut self, _msg: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not a USB cooler",
        ))
    }
    fn read(&mut self, _timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn take_gpu(&mut self) -> Option<Box<dyn std::any::Any + Send>> {
        self.setter
            .take()
            .map(|s| Box::new(s) as Box<dyn std::any::Any + Send>)
    }
}

pub struct GpuFans {
    model: &'static GpuModel,
    setter: Option<Setter>,
    nvml: Option<Nvml>,
    /// Lumora set a speed (rather than leaving the driver in charge).
    manual: bool,
}

impl GpuFans {
    pub fn new(mut link: Box<dyn Link>, model: &'static GpuModel) -> Self {
        let setter = link
            .take_gpu()
            .and_then(|b| b.downcast::<Setter>().ok())
            .map(|b| *b);
        GpuFans {
            model,
            setter,
            nvml: None,
            manual: false,
        }
    }

    fn send(&mut self, line: &str) -> io::Result<()> {
        match self.setter.as_mut() {
            Some(Setter::Helper(c)) => c.command(line).map(|_| ()),
            Some(Setter::Direct) => {
                let n = self
                    .nvml
                    .as_ref()
                    .ok_or_else(|| io::Error::other("no NVIDIA driver"))?;
                let code = match line.split_once(' ') {
                    Some(("gpufan", pct)) => n.set_fans(pct.parse().unwrap_or(100)),
                    _ if line == "gpuauto" => n.auto_fans(),
                    _ => 0,
                };
                if code == 0 {
                    Ok(())
                } else {
                    Err(io::Error::other(nvml_error(code)))
                }
            }
            None => Err(io::Error::other("can't reach the fans")),
        }
    }
}

impl Driver for GpuFans {
    fn initialize(&mut self) -> io::Result<()> {
        self.nvml = Nvml::open();
        if self.nvml.is_none() {
            return Err(io::Error::other("the NVIDIA driver didn't answer"));
        }
        Ok(())
    }

    fn firmware(&self) -> Option<String> {
        None
    }

    fn status(&mut self) -> io::Result<CoolerStatus> {
        if self.manual {
            // Tells the helper Lumora is still running the fans.
            self.send("gpukeep")?;
        }
        let n = self
            .nvml
            .as_ref()
            .ok_or_else(|| io::Error::other("no NVIDIA driver"))?;
        let speeds = n.fan_speeds();
        let avg = (!speeds.is_empty())
            .then(|| (speeds.iter().sum::<u32>() / speeds.len() as u32).min(100) as u8);
        Ok(CoolerStatus {
            liquid_c: None,
            pump_rpm: None,
            pump_duty: None,
            fan_rpm: None,
            fan_duty: avg,
            fan_rpms: Vec::new(),
            fan_duties: Vec::new(),
            fans_connected: Vec::new(),
            pump_mode: None,
            fault: false,
        })
    }

    /// A fixed duty is set as is; anything else hands the fans back to the
    /// driver (which is what a "liquid" curve means for a graphics card).
    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()> {
        if channel != "fan" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown channel",
            ));
        }
        match setting {
            SpeedSetting::Fixed { duty } => {
                let duty = (*duty).clamp(self.model.min_duty, 100);
                self.send(&format!("gpufan {duty}"))?;
                self.manual = true;
            }
            _ => {
                self.send("gpuauto")?;
                self.manual = false;
            }
        }
        Ok(())
    }
}

impl Drop for GpuFans {
    fn drop(&mut self) {
        if self.manual {
            let _ = self.send("gpuauto");
        }
    }
}
