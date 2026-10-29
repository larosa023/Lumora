//! Liquid coolers: live temperatures and speeds, pump and fan curves, and the
//! pump screen.
//!
//! Lighting goes through the bundled engine, but cooling doesn't: each cooler
//! here is driven directly. A cooler is left alone (to NZXT CAM, iCUE, …)
//! until the user turns on "Let Lumora run this cooler", because these devices
//! only answer one app at a time.
//!
//! Protocol knowledge for the NZXT Kraken family comes from the open research
//! documented by the liquidctl project and the Linux `nzxt-kraken3` driver.
//! The implementation here is Lumora's own.

pub mod curve;
pub mod fake;
#[cfg(feature = "hid")]
pub mod hid;
pub mod kraken;
pub mod manager;

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use curve::{Curve, Preset, SpeedSetting};
pub use manager::{CoolerView, CoolingManager};

/// A raw HID connection: 64-byte reports, first byte is the report id.
pub trait HidIo: Send {
    fn write(&mut self, report: &[u8]) -> io::Result<()>;
    /// Returns `Ok(None)` on timeout.
    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

/// A cooler found on the system (not necessarily opened).
#[derive(Clone, Debug, PartialEq)]
pub struct FoundCooler {
    /// Stable id: vendor, product and serial.
    pub id: String,
    pub vid: u16,
    pub pid: u16,
    pub serial: String,
}

/// Finds coolers and opens them. The real implementation uses the OS HID
/// stack; tests use [`fake::FakeBackend`].
pub trait Backend: Send {
    fn discover(&mut self) -> Vec<FoundCooler>;
    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn HidIo>>;
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenSettings {
    pub mode: ScreenMode,
    /// 0..=100
    pub brightness: u8,
    /// 0, 90, 180 or 270
    pub rotation: u16,
}

impl Default for ScreenSettings {
    fn default() -> Self {
        ScreenSettings { mode: ScreenMode::Liquid, brightness: 80, rotation: 0 }
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
}

/// One reading from a cooler.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CoolerStatus {
    pub liquid_c: Option<f32>,
    pub pump_rpm: Option<u32>,
    pub pump_duty: Option<u8>,
    pub fan_rpm: Option<u32>,
    pub fan_duty: Option<u8>,
    /// The cooler reported a fault (often: USB connected but no SATA power).
    pub fault: bool,
}
