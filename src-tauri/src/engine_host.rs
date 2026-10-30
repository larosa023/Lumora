//! Runs the hardware engine that ships inside Lumora, so there is nothing else
//! to install.
//!
//! The engine is OpenRGB (GPL-2.0), bundled unmodified in the Windows build
//! (`engine/openrgb/`) and started hidden as a local SDK server that only
//! listens on 127.0.0.1. Lumora talks to it exactly as it would to any OpenRGB.
//!
//! Two modes:
//!
//! * **Standard** – the engine runs as a normal child process. That covers USB
//!   devices: keyboards, mice, headsets, AIO coolers, fan/LED hubs, most GPUs.
//! * **Full access** – RAM and motherboard lighting need the PawnIO driver and
//!   admin rights. After one Windows permission prompt, Lumora installs PawnIO,
//!   copies the engine to Program Files (writable only by admins, so it can't be
//!   swapped out by other software) and registers a scheduled task that starts
//!   it elevated at sign-in. No prompts after that.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

pub const TASK_NAME: &str = "Lumora\\Engine";
pub const PORT: u16 = 6742;
/// First argument that turns Lumora.exe into the elevated setup helper.
pub const ELEVATED_FLAG: &str = "--lumora-elevated";

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineMode {
    /// Lumora's own engine, as a normal process.
    Standard,
    /// Lumora's own engine with full hardware access.
    Full,
    /// Connecting to an OpenRGB the user runs (or a different host).
    External,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FullAccess {
    /// Not available on this platform/build.
    Unsupported,
    Off,
    On,
    /// On, but the installed engine is older than the one in this Lumora.
    NeedsUpdate,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct EngineStatus {
    pub mode: EngineMode,
    pub bundled: bool,
    pub full_access: FullAccess,
    pub version: Option<String>,
    /// A problem worth showing, if any.
    pub problem: Option<String>,
}

pub struct EngineHost {
    /// The bundled `engine` folder, when this build has one.
    dir: Option<PathBuf>,
    config_dir: PathBuf,
    child: Mutex<Option<Child>>,
    wanted: AtomicBool,
    status: Mutex<EngineStatus>,
    on_change: Box<dyn Fn(EngineStatus) + Send + Sync>,
}

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

fn quiet_ok(program: &str, args: &[&str]) -> bool {
    hidden(&mut Command::new(program).args(args)).status().is_ok_and(|s| s.success())
}

pub fn port_open() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn openrgb_exe(engine_dir: &Path) -> PathBuf {
    engine_dir.join("openrgb").join("OpenRGB.exe")
}

fn read_version(engine_dir: &Path) -> Option<String> {
    std::fs::read_to_string(engine_dir.join("VERSION")).ok().map(|s| s.trim().to_string())
}

/// Where full-access mode installs the engine.
pub fn installed_dir() -> PathBuf {
    let base = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join("Lumora Engine")
}

fn installed_config_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("Lumora").join("openrgb")
}

fn server_args(config: &Path) -> Vec<String> {
    vec![
        "--server".into(),
        "--server-host".into(),
        "127.0.0.1".into(),
        "--server-port".into(),
        PORT.to_string(),
        "--noautoconnect".into(),
        "--config".into(),
        config.display().to_string(),
    ]
}

impl EngineHost {
    pub fn new(resource_dir: Option<PathBuf>, config_dir: PathBuf, on_change: impl Fn(EngineStatus) + Send + Sync + 'static) -> Arc<Self> {
        let dir = resource_dir
            .map(|r| r.join("engine"))
            .filter(|d| cfg!(windows) && openrgb_exe(d).is_file());
        let status = EngineStatus {
            mode: if dir.is_some() { EngineMode::Standard } else { EngineMode::External },
            bundled: dir.is_some(),
            full_access: FullAccess::Unsupported,
            version: dir.as_deref().and_then(read_version),
            problem: None,
        };
        Arc::new(EngineHost {
            dir,
            config_dir: config_dir.join("openrgb"),
            child: Mutex::new(None),
            wanted: AtomicBool::new(false),
            status: Mutex::new(status),
            on_change: Box::new(on_change),
        })
    }

    /// Where the running engine writes its logs.
    pub fn log_dir(&self) -> PathBuf {
        if self.status().mode == EngineMode::Full {
            installed_config_dir().join("logs")
        } else {
            self.config_dir.join("logs")
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut EngineStatus)) {
        let mut s = self.status.lock().unwrap();
        let before = s.clone();
        f(&mut s);
        if *s != before {
            let now = s.clone();
            drop(s);
            (self.on_change)(now);
        }
    }

    pub fn full_access(&self) -> FullAccess {
        let Some(dir) = &self.dir else { return FullAccess::Unsupported };
        let installed = installed_dir();
        if !openrgb_exe(&installed).is_file() || !quiet_ok("schtasks", &["/query", "/tn", TASK_NAME]) {
            return FullAccess::Off;
        }
        if read_version(&installed) != read_version(dir) {
            FullAccess::NeedsUpdate
        } else {
            FullAccess::On
        }
    }

    /// Starts (or stops) the engine to match the settings. Safe to call often.
    pub fn apply(self: &Arc<Self>, builtin: bool, host: &str) {
        let local = matches!(host, "127.0.0.1" | "localhost" | "::1");
        let use_builtin = builtin && local && self.dir.is_some();
        self.wanted.store(use_builtin, Ordering::SeqCst);
        if !use_builtin {
            self.stop_child();
            self.set_status(|s| {
                s.mode = EngineMode::External;
                s.problem = None;
            });
            return;
        }
        let host = self.clone();
        std::thread::spawn(move || host.start());
    }

    fn start(self: &Arc<Self>) {
        let access = self.full_access();
        if matches!(access, FullAccess::On | FullAccess::NeedsUpdate) {
            self.stop_child();
            // Already running is fine: the task ignores a second start.
            let ok = quiet_ok("schtasks", &["/run", "/tn", TASK_NAME]);
            self.set_status(|s| {
                s.mode = EngineMode::Full;
                s.full_access = access;
                s.problem = (!ok).then(|| "Couldn't start the hardware engine with full access".to_string());
            });
            return;
        }
        self.set_status(|s| {
            s.mode = EngineMode::Standard;
            s.full_access = access;
        });
        self.ensure_child();
        self.supervise();
    }

    fn spawn_child(&self) -> std::io::Result<Child> {
        let dir = self.dir.as_ref().expect("bundled engine present");
        std::fs::create_dir_all(&self.config_dir)?;
        prepare_engine_config(&self.config_dir);
        let exe = openrgb_exe(dir);
        hidden(Command::new(&exe).args(server_args(&self.config_dir)).current_dir(exe.parent().unwrap())).spawn()
    }

    fn ensure_child(&self) {
        let mut child = self.child.lock().unwrap();
        let running = child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        if running || !self.wanted.load(Ordering::SeqCst) {
            return;
        }
        // Something already serves the port (an engine left from a crash, or the
        // user's own OpenRGB): use it rather than fighting over it.
        if port_open() {
            return;
        }
        match self.spawn_child() {
            Ok(c) => {
                *child = Some(c);
                drop(child);
                self.set_status(|s| s.problem = None);
            }
            Err(e) => {
                drop(child);
                self.set_status(|s| s.problem = Some(format!("Couldn't start the hardware engine: {e}")));
            }
        }
    }

    /// Restarts the engine if it stops, backing off if it keeps crashing.
    fn supervise(self: &Arc<Self>) {
        static RUNNING: AtomicBool = AtomicBool::new(false);
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        let host = self.clone();
        std::thread::spawn(move || {
            let mut quick_exits = 0u32;
            let mut started = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(3));
                if !host.wanted.load(Ordering::SeqCst) || host.status().mode != EngineMode::Standard {
                    continue;
                }
                let exited = host.child.lock().unwrap().as_mut().is_some_and(|c| !matches!(c.try_wait(), Ok(None)));
                if !exited {
                    continue;
                }
                quick_exits = if started.elapsed() < Duration::from_secs(20) { quick_exits + 1 } else { 0 };
                if quick_exits >= 4 {
                    host.set_status(|s| s.problem = Some("The hardware engine keeps stopping. Restart Lumora to try again.".into()));
                    continue;
                }
                *host.child.lock().unwrap() = None;
                started = Instant::now();
                host.ensure_child();
            }
        });
    }

    pub fn stop_child(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Before an update replaces files, make sure nothing runs from the install folder.
    pub fn stop_for_update(&self) {
        self.wanted.store(false, Ordering::SeqCst);
        self.stop_child();
    }

    /// Runs the elevated helper (one Windows permission prompt) for `action`
    /// (`install` or `remove`).
    fn run_elevated(&self, action: &str) -> Result<(), String> {
        let dir = self.dir.as_ref().ok_or("This version of Lumora doesn't include the hardware engine")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let result = std::env::temp_dir().join(format!("lumora-elevated-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&result);
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let args = format!("{ELEVATED_FLAG} {action} \"{}\" \"{}\" \"{}\"", dir.display(), result.display(), user);
        // The standard Windows permission prompt, straight from the OS.
        let outcome = lumora_core::system::run_as_admin(&exe, &args);
        let detail = std::fs::read_to_string(&result).unwrap_or_default();
        let _ = std::fs::remove_file(&result);
        match outcome {
            Ok(0) => Ok(()),
            Err(lumora_core::system::ElevateError::Declined) => Err("Windows didn't give permission, so nothing changed".into()),
            Err(lumora_core::system::ElevateError::Failed(code)) => Err(format!("Couldn't ask Windows for permission (error {code})")),
            Ok(_) if !detail.trim().is_empty() => Err(detail.trim().to_string()),
            Ok(_) => Err("The setup didn't finish. Nothing was changed.".into()),
        }
    }

    pub fn enable_full_access(self: &Arc<Self>) -> Result<(), String> {
        // The installer copies the engine, so free our copy first.
        self.stop_child();
        let r = self.run_elevated("install");
        self.start();
        r
    }

    pub fn disable_full_access(self: &Arc<Self>) -> Result<(), String> {
        let r = self.run_elevated("remove");
        self.start();
        r
    }

    /// Installs the UsbDk access driver that Corsair Hydro Pro coolers need on
    /// Windows. Unlike swapping the cooler's driver, UsbDk leaves iCUE working.
    pub fn install_cooler_driver(self: &Arc<Self>) -> Result<(), String> {
        if !cfg!(windows) || usbdk_installed() {
            return Ok(());
        }
        self.run_elevated("cooler-driver")
    }
}

fn usbdk_installed() -> bool {
    lumora_core::system::service_exists("UsbDk")
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// OpenRGB detectors Lumora keeps off, because Lumora drives those devices
/// itself and the detector would hold them for the whole session (taking them
/// away from iCUE even when the user hasn't asked Lumora to run them).
const DISABLED_DETECTORS: &[&str] = &["Corsair Hydro Series"];

/// Makes sure the engine config in `dir` has Lumora's detector choices.
fn prepare_engine_config(dir: &Path) {
    let file = dir.join("OpenRGB.json");
    let mut root: serde_json::Value = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let mut changed = false;
    for name in DISABLED_DETECTORS {
        let detectors = &mut root["Detectors"]["detectors"];
        if detectors.get(*name) != Some(&serde_json::Value::Bool(false)) {
            detectors[*name] = serde_json::Value::Bool(false);
            changed = true;
        }
    }
    if changed {
        let _ = std::fs::create_dir_all(dir);
        if let Ok(text) = serde_json::to_string_pretty(&root) {
            let _ = std::fs::write(&file, text);
        }
    }
}

impl Drop for EngineHost {
    fn drop(&mut self) {
        self.stop_child();
    }
}

// --- Elevated helper ---------------------------------------------------------
//
// `Lumora.exe --lumora-elevated <install|remove|cooler-driver> <engine dir> <result file> <user>`
// runs before any window exists, does its job and exits.

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn task_xml(user: &str, exe: &Path, args: &[String]) -> String {
    let args = args
        .iter()
        .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    let workdir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Lumora hardware engine. Lets Lumora control RAM and motherboard lighting.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>5</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{args}</Arguments>
      <WorkingDirectory>{workdir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(user),
        exe = xml_escape(&exe.display().to_string()),
        args = xml_escape(&args),
        workdir = xml_escape(&workdir),
    )
}

/// schtasks wants the XML as UTF-16 with a byte-order mark.
fn utf16_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn stop_installed_engine() {
    let _ = quiet_ok("schtasks", &["/end", "/tn", TASK_NAME]);
    let exe = openrgb_exe(&installed_dir());
    // Only processes running the installed copy, by path.
    for p in lumora_core::system::processes() {
        if p.name == "openrgb.exe" && lumora_core::system::process_path(p.pid).is_some_and(|path| same_file(&path, &exe)) {
            lumora_core::system::kill(p.pid);
        }
    }
    std::thread::sleep(Duration::from_millis(800));
}

fn elevated_install(engine: &Path, user: &str) -> Result<(), String> {
    // 1. The PawnIO driver, for SMBus (RAM, motherboards).
    if !lumora_core::system::service_exists("PawnIO") {
        let setup = engine.join("PawnIO_setup.exe");
        if !setup.is_file() {
            return Err("The PawnIO driver installer is missing from this copy of Lumora".into());
        }
        let ok = hidden(Command::new(&setup).args(["-install", "-silent"]).current_dir(engine)).status().is_ok_and(|s| s.success());
        if !ok && !lumora_core::system::service_exists("PawnIO") {
            return Err("Couldn't install the PawnIO driver that RAM and motherboard lighting need".into());
        }
    }

    // 2. A copy of the engine only admins can change.
    stop_installed_engine();
    let target = installed_dir();
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("Couldn't replace the old engine: {e}"))?;
    }
    copy_dir(&engine.join("openrgb"), &target.join("openrgb")).map_err(|e| format!("Couldn't copy the engine: {e}"))?;
    if engine.join("VERSION").is_file() {
        let _ = std::fs::copy(engine.join("VERSION"), target.join("VERSION"));
    }
    let config = installed_config_dir();
    std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
    prepare_engine_config(&config);

    // 3. Start it elevated at every sign-in.
    let xml = task_xml(user, &openrgb_exe(&target), &server_args(&config));
    let xml_path = std::env::temp_dir().join("lumora-engine-task.xml");
    std::fs::write(&xml_path, utf16_with_bom(&xml)).map_err(|e| e.to_string())?;
    let created = quiet_ok("schtasks", &["/create", "/tn", TASK_NAME, "/xml", &xml_path.display().to_string(), "/f"]);
    let _ = std::fs::remove_file(&xml_path);
    if !created {
        return Err("Couldn't register the engine to start at sign-in".into());
    }
    let _ = quiet_ok("schtasks", &["/run", "/tn", TASK_NAME]);
    Ok(())
}

/// The interesting lines of a Windows Installer log (written as UTF-16).
fn msi_log_errors(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let words: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("Error 1") || l.contains("Error 2") || l.contains("CustomAction") && l.contains("returned actual error"))
        .map(str::trim)
        .collect();
    let picked: Vec<&str> = lines.iter().rev().take(2).rev().copied().collect();
    (!picked.is_empty()).then(|| picked.join(" / ").chars().take(400).collect())
}

fn elevated_cooler_driver(engine: &Path) -> Result<(), String> {
    // A full-access engine must not grab the coolers once UsbDk can reach them.
    let config = installed_config_dir();
    let full_access = config.is_dir();
    if full_access {
        stop_installed_engine();
        prepare_engine_config(&config);
    }
    let result = if usbdk_installed() {
        Ok(())
    } else {
        let msi = engine.join("UsbDk_x64.msi");
        if !msi.is_file() {
            Err("The UsbDk driver installer is missing from this copy of Lumora".to_string())
        } else {
            let msi = msi.display().to_string();
            let log = std::env::temp_dir().join("lumora-usbdk-install.log");
            let log_arg = log.display().to_string();
            let code = hidden(Command::new("msiexec").args(["/i", msi.as_str(), "/qn", "/norestart", "/l*v", log_arg.as_str()]))
                .status()
                .ok()
                .and_then(|s| s.code());
            // 3010 and 1641: installed, a restart finishes the job.
            if matches!(code, Some(0 | 3010 | 1641)) || usbdk_installed() {
                Ok(())
            } else {
                let code = code.unwrap_or(-1);
                let why = match code {
                    1618 => " Another installation is running; try again when it finishes.".to_string(),
                    1625 | 1643 => " A Windows policy blocks driver installs on this PC.".to_string(),
                    _ => msi_log_errors(&log).map(|e| format!(" Windows says: {e}")).unwrap_or_default(),
                };
                Err(format!("Couldn't install the UsbDk driver (code {code}).{why}"))
            }
        }
    };
    if full_access {
        let _ = quiet_ok("schtasks", &["/run", "/tn", TASK_NAME]);
    }
    result
}

fn elevated_remove() -> Result<(), String> {
    stop_installed_engine();
    let _ = quiet_ok("schtasks", &["/delete", "/tn", TASK_NAME, "/f"]);
    let dir = installed_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("Couldn't remove the engine folder: {e}"))?;
    }
    Ok(())
}

/// Entry point for the elevated helper. Returns the process exit code, or
/// `None` when Lumora was started normally.
pub fn run_elevated_from_args() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(ELEVATED_FLAG) {
        return None;
    }
    let action = args.get(2).cloned().unwrap_or_default();
    let engine = PathBuf::from(args.get(3).cloned().unwrap_or_default());
    let result_file = args.get(4).map(PathBuf::from);
    let user = args.get(5).cloned().unwrap_or_default();
    let result = match action.as_str() {
        "install" => elevated_install(&engine, &user),
        "remove" => elevated_remove(),
        "cooler-driver" => elevated_cooler_driver(&engine),
        other => Err(format!("Unknown action {other}")),
    };
    match result {
        Ok(()) => Some(0),
        Err(msg) => {
            if let Some(f) = result_file {
                let _ = std::fs::write(f, msg);
            }
            Some(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_definition_is_sane() {
        let xml = task_xml(r"PC\Someone", Path::new(r"C:\Program Files\Lumora Engine\openrgb\OpenRGB.exe"), &server_args(Path::new(r"C:\ProgramData\Lumora\openrgb")));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("--server-host 127.0.0.1 --server-port 6742 --noautoconnect --config"));
        assert!(xml.contains("<UserId>PC\\Someone</UserId>"));
        let bytes = utf16_with_bom(&xml);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
    }

    #[test]
    fn not_elevated_by_default() {
        assert_eq!(run_elevated_from_args(), None);
    }

    #[test]
    fn engine_config_keeps_corsair_hydro_detector_off() {
        let dir = std::env::temp_dir().join(format!("lumora-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("OpenRGB.json"), r#"{"Detectors":{"detectors":{"Corsair Hydro Series":true,"ENE SMBus DRAM":true}},"Theme":{"theme":"dark"}}"#).unwrap();
        prepare_engine_config(&dir);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("OpenRGB.json")).unwrap()).unwrap();
        assert_eq!(v["Detectors"]["detectors"]["Corsair Hydro Series"], false);
        assert_eq!(v["Detectors"]["detectors"]["ENE SMBus DRAM"], true);
        assert_eq!(v["Theme"]["theme"], "dark");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
