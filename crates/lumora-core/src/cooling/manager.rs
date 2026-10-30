//! Finds coolers, takes them over when asked, keeps their settings applied
//! and reports live readings.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::curve::{self, SpeedSetting};
use super::family::{ChannelKind, Driver, Model};
use super::{owner_app, Backend, CoolerConfig, CoolerStatus, FoundCooler, LightMode, LightSettings, ScreenMode, ScreenSettings};
use crate::temps;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoolerState {
    /// Left to its own app.
    Idle,
    /// Lumora needs a one-time setup before it can reach this cooler.
    NeedsSetup,
    Connecting,
    Active,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub min_duty: u8,
    pub kind: ChannelKind,
    pub setting: SpeedSetting,
    /// The 20..=59 °C duty table that is (or will be) on the cooler.
    pub table: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScreenView {
    pub width: u16,
    pub height: u16,
    pub settings: ScreenSettings,
}

/// Everything the UI shows about one cooler.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CoolerView {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub owner_app: Option<String>,
    /// The owner app is running right now (it will fight Lumora).
    pub owner_running: bool,
    pub takeover: bool,
    pub state: CoolerState,
    pub message: String,
    /// Something that didn't work but doesn't stop the cooler (a picture
    /// that wouldn't go on the screen).
    pub notice: String,
    pub firmware: Option<String>,
    pub status: Option<CoolerStatus>,
    pub channels: Vec<ChannelView>,
    pub screen: Option<ScreenView>,
    /// Lighting the cooler runs itself, if it has any.
    pub light: Option<LightSettings>,
    /// Lumora can't reach this cooler until a one-time setup.
    pub needs_setup: bool,
    /// Set up, but Windows needs one restart before Lumora can reach it.
    pub needs_restart: bool,
}

/// What was last written to a cooler, so unchanged settings aren't resent.
#[derive(Clone, PartialEq, Default)]
struct Applied {
    speeds: BTreeMap<String, SpeedSetting>,
    screen: Option<ScreenSettings>,
    light: Option<LightSettings>,
}

struct Runtime {
    found: FoundCooler,
    model: Model,
    driver: Option<Box<dyn Driver>>,
    firmware: Option<String>,
    state: CoolerState,
    message: String,
    notice: String,
    status: Option<CoolerStatus>,
    applied: Applied,
    retry_at: Instant,
    last_write: Instant,
    /// The numbers on the screen Lumora draws, and when they were drawn.
    screen_value: Vec<Option<i32>>,
    screen_drawn: Instant,
}

/// Lists running vendor apps (process names), injected so tests don't depend
/// on the OS.
pub type OwnerCheck = Box<dyn Fn() -> HashSet<String> + Send>;
pub type Listener = Arc<dyn Fn(Vec<CoolerView>) + Send + Sync>;

struct Inner {
    configs: BTreeMap<String, CoolerConfig>,
    coolers: Vec<Runtime>,
    running_apps: HashSet<String>,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    stop: AtomicBool,
    listener: Listener,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct CoolingManager {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

const POLL: Duration = Duration::from_secs(1);
const DISCOVER_EVERY: Duration = Duration::from_secs(5);
const OWNER_CHECK_EVERY: Duration = Duration::from_secs(10);
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Curves are written whole; spacing writes out keeps the firmware happy.
const MIN_WRITE_GAP: Duration = Duration::from_millis(800);
/// Lumora's own temperature screen is redrawn at most this often.
const LIVE_SCREEN_EVERY: Duration = if cfg!(test) { Duration::from_millis(1500) } else { Duration::from_secs(5) };
/// The shown number only changes when the reading moves this far from it,
/// so a reading sitting on x.5 doesn't flip back and forth.
const LIVE_HYSTERESIS: f32 = 0.7;

/// How each owner app's processes start (`NZXT CAM.exe`, `iCUE.exe`, and
/// helpers that share the name).
fn owner_processes(app: &str) -> &'static [&'static str] {
    match app {
        "NZXT CAM" => &["nzxt cam"],
        "Corsair iCUE" => &["icue"],
        _ => &[],
    }
}

fn owner_is_running(app: &str, running: &HashSet<String>) -> bool {
    owner_processes(app).iter().any(|stem| running.iter().any(|p| p.starts_with(stem)))
}

/// Windows refuses writes when another app opened the cooler without sharing
/// it, which shows up as "Access is denied".
fn is_locked(e: &std::io::Error) -> bool {
    let text = e.to_string();
    e.kind() == std::io::ErrorKind::PermissionDenied || text.contains("os error 5") || text.contains("Access is denied")
}

fn open_error_message(e: &std::io::Error, owner: Option<&str>) -> String {
    let app = owner.unwrap_or("another app");
    if is_locked(e) {
        format!("Another app has the cooler locked, usually {app}, even when it's only in the system tray. Quit it fully (right-click its tray icon, then Exit, or end it in Task Manager). Lumora reconnects by itself.")
    } else if e.kind() == std::io::ErrorKind::TimedOut {
        format!("The cooler isn't answering. {app} may be using it.")
    } else {
        format!("Couldn't open the cooler: {e}")
    }
}

impl CoolingManager {
    pub fn new(backend: Box<dyn Backend>, configs: BTreeMap<String, CoolerConfig>, owner_check: OwnerCheck, listener: Listener) -> CoolingManager {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner { configs, coolers: Vec::new(), running_apps: HashSet::new() }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            listener,
        });
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name("lumora-cooling".into())
            .spawn(move || run(s, backend, owner_check))
            .expect("spawn cooling thread");
        CoolingManager { shared, thread: Mutex::new(Some(thread)) }
    }

    pub fn views(&self) -> Vec<CoolerView> {
        views(&self.shared.lock())
    }

    pub fn configs(&self) -> BTreeMap<String, CoolerConfig> {
        self.shared.lock().configs.clone()
    }

    /// Changes one cooler's settings; they're applied within a second.
    pub fn set_config(&self, id: &str, config: CoolerConfig) {
        self.shared.lock().configs.insert(id.to_string(), config);
        self.shared.wake.notify_all();
    }

    /// Stops the cooling thread. Screens showing Lumora's own temperature go
    /// back to the cooler's built-in one, so no stale number is left behind.
    pub fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

impl Drop for CoolingManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn desired(model: &Model, cfg: &CoolerConfig) -> Applied {
    let speeds = model
        .channels()
        .into_iter()
        .map(|ch| (ch.id.to_string(), cfg.speeds.get(ch.id).cloned().unwrap_or_default()))
        .collect();
    Applied {
        speeds,
        screen: model.screen().map(|_| cfg.screen.clone()),
        light: model.light().then(|| resolve_light(&cfg.light)),
    }
}

/// "Match Lumora" becomes a plain color: the main effect's current color,
/// rounded so tiny flickers don't cause a stream of USB writes.
fn resolve_light(l: &LightSettings) -> LightSettings {
    if l.mode != LightMode::Sync {
        return l.clone();
    }
    let c = crate::ambient::get().unwrap_or(crate::Rgb::BLACK);
    LightSettings { mode: LightMode::Sync, colors: vec![sync_hex(c)], speed: l.speed }
}

fn sync_hex(c: crate::Rgb) -> String {
    let q = |v: u8| ((v as u16 + 4) / 8 * 8).min(255) as u8;
    format!("#{:02x}{:02x}{:02x}", q(c.r), q(c.g), q(c.b))
}

fn views(inner: &Inner) -> Vec<CoolerView> {
    inner
        .coolers
        .iter()
        .map(|r| {
            let cfg = inner.configs.get(&r.found.id).cloned().unwrap_or_default();
            let owner = owner_app(r.found.vid);
            CoolerView {
                id: r.found.id.clone(),
                name: r.model.name().to_string(),
                vendor: r.model.vendor().into(),
                owner_app: owner.map(String::from),
                owner_running: owner.is_some_and(|a| {
                    owner_is_running(a, &inner.running_apps)
                }),
                takeover: cfg.takeover,
                state: r.state,
                message: r.message.clone(),
                notice: r.notice.clone(),
                firmware: r.firmware.clone(),
                status: r.status.clone(),
                channels: r
                    .model
                    .channels()
                    .into_iter()
                    .map(|ch| {
                        let setting = cfg.speeds.get(ch.id).cloned().unwrap_or_default();
                        ChannelView {
                            id: ch.id.into(),
                            name: ch.name.into(),
                            min_duty: ch.min_duty,
                            kind: ch.kind,
                            table: curve::table(&setting, ch.id, ch.min_duty).to_vec(),
                            setting,
                        }
                    })
                    .collect(),
                screen: r.model.screen().map(|(w, h)| ScreenView { width: w, height: h, settings: cfg.screen.clone() }),
                light: r.model.light().then(|| cfg.light.clone()),
                needs_setup: r.found.needs_setup,
                needs_restart: r.found.needs_restart,
            }
        })
        .collect()
}

/// Brings one taken-over cooler in line with its settings and reads it.
fn service(r: &mut Runtime, cfg: &CoolerConfig, backend: &mut dyn Backend) {
    let now = Instant::now();
    if r.found.needs_setup {
        r.state = CoolerState::NeedsSetup;
        r.message = "Lumora needs a one-time setup before it can reach this cooler.".into();
        return;
    }
    if r.found.needs_restart {
        r.state = CoolerState::NeedsSetup;
        r.message = "Setup is done. Restart your PC once so Windows lets Lumora reach this cooler.".into();
        return;
    }
    if r.driver.is_none() {
        if now < r.retry_at {
            return;
        }
        r.state = CoolerState::Connecting;
        let model = r.model;
        let opened = backend.open(&r.found).and_then(|link| {
            let mut d = model.driver(link);
            d.initialize()?;
            Ok(d)
        });
        match opened {
            Ok(d) => {
                r.firmware = d.firmware();
                r.driver = Some(d);
                r.applied = Applied::default();
                r.state = CoolerState::Active;
                r.message.clear();
            }
            Err(e) => {
                r.state = CoolerState::Error;
                r.message = open_error_message(&e, owner_app(r.found.vid));
                r.retry_at = now + RETRY_AFTER;
                return;
            }
        }
    }

    let want = desired(&r.model, cfg);
    // Readings Lumora draws on the screen: the numbers to show right now.
    let live: Option<Vec<Option<i32>>> = want.screen.as_ref().filter(|s| s.shows_readings()).map(|s| {
        s.sources()
            .iter()
            .enumerate()
            .map(|(i, src)| {
                let reading = if src == "liquid" { r.status.as_ref().and_then(|st| st.liquid_c) } else { temps::get(src) };
                match (reading, r.screen_value.get(i).copied().flatten()) {
                    (Some(t), Some(shown)) if (t - shown as f32).abs() < LIVE_HYSTERESIS => Some(shown),
                    (Some(t), _) => Some(t.round() as i32),
                    (None, _) => None,
                }
            })
            .collect()
    });
    let live_due = live.as_ref().is_some_and(|v| *v != r.screen_value && now.duration_since(r.screen_drawn) >= LIVE_SCREEN_EVERY);
    let d = r.driver.as_mut().expect("opened above");
    let result = (|| -> std::io::Result<()> {
        // Settings to apply, or the readings on the screen have moved on.
        if (r.applied != want || live_due) && now.duration_since(r.last_write) >= MIN_WRITE_GAP {
            // One change per pass: firmware dislikes bursts of curve writes.
            if let Some((ch, setting)) = want.speeds.iter().find(|(ch, s)| r.applied.speeds.get(*ch) != Some(s)) {
                d.set_speed(ch, setting)?;
                r.applied.speeds.insert(ch.clone(), setting.clone());
            } else if want.screen != r.applied.screen || live_due {
                if let Some(s) = &want.screen {
                    let size = r.model.screen().map_or(0, |(w, _)| w as usize);
                    let picture = matches!(s.mode, ScreenMode::Color | ScreenMode::Image | ScreenMode::Temperature);
                    let values = live.clone().unwrap_or_default();
                    r.screen_value = values.clone();
                    r.screen_drawn = Instant::now();
                    let shown = (|| -> std::io::Result<()> {
                        let px = super::screen::pixels(s, size, &values)?;
                        if px.is_some() && d.wants_bulk() {
                            d.attach_bulk(backend.open_bulk(&r.found)?);
                        }
                        d.set_screen(s, px.as_deref())
                    })();
                    match shown {
                        Ok(()) => r.notice.clear(),
                        // A picture that won't go up isn't worth dropping the
                        // cooler over: say so and keep running it.
                        Err(e) if picture => r.notice = format!("Couldn't put that on the screen: {e}"),
                        Err(e) => return Err(e),
                    }
                }
                r.applied.screen = want.screen.clone();
            } else if want.light != r.applied.light {
                if let Some(l) = &want.light {
                    d.set_light(l)?;
                }
                r.applied.light = want.light.clone();
            }
            r.last_write = Instant::now();
        }
        r.status = Some(d.status()?);
        Ok(())
    })();
    if let Err(e) = result {
        r.driver = None;
        r.status = None;
        r.state = CoolerState::Error;
        r.message = format!("Lost the connection to the cooler ({e}). Retrying…");
        r.retry_at = now + RETRY_AFTER;
    }
}

fn run(shared: Arc<Shared>, mut backend: Box<dyn Backend>, owner_check: OwnerCheck) {
    let mut next_discover = Instant::now();
    let mut next_owner_check = Instant::now();
    let mut last_sent: Option<Vec<CoolerView>> = None;
    let mut published = false;

    while !shared.stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= next_owner_check {
            let apps = owner_check();
            shared.lock().running_apps = apps;
            next_owner_check = now + OWNER_CHECK_EVERY;
        }
        if now >= next_discover {
            let found = backend.discover();
            let mut inner = shared.lock();
            inner.coolers.retain(|r| found.iter().any(|f| f.id == r.found.id));
            for f in found {
                if let Some(r) = inner.coolers.iter_mut().find(|r| r.found.id == f.id) {
                    // Setup (or the restart after it) may have finished.
                    if (r.found.needs_setup && !f.needs_setup) || (r.found.needs_restart && !f.needs_restart) {
                        r.found.needs_setup = f.needs_setup;
                        r.found.needs_restart = f.needs_restart;
                        r.state = CoolerState::Idle;
                        r.message.clear();
                        r.retry_at = now;
                    }
                    continue;
                }
                if let Some(model) = Model::find(f.vid, f.pid) {
                    inner.coolers.push(Runtime {
                        found: f,
                        model,
                        driver: None,
                        firmware: None,
                        state: CoolerState::Idle,
                        message: String::new(),
                        notice: String::new(),
                        status: None,
                        applied: Applied::default(),
                        retry_at: now,
                        last_write: now - MIN_WRITE_GAP,
                        screen_value: Vec::new(),
                        screen_drawn: now,
                    });
                }
            }
            next_discover = now + DISCOVER_EVERY;
        }

        // Service each cooler without holding the lock during USB traffic.
        let jobs: Vec<(usize, CoolerConfig)> = {
            let inner = shared.lock();
            inner
                .coolers
                .iter()
                .enumerate()
                .map(|(i, r)| (i, inner.configs.get(&r.found.id).cloned().unwrap_or_default()))
                .collect()
        };
        let mut liquid = None;
        for (i, cfg) in jobs {
            let mut runtime = {
                let mut inner = shared.lock();
                let Some(r) = inner.coolers.get_mut(i) else { continue };
                // Take the runtime out briefly; a placeholder keeps indices stable.
                let placeholder = r.shallow();
                std::mem::replace(r, placeholder)
            };
            if cfg.takeover {
                service(&mut runtime, &cfg, backend.as_mut());
            } else {
                // Hand it back: close our handle, keep the last curve on the device.
                runtime.driver = None;
                runtime.status = None;
                runtime.state = CoolerState::Idle;
                runtime.message.clear();
                runtime.retry_at = now;
            }
            if liquid.is_none() {
                liquid = runtime.status.as_ref().and_then(|s| s.liquid_c);
            }
            if let Some(r) = shared.lock().coolers.get_mut(i) {
                *r = runtime;
            }
        }
        // Only touch the shared reading when this manager owns it.
        if liquid.is_some() || published {
            temps::set("liquid", liquid);
            published = liquid.is_some();
        }

        let current = views(&shared.lock());
        if last_sent.as_ref() != Some(&current) {
            (shared.listener)(current.clone());
            last_sent = Some(current);
        }

        let guard = shared.lock();
        if !shared.stop.load(Ordering::SeqCst) {
            let _ = shared.wake.wait_timeout(guard, POLL);
        }
    }

    // Lumora's temperature screen can't update without Lumora: hand those
    // screens back to the cooler's own live display.
    let mut inner = shared.lock();
    let configs = inner.configs.clone();
    for r in inner.coolers.iter_mut() {
        if let (Some(d), Some(cfg)) = (r.driver.as_mut(), configs.get(&r.found.id)) {
            if cfg.takeover && cfg.screen.shows_readings() {
                let _ = d.set_screen(&ScreenSettings { mode: ScreenMode::Liquid, ..cfg.screen.clone() }, None);
            }
        }
    }
}

impl Runtime {
    /// A copy without the device handle (handles aren't cloneable).
    fn shallow(&self) -> Runtime {
        Runtime {
            found: self.found.clone(),
            model: self.model,
            driver: None,
            firmware: self.firmware.clone(),
            state: self.state,
            message: self.message.clone(),
            notice: self.notice.clone(),
            status: self.status.clone(),
            applied: self.applied.clone(),
            retry_at: self.retry_at,
            last_write: self.last_write,
            screen_value: self.screen_value.clone(),
            screen_drawn: self.screen_drawn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::curve::Preset;
    use super::super::fake::FakeBackend;
    use super::super::kraken::PUMP_MIN_DUTY;
    use super::super::LightSpeed;
    use super::*;

    fn wait(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if f() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    fn manager(backend: FakeBackend, configs: BTreeMap<String, CoolerConfig>, apps: &[&str]) -> CoolingManager {
        let apps: HashSet<String> = apps.iter().map(|s| s.to_string()).collect();
        CoolingManager::new(Box::new(backend), configs, Box::new(move || apps.clone()), Arc::new(|_| {}))
    }

    #[test]
    fn leaves_cooler_alone_until_taken_over() {
        let (backend, dev) = FakeBackend::with_kraken(0x300C);
        let m = manager(backend, BTreeMap::new(), &["nzxt cam.exe"]);
        wait("discovery", || m.views().len() == 1);
        let v = &m.views()[0];
        assert_eq!(v.name, "Kraken Elite (2023)");
        assert_eq!(v.state, CoolerState::Idle);
        assert!(v.owner_running, "CAM is detected");
        assert_eq!(v.channels.len(), 2);
        assert_eq!(v.screen.as_ref().unwrap().width, 640);
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(dev.lock().unwrap().writes, 0, "nothing is sent to a cooler left to CAM");
    }

    #[test]
    fn takeover_applies_settings_and_reports_status() {
        let (backend, dev) = FakeBackend::with_kraken(0x300C);
        let m = manager(backend, BTreeMap::new(), &[]);
        wait("discovery", || m.views().len() == 1);
        let id = m.views()[0].id.clone();

        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.speeds.insert("fan".into(), SpeedSetting::Preset { preset: Preset::Silent });
        cfg.speeds.insert("pump".into(), SpeedSetting::Fixed { duty: 5 });
        cfg.screen.brightness = 60;
        cfg.screen.rotation = 180;
        m.set_config(&id, cfg.clone());

        wait("active with status", || {
            let v = &m.views()[0];
            v.state == CoolerState::Active && v.status.is_some()
        });
        wait("all settings applied", || {
            let d = dev.lock().unwrap();
            d.curves.len() >= 2 && d.liquid_screen
        });
        let v = &m.views()[0];
        assert_eq!(v.firmware.as_deref(), Some("2.4.0"));
        let s = v.status.as_ref().unwrap();
        assert!((s.liquid_c.unwrap() - 33.4).abs() < 0.01);
        {
            let d = dev.lock().unwrap();
            assert!(d.initialized);
            assert_eq!(d.brightness, 60);
            assert_eq!(d.orientation, 2);
            let pump = &d.curves.iter().find(|(c, _)| c == "pump").unwrap().1;
            assert!(pump.iter().all(|&x| x >= PUMP_MIN_DUTY), "pump minimum enforced");
            let fan = &d.curves.iter().find(|(c, _)| c == "fan").unwrap().1;
            assert_eq!(fan[0], 25);
        }
        assert!((temps::get("liquid").unwrap() - 33.4).abs() < 0.01);

        // Unchanged settings aren't rewritten.
        let curves = dev.lock().unwrap().curves.len();
        thread::sleep(Duration::from_millis(2500));
        assert_eq!(dev.lock().unwrap().curves.len(), curves);

        // A new curve goes out on its own.
        cfg.speeds.insert("fan".into(), SpeedSetting::Custom { points: vec![(20, 45), (50, 90)] });
        m.set_config(&id, cfg.clone());
        wait("new fan curve", || dev.lock().unwrap().curves.last().is_some_and(|(c, t)| c == "fan" && t[0] == 45));

        // Handing back closes the handle and stops all traffic.
        cfg.takeover = false;
        m.set_config(&id, cfg);
        wait("idle", || m.views()[0].state == CoolerState::Idle);
        let writes = dev.lock().unwrap().writes;
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(dev.lock().unwrap().writes, writes);
    }

    #[test]
    fn busy_cooler_reports_a_clear_error_and_recovers() {
        let (backend, dev) = FakeBackend::with_kraken(0x3012);
        dev.lock().unwrap().busy = true;
        let mut configs = BTreeMap::new();
        configs.insert("hid:1e71:3012:FAKE0001".to_string(), CoolerConfig { takeover: true, ..Default::default() });
        let m = manager(backend, configs, &[]);
        wait("error", || m.views().first().is_some_and(|v| v.state == CoolerState::Error));
        assert!(m.views()[0].message.contains("isn't answering"));
        dev.lock().unwrap().busy = false;
        wait("recovered", || m.views()[0].state == CoolerState::Active);
    }

    #[test]
    fn corsair_hydro_takeover_sets_modes_curves_and_light() {
        let (backend, dev) = FakeBackend::with_hydro(0x0C13);
        let m = manager(backend, BTreeMap::new(), &[]);
        wait("discovery", || m.views().len() == 1);
        let v = m.views()[0].clone();
        assert_eq!((v.vendor.as_str(), v.name.as_str()), ("Corsair", "H115i Pro"));
        assert_eq!(v.owner_app.as_deref(), Some("Corsair iCUE"));
        assert_eq!(v.channels[0].kind, ChannelKind::Modes);
        assert!(v.light.is_some() && v.screen.is_none());
        thread::sleep(Duration::from_millis(1200));
        assert_eq!(dev.lock().unwrap().writes, 0, "left to iCUE until taken over");

        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.speeds.insert("pump".into(), SpeedSetting::Preset { preset: Preset::Performance });
        cfg.speeds.insert("fan".into(), SpeedSetting::Custom { points: vec![(25, 20), (40, 60)] });
        cfg.light = LightSettings { mode: LightMode::Pulse, colors: vec!["#ff0000".into(), "#0000ff".into()], speed: LightSpeed::Slower };
        m.set_config(&v.id, cfg.clone());
        wait("everything applied", || {
            let d = dev.lock().unwrap();
            d.pump_mode == 2 && d.fan_curves.len() >= 2 && d.light.len() >= 3
        });
        wait("status", || m.views()[0].status.as_ref().is_some_and(|s| s.pump_mode.as_deref() == Some("performance")));
        let v = m.views()[0].clone();
        let s = v.status.unwrap();
        assert_eq!(v.firmware.as_deref(), Some("1.1.15.4"));
        assert!((s.liquid_c.unwrap() - 31.7).abs() < 0.01);
        assert_eq!(s.fan_rpms, vec![900, 920]);
        {
            let d = dev.lock().unwrap();
            let (fan, curve) = &d.fan_curves[0];
            assert_eq!(*fan, 0);
            assert_eq!(&curve[..3], &[25, 40, 59]);
            assert_eq!(&curve[7..10], &[20, 60, 100]);
            assert_eq!(d.light.last().unwrap(), &vec![0x52, 0x01]);
        }

        cfg.speeds.insert("fan".into(), SpeedSetting::Fixed { duty: 55 });
        m.set_config(&v.id, cfg);
        wait("fixed fans", || dev.lock().unwrap().fan_fixed.len() >= 2);
        assert_eq!(dev.lock().unwrap().fan_fixed[..2], [(0, 55), (1, 55)]);
    }

    #[test]
    fn corsair_without_access_driver_waits_for_setup() {
        let (backend, dev) = FakeBackend::with_hydro(0x0C13);
        dev.lock().unwrap().needs_setup = true;
        let mut configs = BTreeMap::new();
        configs.insert("usb:1b1c:0c13:1-4".to_string(), CoolerConfig { takeover: true, ..Default::default() });
        let m = manager(backend, configs, &[]);
        wait("needs setup", || m.views().first().is_some_and(|v| v.state == CoolerState::NeedsSetup && v.needs_setup));
        assert_eq!(dev.lock().unwrap().writes, 0);
        dev.lock().unwrap().needs_setup = false;
        wait("active after setup", || m.views()[0].state == CoolerState::Active);
    }

    #[test]
    fn access_denied_points_at_the_vendor_app() {
        let e = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Access is denied. (os error 5)");
        let msg = open_error_message(&e, Some("NZXT CAM"));
        assert!(msg.contains("locked") && msg.contains("NZXT CAM"), "{msg}");
        let running: HashSet<String> = ["nzxt cam.exe".to_string()].into();
        assert!(owner_is_running("NZXT CAM", &running));
        assert!(!owner_is_running("Corsair iCUE", &running));
    }

    #[test]
    fn kraken_screen_shows_colors_and_pictures() {
        let (backend, dev) = FakeBackend::with_kraken(0x300C);
        let m = manager(backend, BTreeMap::new(), &[]);
        wait("discovery", || m.views().len() == 1);
        let id = m.views()[0].id.clone();
        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.screen.mode = ScreenMode::Color;
        cfg.screen.colors = vec!["#ff0000".into(), "#0000ff".into()];
        m.set_config(&id, cfg.clone());
        wait("picture shown", || dev.lock().unwrap().shown_bucket.is_some());
        {
            let d = dev.lock().unwrap();
            let px = 640 * 640 * 4;
            assert_eq!(d.bulk.len(), 20 + px, "header plus every pixel");
            assert_eq!(&d.bulk[..4], &[0x12, 0xFA, 0x01, 0xE8]);
            assert_eq!(&d.bulk[16..20], &(px as u32).to_le_bytes());
            assert_eq!(&d.bulk[20..24], &[255, 0, 0, 0], "top-left is the first color");
        }
        assert!(m.views()[0].notice.is_empty());

        // A picture from a file, then back to the built-in screen.
        let file = std::env::temp_dir().join(format!("lumora-screen-test-{}.rgba", std::process::id()));
        std::fs::write(&file, vec![9u8; 640 * 640 * 4]).unwrap();
        cfg.screen.mode = ScreenMode::Image;
        cfg.screen.image = Some(file.display().to_string());
        m.set_config(&id, cfg.clone());
        wait("picture from file", || dev.lock().unwrap().bulk.get(20..24) == Some(&[9, 9, 9, 0]));
        let _ = std::fs::remove_file(&file);

        cfg.screen.mode = ScreenMode::Liquid;
        m.set_config(&id, cfg.clone());
        wait("liquid screen", || {
            let d = dev.lock().unwrap();
            d.liquid_screen && d.shown_bucket.is_none()
        });

        // A missing file is a notice, not a lost cooler.
        cfg.screen.mode = ScreenMode::Image;
        cfg.screen.image = Some("/nonexistent/lumora.rgba".into());
        m.set_config(&id, cfg);
        wait("notice", || !m.views()[0].notice.is_empty());
        assert_eq!(m.views()[0].state, CoolerState::Active);
    }

    #[test]
    fn corsair_follows_lumora_color() {
        let (backend, dev) = FakeBackend::with_hydro(0x0C13);
        let mut configs = BTreeMap::new();
        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.light.mode = LightMode::Sync;
        configs.insert("usb:1b1c:0c13:1-4".to_string(), cfg);
        crate::ambient::set(crate::Rgb { r: 250, g: 10, b: 120 });
        let m = manager(backend, configs, &[]);
        // Other tests run the engine too, so only check that a color went out.
        wait("synced color", || dev.lock().unwrap().light.iter().any(|msg| msg.first() == Some(&0x56)));
        drop(m);
        assert_eq!(sync_hex(crate::Rgb { r: 250, g: 10, b: 120 }), "#f80878");
        assert_eq!(sync_hex(crate::Rgb { r: 255, g: 3, b: 0 }), "#ff0000");
    }

    #[test]
    fn lumora_temperature_screen_swaps_slots_and_hands_back_on_exit() {
        let (backend, dev) = FakeBackend::with_kraken(0x300C);
        let m = manager(backend, BTreeMap::new(), &[]);
        wait("discovery", || m.views().len() == 1);
        let id = m.views()[0].id.clone();
        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.screen.mode = ScreenMode::Temperature;
        cfg.screen.text_color = "#00ff00".into();
        m.set_config(&id, cfg.clone());
        wait("drawn", || dev.lock().unwrap().shown_bucket.is_some());
        let first = dev.lock().unwrap().shown_bucket;
        {
            let d = dev.lock().unwrap();
            let green = d.bulk[20..].chunks(4).filter(|p| p[1] > 200 && p[0] < 40).count();
            assert!(green > 1000, "digits in the text color ({green} px)");
        }
        // A settings change redraws into the other slot.
        cfg.screen.background = vec!["#202020".into()];
        m.set_config(&id, cfg);
        wait("other slot", || dev.lock().unwrap().shown_bucket.is_some_and(|b| Some(b) != first));
        m.shutdown();
        let d = dev.lock().unwrap();
        assert!(d.liquid_screen && d.shown_bucket.is_none(), "back to the built-in screen");
    }

    #[test]
    fn temperature_screen_follows_the_reading() {
        let (backend, dev) = FakeBackend::with_kraken(0x300C);
        let m = manager(backend, BTreeMap::new(), &[]);
        wait("discovery", || m.views().len() == 1);
        let id = m.views()[0].id.clone();
        let mut cfg = CoolerConfig { takeover: true, ..Default::default() };
        cfg.screen.mode = ScreenMode::Temperature;
        m.set_config(&id, cfg);
        wait("first drawing", || dev.lock().unwrap().shown_bucket.is_some());
        let first = dev.lock().unwrap().bulk.clone();
        // Nothing changed in the settings; only the liquid warms up.
        dev.lock().unwrap().liquid_c = 41.0;
        wait("redrawn with the new reading", || {
            let d = dev.lock().unwrap();
            d.writing.is_none() && d.bulk.len() == first.len() && d.bulk != first
        });
        // A reading that doesn't change the shown number causes no redraws.
        let writes = dev.lock().unwrap().bulk_writes;
        dev.lock().unwrap().liquid_c = 41.2;
        thread::sleep(Duration::from_millis(2500));
        assert_eq!(dev.lock().unwrap().bulk_writes, writes);
    }

    #[test]
    fn corsair_waits_for_the_restart_after_setup_then_connects() {
        let (backend, dev) = FakeBackend::with_hydro(0x0C13);
        dev.lock().unwrap().needs_restart = true;
        let mut configs = BTreeMap::new();
        configs.insert("usb:1b1c:0c13:1-4".to_string(), CoolerConfig { takeover: true, ..Default::default() });
        let m = manager(backend, configs, &[]);
        wait("restart notice", || m.views().first().is_some_and(|v| v.needs_restart && v.message.contains("Restart")));
        assert_eq!(dev.lock().unwrap().writes, 0);
        dev.lock().unwrap().needs_restart = false;
        wait("active after the restart", || m.views()[0].state == CoolerState::Active);
    }
}
