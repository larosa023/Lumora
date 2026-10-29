//! Settings, profiles and the last session, stored as small JSON files in the
//! app's config folder.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::effects::{Direction, EffectConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DemoMode {
    /// Show demo devices only while no real devices are connected.
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    /// Render rate for animated effects.
    pub fps: u32,
    pub theme: String,
    pub accent: Rgb,
    /// Tint the interface with the current lighting colors.
    pub accent_follows_lighting: bool,
    pub demo_devices: DemoMode,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    /// Look for new Lumora versions in the background.
    pub check_for_updates: bool,
    /// Run the hardware engine that ships inside Lumora (Windows). Off means
    /// "connect to an OpenRGB I run myself".
    pub builtin_engine: bool,
    /// Per-device choice of whether Lumora drives the lights, by device id.
    /// Devices not listed use [`default_control`].
    pub device_control: BTreeMap<String, bool>,
    /// Cooling choices per cooler (takeover, curves, screen).
    pub coolers: BTreeMap<String, crate::cooling::CoolerConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            host: "127.0.0.1".into(),
            port: crate::openrgb::DEFAULT_PORT,
            fps: 30,
            theme: "dark".into(),
            accent: Rgb::new(0x7c, 0x5c, 0xff),
            accent_follows_lighting: false,
            demo_devices: DemoMode::Auto,
            start_minimized: false,
            close_to_tray: true,
            check_for_updates: true,
            builtin_engine: true,
            device_control: BTreeMap::new(),
            coolers: BTreeMap::new(),
        }
    }
}

/// Vendors whose own app usually stays installed for cooling or screens, so
/// Lumora leaves their lighting alone until asked.
pub const LEAVE_ALONE_BY_DEFAULT: &[&str] = &["NZXT"];

pub fn default_control(vendor: &str) -> bool {
    !LEAVE_ALONE_BY_DEFAULT.iter().any(|v| v.eq_ignore_ascii_case(vendor.trim()))
}

impl Settings {
    /// Whether Lumora drives this device's lights.
    pub fn controls(&self, id: &str, vendor: &str) -> bool {
        self.device_control.get(id).copied().unwrap_or_else(|| default_control(vendor))
    }

    pub fn sanitized(mut self) -> Self {
        self.fps = self.fps.clamp(10, 60);
        if self.host.trim().is_empty() {
            self.host = Settings::default().host;
        }
        self.host = self.host.trim().to_string();
        if self.port == 0 {
            self.port = crate::openrgb::DEFAULT_PORT;
        }
        self
    }
}

/// A saved lighting setup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Applied to every device without its own override.
    pub global: EffectConfig,
    /// Per-device effects, keyed by device id.
    pub overrides: BTreeMap<String, EffectConfig>,
    /// 0..=100
    pub brightness: u8,
}

impl Default for Profile {
    fn default() -> Self {
        Profile { name: String::new(), global: EffectConfig::default(), overrides: BTreeMap::new(), brightness: 100 }
    }
}

/// What was showing when the app last closed, restored on launch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub lighting: Profile,
    pub enabled: bool,
    pub active_profile: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Session { lighting: Profile::default(), enabled: true, active_profile: None }
    }
}

fn preset(name: &str, effect: &str, colors: &[&str], speed: u8, size: u8, direction: Direction) -> Profile {
    Profile {
        name: name.into(),
        global: EffectConfig {
            effect: effect.into(),
            colors: colors.iter().filter_map(|c| Rgb::from_hex(c)).collect(),
            speed,
            size,
            direction,
            ..EffectConfig::default()
        },
        overrides: BTreeMap::new(),
        brightness: 100,
    }
}

/// Profiles created on first launch.
pub fn starter_profiles() -> Vec<Profile> {
    vec![
        preset("Neon Drift", "color_wave", &["#7c5cff", "#00d4ff", "#ff3da8"], 40, 50, Direction::Right),
        preset("Arctic", "aurora", &["#e8fbff", "#00d4ff", "#3d6bff"], 25, 60, Direction::Right),
        preset("Ember", "fire", &["#ff1e00", "#ff8a00", "#ffe066"], 45, 55, Direction::Up),
        preset("Focus", "static", &["#ffd7a8"], 0, 50, Direction::Right),
        preset("Rainbow", "rainbow_wave", &[], 45, 50, Direction::Right),
    ]
}

/// JSON files in one folder. Writes go to a temp file first so a crash never
/// leaves a half-written config behind.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn load<T: DeserializeOwned + Default>(&self, file: &str) -> Option<T> {
        let text = fs::read_to_string(self.dir.join(file)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save<T: Serialize>(&self, file: &str, value: &T) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(file);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        fs::rename(tmp, path)
    }

    pub fn settings(&self) -> Settings {
        self.load::<Settings>("settings.json").unwrap_or_default().sanitized()
    }

    pub fn save_settings(&self, s: &Settings) -> io::Result<()> {
        self.save("settings.json", s)
    }

    pub fn profiles(&self) -> Vec<Profile> {
        self.load::<Vec<Profile>>("profiles.json").unwrap_or_else(starter_profiles)
    }

    pub fn save_profiles(&self, p: &[Profile]) -> io::Result<()> {
        self.save("profiles.json", &p)
    }

    pub fn session(&self) -> Session {
        self.load::<Session>("session.json").unwrap_or_default()
    }

    pub fn save_session(&self, s: &Session) -> io::Result<()> {
        self.save("session.json", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("lumora-store-{}", std::process::id()));
        let store = Store::new(&dir);
        assert_eq!(store.settings(), Settings::default());
        assert_eq!(store.profiles().len(), starter_profiles().len());

        let s = Settings { theme: "oled".into(), fps: 500, ..Default::default() };
        store.save_settings(&s).unwrap();
        let loaded = store.settings();
        assert_eq!(loaded.theme, "oled");
        assert_eq!(loaded.fps, 60, "fps is clamped");

        // Older/partial files still load thanks to serde defaults.
        fs::write(dir.join("session.json"), r#"{"enabled":false}"#).unwrap();
        assert!(!store.session().enabled);
        assert_eq!(store.session().lighting.brightness, 100);
        let _ = fs::remove_dir_all(dir);
    }
}
