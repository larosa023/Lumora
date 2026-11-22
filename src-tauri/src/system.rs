//! The System pages' commands (processes, startup apps, services, drives,
//! specs, power plans, history), the mini overlay, hotkeys, alerts and the
//! live tray icon.

use std::sync::{Arc, Mutex};

use lumora_core::monitor::{self, now_ms};
use serde::Serialize;
use tauri::menu::{CheckMenuItem, MenuItem};
use tauri::{AppHandle, Manager, State, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

use crate::monitor_host::{MonitorHost, Sensor};
use crate::{show_main, App, CmdResult};

type Host<'a> = State<'a, Arc<MonitorHost>>;

/// Tray items that change: the overlay toggle and the power plans.
#[derive(Default)]
pub struct TrayItems {
    pub overlay: Mutex<Option<MenuItem<Wry>>>,
    pub plans: Mutex<Vec<(String, CheckMenuItem<Wry>)>>,
}

pub fn notify(handle: &AppHandle, title: &str, body: &str) {
    let _ = handle.notification().builder().title(title).body(body).show();
}

/// Puts a number on the tray icon, or the logo back (`None`).
pub fn set_tray_icon(handle: &AppHandle, shown: Option<(String, [u8; 3])>) {
    let Some(tray) = handle.tray_by_id("lumora") else { return };
    let icon = match shown {
        Some((text, color)) => {
            let n = monitor::tray_icon::SIZE as u32;
            Some(tauri::image::Image::new_owned(monitor::tray_icon::render(&text, color), n, n))
        }
        None => handle.default_window_icon().cloned(),
    };
    let _ = tray.set_icon(icon);
}

/// Something that needs Lumora's helper, said plainly.
fn helper_error(e: String) -> String {
    match e.as_str() {
        "helper" | "Lumora's helper isn't set up" => "needs-helper".into(),
        "helper-old" => "helper-old".into(),
        _ => e,
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

// ---- Live readings ---------------------------------------------------------

/// A page (or the overlay) wants live readings, or is done with them.
#[tauri::command]
pub fn monitor_watch(host: Host, what: String, on: bool) {
    host.watch(&what, on);
}

#[derive(Serialize)]
pub struct Snapshot {
    sample: Option<monitor::Sample>,
    sensors: Vec<Sensor>,
}

#[tauri::command]
pub fn system_snapshot(host: Host) -> Snapshot {
    Snapshot { sample: host.latest(), sensors: host.sensors() }
}

// ---- Processes -------------------------------------------------------------

#[tauri::command]
pub async fn process_action(host: Host<'_>, pid: u32, action: String, value: Option<String>, path: Option<String>) -> CmdResult<()> {
    let h = host.inner().clone();
    blocking(move || {
        let end = |pid: u32| {
            monitor::processes::end(pid).or_else(|_| {
                monitor::admin::ask(h.relay_key(), &format!("kill {pid}")).map(|_| ()).map_err(|e| match e.as_str() {
                    "helper" | "helper-old" | "Lumora's helper isn't set up" => "Windows didn't allow ending it".to_string(),
                    _ => e,
                })
            })
        };
        match action.as_str() {
            "end" => end(pid),
            "tree" => {
                let pairs = h.process_pairs();
                let mut first_err = None;
                // Children first, so nothing respawns from a parent.
                for p in monitor::processes::tree(&pairs, pid).into_iter().rev() {
                    if let Err(e) = end(p) {
                        first_err.get_or_insert(e);
                    }
                }
                first_err.map_or(Ok(()), Err)
            }
            "priority" => monitor::processes::set_priority(pid, value.as_deref().unwrap_or("normal")),
            "efficiency" => monitor::processes::set_efficiency(pid, value.as_deref() == Some("on")),
            "location" => monitor::processes::open_location(path.as_deref().ok_or("unknown file")?),
            "properties" => monitor::processes::properties(path.as_deref().ok_or("unknown file")?),
            _ => Err("unknown action".into()),
        }
    })
    .await?
}

/// Icons already looked up (path → PNG data URL, or none).
#[derive(Default)]
pub struct IconCache(Mutex<std::collections::HashMap<String, Option<String>>>);

/// Program icons for a list of files, as data URLs (`None`: no icon).
#[tauri::command]
pub async fn app_icons(cache: State<'_, IconCache>, paths: Vec<String>) -> CmdResult<std::collections::HashMap<String, Option<String>>> {
    let mut out = std::collections::HashMap::new();
    let mut missing = Vec::new();
    {
        let c = cache.0.lock().unwrap();
        for p in paths.into_iter().take(400) {
            match c.get(&p) {
                Some(v) => {
                    out.insert(p, v.clone());
                }
                None => missing.push(p),
            }
        }
    }
    if !missing.is_empty() {
        let found = blocking(move || {
            use base64::Engine;
            missing
                .into_iter()
                .map(|p| {
                    let url = monitor::icons::icon_png(&p)
                        .map(|png| format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)));
                    (p, url)
                })
                .collect::<Vec<_>>()
        })
        .await?;
        let mut c = cache.0.lock().unwrap();
        for (p, url) in found {
            c.insert(p.clone(), url.clone());
            out.insert(p, url);
        }
    }
    Ok(out)
}

// ---- Startup apps & services ----------------------------------------------

#[tauri::command]
pub async fn startup_list() -> CmdResult<Vec<monitor::startup::StartupItem>> {
    blocking(monitor::startup::list).await
}

#[tauri::command]
pub async fn startup_set(host: Host<'_>, id: String, on: bool) -> CmdResult<()> {
    let h = host.inner().clone();
    blocking(move || match monitor::startup::set_enabled(&id, on) {
        Err(e) if e == "admin" => monitor::admin::ask(h.relay_key(), &format!("startup {} {id}", on as u8))
            .map(|_| ())
            .map_err(helper_error),
        r => r,
    })
    .await?
}

#[tauri::command]
pub async fn services_list() -> CmdResult<Vec<monitor::services::Service>> {
    blocking(monitor::services::list).await
}

#[tauri::command]
pub async fn service_action(host: Host<'_>, name: String, action: String) -> CmdResult<()> {
    let h = host.inner().clone();
    blocking(move || {
        monitor::admin::ask(h.relay_key(), &format!("svc {action} {name}")).map(|_| ()).map_err(helper_error)
    })
    .await?
}

#[tauri::command]
pub async fn service_start_type(host: Host<'_>, name: String, start: String) -> CmdResult<()> {
    let h = host.inner().clone();
    blocking(move || {
        monitor::admin::ask(h.relay_key(), &format!("svcstart {name} {start}")).map(|_| ()).map_err(helper_error)
    })
    .await?
}

// ---- Drives, specs, power ---------------------------------------------------

#[derive(Serialize)]
pub struct StorageInfo {
    volumes: Vec<monitor::storage::Volume>,
    disks: Vec<monitor::storage::DiskHealth>,
    /// Why drive health is missing ("needs-helper", "helper-old"), if it is.
    health_error: Option<String>,
}

#[tauri::command]
pub async fn storage_info(host: Host<'_>, fresh: bool) -> CmdResult<StorageInfo> {
    let h = host.inner().clone();
    blocking(move || {
        let volumes = monitor::storage::volumes();
        let (disks, health_error) = match h.disk_health(fresh) {
            Ok(d) => (d, None),
            Err(e) => (Vec::new(), Some(helper_error(e))),
        };
        StorageInfo { volumes, disks, health_error }
    })
    .await
}

#[derive(Serialize)]
pub struct SpecsInfo {
    specs: monitor::specs::Specs,
    text: String,
}

#[tauri::command]
pub async fn specs() -> CmdResult<SpecsInfo> {
    blocking(|| {
        let specs = monitor::specs::gather();
        let text = monitor::specs::as_text(&specs);
        SpecsInfo { specs, text }
    })
    .await
}

#[tauri::command]
pub async fn power_plans() -> CmdResult<Vec<monitor::power::PowerPlan>> {
    blocking(monitor::power::plans).await
}

#[tauri::command]
pub async fn set_power_plan(handle: AppHandle, id: String) -> CmdResult<Vec<monitor::power::PowerPlan>> {
    let h = handle.clone();
    blocking(move || {
        monitor::power::set_plan(&id)?;
        let plans = monitor::power::plans();
        sync_plan_checks(&h, &plans);
        Ok(plans)
    })
    .await?
}

pub fn sync_plan_checks(handle: &AppHandle, plans: &[monitor::power::PowerPlan]) {
    if let Some(items) = handle.try_state::<TrayItems>() {
        for (id, item) in items.plans.lock().unwrap().iter() {
            let _ = item.set_checked(plans.iter().any(|p| &p.id == id && p.active));
        }
    }
}

#[derive(Serialize)]
pub struct Freed {
    /// Available memory before and after, bytes.
    before: u64,
    after: u64,
}

#[tauri::command]
pub async fn free_memory(host: Host<'_>) -> CmdResult<Freed> {
    let h = host.inner().clone();
    blocking(move || {
        let mut s = monitor::Sampler::new();
        let before = s.sample(false).memory.available;
        monitor::admin::ask(h.relay_key(), "purge").map_err(helper_error)?;
        std::thread::sleep(std::time::Duration::from_millis(400));
        let after = s.sample(false).memory.available;
        Ok(Freed { before, after })
    })
    .await?
}

// ---- History -----------------------------------------------------------------

#[tauri::command]
pub fn history_range(host: Host, minutes: u64) -> Vec<monitor::history::Point> {
    let since = now_ms().saturating_sub(minutes * 60_000);
    host.history.lock().unwrap().range(since, 720)
}

/// Saves the readings to a CSV in Downloads and shows it.
#[tauri::command]
pub async fn history_export(handle: AppHandle, host: Host<'_>, minutes: u64) -> CmdResult<String> {
    let h = host.inner().clone();
    let dir = handle.path().download_dir().map_err(|e| e.to_string())?;
    blocking(move || {
        let since = now_ms().saturating_sub(minutes * 60_000);
        let csv = h.history.lock().unwrap().export(since);
        let name = format!(
            "Lumora history {} {}.csv",
            monitor::history::day_name(now_ms()),
            monitor::history::time_name(now_ms()).replace(':', "-")
        );
        let path = dir.join(name);
        std::fs::write(&path, csv).map_err(|e| format!("Couldn't save it: {e}"))?;
        let p = path.display().to_string();
        let _ = monitor::processes::open_location(&p);
        Ok(p)
    })
    .await?
}

/// The cooler page's half-hour chart, from the same history.
pub fn temp_history(host: &MonitorHost) -> Vec<lumora_core::temps::HistoryPoint> {
    let since = now_ms().saturating_sub(30 * 60_000);
    host.history
        .lock()
        .unwrap()
        .range(since, 400)
        .into_iter()
        .map(|p| lumora_core::temps::HistoryPoint {
            at: p.at,
            values: ["liquid", "cpu", "gpu"]
                .into_iter()
                .filter_map(|k| p.get(k).map(|v| (k.to_string(), v)))
                .collect(),
        })
        .collect()
}

// ---- The overlay -------------------------------------------------------------

/// The overlay's size: readings on top, the temperature chart below.
const OVERLAY_W: f64 = 236.0;
const OVERLAY_H: f64 = 282.0;

pub fn overlay_shown(handle: &AppHandle) -> bool {
    handle.get_webview_window("overlay").is_some()
}

/// Shows or hides the overlay, and remembers which.
pub fn set_overlay(handle: &AppHandle, show: bool) {
    let locked = handle
        .try_state::<App>()
        .map(|a| a.settings.lock().unwrap().overlay.locked)
        .unwrap_or(false);
    if show {
        if handle.get_webview_window("overlay").is_none() {
            let built = tauri::WebviewWindowBuilder::new(handle, "overlay", tauri::WebviewUrl::App("overlay.html".into()))
                .title("Lumora overlay")
                .inner_size(OVERLAY_W, OVERLAY_H)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .shadow(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .focused(false)
                .build();
            if let Ok(w) = built {
                // A size remembered from an older, shorter overlay doesn't apply.
                let _ = w.set_size(tauri::LogicalSize::new(OVERLAY_W, OVERLAY_H));
                let _ = w.set_ignore_cursor_events(locked);
            }
        }
    } else if let Some(w) = handle.get_webview_window("overlay") {
        let _ = w.destroy();
        if let Some(h) = handle.try_state::<Arc<MonitorHost>>() {
            h.watch("overlay", false);
        }
    }
    if let Some(app) = handle.try_state::<App>() {
        let _ = app.edit_settings(|s| s.overlay.shown = show);
    }
    if let Some(items) = handle.try_state::<TrayItems>() {
        if let Some(i) = items.overlay.lock().unwrap().as_ref() {
            let _ = i.set_text(if show { "Hide overlay" } else { "Show overlay" });
        }
    }
    use tauri::Emitter;
    let _ = handle.emit("lumora://overlay", show);
}

#[derive(Serialize)]
pub struct OverlayState {
    shown: bool,
    locked: bool,
}

// Async: building a window inside a plain command can hang on Windows.
#[tauri::command]
pub async fn overlay_toggle(handle: AppHandle, show: Option<bool>) -> OverlayState {
    let show = show.unwrap_or(!overlay_shown(&handle));
    set_overlay(&handle, show);
    overlay_state(&handle)
}

#[tauri::command]
pub fn overlay_lock(handle: AppHandle, app: State<App>, locked: bool) -> OverlayState {
    let _ = app.edit_settings(|s| s.overlay.locked = locked);
    if let Some(w) = handle.get_webview_window("overlay") {
        let _ = w.set_ignore_cursor_events(locked);
    }
    use tauri::Emitter;
    let _ = handle.emit("lumora://overlay-lock", locked);
    overlay_state(&handle)
}

fn overlay_state(handle: &AppHandle) -> OverlayState {
    OverlayState {
        shown: overlay_shown(handle),
        locked: handle
            .try_state::<App>()
            .map(|a| a.settings.lock().unwrap().overlay.locked)
            .unwrap_or(false),
    }
}

// ---- Hotkeys -------------------------------------------------------------------

/// The shortcuts in use, so a press can be told apart.
#[derive(Default)]
pub struct Hotkeys(Mutex<Vec<(Shortcut, &'static str)>>);

/// (Re)registers the hotkeys from the settings. Returns any that couldn't be
/// used (taken by another app, or not a shortcut).
pub fn register_hotkeys(handle: &AppHandle) -> Vec<String> {
    let Some(app) = handle.try_state::<App>() else { return Vec::new() };
    let keys = app.settings.lock().unwrap().hotkeys.clone();
    let gs = handle.global_shortcut();
    let _ = gs.unregister_all();
    let mut used = Vec::new();
    let mut failed = Vec::new();
    for (text, what) in [(keys.open, "open"), (keys.overlay, "overlay")] {
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        match text.parse::<Shortcut>() {
            Ok(s) if gs.register(s).is_ok() => used.push((s, what)),
            _ => failed.push(text),
        }
    }
    if let Some(h) = handle.try_state::<Hotkeys>() {
        *h.0.lock().unwrap() = used;
    }
    failed
}

pub fn hotkey_plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|handle, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let what = handle
                .try_state::<Hotkeys>()
                .and_then(|h| h.0.lock().unwrap().iter().find(|(s, _)| s == shortcut).map(|(_, w)| *w));
            match what {
                Some("open") => show_main(handle),
                Some("overlay") => {
                    let h = handle.clone();
                    let show = !overlay_shown(&h);
                    // Windows are made on the main thread.
                    let _ = handle.run_on_main_thread(move || set_overlay(&h, show));
                }
                _ => {}
            }
        })
        .build()
}
