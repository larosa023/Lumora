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
    /// LED counts the user set for resizable zones (ARGB headers), by device
    /// id then zone index, so they come back even if the engine forgets.
    pub zone_sizes: BTreeMap<String, BTreeMap<usize, usize>>,
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
            zone_sizes: BTreeMap::new(),
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

    /// Takes this window's choices but keeps what the app itself manages
    /// (per-device control, coolers, zone sizes) from `current`, so a window
    /// holding an older copy can never undo those.
    pub fn merged_into(mut self, current: &Settings) -> Self {
        self.device_control = current.device_control.clone();
        self.coolers = current.coolers.clone();
        self.zone_sizes = current.zone_sizes.clone();
        self
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

/// JSON files in one folder.
///
/// Nothing the user set should ever silently reset:
/// * writes go to a temp file, are flushed to disk, then replace the old file,
///   and the previous good copy is kept as `<name>.bak`;
/// * reading falls back to the `.bak` if the main file is unreadable;
/// * a file that's partly unreadable (say, a value written by a newer
///   version) keeps every part that still reads, instead of starting over;
///   the original is kept as `<name>.unreadable` for troubleshooting.
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

    fn load<T: DeserializeOwned + Serialize + Default>(&self, file: &str) -> Option<T> {
        let path = self.dir.join(file);
        let backup = self.dir.join(format!("{file}.bak"));
        let value = read_json(&path).or_else(|| read_json(&backup))?;
        if let Ok(v) = serde_json::from_value::<T>(value.clone()) {
            return Some(v);
        }
        // Keep the original for troubleshooting, then salvage what reads.
        let _ = fs::copy(&path, self.dir.join(format!("{file}.unreadable")));
        Some(salvage(value))
    }

    fn save<T: Serialize>(&self, file: &str, value: &T) -> io::Result<()> {
        use std::io::Write;
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(file);
        let tmp = self.dir.join(format!("{file}.tmp"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(value)?)?;
            f.sync_all()?;
        }
        if path.is_file() {
            let _ = fs::copy(&path, self.dir.join(format!("{file}.bak")));
        }
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

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Rebuilds a value from JSON that doesn't fully read: every part that reads
/// on its own is kept, the rest falls back to defaults. Works on objects key
/// by key (recursively) and on lists item by item.
fn salvage<T: DeserializeOwned + Serialize + Default>(value: serde_json::Value) -> T {
    use serde_json::Value;
    let default = serde_json::to_value(T::default()).unwrap_or(Value::Null);
    let fixed = salvage_value(&default, value, &|v| serde_json::from_value::<T>(v.clone()).is_ok());
    serde_json::from_value(fixed).unwrap_or_default()
}

/// Merges `value` into `base` one piece at a time, keeping only pieces that
/// leave the whole thing readable (checked by `ok`).
fn salvage_value(base: &serde_json::Value, value: serde_json::Value, ok: &dyn Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    use serde_json::Value;
    match (base, value) {
        (Value::Object(b), Value::Object(v)) => {
            let mut out = b.clone();
            for (k, item) in v {
                let mut trial = Value::Object(out.clone());
                trial[k.as_str()] = item.clone();
                if ok(&trial) {
                    out.insert(k, item);
                    continue;
                }
                // Not readable as a whole: try its parts.
                let inner_base = out.get(&k).cloned().unwrap_or(Value::Null);
                let candidate = match (&inner_base, &item) {
                    (Value::Object(_), Value::Object(_)) | (Value::Null, Value::Object(_)) => {
                        let start = if inner_base.is_null() { Value::Object(Default::default()) } else { inner_base.clone() };
                        Some(salvage_value(&start, item, &|piece| {
                            let mut t = Value::Object(out.clone());
                            t[k.as_str()] = piece.clone();
                            ok(&t)
                        }))
                    }
                    (_, Value::Array(items)) => {
                        let mut kept: Vec<Value> = Vec::new();
                        for it in items {
                            let mut t = Value::Object(out.clone());
                            let mut list = kept.clone();
                            list.push(it.clone());
                            t[k.as_str()] = Value::Array(list);
                            if ok(&t) {
                                kept.push(it.clone());
                            }
                        }
                        Some(Value::Array(kept))
                    }
                    _ => None,
                };
                if let Some(c) = candidate {
                    let mut t = Value::Object(out.clone());
                    t[k.as_str()] = c.clone();
                    if ok(&t) {
                        out.insert(k, c);
                    }
                }
            }
            Value::Object(out)
        }
        (_, Value::Array(items)) => {
            let mut kept = Vec::new();
            for it in items {
                let mut list = kept.clone();
                list.push(it.clone());
                if ok(&Value::Array(list)) {
                    kept.push(it);
                }
            }
            Value::Array(kept)
        }
        (_, v) if ok(&v) => v,
        (b, _) => b.clone(),
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

    #[test]
    fn a_bad_value_keeps_everything_else() {
        let dir = std::env::temp_dir().join(format!("lumora-store-salvage-{}", std::process::id()));
        let store = Store::new(&dir);
        let mut s = Settings { theme: "frost".into(), start_minimized: true, ..Default::default() };
        s.device_control.insert("dev1".into(), false);
        s.coolers.insert("hid:a".into(), crate::cooling::CoolerConfig { takeover: true, ..Default::default() });
        s.coolers.insert("hid:b".into(), crate::cooling::CoolerConfig { takeover: true, ..Default::default() });
        store.save_settings(&s).unwrap();
        // Say a newer version wrote a screen mode this one doesn't know.
        let path = dir.join("settings.json");
        let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["coolers"]["hid:b"]["screen"]["mode"] = "hologram".into();
        fs::write(&path, v.to_string()).unwrap();

        let loaded = store.settings();
        assert_eq!(loaded.theme, "frost");
        assert!(loaded.start_minimized);
        assert_eq!(loaded.device_control.get("dev1"), Some(&false));
        assert!(loaded.coolers["hid:a"].takeover, "the readable cooler is kept");
        assert!(loaded.coolers["hid:b"].takeover, "and the rest of the other one");
        assert!(dir.join("settings.json.unreadable").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_older_window_copy_cant_undo_cooler_or_device_choices() {
        let mut current = Settings::default();
        current.coolers.insert("hid:kraken".into(), crate::cooling::CoolerConfig { takeover: true, ..Default::default() });
        current.device_control.insert("nzxt".into(), true);
        current.zone_sizes.entry("board".into()).or_default().insert(1, 24);
        // The window opened before those changes, then the user picked a theme.
        let from_window = Settings { theme: "frost".into(), ..Default::default() };
        let merged = from_window.merged_into(&current);
        assert_eq!(merged.theme, "frost");
        assert!(merged.coolers["hid:kraken"].takeover);
        assert_eq!(merged.device_control.get("nzxt"), Some(&true));
        assert_eq!(merged.zone_sizes["board"][&1], 24);
    }

    #[test]
    fn a_broken_file_falls_back_to_the_backup() {
        let dir = std::env::temp_dir().join(format!("lumora-store-bak-{}", std::process::id()));
        let store = Store::new(&dir);
        store.save_settings(&Settings { theme: "oled".into(), ..Default::default() }).unwrap();
        store.save_settings(&Settings { theme: "midnight".into(), ..Default::default() }).unwrap();
        fs::write(dir.join("settings.json"), "{ half written").unwrap();
        assert_eq!(store.settings().theme, "oled", "previous good copy");
        let _ = fs::remove_dir_all(dir);
    }
}
