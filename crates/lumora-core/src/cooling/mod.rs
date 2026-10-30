//! Liquid coolers: live temperatures and speeds, pump and fan curves, and the
//! pump screen.
//!
//! Lighting goes through the bundled engine, but cooling doesn't: each cooler
//! here is driven directly. A cooler is left alone (to NZXT CAM, iCUE, …)
//! until the user turns on "Let Lumora run this cooler", because these devices
//! only answer one app at a time.
//!
//! Protocol knowledge for the NZXT Kraken family and the Corsair Hydro Pro
//! family comes from the open research documented by the liquidctl project
//! and the Linux `nzxt-kraken3` driver. The implementation here is Lumora's own.

pub mod curve;
pub mod fake;
pub mod family;
#[cfg(feature = "hid")]
pub mod hid;
pub mod hydro;
pub mod kraken;
pub mod manager;
pub mod screen;
#[cfg(feature = "usb")]
pub mod usb;

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use curve::{Curve, Preset, SpeedSetting};
pub use manager::{CoolerView, CoolingManager};

/// A raw connection to a cooler: send a message, read what comes back.
/// HID coolers send 64-byte reports (first byte is the report id); USB
/// coolers send bulk messages.
pub trait Link: Send {
    fn write(&mut self, msg: &[u8]) -> io::Result<()>;
    /// Returns `Ok(None)` on timeout.
    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

/// A one-way bulk connection for large transfers (pictures for a Kraken's
/// screen). Separate from the [`Link`], which carries commands.
pub trait Bulk: Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
}

/// A cooler found on the system (not necessarily opened).
#[derive(Clone, Debug, PartialEq)]
pub struct FoundCooler {
    /// Stable id: vendor, product and serial.
    pub id: String,
    pub vid: u16,
    pub pid: u16,
    pub serial: String,
    /// The OS can see the cooler but Lumora can't talk to it until a one-time
    /// setup (on Windows: the UsbDk access driver for Corsair Hydro Pro).
    pub needs_setup: bool,
}

/// Finds coolers and opens them. The real implementation uses the OS HID
/// stack; tests use [`fake::FakeBackend`].
pub trait Backend: Send {
    fn discover(&mut self) -> Vec<FoundCooler>;
    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>>;
    /// Opens the cooler's bulk connection, for coolers that take pictures.
    fn open_bulk(&mut self, _cooler: &FoundCooler) -> io::Result<Box<dyn Bulk>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "this cooler doesn't take pictures"))
    }
}

/// Several backends as one (HID for NZXT, raw USB for Corsair Hydro Pro).
pub struct MultiBackend {
    backends: Vec<Box<dyn Backend>>,
    owners: BTreeMap<String, usize>,
}

impl MultiBackend {
    pub fn new(backends: Vec<Box<dyn Backend>>) -> Self {
        MultiBackend { backends, owners: BTreeMap::new() }
    }
}

impl Backend for MultiBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        self.owners.clear();
        let mut all = Vec::new();
        for (i, b) in self.backends.iter_mut().enumerate() {
            for f in b.discover() {
                self.owners.insert(f.id.clone(), i);
                all.push(f);
            }
        }
        all
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        let i = *self.owners.get(&cooler.id).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler was unplugged"))?;
        self.backends[i].open(cooler)
    }

    fn open_bulk(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Bulk>> {
        let i = *self.owners.get(&cooler.id).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler was unplugged"))?;
        self.backends[i].open_bulk(cooler)
    }
}

/// The vendor app that normally owns a cooler, by USB vendor id.
pub fn owner_app(vid: u16) -> Option<&'static str> {
    match vid {
        0x1E71 => Some("NZXT CAM"),
        0x1B1C => Some("Corsair iCUE"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScreenMode {
    /// The cooler's own liquid temperature display.
    #[default]
    Liquid,
    /// Leave whatever is on the screen alone.
    Unchanged,
    /// A color or blend of colors drawn by Lumora.
    Color,
    /// A picture the user chose.
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenSettings {
    pub mode: ScreenMode,
    /// 0..=100
    pub brightness: u8,
    /// 0, 90, 180 or 270
    pub rotation: u16,
    /// Colors for [`ScreenMode::Color`], `#rrggbb`, one to three.
    pub colors: Vec<String>,
    /// Picture for [`ScreenMode::Image`]: a file of raw RGBA pixels at the
    /// screen's size, saved by the app. A new pick gets a new file name.
    pub image: Option<String>,
}

impl Default for ScreenSettings {
    fn default() -> Self {
        ScreenSettings {
            mode: ScreenMode::Liquid,
            brightness: 80,
            rotation: 0,
            colors: vec!["#7c5cff".into(), "#00d2ff".into()],
            image: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LightMode {
    /// One color.
    #[default]
    Static,
    /// Fades between the colors.
    Shift,
    /// Breathes each color in turn.
    Pulse,
    /// Blinks each color in turn.
    Blink,
    /// Follows the color of Lumora's main effect (about once a second).
    Sync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LightSpeed {
    Slower,
    #[default]
    Normal,
    Faster,
}

/// Lighting a cooler runs by itself (the Corsair Hydro Pro's pump logo).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightSettings {
    pub mode: LightMode,
    /// `#rrggbb`, up to four.
    pub colors: Vec<String>,
    pub speed: LightSpeed,
}

impl Default for LightSettings {
    fn default() -> Self {
        LightSettings { mode: LightMode::Static, colors: vec!["#7c5cff".into(), "#00d2ff".into()], speed: LightSpeed::Normal }
    }
}

/// What the user chose for one cooler. Saved with the app settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CoolerConfig {
    /// Lumora drives the cooler (instead of its vendor app).
    pub takeover: bool,
    /// Per channel ("pump", "fan").
    pub speeds: BTreeMap<String, SpeedSetting>,
    pub screen: ScreenSettings,
    pub light: LightSettings,
}

/// One reading from a cooler.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CoolerStatus {
    pub liquid_c: Option<f32>,
    pub pump_rpm: Option<u32>,
    pub pump_duty: Option<u8>,
    pub fan_rpm: Option<u32>,
    pub fan_duty: Option<u8>,
    /// Each fan's speed, when the cooler reports them one by one.
    pub fan_rpms: Vec<u32>,
    /// The pump's built-in speed mode, for pumps that only have modes.
    pub pump_mode: Option<String>,
    /// The cooler reported a fault (often: USB connected but no SATA power).
    pub fault: bool,
}
