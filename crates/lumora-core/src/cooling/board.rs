//! Motherboard fan headers, through the board's Nuvoton Super I/O chip (the
//! NCT679x family found on most ASUS, and many other, boards).
//!
//! The chip sits on the LPC bus, which only admins can reach, so Lumora's
//! admin helper does the talking (through PawnIO's signed `LpcIO` module,
//! like LibreHardwareMonitor) and Lumora asks it over the helper connection:
//!
//! ```text
//! → board              ← ok <chip> <rpm,rpm,…> <duty,duty,…>   ("-" = no reading)
//! → boardfan <i> <pct> ← ok | err <why>     header i (0-based) to this speed
//! → boardauto <i>      ← ok | err <why>     header i back to the BIOS curve
//! → boardkeep          ← ok                 "Lumora is still here"
//! ```
//!
//! Every header Lumora sets goes back to its BIOS setting as soon as the
//! connection that set it closes, or when the helper hears nothing for a few
//! seconds, so a closed or crashed Lumora never leaves a fan stuck.
//!
//! Registers follow LibreHardwareMonitor's `Nct677X`/`LpcIO` (MPL-2.0).

use std::io;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::curve::SpeedSetting;
use super::family::Driver;
use super::{Backend, CoolerStatus, FoundCooler, Link};

/// Nuvoton's vendor id, standing in for a USB vendor id.
pub const BOARD_VID: u16 = 0x5CA3;
/// Lowest speed Lumora sets; many fans stall below it.
pub const MIN_DUTY: u8 = 20;
pub const CHANNELS: [&str; 7] = ["h1", "h2", "h3", "h4", "h5", "h6", "h7"];
pub const CHANNEL_NAMES: [&str; 7] = [
    "Header 1", "Header 2", "Header 3", "Header 4", "Header 5", "Header 6", "Header 7",
];

/// Raw access to the Super I/O chip. The helper provides it (PawnIO);
/// tests provide a simulated chip.
pub trait SuperIo: Send {
    /// Picks the chip's configuration ports: 0 = 0x2E/0x2F, 1 = 0x4E/0x4F.
    fn select_slot(&mut self, slot: u8) -> Result<(), String>;
    /// Learns the chip's I/O ranges (needed before `inb`/`outb` there).
    fn find_bars(&mut self) -> Result<(), String>;
    fn inb(&mut self, port: u16) -> Result<u8, String>;
    fn outb(&mut self, port: u16, value: u8) -> Result<(), String>;
    /// A configuration register of the selected chip.
    fn sio_inb(&mut self, reg: u8) -> Result<u8, String>;
    fn sio_outb(&mut self, reg: u8, value: u8) -> Result<(), String>;
    /// Takes the shared ISA-bus lock other monitoring tools also use.
    fn lock(&mut self) -> bool {
        true
    }
    fn unlock(&mut self) {}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chip {
    pub name: &'static str,
    pub fans: usize,
    pwm_out: &'static [u16],
}

const PWM_OUT_A: &[u16] = &[0x001, 0x003, 0x011, 0x013, 0x015, 0x017, 0x029];
const PWM_OUT_B: &[u16] = &[0x001, 0x003, 0x011, 0x013, 0x015, 0xA09, 0xB09];
const PWM_COMMAND: [u16; 7] = [0x109, 0x209, 0x309, 0x809, 0x909, 0xA09, 0xB09];
const CONTROL_MODE: [u16; 7] = [0x102, 0x202, 0x302, 0x802, 0x902, 0xA02, 0xB02];
const FAN_COUNT: [u16; 7] = [0x4B0, 0x4B2, 0x4B4, 0x4B6, 0x4B8, 0x4BA, 0x4CC];
const MAX_COUNT: u32 = 0x1FFF;
const MIN_COUNT: u32 = 0x15;

/// The chips Lumora drives, by (chip id, revision).
pub fn chip(id: u8, rev: u8) -> Option<Chip> {
    let c = |name, fans, pwm_out| {
        Some(Chip {
            name,
            fans,
            pwm_out,
        })
    };
    match (id, rev) {
        (0xC8, 0x03) => c("NCT6791D", 6, PWM_OUT_A),
        (0xC9, 0x11) => c("NCT6792D", 6, PWM_OUT_A),
        (0xC9, 0x13) => c("NCT6792DA", 6, PWM_OUT_A),
        (0xD1, 0x21) => c("NCT6793D", 6, PWM_OUT_A),
        (0xD3, 0x52) => c("NCT6795D", 6, PWM_OUT_A),
        (0xD4, 0x23) => c("NCT6796D", 6, PWM_OUT_A),
        (0xD4, 0x2A) => c("NCT6796D-R", 7, PWM_OUT_A),
        (0xD4, 0x51) => c("NCT6797D", 7, PWM_OUT_B),
        (0xD4, 0x2B) => c("NCT6798D", 7, PWM_OUT_B),
        (0xD8, 0x02) => c("NCT6799D", 7, PWM_OUT_B),
        _ => None,
    }
}

/// One Nuvoton chip, found and ready.
pub struct Nct {
    io: Box<dyn SuperIo>,
    pub chip: Chip,
    base: u16,
    /// What each header had before Lumora set it: (mode, PWM command).
    saved: [Option<(u8, u8)>; 7],
}

impl Nct {
    /// Looks for a supported chip on both configuration ports.
    pub fn detect(mut io: Box<dyn SuperIo>) -> Result<Nct, String> {
        let mut seen = Vec::new();
        for (slot, port) in [(0u8, 0x2Eu16), (1, 0x4E)] {
            if !io.lock() {
                return Err("another program is using the motherboard sensors".into());
            }
            let found = Self::probe(io.as_mut(), slot, port);
            io.unlock();
            match found {
                Ok(Some((chip, base))) => {
                    let mut n = Nct {
                        io,
                        chip,
                        base,
                        saved: [None; 7],
                    };
                    let vendor =
                        n.locked(|n| Ok((n.read(0x804F)? as u16) << 8 | n.read(0x004F)? as u16))?;
                    if vendor != BOARD_VID {
                        return Err(format!("the {} didn't answer as a Nuvoton chip", chip.name));
                    }
                    return Ok(n);
                }
                Ok(None) => {}
                Err(e) => seen.push(e),
            }
        }
        Err(if seen.is_empty() {
            "no supported fan chip on this motherboard".into()
        } else {
            seen.join("; ")
        })
    }

    fn probe(io: &mut dyn SuperIo, slot: u8, port: u16) -> Result<Option<(Chip, u16)>, String> {
        io.select_slot(slot)?;
        // Enter configuration mode.
        io.outb(port, 0x87)?;
        io.outb(port, 0x87)?;
        let id = io.sio_inb(0x20)?;
        let rev = io.sio_inb(0x21)?;
        let Some(chip) = chip(id, rev) else {
            if id != 0 && id != 0xFF {
                io.outb(port, 0xAA)?;
                return Err(format!("unsupported fan chip {id:02x}{rev:02x}"));
            }
            return Ok(None);
        };
        io.find_bars()?;
        // The hardware monitor's logical device and its I/O base.
        io.sio_outb(0x07, 0x0B)?;
        let word = |io: &mut dyn SuperIo| -> Result<u16, String> {
            Ok((io.sio_inb(0x60)? as u16) << 8 | io.sio_inb(0x61)? as u16)
        };
        let a = word(io)?;
        std::thread::sleep(Duration::from_millis(1));
        let v = word(io)?;
        if a == v {
            // Some boards lock the monitor's I/O space; unlock it.
            let lock = io.sio_inb(0x28)?;
            if lock & 0x10 != 0 {
                io.sio_outb(0x28, lock & !0x10)?;
            }
        }
        io.outb(port, 0xAA)?;
        if a != v {
            return Err(format!("the {}'s address kept changing", chip.name));
        }
        let mut base = a;
        if base & 0x07 == 0x05 {
            base &= 0xFFF8;
        }
        if base < 0x100 || base & 0xF007 != 0 {
            return Err(format!(
                "the {} reported an odd address ({base:#x})",
                chip.name
            ));
        }
        Ok(Some((chip, base)))
    }

    /// A banked hardware-monitor register.
    fn read(&mut self, addr: u16) -> Result<u8, String> {
        let (a, d) = (self.base + 5, self.base + 6);
        self.io.outb(a, 0x4E)?;
        self.io.outb(d, (addr >> 8) as u8)?;
        self.io.outb(a, addr as u8)?;
        self.io.inb(d)
    }

    fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        let (a, d) = (self.base + 5, self.base + 6);
        self.io.outb(a, 0x4E)?;
        self.io.outb(d, (addr >> 8) as u8)?;
        self.io.outb(a, addr as u8)?;
        self.io.outb(d, value)
    }

    fn locked<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        if !self.io.lock() {
            return Err("another program is using the motherboard sensors".into());
        }
        let r = f(self);
        self.io.unlock();
        r
    }

    /// Each header's fan speed (`None`: no usable reading) and duty, percent.
    pub fn readings(&mut self) -> Result<(Vec<Option<u32>>, Vec<u8>), String> {
        self.locked(|n| {
            let mut rpms = Vec::new();
            let mut duties = Vec::new();
            for i in 0..n.chip.fans {
                let hi = n.read(FAN_COUNT[i])? as u32;
                let lo = n.read(FAN_COUNT[i] + 1)? as u32;
                let count = (hi << 5) | (lo & 0x1F);
                rpms.push(if count >= MAX_COUNT {
                    Some(0)
                } else if count < MIN_COUNT {
                    None
                } else {
                    Some((1_350_000.0 / count as f32).round() as u32)
                });
                let pwm = n.read(n.chip.pwm_out[i])?;
                duties.push(((pwm as f32) / 2.55).round().min(100.0) as u8);
            }
            Ok((rpms, duties))
        })
    }

    /// Runs header `i` at `pct` percent, remembering its BIOS setting first.
    pub fn set(&mut self, i: usize, pct: u8) -> Result<(), String> {
        if i >= self.chip.fans {
            return Err("no such header".into());
        }
        let pct = pct.clamp(MIN_DUTY, 100);
        self.locked(|n| {
            if n.saved[i].is_none() {
                let mode = n.read(CONTROL_MODE[i])?;
                let cmd = n.read(PWM_COMMAND[i])?;
                n.saved[i] = Some((mode, cmd));
            }
            n.write(CONTROL_MODE[i], 0)?;
            n.write(PWM_COMMAND[i], ((pct as u32 * 255 + 50) / 100) as u8)
        })
    }

    /// Gives header `i` back to its BIOS setting.
    pub fn restore(&mut self, i: usize) -> Result<(), String> {
        let Some((mode, cmd)) = self.saved.get(i).copied().flatten() else {
            return Ok(());
        };
        self.locked(|n| {
            n.write(CONTROL_MODE[i], mode)?;
            n.write(PWM_COMMAND[i], cmd)?;
            n.saved[i] = None;
            Ok(())
        })
    }

    pub fn restore_all(&mut self) {
        for i in 0..self.chip.fans {
            let _ = self.restore(i);
        }
    }

    pub fn any_set(&self) -> bool {
        self.saved.iter().any(Option::is_some)
    }
}

// --- Helper side ------------------------------------------------------------

/// How long the helper keeps Lumora's speeds without hearing from it.
const KEEP_FOR: Duration = Duration::from_secs(10);

type Provider = Box<dyn Fn() -> Result<Box<dyn SuperIo>, String> + Send>;

struct Service {
    provider: Option<Provider>,
    chip: Option<Nct>,
    /// Why the chip couldn't be opened, so it isn't retried every second.
    failed: Option<(String, Instant)>,
    owner: Option<(u64, Instant)>,
}

fn service() -> &'static Mutex<Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Service {
            provider: None,
            chip: None,
            failed: None,
            owner: None,
        })
    })
}

/// The helper says how to reach the chip (PawnIO); without it the
/// commands answer that motherboard fans aren't available here.
pub fn set_provider(p: Provider) {
    service().lock().unwrap_or_else(|e| e.into_inner()).provider = Some(p);
}

fn chip_of(s: &mut Service) -> Result<&mut Nct, String> {
    if s.chip.is_none() {
        if let Some((why, at)) = &s.failed {
            if at.elapsed() < Duration::from_secs(60) {
                return Err(why.clone());
            }
        }
        let opened = match &s.provider {
            Some(p) => p().and_then(Nct::detect),
            None => Err("motherboard fans aren't available here".into()),
        };
        match opened {
            Ok(n) => {
                s.chip = Some(n);
                s.failed = None;
            }
            Err(e) => {
                s.failed = Some((e.clone(), Instant::now()));
                return Err(e);
            }
        }
    }
    Ok(s.chip.as_mut().expect("just opened"))
}

fn list<T: ToString>(v: impl Iterator<Item = Option<T>>) -> String {
    v.map(|x| x.map_or("-".to_string(), |x| x.to_string()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Handles one helper command for connection `conn`; `None` when it isn't a
/// motherboard command.
pub fn serve_command(conn: u64, cmd: &str, arg: &str) -> Option<String> {
    if !matches!(cmd, "board" | "boardfan" | "boardauto" | "boardkeep") {
        return None;
    }
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    let answer = match cmd {
        "boardkeep" => {
            if let Some((c, at)) = s.owner.as_mut() {
                if *c == conn {
                    *at = Instant::now();
                }
            }
            Ok(String::new())
        }
        "board" => chip_of(&mut s).and_then(|n| {
            let name = n.chip.name;
            let (rpms, duties) = n.readings()?;
            Ok(format!(
                "{name} {} {}",
                list(rpms.into_iter()),
                list(duties.into_iter().map(Some))
            ))
        }),
        "boardfan" => {
            let mut it = arg.split_whitespace();
            let i = it.next().and_then(|v| v.parse::<usize>().ok());
            let pct = it.next().and_then(|v| v.parse::<u8>().ok());
            match (i, pct) {
                (Some(i), Some(pct)) => chip_of(&mut s).and_then(|n| n.set(i, pct)).map(|_| {
                    s.owner = Some((conn, Instant::now()));
                    String::new()
                }),
                _ => Err("expected a header and a speed".into()),
            }
        }
        _ => {
            let i = arg.trim().parse::<usize>().unwrap_or(usize::MAX);
            let r = chip_of(&mut s).and_then(|n| n.restore(i).map(|_| n.any_set()));
            if let Ok(false) = r {
                s.owner = None;
            }
            r.map(|_| String::new())
        }
    };
    Some(match answer {
        Ok(a) if a.is_empty() => "ok".into(),
        Ok(a) => format!("ok {a}"),
        Err(e) => format!("err {e}"),
    })
}

fn give_back(s: &mut Service) {
    s.owner = None;
    if let Some(n) = s.chip.as_mut() {
        n.restore_all();
    }
}

/// A connection closed: if it was running headers, give them back.
pub fn connection_closed(conn: u64) {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    if s.owner.is_some_and(|(c, _)| c == conn) {
        give_back(&mut s);
    }
}

/// Gives the headers back to the BIOS when Lumora goes quiet. Runs in the
/// helper for as long as it lives.
pub fn start_watchdog() {
    std::thread::Builder::new()
        .name("lumora-board-fans".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
            if s.owner.is_some_and(|(_, at)| at.elapsed() > KEEP_FOR) {
                give_back(&mut s);
            }
        })
        .ok();
}

// --- Lumora side ------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct BoardModel {
    pub pid: u16,
    pub name: &'static str,
    pub fans: u8,
}

fn registered() -> &'static Mutex<Option<&'static BoardModel>> {
    static M: OnceLock<Mutex<Option<&'static BoardModel>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}

/// The board found on this PC (one per run).
pub fn model(pid: u16) -> Option<&'static BoardModel> {
    registered()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .filter(|m| m.pid == pid)
}

fn register(m: BoardModel) -> &'static BoardModel {
    let mut slot = registered().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = *slot {
        if *old == m {
            return old;
        }
    }
    let leaked: &'static BoardModel = Box::leak(Box::new(m));
    *slot = Some(leaked);
    leaked
}

/// A reading from the helper: chip name, speeds, duties.
pub fn parse_reading(line: &str) -> Option<(String, Vec<Option<u32>>, Vec<u8>)> {
    let mut it = line.split_whitespace();
    let name = it.next()?.to_string();
    let rpms = it.next()?.split(',').map(|v| v.parse().ok()).collect();
    let duties = it
        .next()?
        .split(',')
        .map(|v| v.parse().unwrap_or(0))
        .collect();
    Some((name, rpms, duties))
}

/// Finds the motherboard's fan headers through Lumora's admin helper.
pub struct BoardBackend {
    relay_key: Option<String>,
    /// The helper said there's nothing to control here; don't keep asking.
    absent: Option<Instant>,
}

impl BoardBackend {
    pub fn new(relay_key: Option<String>) -> Self {
        BoardBackend {
            relay_key,
            absent: None,
        }
    }

    fn helper(&self) -> io::Result<super::relay::Client> {
        let key = self
            .relay_key
            .as_deref()
            .ok_or_else(|| io::Error::other("no helper key"))?;
        super::relay::Client::connect(super::relay::PORT, key)
    }
}

const SETUP_TEXT: &str = "Setting motherboard fan speeds needs Lumora's small helper with admin rights (the same one that reads the CPU temperature). One Windows prompt sets it up.";
const UPDATE_TEXT: &str = "Lumora's helper is from an older version. Run the setup once to update it (one Windows prompt).";

impl Backend for BoardBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        if !cfg!(windows) {
            return Vec::new();
        }
        if self
            .absent
            .is_some_and(|at| at.elapsed() < Duration::from_secs(300))
        {
            return Vec::new();
        }
        let setup = |problem: &str| {
            vec![FoundCooler {
                id: format!("board:{BOARD_VID:04x}:0000:0"),
                vid: BOARD_VID,
                pid: 0,
                serial: String::new(),
                needs_setup: true,
                needs_restart: false,
                problem: Some(problem.into()),
            }]
        };
        let mut client = match self.helper() {
            Ok(c) => c,
            Err(_) => {
                register(BoardModel {
                    pid: 0,
                    name: "Motherboard fans",
                    fans: 0,
                });
                return setup(SETUP_TEXT);
            }
        };
        match client.command("board") {
            Ok(line) => {
                let Some((chip, rpms, _)) = parse_reading(&line) else {
                    return Vec::new();
                };
                let pid = rpms.len() as u16 | 0x100;
                register(BoardModel {
                    pid,
                    name: Box::leak(format!("Motherboard fans ({chip})").into_boxed_str()),
                    fans: rpms.len().min(7) as u8,
                });
                vec![FoundCooler {
                    id: format!("board:{BOARD_VID:04x}:{pid:04x}:0"),
                    vid: BOARD_VID,
                    pid,
                    serial: String::new(),
                    needs_setup: false,
                    needs_restart: false,
                    problem: None,
                }]
            }
            Err(e) if e.to_string().contains("unknown command") => {
                register(BoardModel {
                    pid: 0,
                    name: "Motherboard fans",
                    fans: 0,
                });
                setup(UPDATE_TEXT)
            }
            Err(e) if e.to_string().contains("PawnIO") => {
                register(BoardModel {
                    pid: 0,
                    name: "Motherboard fans",
                    fans: 0,
                });
                setup(SETUP_TEXT)
            }
            Err(_) => {
                // No supported chip: nothing to show.
                self.absent = Some(Instant::now());
                Vec::new()
            }
        }
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        if cooler.vid != BOARD_VID {
            return Err(io::Error::new(io::ErrorKind::NotFound, "not a motherboard"));
        }
        Ok(Box::new(BoardLink {
            client: Some(self.helper()?),
        }))
    }

    fn diagnose(&mut self) -> String {
        match self.helper().and_then(|mut c| c.command("board")) {
            Ok(l) => format!("motherboard fans: {l}"),
            Err(e) => format!("motherboard fans: {e}"),
        }
    }
}

pub struct BoardLink {
    client: Option<super::relay::Client>,
}

impl Link for BoardLink {
    fn write(&mut self, _msg: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not a USB cooler",
        ))
    }
    fn read(&mut self, _timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn take_gpu(&mut self) -> Option<Box<dyn std::any::Any + Send>> {
        self.client
            .take()
            .map(|c| Box::new(c) as Box<dyn std::any::Any + Send>)
    }
}

pub struct BoardFans {
    model: &'static BoardModel,
    client: Option<super::relay::Client>,
    /// Headers Lumora has set (rather than left to the BIOS).
    manual: [bool; 7],
}

impl BoardFans {
    pub fn new(mut link: Box<dyn Link>, model: &'static BoardModel) -> Self {
        let client = link
            .take_gpu()
            .and_then(|b| b.downcast::<super::relay::Client>().ok())
            .map(|b| *b);
        BoardFans {
            model,
            client,
            manual: [false; 7],
        }
    }

    fn send(&mut self, line: &str) -> io::Result<String> {
        self.client
            .as_mut()
            .ok_or_else(|| io::Error::other("can't reach Lumora's helper"))?
            .command(line)
    }
}

impl Driver for BoardFans {
    fn initialize(&mut self) -> io::Result<()> {
        self.send("board").map(|_| ())
    }

    fn firmware(&self) -> Option<String> {
        None
    }

    fn status(&mut self) -> io::Result<CoolerStatus> {
        if self.manual.iter().any(|m| *m) {
            self.send("boardkeep")?;
        }
        let line = self.send("board")?;
        let (_, rpms, duties) =
            parse_reading(&line).ok_or_else(|| io::Error::other("odd reading from the helper"))?;
        let n = self.model.fans as usize;
        let connected: Vec<bool> = rpms
            .iter()
            .take(n)
            .map(|r| r.is_some_and(|v| v > 0))
            .collect();
        let on: Vec<u32> = rpms
            .iter()
            .take(n)
            .flatten()
            .copied()
            .filter(|v| *v > 0)
            .collect();
        let busy: Vec<u32> = duties
            .iter()
            .zip(&connected)
            .filter(|(_, c)| **c)
            .map(|(d, _)| *d as u32)
            .collect();
        Ok(CoolerStatus {
            fan_rpm: (!on.is_empty()).then(|| on.iter().sum::<u32>() / on.len() as u32),
            fan_duty: (!busy.is_empty())
                .then(|| (busy.iter().sum::<u32>() / busy.len() as u32) as u8),
            fan_rpms: rpms.iter().take(n).map(|r| r.unwrap_or(0)).collect(),
            fan_duties: duties.into_iter().take(n).collect(),
            fans_connected: connected,
            ..Default::default()
        })
    }

    /// A fixed duty is set as is; anything else hands the header back to
    /// the BIOS.
    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()> {
        let i = CHANNELS
            .iter()
            .position(|c| *c == channel)
            .filter(|i| *i < self.model.fans as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown channel"))?;
        match setting {
            SpeedSetting::Fixed { duty } => {
                self.send(&format!("boardfan {i} {}", (*duty).max(MIN_DUTY)))?;
                self.manual[i] = true;
            }
            _ => {
                self.send(&format!("boardauto {i}"))?;
                self.manual[i] = false;
            }
        }
        Ok(())
    }
}

impl Drop for BoardFans {
    fn drop(&mut self) {
        for i in 0..7 {
            if self.manual[i] {
                let _ = self.send(&format!("boardauto {i}"));
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A simulated NCT6798D at 0x290 behind 0x2E.
    #[derive(Default)]
    pub struct Sim {
        pub regs: HashMap<u16, u8>,
        cfg_mode: u8,
        ldn: u8,
        bank: u8,
        addr: u8,
        pub lock_bit: bool,
    }

    pub struct SimIo(pub Arc<Mutex<Sim>>);

    impl SuperIo for SimIo {
        fn select_slot(&mut self, _slot: u8) -> Result<(), String> {
            Ok(())
        }
        fn find_bars(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn inb(&mut self, port: u16) -> Result<u8, String> {
            let s = self.0.lock().unwrap();
            assert_eq!(port, 0x296, "data port");
            let a = (s.bank as u16) << 8 | s.addr as u16;
            Ok(*s.regs.get(&a).unwrap_or(&0))
        }
        fn outb(&mut self, port: u16, v: u8) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            match port {
                0x2E if v == 0x87 => s.cfg_mode += 1,
                0x2E if v == 0xAA => s.cfg_mode = 0,
                0x4E => {}
                0x295 => s.addr = v,
                0x296 if s.addr == 0x4E => s.bank = v,
                0x296 => {
                    let a = (s.bank as u16) << 8 | s.addr as u16;
                    s.regs.insert(a, v);
                }
                p => panic!("unexpected port {p:#x}"),
            }
            Ok(())
        }
        fn sio_inb(&mut self, reg: u8) -> Result<u8, String> {
            let s = self.0.lock().unwrap();
            assert!(s.cfg_mode >= 2, "configuration mode entered");
            Ok(match reg {
                0x20 => 0xD4,
                0x21 => 0x2B,
                0x60 if s.ldn == 0x0B => 0x02,
                0x61 if s.ldn == 0x0B => 0x90,
                0x28 => {
                    if s.lock_bit {
                        0x10
                    } else {
                        0
                    }
                }
                _ => 0,
            })
        }
        fn sio_outb(&mut self, reg: u8, v: u8) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            match reg {
                0x07 => s.ldn = v,
                0x28 => s.lock_bit = v & 0x10 != 0,
                _ => {}
            }
            Ok(())
        }
    }

    pub fn sim() -> Arc<Mutex<Sim>> {
        let mut s = Sim {
            lock_bit: true,
            ..Default::default()
        };
        s.regs.insert(0x804F, 0x5C);
        s.regs.insert(0x004F, 0xA3);
        // Header 1: count 1350 → 1000 rpm; header 2: nothing plugged in.
        let count: u32 = 1350;
        s.regs.insert(0x4B0, (count >> 5) as u8);
        s.regs.insert(0x4B1, (count & 0x1F) as u8);
        s.regs.insert(0x4B2, 0xFF);
        s.regs.insert(0x4B3, 0x1F);
        s.regs.insert(0x001, 128);
        // BIOS: header 1 in Smart Fan mode (0x40) at command 0x80.
        s.regs.insert(0x102, 0x40);
        s.regs.insert(0x109, 0x80);
        Arc::new(Mutex::new(s))
    }

    #[test]
    fn finds_the_chip_reads_fans_and_hands_back() {
        let s = sim();
        let mut n = Nct::detect(Box::new(SimIo(s.clone()))).expect("found");
        assert_eq!(n.chip.name, "NCT6798D");
        assert_eq!(n.chip.fans, 7);
        assert!(!s.lock().unwrap().lock_bit, "I/O space unlocked");
        let (rpms, duties) = n.readings().unwrap();
        assert_eq!(rpms[0], Some(1000));
        assert_eq!(rpms[1], Some(0));
        assert_eq!(duties[0], 50);

        n.set(0, 70).unwrap();
        {
            let s = s.lock().unwrap();
            assert_eq!(s.regs[&0x102], 0, "manual mode");
            assert_eq!(s.regs[&0x109], 179);
        }
        n.set(0, 5).unwrap();
        assert_eq!(s.lock().unwrap().regs[&0x109], 51, "never below 20 %");
        n.restore_all();
        let s = s.lock().unwrap();
        assert_eq!(s.regs[&0x102], 0x40, "BIOS mode back");
        assert_eq!(s.regs[&0x109], 0x80);
    }

    #[test]
    fn readings_parse() {
        let (name, rpms, duties) = parse_reading("NCT6798D 1000,-,0 50,40,30").unwrap();
        assert_eq!(name, "NCT6798D");
        assert_eq!(rpms, vec![Some(1000), None, Some(0)]);
        assert_eq!(duties, vec![50, 40, 30]);
    }
}
