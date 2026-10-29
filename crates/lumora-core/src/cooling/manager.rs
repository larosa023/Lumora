//! Finds coolers, takes them over when asked, keeps their settings applied
//! and reports live readings.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::curve::{self, SpeedSetting};
use super::kraken::{self, Kraken, KrakenModel, PUMP_MIN_DUTY};
use super::{owner_app, Backend, CoolerConfig, CoolerStatus, FoundCooler, ScreenMode, ScreenSettings};
use crate::temps;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoolerState {
    /// Left to its own app.
    Idle,
    Connecting,
    Active,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub min_duty: u8,
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
    pub firmware: Option<String>,
    pub status: Option<CoolerStatus>,
    pub channels: Vec<ChannelView>,
    pub screen: Option<ScreenView>,
}

/// What was last written to a cooler, so unchanged settings aren't resent.
#[derive(Clone, PartialEq, Default)]
struct Applied {
    tables: BTreeMap<String, [u8; 40]>,
    screen: Option<ScreenSettings>,
}

struct Runtime {
    found: FoundCooler,
    model: &'static KrakenModel,
    kraken: Option<Kraken>,
    state: CoolerState,
    message: String,
    status: Option<CoolerStatus>,
    applied: Applied,
    retry_at: Instant,
    last_write: Instant,
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
    thread: Option<JoinHandle<()>>,
}

const POLL: Duration = Duration::from_secs(1);
const DISCOVER_EVERY: Duration = Duration::from_secs(5);
const OWNER_CHECK_EVERY: Duration = Duration::from_secs(10);
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Curves are written whole; spacing writes out keeps the firmware happy.
const MIN_WRITE_GAP: Duration = Duration::from_millis(800);

fn channels(model: &KrakenModel) -> Vec<(&'static str, &'static str, u8)> {
    let mut v = vec![("pump", "Pump", PUMP_MIN_DUTY)];
    if model.fan_id.is_some() {
        v.push(("fan", "Radiator fans", 0));
    }
    v
}

/// The process names each owner app runs as.
fn owner_processes(app: &str) -> &'static [&'static str] {
    match app {
        "NZXT CAM" => &["nzxt cam.exe", "nzxt cam"],
        "Corsair iCUE" => &["icue.exe", "icue"],
        _ => &[],
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
        CoolingManager { shared, thread: Some(thread) }
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

    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CoolingManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn desired(model: &KrakenModel, cfg: &CoolerConfig) -> Applied {
    let tables = channels(model)
        .into_iter()
        .map(|(id, _, min)| {
            let setting = cfg.speeds.get(id).cloned().unwrap_or_default();
            (id.to_string(), curve::table(&setting, id, min))
        })
        .collect();
    Applied { tables, screen: model.lcd.map(|_| cfg.screen.clone()) }
}

fn views(inner: &Inner) -> Vec<CoolerView> {
    inner
        .coolers
        .iter()
        .map(|r| {
            let cfg = inner.configs.get(&r.found.id).cloned().unwrap_or_default();
            let owner = owner_app(r.found.vid);
            let want = desired(r.model, &cfg);
            CoolerView {
                id: r.found.id.clone(),
                name: r.model.name.to_string(),
                vendor: "NZXT".into(),
                owner_app: owner.map(String::from),
                owner_running: owner.is_some_and(|a| {
                    owner_processes(a).iter().any(|p| inner.running_apps.contains(*p))
                }),
                takeover: cfg.takeover,
                state: r.state,
                message: r.message.clone(),
                firmware: r.kraken.as_ref().and_then(|k| k.firmware).map(|(a, b, c)| format!("{a}.{b}.{c}")),
                status: r.status.clone(),
                channels: channels(r.model)
                    .into_iter()
                    .map(|(id, name, min)| ChannelView {
                        id: id.into(),
                        name: name.into(),
                        min_duty: min,
                        setting: cfg.speeds.get(id).cloned().unwrap_or_default(),
                        table: want.tables[id].to_vec(),
                    })
                    .collect(),
                screen: r.model.lcd.map(|(w, h)| ScreenView { width: w, height: h, settings: cfg.screen.clone() }),
            }
        })
        .collect()
}

/// Brings one taken-over cooler in line with its settings and reads it.
fn service(r: &mut Runtime, cfg: &CoolerConfig, backend: &mut dyn Backend) {
    let now = Instant::now();
    if r.kraken.is_none() {
        if now < r.retry_at {
            return;
        }
        r.state = CoolerState::Connecting;
        let opened = backend.open(&r.found).and_then(|io| {
            let mut k = Kraken::new(io, r.model);
            k.initialize()?;
            Ok(k)
        });
        match opened {
            Ok(k) => {
                r.kraken = Some(k);
                r.applied = Applied::default();
                r.state = CoolerState::Active;
                r.message.clear();
            }
            Err(e) => {
                r.state = CoolerState::Error;
                r.message = match e.kind() {
                    std::io::ErrorKind::TimedOut => "The cooler isn't answering. Another app may be using it.".into(),
                    _ => format!("Couldn't open the cooler: {e}"),
                };
                r.retry_at = now + RETRY_AFTER;
                return;
            }
        }
    }

    let want = desired(r.model, cfg);
    let k = r.kraken.as_mut().expect("opened above");
    let result = (|| -> std::io::Result<()> {
        if r.applied != want && now.duration_since(r.last_write) >= MIN_WRITE_GAP {
            // One change per pass: firmware dislikes bursts of curve writes.
            if let Some((ch, table)) = want.tables.iter().find(|(ch, t)| r.applied.tables.get(*ch) != Some(t)) {
                k.set_curve(ch, table)?;
                r.applied.tables.insert(ch.clone(), *table);
            } else if want.screen != r.applied.screen {
                if let Some(s) = &want.screen {
                    if s.mode != ScreenMode::Unchanged {
                        k.set_screen(s.brightness, s.rotation)?;
                        if s.mode == ScreenMode::Liquid {
                            k.show_liquid_temperature()?;
                        }
                    }
                }
                r.applied.screen = want.screen.clone();
            }
            r.last_write = Instant::now();
        }
        r.status = Some(k.status()?);
        Ok(())
    })();
    if let Err(e) = result {
        r.kraken = None;
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
                if inner.coolers.iter().any(|r| r.found.id == f.id) {
                    continue;
                }
                if let Some(model) = kraken::model(f.pid) {
                    inner.coolers.push(Runtime {
                        found: f,
                        model,
                        kraken: None,
                        state: CoolerState::Idle,
                        message: String::new(),
                        status: None,
                        applied: Applied::default(),
                        retry_at: now,
                        last_write: now - MIN_WRITE_GAP,
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
                runtime.kraken = None;
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
}

impl Runtime {
    /// A copy without the device handle (handles aren't cloneable).
    fn shallow(&self) -> Runtime {
        Runtime {
            found: self.found.clone(),
            model: self.model,
            kraken: None,
            state: self.state,
            message: self.message.clone(),
            status: self.status.clone(),
            applied: self.applied.clone(),
            retry_at: self.retry_at,
            last_write: self.last_write,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::curve::Preset;
    use super::super::fake::FakeBackend;
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
        assert!(m.views()[0].message.contains("Another app"));
        dev.lock().unwrap().busy = false;
        wait("recovered", || m.views()[0].state == CoolerState::Active);
    }
}
