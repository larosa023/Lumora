//! A simulated Kraken and Corsair Hydro Pro for tests and for trying the cooling page without
//! hardware (`LUMORA_FAKE_COOLER=1`).

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::hydro::CORSAIR_VID;
use super::kraken::NZXT_VID;
use super::{Backend, Bulk, FoundCooler, Link};

#[derive(Debug)]
pub struct FakeKrakenState {
    pub pid: u16,
    pub serial: String,
    pub liquid_c: f32,
    pub initialized: bool,
    pub curves: Vec<(String, Vec<u8>)>,
    pub brightness: u8,
    pub orientation: u8,
    pub liquid_screen: bool,
    pub writes: usize,
    /// Simulates another app holding the device: reads time out.
    pub busy: bool,
    pub unplugged: bool,
    /// Picture slots: (start KiB, size KiB) when used.
    pub buckets: [Option<(u16, u16)>; 16],
    /// The slot being written and the slot shown (`None`: built-in screen).
    pub writing: Option<u8>,
    pub shown_bucket: Option<u8>,
    /// Everything sent over the bulk connection.
    pub bulk: Vec<u8>,
    pub bulk_writes: usize,
    queue: VecDeque<Vec<u8>>,
    pump_duty: u8,
    fan_duty: u8,
}

impl FakeKrakenState {
    pub fn new(pid: u16, serial: &str) -> Self {
        FakeKrakenState {
            pid,
            serial: serial.into(),
            liquid_c: 33.4,
            initialized: false,
            curves: Vec::new(),
            brightness: 50,
            orientation: 0,
            liquid_screen: false,
            writes: 0,
            busy: false,
            unplugged: false,
            buckets: [None; 16],
            writing: None,
            shown_bucket: None,
            bulk: Vec::new(),
            bulk_writes: 0,
            queue: VecDeque::new(),
            pump_duty: 70,
            fan_duty: 40,
        }
    }

    fn duty_at(table: &[u8], temp: f32) -> u8 {
        let i = (temp.round() as i32 - 20).clamp(0, 39) as usize;
        table.get(i).copied().unwrap_or(100)
    }

    fn reply(&mut self, mut msg: Vec<u8>) {
        msg.resize(64, 0);
        self.queue.push_back(msg);
    }

    fn status_report(&self) -> Vec<u8> {
        let mut m = vec![0u8; 64];
        m[0] = 0x75;
        m[1] = 0x01;
        m[15] = self.liquid_c.trunc() as u8;
        m[16] = ((self.liquid_c.fract() * 10.0).round() as u8).min(9);
        let pump_rpm = 1000 + self.pump_duty as u16 * 18;
        m[17..19].copy_from_slice(&pump_rpm.to_le_bytes());
        m[19] = self.pump_duty;
        let fan_rpm = self.fan_duty as u16 * 18;
        m[23..25].copy_from_slice(&fan_rpm.to_le_bytes());
        m[25] = self.fan_duty;
        m
    }

    fn handle(&mut self, r: &[u8]) {
        self.writes += 1;
        match (r[0], r[1]) {
            (0x70, 0x01) => self.initialized = true,
            (0x10, 0x01) => {
                let mut m = vec![0x11, 0x01];
                m.resize(0x14, 0);
                m[0x11] = 2;
                m[0x12] = 4;
                m[0x13] = 0;
                self.reply(m);
            }
            (0x30, 0x01) => {
                let mut m = vec![0x31, 0x01];
                m.resize(0x1B, 0);
                m[0x18] = self.brightness;
                m[0x1A] = self.orientation;
                self.reply(m);
            }
            (0x30, 0x02) => {
                self.brightness = r[3];
                self.orientation = r[7];
            }
            (0x38, 0x01) => {
                self.liquid_screen = r[2] == 0x02;
                let ok = r[2] == 0x02 || self.buckets.get(r[3] as usize).is_some_and(|b| b.is_some());
                if ok {
                    self.shown_bucket = (r[2] == 0x04).then_some(r[3]);
                }
                let mut m = vec![0x39, 0x01];
                m.resize(15, 0);
                m[14] = ok as u8;
                self.reply(m);
            }
            (0x36, 0x03) => self.reply(vec![0x37, 0x03]),
            (0x30, 0x04) => {
                let mut m = vec![0x31, 0x04];
                m.resize(23, 0);
                if let Some(Some((start, size))) = self.buckets.get(r[2] as usize) {
                    m[14] = 0x01;
                    m[15] = r[2];
                    m[16] = r[2] + 1;
                    m[17..19].copy_from_slice(&start.to_le_bytes());
                    m[19..21].copy_from_slice(&size.to_le_bytes());
                    m[21] = 1;
                }
                self.reply(m);
            }
            (0x32, 0x02) => {
                if let Some(b) = self.buckets.get_mut(r[2] as usize) {
                    *b = None;
                }
                let mut m = vec![0x33, 0x02];
                m.resize(15, 0);
                m[14] = 1;
                self.reply(m);
            }
            (0x32, 0x01) => {
                let start = u16::from_le_bytes([r[4], r[5]]);
                let size = u16::from_le_bytes([r[6], r[7]]);
                self.buckets[r[2] as usize] = Some((start, size));
                let mut m = vec![0x33, 0x01];
                m.resize(15, 0);
                m[14] = 1;
                self.reply(m);
            }
            (0x36, 0x01) => {
                self.writing = Some(r[2]);
                self.bulk.clear();
                self.reply(vec![0x37, 0x01]);
            }
            (0x36, 0x02) => self.writing = None,
            (0x72, _) => {
                let channel = if r[1] == 0x01 { "pump" } else { "fan" };
                let table = r[4..44].to_vec();
                let duty = Self::duty_at(&table, self.liquid_c);
                if channel == "pump" {
                    self.pump_duty = duty;
                } else {
                    self.fan_duty = duty;
                }
                self.curves.push((channel.into(), table));
            }
            (0x74, 0x01) => {
                let m = self.status_report();
                // A periodic report sneaks in first, like on real hardware.
                self.reply(m.clone());
                self.reply(m);
            }
            _ => {}
        }
    }
}

pub struct FakeKraken(pub Arc<Mutex<FakeKrakenState>>);

pub struct FakeBulk(pub Arc<Mutex<FakeKrakenState>>);

impl Bulk for FakeBulk {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let mut s = self.0.lock().unwrap();
        assert!(s.writing.is_some(), "bulk data only flows during a transfer");
        s.bulk.extend_from_slice(data);
        s.bulk_writes += 1;
        Ok(())
    }
}

impl Link for FakeKraken {
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        let mut s = self.0.lock().unwrap();
        if s.unplugged {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device unplugged"));
        }
        assert_eq!(report.len(), 64, "reports are always 64 bytes");
        s.handle(report);
        Ok(())
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut s = self.0.lock().unwrap();
        if s.unplugged {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device unplugged"));
        }
        if s.busy {
            s.queue.clear();
            drop(s);
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            return Ok(None);
        }
        Ok(s.queue.pop_front())
    }
}

/// A simulated Corsair Hydro Pro.
#[derive(Debug)]
pub struct FakeHydroState {
    pub pid: u16,
    pub port: String,
    pub liquid_c: f32,
    pub pump_mode: u8,
    /// Last curve per fan: 7 temperatures then 7 duties.
    pub fan_curves: Vec<(u8, Vec<u8>)>,
    pub fan_fixed: Vec<(u8, u8)>,
    pub light: Vec<Vec<u8>>,
    pub writes: usize,
    /// Simulates a missing access driver.
    pub needs_setup: bool,
    /// Simulates "installed, restart pending".
    pub needs_restart: bool,
    queue: VecDeque<Vec<u8>>,
}

impl FakeHydroState {
    pub fn new(pid: u16) -> Self {
        FakeHydroState {
            pid,
            port: "1-4".into(),
            liquid_c: 31.7,
            pump_mode: 1,
            fan_curves: Vec::new(),
            fan_fixed: Vec::new(),
            light: Vec::new(),
            writes: 0,
            needs_setup: false,
            needs_restart: false,
            queue: VecDeque::new(),
        }
    }

    fn handle(&mut self, m: &[u8]) {
        self.writes += 1;
        let mut reply = vec![m[0], 0x12, 0x34];
        match m[0] {
            0xAA => reply.extend([1, 1, 15, 4]),
            0xA9 => reply.extend([self.liquid_c.trunc() as u8, ((self.liquid_c.fract() * 10.0).round() as u8).min(9)]),
            0x41 => {
                let rpm: u16 = 900 + m[1] as u16 * 20;
                reply.push(m[1]);
                reply.extend(rpm.to_be_bytes());
            }
            0x33 => reply.push(self.pump_mode),
            0x31 => reply.extend((2000u16 + self.pump_mode as u16 * 350).to_be_bytes()),
            0x32 => self.pump_mode = m[1],
            0x40 => self.fan_curves.push((m[1], m[2..16].to_vec())),
            0x42 => self.fan_fixed.push((m[1], m[2])),
            0x52..=0x58 => self.light.push(m.to_vec()),
            _ => {}
        }
        self.queue.push_back(reply);
    }
}

pub struct FakeHydro(pub Arc<Mutex<FakeHydroState>>);

impl Link for FakeHydro {
    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        assert!(msg.len() <= 32, "Hydro messages are short");
        self.0.lock().unwrap().handle(msg);
        Ok(())
    }

    fn read(&mut self, _timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().queue.pop_front())
    }
}

#[derive(Default, Clone)]
pub struct FakeBackend {
    pub devices: Arc<Mutex<Vec<Arc<Mutex<FakeKrakenState>>>>>,
    pub hydros: Arc<Mutex<Vec<Arc<Mutex<FakeHydroState>>>>>,
}

impl FakeBackend {
    pub fn with_kraken(pid: u16) -> (FakeBackend, Arc<Mutex<FakeKrakenState>>) {
        let state = Arc::new(Mutex::new(FakeKrakenState::new(pid, "FAKE0001")));
        let backend = FakeBackend { devices: Arc::new(Mutex::new(vec![state.clone()])), ..Default::default() };
        (backend, state)
    }

    pub fn with_hydro(pid: u16) -> (FakeBackend, Arc<Mutex<FakeHydroState>>) {
        let backend = FakeBackend::default();
        let state = backend.add_hydro(pid);
        (backend, state)
    }

    pub fn add_hydro(&self, pid: u16) -> Arc<Mutex<FakeHydroState>> {
        let state = Arc::new(Mutex::new(FakeHydroState::new(pid)));
        self.hydros.lock().unwrap().push(state.clone());
        state
    }

    fn hydro_id(d: &FakeHydroState) -> String {
        format!("usb:{CORSAIR_VID:04x}:{:04x}:{}", d.pid, d.port)
    }
}

impl Backend for FakeBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        let mut out: Vec<FoundCooler> = self
            .devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.lock().unwrap())
            .filter(|d| !d.unplugged)
            .map(|d| FoundCooler {
                id: format!("hid:{NZXT_VID:04x}:{:04x}:{}", d.pid, d.serial),
                vid: NZXT_VID,
                pid: d.pid,
                serial: d.serial.clone(),
                needs_setup: false,
                needs_restart: false,
            })
            .collect();
        for h in self.hydros.lock().unwrap().iter() {
            let d = h.lock().unwrap();
            out.push(FoundCooler { id: Self::hydro_id(&d), vid: CORSAIR_VID, pid: d.pid, serial: String::new(), needs_setup: d.needs_setup, needs_restart: d.needs_restart });
        }
        out
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        if cooler.vid == CORSAIR_VID {
            let hydros = self.hydros.lock().unwrap();
            let d = hydros
                .iter()
                .find(|d| Self::hydro_id(&d.lock().unwrap()) == cooler.id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gone"))?;
            return Ok(Box::new(FakeHydro(d.clone())));
        }
        let devices = self.devices.lock().unwrap();
        let d = devices
            .iter()
            .find(|d| d.lock().unwrap().serial == cooler.serial)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gone"))?;
        Ok(Box::new(FakeKraken(d.clone())))
    }

    fn open_bulk(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Bulk>> {
        let devices = self.devices.lock().unwrap();
        let d = devices
            .iter()
            .find(|d| d.lock().unwrap().serial == cooler.serial)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no picture connection"))?;
        Ok(Box::new(FakeBulk(d.clone())))
    }
}
