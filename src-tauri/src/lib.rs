//! The desktop shell: window, tray, persistence and the commands the UI calls.
//! All the lighting work happens in `lumora-core`.

mod engine_host;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use engine_host::{EngineHost, EngineStatus};
use lumora_core::cooling::{CoolerConfig, CoolerView, CoolingManager};
use lumora_core::effects::catalog;
use lumora_core::engine::EngineEvent;
use lumora_core::store::Store;
use lumora_core::{EffectConfig, EffectDescriptor, Engine, Profile, Settings, Snapshot};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};

struct App {
    engine: Engine,
    store: Store,
    settings: Mutex<Settings>,
    profiles: Mutex<Vec<Profile>>,
    active_profile: Mutex<Option<String>>,
    /// The lighting changed since the session was last written to disk.
    dirty: AtomicBool,
    tray_toggle: Mutex<Option<MenuItem<Wry>>>,
}

impl App {
    /// Changes the settings and saves them, holding the lock throughout so two
    /// changes at once can't lose either. Returns the new settings.
    fn edit_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let mut guard = self.settings.lock().unwrap();
        let mut next = guard.clone();
        change(&mut next);
        self.store.save_settings(&next).map_err(|e| format!("Couldn't save settings: {e}"))?;
        *guard = next.clone();
        Ok(next)
    }

    fn touched(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    fn save_session(&self) {
        let session = self.engine.session(self.active_profile.lock().unwrap().clone());
        let _ = self.store.save_session(&session);
    }

    fn sync_tray(&self) {
        if let Some(item) = self.tray_toggle.lock().unwrap().as_ref() {
            let on = self.engine.snapshot().enabled;
            let _ = item.set_text(if on { "Turn lights off" } else { "Turn lights on" });
        }
    }
}

#[derive(Serialize)]
struct AppState {
    snapshot: Snapshot,
    effects: Vec<EffectDescriptor>,
    settings: Settings,
    profiles: Vec<Profile>,
    active_profile: Option<String>,
    launch_at_login: bool,
    version: &'static str,
    engine: EngineStatus,
    coolers: Vec<CoolerView>,
    temps: Vec<lumora_core::temps::TempReading>,
}

type CmdResult<T> = Result<T, String>;

#[tauri::command]
fn get_state(app: State<App>, host: State<Arc<EngineHost>>, cooling: State<CoolingManager>, handle: AppHandle) -> AppState {
    AppState {
        snapshot: app.engine.snapshot(),
        effects: catalog(),
        settings: app.settings.lock().unwrap().clone(),
        profiles: app.profiles.lock().unwrap().clone(),
        active_profile: app.active_profile.lock().unwrap().clone(),
        launch_at_login: handle.autolaunch().is_enabled().unwrap_or(false),
        version: env!("CARGO_PKG_VERSION"),
        engine: host.status(),
        coolers: cooling.views(),
        temps: lumora_core::temps::all(),
    }
}

/// Saves and applies one cooler's settings (takeover, curves, screen).
#[tauri::command]
fn set_cooler_config(app: State<App>, cooling: State<CoolingManager>, id: String, config: CoolerConfig) -> CmdResult<Vec<CoolerView>> {
    app.edit_settings(|s| {
        s.coolers.insert(id.clone(), config.clone());
    })?;
    cooling.set_config(&id, config);
    Ok(cooling.views())
}

/// Saves a picture for a cooler's screen (raw RGBA at the screen's size, from
/// the UI) and returns the file to put in the cooler's settings. Each pick gets
/// a new file so the change is noticed; older picks for that cooler are removed.
#[tauri::command]
fn save_cooler_picture(handle: tauri::AppHandle, id: String, rgba: String) -> CmdResult<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(rgba.as_bytes()).map_err(|e| e.to_string())?;
    let side = ((bytes.len() / 4) as f64).sqrt() as usize;
    if bytes.is_empty() || side * side * 4 != bytes.len() {
        return Err("The picture isn't square RGBA data".into());
    }
    let dir = handle.path().app_config_dir().map_err(|e| e.to_string())?.join("screens");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem: String = id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{stem}_")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("{stem}_{stamp}.rgba"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

pub fn run_elevated_helper() -> Option<i32> {
    engine_host::run_elevated_from_args()
}

#[tauri::command]
async fn enable_full_access(host: State<'_, Arc<EngineHost>>) -> CmdResult<EngineStatus> {
    let h = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || h.enable_full_access().map(|_| h.status()))
        .await
        .map_err(|e| e.to_string())?
}

/// One-time setup for CPU temperature (a small helper with admin rights).
#[tauri::command]
async fn setup_cpu_sensor(host: State<'_, Arc<EngineHost>>) -> CmdResult<()> {
    let h = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || h.install_cpu_sensor()).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn cpu_sensor() -> lumora_core::temps::cpu::Status {
    lumora_core::temps::cpu::status()
}

/// One-time setup so Lumora can reach coolers that need an access driver
/// (Corsair Hydro Pro on Windows). One Windows permission prompt.
#[tauri::command]
async fn setup_cooler_access(host: State<'_, Arc<EngineHost>>) -> CmdResult<()> {
    let h = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || h.install_cooler_driver()).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn disable_full_access(host: State<'_, Arc<EngineHost>>) -> CmdResult<EngineStatus> {
    let h = host.inner().clone();
    tauri::async_runtime::spawn_blocking(move || h.disable_full_access().map(|_| h.status()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_snapshot(app: State<App>) -> Snapshot {
    app.engine.snapshot()
}

#[tauri::command]
fn set_effect(app: State<App>, device: Option<String>, config: EffectConfig) {
    app.engine.set_effect(device.as_deref(), config);
    app.touched();
}

#[tauri::command]
fn clear_override(app: State<App>, device: String) {
    app.engine.clear_override(&device);
    app.touched();
}

#[tauri::command]
fn sync_all(app: State<App>, config: EffectConfig) {
    app.engine.sync_all(config);
    app.touched();
}

#[tauri::command]
fn set_brightness(app: State<App>, value: u8) {
    app.engine.set_brightness(value);
    app.touched();
}

#[tauri::command]
fn set_enabled(app: State<App>, on: bool) {
    app.engine.set_enabled(on);
    app.sync_tray();
    app.touched();
}

#[tauri::command]
fn set_preview(app: State<App>, on: bool) {
    app.engine.set_preview(on);
}

fn clean_name(name: &str) -> CmdResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("Profile names need 1–40 characters".into());
    }
    Ok(name.to_string())
}

#[tauri::command]
fn save_profile(app: State<App>, name: String) -> CmdResult<Vec<Profile>> {
    let name = clean_name(&name)?;
    let mut profile = app.engine.lighting();
    profile.name = name.clone();
    let mut profiles = app.profiles.lock().unwrap();
    match profiles.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
        Some(existing) => *existing = profile,
        None => profiles.push(profile),
    }
    app.store.save_profiles(&profiles).map_err(|e| e.to_string())?;
    *app.active_profile.lock().unwrap() = Some(name);
    app.touched();
    Ok(profiles.clone())
}

#[tauri::command]
fn load_profile(app: State<App>, name: String) -> CmdResult<()> {
    let profile = app
        .profiles
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.name == name)
        .cloned()
        .ok_or_else(|| format!("No profile named {name}"))?;
    app.engine.load_lighting(profile);
    *app.active_profile.lock().unwrap() = Some(name);
    app.sync_tray();
    app.touched();
    Ok(())
}

#[tauri::command]
fn delete_profile(app: State<App>, name: String) -> CmdResult<Vec<Profile>> {
    let mut profiles = app.profiles.lock().unwrap();
    profiles.retain(|p| p.name != name);
    app.store.save_profiles(&profiles).map_err(|e| e.to_string())?;
    let mut active = app.active_profile.lock().unwrap();
    if active.as_deref() == Some(name.as_str()) {
        *active = None;
    }
    app.touched();
    Ok(profiles.clone())
}

#[tauri::command]
fn update_settings(app: State<App>, host: State<Arc<EngineHost>>, settings: Settings) -> CmdResult<Settings> {
    let mut before = None;
    let settings = app.edit_settings(|s| {
        before = Some(s.clone());
        *s = settings.merged_into(s).sanitized();
    })?;
    let before = before.expect("set above");
    if before.builtin_engine != settings.builtin_engine || before.host != settings.host {
        host.apply(settings.builtin_engine, &settings.host);
    }
    app.engine.apply_settings(settings.clone());
    Ok(settings)
}

/// Lines worth keeping from the engine log: what it found, what failed.
fn interesting_log_lines(text: &str) -> Vec<String> {
    const KEEP: &[&str] = &[
        "OpenRGB v", "Registering", "PawnIO", "[Error", "HID device connected", "not respond",
        "Detected occupied slot", "Detection completed", "Setting config", "failed", "Failed",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut repeats = std::collections::HashMap::<String, usize>::new();
    for line in text.lines() {
        if !KEEP.iter().any(|k| line.contains(k)) {
            continue;
        }
        // Collapse the same message repeating (a device timing out every second).
        let key: String = line.splitn(2, ']').nth(1).unwrap_or(line).chars().filter(|c| !c.is_ascii_digit()).collect();
        let n = repeats.entry(key).or_insert(0);
        *n += 1;
        if *n <= 2 {
            out.push(line.trim_end().to_string());
        }
    }
    let skip = out.len().saturating_sub(250);
    out.split_off(skip)
}

/// Other lighting apps that are installed or running, for the report.
fn lighting_services() -> String {
    const SERVICES: &[&str] = &[
        "LightingService", "ArmouryCrateService", "ArmouryCrateControlInterface", "AsusCertService", "asComSvc",
        "CorsairService", "CorsairLLAService", "CAM_Service", "NZXT CAM Service", "MSI_Center_Service",
        "MysticLight2_Service", "LGHUBUpdaterService", "Razer Synapse Service", "SignalRgbService",
    ];
    const APPS: &[&str] = &[
        "armourycrate", "lightingservice", "nzxt cam", "icue", "signalrgb", "mysticlight", "rgbfusion", "synapse", "lghub", "ngenuity", "openrgb",
    ];
    let mut lines: Vec<String> = SERVICES
        .iter()
        .filter(|s| lumora_core::system::service_exists(s))
        .map(|s| format!("service: {s}"))
        .collect();
    let mut running: Vec<String> = lumora_core::system::processes()
        .into_iter()
        .filter(|p| APPS.iter().any(|a| p.name.contains(a)))
        .map(|p| format!("process: {}", p.name))
        .collect();
    running.sort();
    running.dedup();
    lines.extend(running);
    lines.join("\n")
}

/// A plain-text report for troubleshooting a PC, to copy and paste.
#[tauri::command]
async fn diagnostic_report(app: State<'_, App>, host: State<'_, Arc<EngineHost>>, cooling: State<'_, CoolingManager>) -> CmdResult<String> {
    use std::fmt::Write as _;
    let snap = app.engine.snapshot();
    let status = host.status();
    let settings = app.settings.lock().unwrap().clone();
    let mut r = String::new();
    let _ = writeln!(r, "Lumora {} on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    let _ = writeln!(
        r,
        "Engine: {:?}, RAM & motherboard access: {:?}, version {}",
        status.mode,
        status.full_access,
        status.version.clone().unwrap_or_else(|| "?".into())
    );
    if let Some(p) = &status.problem {
        let _ = writeln!(r, "Engine problem: {p}");
    }
    let _ = writeln!(r, "Connection: {:?} {} ({})", snap.connection.status, snap.connection.server, snap.connection.message);
    let _ = writeln!(r, "\n--- Devices ---");
    for d in &snap.devices {
        let _ = writeln!(
            r,
            "{} | {} | {:?} | {} LEDs | {} zones | {}{}{}",
            d.vendor,
            d.name,
            d.kind,
            d.led_count,
            d.zones.len(),
            if d.managed { "controlled" } else { "NOT controlled" },
            if d.controllable { "" } else { " | no direct mode" },
            if d.virtual_device { " | demo" } else { "" },
        );
    }
    let off: Vec<_> = settings.device_control.iter().filter(|(_, on)| !**on).map(|(id, _)| id.as_str()).collect();
    if !off.is_empty() {
        let _ = writeln!(r, "Switched off: {}", off.join(", "));
    }
    {
        let cooling = cooling.views();
        let cpu = lumora_core::temps::cpu::status();
        let _ = writeln!(r, "CPU temperature: {:?} {} ({:?})", cpu.state, cpu.message, lumora_core::temps::get("cpu"));
        let _ = writeln!(r, "\n--- Coolers ---");
        let _ = writeln!(r, "{}", lumora_core::cooling::usb::report());
        if cooling.is_empty() {
            let _ = writeln!(r, "(none found)");
        }
        for c in cooling {
            let _ = writeln!(
                r,
                "{} {} | {} | takeover {} | {:?} {} | {:?}",
                c.vendor, c.name, c.id, c.takeover, c.state, c.message, c.status
            );
        }
    }
    let services = tauri::async_runtime::spawn_blocking(lighting_services).await.unwrap_or_default();
    let _ = writeln!(r, "\n--- Other RGB software ---\n{}", if services.is_empty() { "(none found)" } else { &services });

    let dir = host.log_dir();
    let newest = std::fs::read_dir(&dir).ok().and_then(|it| {
        it.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
    });
    let _ = writeln!(r, "\n--- Engine log ({}) ---", dir.display());
    match newest.and_then(|e| std::fs::read(e.path()).ok()) {
        Some(bytes) => {
            for line in interesting_log_lines(&String::from_utf8_lossy(&bytes)) {
                let _ = writeln!(r, "{line}");
            }
        }
        None => {
            let _ = writeln!(r, "(no log found)");
        }
    }
    Ok(r)
}

/// Lets Lumora drive a device's lights, or leaves it to the vendor's own app.
#[tauri::command]
fn set_device_control(app: State<App>, device: String, on: bool) -> CmdResult<Settings> {
    let settings = app.edit_settings(|s| {
        s.device_control.insert(device, on);
    })?;
    app.engine.apply_settings(settings.clone());
    Ok(settings)
}

#[tauri::command]
fn rescan(app: State<App>) {
    app.engine.rescan();
}

#[tauri::command]
fn resize_zone(app: State<App>, device: String, zone: usize, size: usize) -> CmdResult<()> {
    app.engine.resize_zone(&device, zone, size);
    // Remembered, so the size comes back after restarts and updates.
    let settings = app.edit_settings(|s| {
        s.zone_sizes.entry(device).or_default().insert(zone, size);
    })?;
    app.engine.apply_settings(settings);
    Ok(())
}

#[tauri::command]
fn reconnect(app: State<App>) {
    app.engine.reconnect();
}

#[tauri::command]
fn set_launch_at_login(handle: AppHandle, on: bool) -> CmdResult<bool> {
    let auto = handle.autolaunch();
    let r = if on { auto.enable() } else { auto.disable() };
    r.map_err(|e| e.to_string())?;
    Ok(auto.is_enabled().unwrap_or(on))
}

#[tauri::command]
fn open_external(handle: AppHandle, url: String) -> CmdResult<()> {
    if !url.starts_with("https://") {
        return Err("Only https links can be opened".into());
    }
    handle.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// --- Updates ---------------------------------------------------------------
//
// Releases are published to GitHub with a signed `latest.json` (see
// .github/workflows/release.yml). The updater checks it, verifies the
// download against the public key in tauri.conf.json, installs, restarts.

#[derive(Default)]
struct Updates {
    ready: Mutex<Option<Update>>,
    installing: AtomicBool,
}

#[derive(Serialize, Clone)]
struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
    date: Option<String>,
}

#[derive(Serialize, Clone)]
struct UpdateProgress {
    downloaded: u64,
    total: Option<u64>,
}

async fn find_update(handle: &AppHandle) -> CmdResult<Option<UpdateInfo>> {
    let updater = handle.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    let updates = handle.state::<Updates>();
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone().filter(|b| !b.trim().is_empty()),
        date: u.date.map(|d| d.date().to_string()),
    });
    *updates.ready.lock().unwrap() = found;
    Ok(info)
}

#[tauri::command]
async fn check_update(handle: AppHandle) -> CmdResult<Option<UpdateInfo>> {
    find_update(&handle).await.map_err(|e| {
        if e.contains("404") || e.to_lowercase().contains("could not fetch") {
            "No releases have been published yet".to_string()
        } else {
            format!("Couldn't check for updates: {e}")
        }
    })
}

#[tauri::command]
async fn install_update(handle: AppHandle) -> CmdResult<()> {
    let updates = handle.state::<Updates>();
    if updates.installing.swap(true, Ordering::SeqCst) {
        return Err("An update is already installing".into());
    }
    let update = updates.ready.lock().unwrap().take();
    let Some(update) = update else {
        updates.installing.store(false, Ordering::SeqCst);
        return Err("Check for updates first".into());
    };
    // Lighting comes back exactly as it was after the restart.
    if let Some(app) = handle.try_state::<App>() {
        app.save_session();
    }
    let progress = handle.clone();
    let mut downloaded = 0u64;
    let result = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit("lumora://update-progress", UpdateProgress { downloaded, total });
            },
            || {},
        )
        .await;
    // The installer replaces the bundled engine, so it mustn't be running.
    let result = match result {
        Ok(bytes) => {
            if let Some(host) = handle.try_state::<Arc<EngineHost>>() {
                host.stop_for_update();
            }
            update.install(bytes)
        }
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        updates.installing.store(false, Ordering::SeqCst);
        return Err(format!("The update couldn't be installed: {e}"));
    }
    handle.restart();
}

/// Checks shortly after launch, then every six hours while the app runs.
fn spawn_update_checks(handle: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(8));
        loop {
            let enabled = handle.try_state::<App>().is_some_and(|a| a.settings.lock().unwrap().check_for_updates);
            if enabled && !handle.state::<Updates>().installing.load(Ordering::SeqCst) {
                if let Ok(Some(info)) = tauri::async_runtime::block_on(find_update(&handle)) {
                    let _ = handle.emit("lumora://update-available", info);
                }
            }
            std::thread::sleep(Duration::from_secs(6 * 60 * 60));
        }
    });
}

fn show_main(handle: &AppHandle) {
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    if let Some(app) = handle.try_state::<App>() {
        app.engine.set_preview(true);
    }
}

fn build_tray(handle: &AppHandle) -> tauri::Result<MenuItem<Wry>> {
    let show = MenuItem::with_id(handle, "show", "Open Lumora", true, None::<&str>)?;
    let toggle = MenuItem::with_id(handle, "toggle", "Turn lights off", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(handle, &[&show, &toggle, &PredefinedMenuItem::separator(handle)?, &quit])?;
    let mut tray = TrayIconBuilder::with_id("lumora")
        .tooltip("Lumora")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|h, e| match e.id.as_ref() {
            "show" => show_main(h),
            "toggle" => {
                if let Some(app) = h.try_state::<App>() {
                    let on = !app.engine.snapshot().enabled;
                    app.engine.set_enabled(on);
                    app.sync_tray();
                    app.touched();
                }
            }
            "quit" => h.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = handle.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(handle)?;
    Ok(toggle)
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|h, _, _| show_main(h)))
        .plugin(tauri_plugin_opener::init())
        // Window size and position come back on the next launch. Whether it
        // starts visible is Lumora's own choice (tray, start minimized).
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(tauri_plugin_window_state::StateFlags::all() - tauri_plugin_window_state::StateFlags::VISIBLE)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Updates::default())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .setup(|app| {
            let handle = app.handle().clone();
            let dir = app.path().app_config_dir()?;
            let store = Store::new(dir);
            let settings = store.settings();
            let session = store.session();
            let active_profile = session.active_profile.clone();

            let emitter = handle.clone();
            let listener = Arc::new(move |e: EngineEvent| match e {
                EngineEvent::Changed => {
                    if let Some(app) = emitter.try_state::<App>() {
                        let _ = emitter.emit("lumora://changed", app.engine.snapshot());
                    }
                }
                EngineEvent::Frames(frames) => {
                    let b64 = base64::engine::general_purpose::STANDARD;
                    let payload: HashMap<String, String> = frames.into_iter().map(|(id, rgb)| (id, b64.encode(rgb))).collect();
                    let _ = emitter.emit("lumora://frames", payload);
                }
            });

            let engine_events = handle.clone();
            let host = EngineHost::new(app.path().resource_dir().ok(), app.path().app_config_dir()?, move |status| {
                let _ = engine_events.emit("lumora://engine", status);
            });
            host.apply(settings.builtin_engine, &settings.host);
            app.manage(host);

            let cooler_events = handle.clone();
            // A fresh key for the admin helper's cooler relay, every start.
            let relay_key = app
                .path()
                .app_config_dir()
                .ok()
                .and_then(|d| lumora_core::cooling::relay::new_key(&d.join("relay.key")).ok());
            let backend: Box<dyn lumora_core::cooling::Backend> = match std::env::var("LUMORA_FAKE_COOLER") {
                // Pretend hardware for trying the cooling page: a Kraken Elite and
                // an H115i Pro (`setup` makes the Corsair need its one-time setup).
                Ok(v) => {
                    let fake = lumora_core::cooling::fake::FakeBackend::with_kraken(0x300C).0;
                    fake.add_hydro(0x0C13).lock().unwrap().needs_setup = v == "setup";
                    Box::new(fake)
                }
                Err(_) => Box::new(lumora_core::cooling::MultiBackend::new(vec![
                    Box::new(lumora_core::cooling::hid::HidBackend::new()),
                    Box::new(lumora_core::cooling::usb::UsbBackend::new(relay_key.clone())),
                ])),
            };
            let cooling = CoolingManager::new(
                backend,
                settings.coolers.clone(),
                Box::new(lumora_core::cooling::hid::running_processes),
                Arc::new(move |views| {
                    let _ = cooler_events.emit("lumora://coolers", views);
                }),
            );
            app.manage(cooling);
            lumora_core::temps::gpu::start_polling();
            lumora_core::temps::cpu::start_polling();
            let temp_events = handle.clone();
            std::thread::spawn(move || loop {
                let _ = temp_events.emit("lumora://temps", lumora_core::temps::all());
                std::thread::sleep(Duration::from_secs(2));
            });

            let start_hidden = settings.start_minimized || std::env::args().any(|a| a == "--minimized");
            let engine = Engine::new(settings.clone(), session, listener);
            app.manage(App {
                engine,
                profiles: Mutex::new(store.profiles()),
                store,
                settings: Mutex::new(settings),
                active_profile: Mutex::new(active_profile),
                dirty: AtomicBool::new(false),
                tray_toggle: Mutex::new(None),
            });

            let toggle = build_tray(&handle)?;
            let state = handle.state::<App>();
            *state.tray_toggle.lock().unwrap() = Some(toggle);
            state.sync_tray();

            // Write the session a moment after changes settle, not on every slider tick.
            let saver = handle.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(1500));
                let app = saver.state::<App>();
                if app.dirty.swap(false, Ordering::SeqCst) {
                    app.save_session();
                }
            });

            spawn_update_checks(handle.clone());

            if !start_hidden {
                show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.state::<App>();
                if app.settings.lock().unwrap().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                    app.engine.set_preview(false);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            setup_cooler_access,
            setup_cpu_sensor,
            cpu_sensor,
            save_cooler_picture,
            get_snapshot,
            set_effect,
            clear_override,
            sync_all,
            set_brightness,
            set_enabled,
            set_preview,
            save_profile,
            load_profile,
            delete_profile,
            update_settings,
            reconnect,
            rescan,
            resize_zone,
            set_device_control,
            set_cooler_config,
            diagnostic_report,
            set_launch_at_login,
            open_external,
            check_update,
            install_update,
            enable_full_access,
            disable_full_access,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Lumora");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            use tauri_plugin_window_state::AppHandleExt;
            let _ = handle.save_window_state(tauri_plugin_window_state::StateFlags::all() - tauri_plugin_window_state::StateFlags::VISIBLE);
            if let Some(app) = handle.try_state::<App>() {
                app.save_session();
            }
            if let Some(host) = handle.try_state::<Arc<EngineHost>>() {
                host.stop_child();
            }
            if let Some(cooling) = handle.try_state::<CoolingManager>() {
                cooling.shutdown();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::interesting_log_lines;

    #[test]
    fn log_filter_keeps_findings_and_collapses_repeats() {
        let log = "[1][Info   ]    OpenRGB v1.1\n[2][Info   ][DetectionManager] Registering RGB controller ENE DRAM\n\
                   [3][Warning][HyperX] No response received from device (timeout: 2000 ms)\n\
                   [4][Error  ][HyperX] Multiple consecutive communication errors detected.\n\
                   [5][Error  ][HyperX] Multiple consecutive communication errors detected.\n\
                   [6][Error  ][HyperX] Multiple consecutive communication errors detected.\n\
                   [7][Info   ]PawnIO initialized successully\n";
        let lines = interesting_log_lines(log);
        assert!(lines.iter().any(|l| l.contains("OpenRGB v1.1")));
        assert!(lines.iter().any(|l| l.contains("Registering RGB controller ENE DRAM")));
        assert!(lines.iter().any(|l| l.contains("PawnIO")));
        assert_eq!(lines.iter().filter(|l| l.contains("consecutive")).count(), 2);
        assert!(!lines.iter().any(|l| l.contains("No response")));

        if let Ok(path) = std::env::var("LUMORA_SAMPLE_LOG") {
            let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).to_string();
            let out = interesting_log_lines(&text);
            println!("{} lines kept\n{}", out.len(), out.join("\n"));
        }
    }
}
