//! A simulated Kraken for tests and for trying the cooling page without
//! hardware (`LUMORA_FAKE_COOLER=1`).

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::kraken::NZXT_VID;
use super::{Backend, FoundCooler, HidIo};

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
                let mut m = vec![0x39, 0x01];
                m.resize(15, 0);
                m[14] = 1;
                self.reply(m);
            }
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

impl HidIo for FakeKraken {
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

#[derive(Default, Clone)]
pub struct FakeBackend {
    pub devices: Arc<Mutex<Vec<Arc<Mutex<FakeKrakenState>>>>>,
}

impl FakeBackend {
    pub fn with_kraken(pid: u16) -> (FakeBackend, Arc<Mutex<FakeKrakenState>>) {
        let state = Arc::new(Mutex::new(FakeKrakenState::new(pid, "FAKE0001")));
        let backend = FakeBackend { devices: Arc::new(Mutex::new(vec![state.clone()])) };
        (backend, state)
    }
}

impl Backend for FakeBackend {
    fn discover(&mut self) -> Vec<FoundCooler> {
        self.devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.lock().unwrap())
            .filter(|d| !d.unplugged)
            .map(|d| FoundCooler { id: format!("hid:{NZXT_VID:04x}:{:04x}:{}", d.pid, d.serial), vid: NZXT_VID, pid: d.pid, serial: d.serial.clone() })
            .collect()
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn HidIo>> {
        let devices = self.devices.lock().unwrap();
        let d = devices
            .iter()
            .find(|d| d.lock().unwrap().serial == cooler.serial)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gone"))?;
        Ok(Box::new(FakeKraken(d.clone())))
    }
}
