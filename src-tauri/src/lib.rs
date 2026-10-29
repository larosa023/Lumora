//! The desktop shell: window, tray, persistence and the commands the UI calls.
//! All the lighting work happens in `lumora-core`.

mod engine_host;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use engine_host::{EngineHost, EngineStatus};
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
}

type CmdResult<T> = Result<T, String>;

#[tauri::command]
fn get_state(app: State<App>, host: State<Arc<EngineHost>>, handle: AppHandle) -> AppState {
    AppState {
        snapshot: app.engine.snapshot(),
        effects: catalog(),
        settings: app.settings.lock().unwrap().clone(),
        profiles: app.profiles.lock().unwrap().clone(),
        active_profile: app.active_profile.lock().unwrap().clone(),
        launch_at_login: handle.autolaunch().is_enabled().unwrap_or(false),
        version: env!("CARGO_PKG_VERSION"),
        engine: host.status(),
    }
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
    let settings = settings.sanitized();
    app.store.save_settings(&settings).map_err(|e| e.to_string())?;
    let before = app.settings.lock().unwrap().clone();
    if before.builtin_engine != settings.builtin_engine || before.host != settings.host {
        host.apply(settings.builtin_engine, &settings.host);
    }
    app.engine.apply_settings(settings.clone());
    *app.settings.lock().unwrap() = settings.clone();
    Ok(settings)
}

/// Lets Lumora drive a device's lights, or leaves it to the vendor's own app.
#[tauri::command]
fn set_device_control(app: State<App>, device: String, on: bool) -> CmdResult<Settings> {
    let mut settings = app.settings.lock().unwrap().clone();
    settings.device_control.insert(device, on);
    app.store.save_settings(&settings).map_err(|e| e.to_string())?;
    app.engine.apply_settings(settings.clone());
    *app.settings.lock().unwrap() = settings.clone();
    Ok(settings)
}

#[tauri::command]
fn rescan(app: State<App>) {
    app.engine.rescan();
}

#[tauri::command]
fn resize_zone(app: State<App>, device: String, zone: usize, size: usize) {
    app.engine.resize_zone(&device, zone, size);
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
            if let Some(app) = handle.try_state::<App>() {
                app.save_session();
            }
            if let Some(host) = handle.try_state::<Arc<EngineHost>>() {
                host.stop_child();
            }
        }
    });
}
