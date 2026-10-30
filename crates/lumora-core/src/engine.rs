//! The render loop.
//!
//! One background thread owns the OpenRGB connection. Each tick it renders
//! every device's effect from a shared clock, applies brightness, and sends
//! only the frames that actually changed – so a static color costs nothing
//! and the loop drops to a slow idle tick when nothing is animating.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::color::Rgb;
use crate::device::{stable_ids, DeviceInfo, LedPos};
use crate::effects::{self, Effect, EffectConfig, Frame};
use crate::openrgb::{Client, Controller};
use crate::store::{DemoMode, Profile, Session, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConnectionInfo {
    pub status: ConnectionStatus,
    pub server: String,
    pub message: String,
    pub protocol: u32,
}

/// Everything the UI shows, in one serializable piece.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub connection: ConnectionInfo,
    pub devices: Vec<DeviceInfo>,
    pub global: EffectConfig,
    pub overrides: BTreeMap<String, EffectConfig>,
    pub brightness: u8,
    pub enabled: bool,
}

pub enum EngineEvent {
    /// Devices, connection or lighting changed – fetch a new [`Snapshot`].
    Changed,
    /// Live LED colors for previews: `(device id, r g b r g b …)`.
    Frames(Vec<(String, Vec<u8>)>),
}

pub type Listener = Arc<dyn Fn(EngineEvent) + Send + Sync>;

enum Sink {
    OpenRgb(u32),
    Virtual,
}

struct Runtime {
    info: DeviceInfo,
    leds: Vec<LedPos>,
    sink: Sink,
    cfg: Option<EffectConfig>,
    effect: Option<Box<dyn Effect>>,
    raw: Vec<Rgb>,
    sent: Option<Vec<Rgb>>,
    previewed: Option<Vec<Rgb>>,
    /// Switch to the per-LED mode before the next frame is sent.
    needs_custom_mode: bool,
}

impl Runtime {
    fn new(info: DeviceInfo, sink: Sink) -> Self {
        let leds = info.led_positions();
        let n = info.led_count;
        Runtime { info, leds, sink, cfg: None, effect: None, raw: vec![Rgb::BLACK; n], sent: None, previewed: None, needs_custom_mode: true }
    }
}

struct State {
    lighting: Profile,
    enabled: bool,
    settings: Settings,
    connection: ConnectionInfo,
    real: Vec<Runtime>,
    demo: Vec<Runtime>,
    preview: bool,
    reconnect: bool,
    rescan: bool,
    resizes: Vec<(String, usize, usize)>,
    /// 0 = dark, 1 = full; eases toward `enabled` for soft on/off.
    fade: f32,
    /// The main effect rendered for one light in the middle, published for
    /// single-color lights that follow Lumora.
    ambient: Option<(EffectConfig, Box<dyn Effect>)>,
}

impl State {
    fn devices(&self) -> impl Iterator<Item = &Runtime> {
        self.real.iter().chain(self.demo.iter())
    }

    fn refresh_demo(&mut self) {
        let want = match self.settings.demo_devices {
            DemoMode::Always => true,
            DemoMode::Never => false,
            DemoMode::Auto => self.real.is_empty(),
        };
        if want && self.demo.is_empty() {
            self.demo = crate::demo::devices().into_iter().map(|d| Runtime::new(d, Sink::Virtual)).collect();
        } else if !want {
            self.demo.clear();
        }
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
    listener: Listener,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        (self.listener)(EngineEvent::Changed);
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(settings: Settings, session: Session, listener: Listener) -> Engine {
        let settings = settings.sanitized();
        let mut state = State {
            connection: ConnectionInfo {
                status: ConnectionStatus::Connecting,
                server: format!("{}:{}", settings.host, settings.port),
                message: String::new(),
                protocol: 0,
            },
            lighting: session.lighting,
            enabled: session.enabled,
            fade: if session.enabled { 1.0 } else { 0.0 },
            settings,
            real: Vec::new(),
            demo: Vec::new(),
            preview: false,
            reconnect: false,
            rescan: false,
            resizes: Vec::new(),
            ambient: None,
        };
        state.refresh_demo();
        let shared = Arc::new(Shared { state: Mutex::new(state), wake: Condvar::new(), stop: AtomicBool::new(false), listener });
        let s = shared.clone();
        let thread = thread::Builder::new().name("lumora-engine".into()).spawn(move || run(s)).expect("spawn engine thread");
        Engine { shared, thread: Some(thread) }
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.shared.lock());
        self.shared.wake.notify_all();
        self.shared.changed();
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = self.shared.lock();
        Snapshot {
            connection: s.connection.clone(),
            devices: s
                .devices()
                .map(|d| {
                    let mut info = d.info.clone();
                    info.managed = d.info.virtual_device || s.settings.controls(&info.id, &info.vendor);
                    info
                })
                .collect(),
            global: s.lighting.global.clone(),
            overrides: s.lighting.overrides.clone(),
            brightness: s.lighting.brightness,
            enabled: s.enabled,
        }
    }

    /// Sets the effect for one device (`Some(id)`) or for everything (`None`).
    pub fn set_effect(&self, device: Option<&str>, cfg: EffectConfig) {
        self.update(|s| match device {
            Some(id) => {
                s.lighting.overrides.insert(id.to_string(), cfg);
            }
            None => s.lighting.global = cfg,
        });
    }

    /// Puts a device back on the shared ("all devices") effect.
    pub fn clear_override(&self, device: &str) {
        self.update(|s| {
            s.lighting.overrides.remove(device);
        });
    }

    /// Applies one effect everywhere, dropping every per-device override.
    pub fn sync_all(&self, cfg: EffectConfig) {
        self.update(|s| {
            s.lighting.global = cfg;
            s.lighting.overrides.clear();
        });
    }

    pub fn set_brightness(&self, brightness: u8) {
        self.update(|s| s.lighting.brightness = brightness.min(100));
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.update(|s| s.enabled = enabled);
    }

    pub fn lighting(&self) -> Profile {
        self.shared.lock().lighting.clone()
    }

    pub fn session(&self, active_profile: Option<String>) -> Session {
        let s = self.shared.lock();
        Session { lighting: s.lighting.clone(), enabled: s.enabled, active_profile }
    }

    pub fn load_lighting(&self, mut p: Profile) {
        p.brightness = p.brightness.min(100);
        self.update(|s| {
            s.lighting = p;
            s.enabled = true;
        });
    }

    pub fn apply_settings(&self, settings: Settings) {
        let settings = settings.sanitized();
        self.update(|s| {
            if s.settings.host != settings.host || s.settings.port != settings.port {
                s.reconnect = true;
            }
            s.settings = settings;
            s.refresh_demo();
        });
    }

    /// Asks the hardware engine to look for devices again.
    pub fn rescan(&self) {
        self.update(|s| s.rescan = true);
    }

    /// Sets how many LEDs are on a resizable zone (an ARGB header, a strip).
    pub fn resize_zone(&self, device: &str, zone: usize, size: usize) {
        self.update(|s| s.resizes.push((device.to_string(), zone, size)));
    }

    pub fn reconnect(&self) {
        self.update(|s| s.reconnect = true);
    }

    /// Live preview frames are only produced while the window is visible.
    pub fn set_preview(&self, on: bool) {
        let mut guard = self.shared.lock();
        let s = &mut *guard;
        s.preview = on;
        if on {
            for d in s.real.iter_mut().chain(s.demo.iter_mut()) {
                d.previewed = None;
            }
        }
        drop(guard);
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

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn set_connection(shared: &Shared, status: ConnectionStatus, server: String, message: impl Into<String>, protocol: u32) {
    let info = ConnectionInfo { status, server, message: message.into(), protocol };
    let mut s = shared.lock();
    if s.connection != info {
        s.connection = info;
        drop(s);
        shared.changed();
    }
}

/// The client, the controllers that answered (with their server index), and
/// how many didn't.
type Connected = std::io::Result<(Client, Vec<(u32, Controller)>, usize)>;

/// Connecting and reading devices can take seconds (slow SMBus RAM), so it
/// happens on a helper thread and the render loop never stalls.
enum Conn {
    Idle,
    Pending(Receiver<Connected>),
    Ready(Client),
}

fn spawn_connect(host: String, port: u16, existing: Option<Client>) -> Receiver<Connected> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| {
            let mut c = match existing {
                Some(c) => c,
                None => Client::connect(&host, port, Duration::from_millis(800))?,
            };
            // One misbehaving device (busy, or held by its vendor's app) must not
            // hide all the others, so a device that doesn't answer is skipped.
            let count = c.controller_count()?;
            let mut found = Vec::new();
            let mut skipped = 0;
            for i in 0..count {
                match c.controller(i) {
                    Ok(ctrl) => found.push((i, ctrl)),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::InvalidData) => skipped += 1,
                    Err(e) => return Err(e),
                }
            }
            if count > 0 && found.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "none of the devices answered"));
            }
            Ok((c, found, skipped))
        })();
        let _ = tx.send(result);
    });
    rx
}

/// Swaps freshly read controllers in as the real device list.
fn install(shared: &Shared, client: &mut Client, controllers: Vec<(u32, Controller)>, skipped: usize) -> std::io::Result<()> {
    let plain: Vec<Controller> = controllers.iter().map(|(_, c)| c.clone()).collect();
    let ids = stable_ids(&plain);
    let runtimes: Vec<Runtime> = controllers
        .iter()
        .zip(ids)
        .map(|((i, c), id)| Runtime::new(DeviceInfo::from_controller(c, id), Sink::OpenRgb(*i)))
        .collect();
    let count = runtimes.len();
    {
        let mut s = shared.lock();
        s.real = runtimes;
        s.refresh_demo();
    }
    let mut message = match count {
        0 => "Connected, but no devices found yet".to_string(),
        1 => "1 device".to_string(),
        n => format!("{n} devices"),
    };
    if skipped > 0 {
        message.push_str(&format!(" ({skipped} not responding)"));
    }
    set_connection(shared, ConnectionStatus::Connected, client.server.clone(), message, client.protocol);
    shared.changed();
    Ok(())
}

fn drop_real_devices(shared: &Shared) {
    let mut s = shared.lock();
    if !s.real.is_empty() {
        s.real.clear();
        s.refresh_demo();
        drop(s);
        shared.changed();
    }
}

fn run(shared: Arc<Shared>) {
    let start = Instant::now();
    let mut last = start;
    let mut conn = Conn::Idle;
    let mut next_attempt = Instant::now();
    let mut failures = 0u32;
    // When some devices didn't answer, look again a little later.
    let mut retry_at: Option<Instant> = None;
    let mut retries = 0u32;

    while !shared.stop.load(Ordering::SeqCst) {
        // --- Connection upkeep -------------------------------------------------
        let (host, port, reconnect) = {
            let mut s = shared.lock();
            let r = std::mem::take(&mut s.reconnect);
            (s.settings.host.clone(), s.settings.port, r)
        };
        let server = format!("{host}:{port}");
        if reconnect {
            // An in-flight attempt to the old address is simply abandoned.
            conn = Conn::Idle;
            drop_real_devices(&shared);
            next_attempt = Instant::now();
            failures = 0;
        }
        // Requests that need the live connection.
        let mut refresh = false;
        if let Conn::Ready(c) = &mut conn {
            let (resizes, rescan) = {
                let mut s = shared.lock();
                let resizes: Vec<(u32, usize, usize)> = std::mem::take(&mut s.resizes)
                    .into_iter()
                    .filter_map(|(id, zone, size)| {
                        s.real.iter().find(|d| d.info.id == id).and_then(|d| match d.sink {
                            Sink::OpenRgb(i) => Some((i, zone, size)),
                            Sink::Virtual => None,
                        })
                    })
                    .collect();
                (resizes, std::mem::take(&mut s.rescan))
            };
            for (i, zone, size) in resizes {
                let _ = c.resize_zone(i, zone as u32, size as u32);
                refresh = true;
            }
            if rescan {
                // The server re-detects, then announces the new list.
                let _ = c.rescan();
                retries = 0;
                retry_at = Some(Instant::now() + Duration::from_secs(15));
            }
            if retry_at.is_some_and(|t| Instant::now() >= t) {
                retry_at = None;
                refresh = true;
            }
        }
        conn = match conn {
            Conn::Ready(c) if !c.is_alive() => {
                drop_real_devices(&shared);
                set_connection(&shared, ConnectionStatus::Offline, server.clone(), "Lost connection to OpenRGB", 0);
                next_attempt = Instant::now() + Duration::from_secs(2);
                Conn::Idle
            }
            Conn::Ready(c) if refresh || c.take_list_updated() => Conn::Pending(spawn_connect(host.clone(), port, Some(c))),
            Conn::Idle if Instant::now() >= next_attempt => {
                if failures == 0 {
                    set_connection(&shared, ConnectionStatus::Connecting, server.clone(), "", 0);
                }
                Conn::Pending(spawn_connect(host.clone(), port, None))
            }
            Conn::Pending(rx) => match rx.try_recv() {
                Ok(Ok((mut c, controllers, skipped))) => match install(&shared, &mut c, controllers, skipped) {
                    Ok(()) => {
                        failures = 0;
                        // Retry a few times, further apart each time (20s, 60s, 3 min).
                        retry_at = if skipped > 0 && retries < 3 {
                            retries += 1;
                            Some(Instant::now() + Duration::from_secs([20, 60, 180][retries as usize - 1]))
                        } else {
                            if skipped == 0 {
                                retries = 0;
                            }
                            None
                        };
                        Conn::Ready(c)
                    }
                    Err(_) => Conn::Idle,
                },
                Ok(Err(e)) => {
                    failures += 1;
                    drop_real_devices(&shared);
                    let message = match e.kind() {
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut | std::io::ErrorKind::NotFound => {
                            "OpenRGB isn't running, or its SDK server is off".to_string()
                        }
                        _ => format!("Couldn't read your devices: {e}"),
                    };
                    set_connection(&shared, ConnectionStatus::Offline, server.clone(), message, 0);
                    // Back off gently: 2s, 4s … capped at 10s.
                    next_attempt = Instant::now() + Duration::from_secs((2 * failures as u64).clamp(2, 10));
                    Conn::Idle
                }
                Err(TryRecvError::Empty) => Conn::Pending(rx),
                Err(TryRecvError::Disconnected) => Conn::Idle,
            },
            other => other,
        };

        // --- Render -------------------------------------------------------------
        let now = Instant::now();
        let frame = Frame { t: (now - start).as_secs_f32(), dt: (now - last).as_secs_f32().min(0.25), temp: None };
        last = now;

        let mut outgoing: Vec<(u32, Vec<Rgb>)> = Vec::new();
        let mut take_over: Vec<u32> = Vec::new();
        let mut previews: Vec<(String, Vec<u8>)> = Vec::new();
        let (animating, fps) = {
            let mut guard = shared.lock();
            let s = &mut *guard;
            let target = if s.enabled { 1.0 } else { 0.0 };
            let fading = (s.fade - target).abs() > f32::EPSILON;
            if fading {
                let step = frame.dt * 3.0;
                s.fade = if s.fade < target { (s.fade + step).min(target) } else { (s.fade - step).max(target) };
            }
            let b = s.lighting.brightness as f32 / 100.0;
            // Perceptual curve: LEDs look far brighter than their PWM duty.
            let level = (b * b) * s.fade;

            let mut animating = fading;
            let lighting = &s.lighting;
            let preview = s.preview;
            let settings = &s.settings;
            for d in s.real.iter_mut().chain(s.demo.iter_mut()) {
                // Left to its own app (NZXT CAM, …): send nothing at all.
                if !d.info.virtual_device && !settings.controls(&d.info.id, &d.info.vendor) {
                    d.sent = None;
                    d.needs_custom_mode = true;
                    if preview && d.previewed.is_some() {
                        d.previewed = None;
                        previews.push((d.info.id.clone(), Vec::new()));
                    }
                    continue;
                }
                let cfg = lighting.overrides.get(&d.info.id).unwrap_or(&lighting.global);
                if d.cfg.as_ref() != Some(cfg) {
                    d.effect = Some(effects::build(cfg, d.info.led_count));
                    d.cfg = Some(cfg.clone());
                }
                let fx = d.effect.as_mut().expect("effect built above");
                let mut f = frame;
                if cfg.effect == "temperature" {
                    f.temp = crate::temps::get(&cfg.temp_source);
                }
                fx.render(&f, &d.leds, &mut d.raw);
                animating |= !fx.is_static();

                let out: Vec<Rgb> = d.raw.iter().map(|c| c.scale(level)).collect();
                if let (Sink::OpenRgb(i), true) = (&d.sink, d.info.controllable) {
                    if d.needs_custom_mode {
                        take_over.push(*i);
                        d.needs_custom_mode = false;
                    }
                    if d.sent.as_ref() != Some(&out) {
                        outgoing.push((*i, out.clone()));
                        d.sent = Some(out.clone());
                    }
                }
                if preview && d.previewed.as_ref() != Some(&out) {
                    previews.push((d.info.id.clone(), out.iter().flat_map(|c| [c.r, c.g, c.b]).collect()));
                    d.previewed = Some(out);
                }
            }

            // One light in the middle of the layout, for coolers that follow.
            let global = &s.lighting.global;
            if s.ambient.as_ref().map(|(c, _)| c) != Some(global) {
                s.ambient = Some((global.clone(), effects::build(global, 1)));
            }
            if let Some((cfg, fx)) = s.ambient.as_mut() {
                let mut f = frame;
                if cfg.effect == "temperature" {
                    f.temp = crate::temps::get(&cfg.temp_source);
                }
                let mut one = [Rgb::BLACK];
                fx.render(&f, &[LedPos { x: 0.5, y: 0.5 }], &mut one);
                crate::ambient::set(one[0].scale(level));
            }
            (animating, s.settings.fps)
        };

        if let Conn::Ready(c) = &mut conn {
            for i in &take_over {
                let _ = c.set_custom_mode(*i);
            }
            for (i, colors) in &outgoing {
                if c.update_leds(*i, colors).is_err() {
                    break; // The liveness check above handles reconnecting.
                }
            }
        }
        if !previews.is_empty() {
            (shared.listener)(EngineEvent::Frames(previews));
        }

        // --- Sleep until the next frame (or until something changes) ------------
        let tick = if animating {
            Duration::from_secs_f32(1.0 / fps.max(1) as f32)
        } else {
            Duration::from_millis(250)
        };
        let deadline = now + tick;
        let guard = shared.lock();
        let wait = deadline.saturating_duration_since(Instant::now());
        if !wait.is_zero() && !shared.stop.load(Ordering::SeqCst) {
            let _ = shared.wake.wait_timeout(guard, wait);
        }
    }
}
