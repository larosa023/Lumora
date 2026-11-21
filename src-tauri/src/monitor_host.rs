//! Runs the system monitor for the window, the overlay, the tray icon and
//! alerts, sampling only as fast as what's showing needs:
//!
//! - in the background (tray only): one light reading every 5 seconds for
//!   the history, alerts and (if chosen) the live tray icon;
//! - while a System page or the overlay is open: a full reading each second;
//! - the process list only while the Processes page is open.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lumora_core::monitor::alerts::Alerts;
use lumora_core::monitor::history::{History, Point};
use lumora_core::monitor::processes::{ProcessRow, ProcessSampler};
use lumora_core::monitor::storage::{DiskHealth, Volume};
use lumora_core::monitor::{now_ms, Sample, Sampler};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

const HISTORY_EVERY: Duration = Duration::from_secs(5);
const DISK_CHECK_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Sensor {
    pub group: String,
    pub name: String,
    pub value: f32,
    /// "°C", "%", "W", "MHz", "rpm", "MB/s"…
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct SystemEvent {
    pub sample: Sample,
    pub sensors: Vec<Sensor>,
}

#[derive(Default)]
struct Shared {
    /// Pages (and the overlay) that want live readings.
    watching: HashSet<String>,
    latest: Option<Sample>,
    sensors: BTreeMap<String, Sensor>,
    disks: Option<(Instant, Vec<DiskHealth>)>,
    volumes: Vec<Volume>,
    /// (pid, parent) from the last process list, for "end process tree".
    pairs: Vec<(u32, u32)>,
    stop: bool,
}

pub struct MonitorHost {
    shared: Mutex<Shared>,
    wake: Condvar,
    pub history: Mutex<History>,
    relay_key: Option<String>,
}

impl MonitorHost {
    pub fn new(history_dir: Option<std::path::PathBuf>, relay_key: Option<String>) -> Arc<Self> {
        Arc::new(MonitorHost {
            shared: Mutex::new(Shared::default()),
            wake: Condvar::new(),
            history: Mutex::new(History::new(history_dir, now_ms())),
            relay_key,
        })
    }

    pub fn relay_key(&self) -> Option<&str> {
        self.relay_key.as_deref()
    }

    pub fn watch(&self, what: &str, on: bool) {
        let mut s = self.shared.lock().unwrap();
        if on {
            s.watching.insert(what.to_string());
        } else {
            s.watching.remove(what);
        }
        drop(s);
        self.wake.notify_all();
    }

    /// The window closed: only the overlay may still be watching.
    pub fn window_closed(&self) {
        let mut s = self.shared.lock().unwrap();
        s.watching.retain(|w| w == "overlay");
        drop(s);
        self.wake.notify_all();
    }

    fn watching(&self, what: &str) -> bool {
        self.shared.lock().unwrap().watching.contains(what)
    }

    pub fn latest(&self) -> Option<Sample> {
        self.shared.lock().unwrap().latest.clone()
    }

    pub fn sensors(&self) -> Vec<Sensor> {
        self.shared.lock().unwrap().sensors.values().cloned().collect()
    }

    pub fn process_pairs(&self) -> Vec<(u32, u32)> {
        self.shared.lock().unwrap().pairs.clone()
    }

    /// Drive health, asked from the helper at most every few minutes.
    pub fn disk_health(&self, fresh: bool) -> Result<Vec<DiskHealth>, String> {
        if let Some((at, d)) = &self.shared.lock().unwrap().disks {
            if !fresh && at.elapsed() < Duration::from_secs(300) {
                return Ok(d.clone());
            }
        }
        let line = lumora_core::monitor::admin::ask(self.relay_key(), "disks")?;
        let disks: Vec<DiskHealth> = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        self.shared.lock().unwrap().disks = Some((Instant::now(), disks.clone()));
        Ok(disks)
    }

    pub fn stop(&self) {
        self.shared.lock().unwrap().stop = true;
        self.wake.notify_all();
        self.history.lock().unwrap().flush();
    }

    fn sleep(&self, d: Duration) -> bool {
        let guard = self.shared.lock().unwrap();
        let (guard, _) = self.wake.wait_timeout(guard, d).unwrap();
        !guard.stop
    }

    /// Starts the background threads.
    pub fn start(self: &Arc<Self>, handle: AppHandle) {
        let me = self.clone();
        let h = handle.clone();
        std::thread::Builder::new()
            .name("lumora-monitor".into())
            .spawn(move || me.run(h))
            .ok();
        let me = self.clone();
        std::thread::Builder::new()
            .name("lumora-processes".into())
            .spawn(move || me.run_processes(handle))
            .ok();
    }

    fn run(&self, handle: AppHandle) {
        let mut sampler = Sampler::new();
        let mut alerts = Alerts::default();
        // (Instants can't go before the PC started, so "long ago" is checked.)
        let long_ago = |d: Duration| Instant::now().checked_sub(d).unwrap_or_else(Instant::now);
        let mut last_history = long_ago(HISTORY_EVERY);
        let mut last_disk_check = long_ago(DISK_CHECK_EVERY);
        // The first reading has no usage yet (it needs two to compare).
        let mut first = true;
        let mut last_icon = String::new();
        loop {
            let (live, storage) = {
                let s = self.shared.lock().unwrap();
                if s.stop {
                    return;
                }
                let live = s.watching.iter().any(|w| w != "processes");
                (live, s.watching.contains("storage"))
            };
            let sample = sampler.sample(storage);

            // History: one point every few seconds.
            if !first && last_history.elapsed() >= HISTORY_EVERY {
                last_history = Instant::now();
                let mut p = Point::empty(sample.at);
                p.set("liquid", lumora_core::temps::get("liquid"));
                p.set("cpu", sample.cpu.temp);
                p.set("gpu", sample.gpu.as_ref().and_then(|g| g.temp));
                p.set("cpu_usage", Some(sample.cpu.usage));
                p.set("gpu_usage", sample.gpu.as_ref().and_then(|g| g.usage));
                p.set("ram", Some(sample.memory.percent));
                p.set("cpu_power", sample.cpu.power);
                p.set("gpu_power", sample.gpu.as_ref().and_then(|g| g.power));
                p.set("net_down", Some(sample.net_down() as f32));
                p.set("net_up", Some(sample.net_up() as f32));
                self.history.lock().unwrap().push(p);

                // Alerts (drives are checked once a minute).
                let settings = handle
                    .try_state::<crate::App>()
                    .map(|a| a.settings.lock().unwrap().clone());
                let check_disks = last_disk_check.elapsed() >= DISK_CHECK_EVERY || storage;
                let volumes = if check_disks {
                    last_disk_check = Instant::now();
                    let v = lumora_core::monitor::storage::volumes();
                    self.shared.lock().unwrap().volumes = v.clone();
                    v
                } else {
                    Vec::new()
                };
                if let Some(settings) = &settings {
                    for a in alerts.check(&settings.alerts, &sample, &volumes, sample.at) {
                        crate::system::notify(&handle, &a.title, &a.body);
                    }
                }
            }

            // The live tray icon.
            if let Some(app) = handle.try_state::<crate::App>() {
                let (mode, unit) = {
                    let s = app.settings.lock().unwrap();
                    (s.tray_icon.clone(), s.units.temp.clone())
                };
                let shown = tray_text(&mode, &unit, &sample);
                let key = shown.as_ref().map(|(t, c)| format!("{t}{c:?}")).unwrap_or_default();
                if key != last_icon {
                    last_icon = key;
                    crate::system::set_tray_icon(&handle, shown);
                }
            }

            let sensors = self.update_sensors(&handle, &sample);
            self.shared.lock().unwrap().latest = Some(sample.clone());
            if live {
                let _ = handle.emit("lumora://system", SystemEvent { sample, sensors });
            }

            let tray_live = handle
                .try_state::<crate::App>()
                .is_some_and(|a| a.settings.lock().unwrap().tray_icon != "logo");
            let wait = if first {
                // A second reading soon, so usage is there from the start.
                Duration::from_secs(1)
            } else if live {
                Duration::from_secs(1)
            } else if tray_live {
                Duration::from_secs(2)
            } else {
                HISTORY_EVERY
            };
            first = false;
            if !self.sleep(wait) {
                return;
            }
        }
    }

    fn run_processes(&self, handle: AppHandle) {
        let mut sampler: Option<ProcessSampler> = None;
        loop {
            if self.shared.lock().unwrap().stop {
                return;
            }
            if self.watching("processes") {
                let s = sampler.get_or_insert_with(ProcessSampler::new);
                let rows: Vec<ProcessRow> = s.sample();
                self.shared.lock().unwrap().pairs = rows.iter().map(|r| (r.pid, r.parent)).collect();
                let _ = handle.emit("lumora://processes", rows);
                if !self.sleep(Duration::from_millis(2000)) {
                    return;
                }
            } else {
                // Nobody's looking: drop the sampler (and its caches).
                sampler = None;
                self.shared.lock().unwrap().pairs = Vec::new();
                if !self.sleep(Duration::from_secs(30)) {
                    return;
                }
            }
        }
    }

    /// Every reading Lumora has, with the lowest and highest seen.
    fn update_sensors(&self, handle: &AppHandle, s: &Sample) -> Vec<Sensor> {
        let mut now: Vec<(String, String, f32, &'static str)> = Vec::new();
        let mut add = |group: &str, name: &str, v: Option<f32>, unit: &'static str| {
            if let Some(v) = v.filter(|v| v.is_finite()) {
                now.push((group.to_string(), name.to_string(), v, unit));
            }
        };
        let cpu = s.cpu.name.clone();
        add(&cpu, "Temperature", s.cpu.temp, "°C");
        add(&cpu, "Package power", s.cpu.power, "W");
        add(&cpu, "Usage", Some(s.cpu.usage), "%");
        add(&cpu, "Clock", s.cpu.clock_mhz.map(|m| m as f32), "MHz");
        for (i, c) in s.cpu.cores.iter().enumerate() {
            add(&cpu, &format!("Core {} usage", i + 1), Some(*c), "%");
        }
        if let Some(g) = &s.gpu {
            add(&g.name, "Temperature", g.temp, "°C");
            add(&g.name, "Usage", g.usage, "%");
            add(&g.name, "Power", g.power, "W");
            add(&g.name, "Core clock", g.core_mhz.map(|m| m as f32), "MHz");
            add(&g.name, "Memory clock", g.mem_mhz.map(|m| m as f32), "MHz");
            add(&g.name, "Fan", g.fan.map(|f| f as f32), "%");
            add(&g.name, "Video memory used", g.vram_used.map(|v| v as f32 / 1_048_576.0), "MB");
        }
        add("Memory", "Used", Some(s.memory.used as f32 / 1_073_741_824.0), "GB");
        add("Memory", "Load", Some(s.memory.percent), "%");
        if let Some(c) = handle.try_state::<lumora_core::cooling::CoolingManager>() {
            for v in c.views() {
                let Some(st) = &v.status else { continue };
                let g = v.name.clone();
                add(&g, "Liquid", st.liquid_c, "°C");
                add(&g, "Pump", st.pump_rpm.map(|r| r as f32), "rpm");
                if st.fan_rpms.is_empty() {
                    add(&g, "Fans", st.fan_rpm.map(|r| r as f32), "rpm");
                }
                for (i, r) in st.fan_rpms.iter().enumerate() {
                    if st.fans_connected.get(i).copied().unwrap_or(true) {
                        add(&g, &format!("Fan {}", i + 1), Some(*r as f32), "rpm");
                    }
                }
            }
        }
        if let Some((_, disks)) = &self.shared.lock().unwrap().disks {
            for d in disks {
                add(&d.model, "Temperature", d.temp, "°C");
                add(&d.model, "Life left", d.life_left.map(|l| l as f32), "%");
            }
        }
        for n in &s.net {
            add("Network", &format!("{} download", n.name), Some(n.down as f32 / 1_048_576.0), "MB/s");
            add("Network", &format!("{} upload", n.name), Some(n.up as f32 / 1_048_576.0), "MB/s");
        }
        let mut sh = self.shared.lock().unwrap();
        let mut seen = HashSet::new();
        for (group, name, v, unit) in now {
            let key = format!("{group}\u{1f}{name}");
            seen.insert(key.clone());
            let e = sh.sensors.entry(key).or_insert(Sensor {
                group,
                name,
                value: v,
                unit,
                min: v,
                max: v,
            });
            e.value = v;
            e.min = e.min.min(v);
            e.max = e.max.max(v);
        }
        sh.sensors.retain(|k, _| seen.contains(k));
        sh.sensors.values().cloned().collect()
    }
}

/// What the live tray icon shows: its text and color.
fn tray_text(mode: &str, unit: &str, s: &Sample) -> Option<(String, [u8; 3])> {
    use lumora_core::monitor::tray_icon::color_for;
    let temp = |c: f32| {
        if unit == "f" {
            c * 9.0 / 5.0 + 32.0
        } else {
            c
        }
    };
    match mode {
        "cpu_temp" => s.cpu.temp.map(|t| (format!("{:.0}", temp(t)), color_for(t, 75.0, 90.0))),
        "gpu_temp" => s
            .gpu
            .as_ref()
            .and_then(|g| g.temp)
            .map(|t| (format!("{:.0}", temp(t)), color_for(t, 70.0, 83.0))),
        "cpu_usage" => Some((format!("{:.0}", s.cpu.usage), color_for(s.cpu.usage, 60.0, 90.0))),
        _ => None,
    }
}
