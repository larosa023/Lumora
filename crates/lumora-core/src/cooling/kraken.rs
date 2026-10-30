//! NZXT Kraken Z, 2023 and 2024 coolers over USB HID.
//!
//! Every message is a 64-byte report whose first byte is the report id.
//! The cooler also sends status reports (id 0x75) on its own once
//! initialized, so replies are picked out by their first two bytes.

use std::io;
use std::time::Duration;

use super::curve::{self, SpeedSetting};
use super::family::Driver;
use super::{Bulk, CoolerStatus, Link, ScreenMode, ScreenSettings};

pub const NZXT_VID: u16 = 0x1E71;
const REPORT_LEN: usize = 64;
const READ_ATTEMPTS: usize = 12;
const READ_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq)]
pub struct KrakenModel {
    pub pid: u16,
    pub name: &'static str,
    pub pump_id: [u8; 3],
    pub fan_id: Option<[u8; 3]>,
    /// Screen size in pixels, if it has one.
    pub lcd: Option<(u16, u16)>,
}

pub const MODELS: &[KrakenModel] = &[
    KrakenModel { pid: 0x3008, name: "Kraken Z", pump_id: [1, 0, 0], fan_id: Some([2, 0, 0]), lcd: Some((320, 320)) },
    KrakenModel { pid: 0x300C, name: "Kraken Elite (2023)", pump_id: [1, 1, 0], fan_id: Some([2, 1, 1]), lcd: Some((640, 640)) },
    KrakenModel { pid: 0x300E, name: "Kraken (2023)", pump_id: [1, 1, 0], fan_id: Some([2, 1, 1]), lcd: Some((240, 240)) },
    KrakenModel { pid: 0x3012, name: "Kraken Elite RGB (2024)", pump_id: [1, 1, 0], fan_id: Some([2, 1, 1]), lcd: Some((640, 640)) },
    KrakenModel { pid: 0x3014, name: "Kraken Plus (2024)", pump_id: [1, 1, 0], fan_id: Some([2, 1, 1]), lcd: Some((240, 240)) },
];

pub fn model(pid: u16) -> Option<&'static KrakenModel> {
    MODELS.iter().find(|m| m.pid == pid)
}

/// Pumps stall below this.
pub const PUMP_MIN_DUTY: u8 = 20;

/// Screen memory, in KiB, shared by all stored pictures.
const LCD_MEMORY_KIB: usize = 24320;
/// Pictures are stored in up to 16 slots ("buckets").
const BUCKETS: u8 = 16;
/// Largest bulk write.
const BULK_CHUNK: usize = 2 * 1024 * 1024;

pub struct Kraken {
    io: Box<dyn Link>,
    bulk: Option<Box<dyn Bulk>>,
    pub model: &'static KrakenModel,
    pub firmware: Option<(u8, u8, u8)>,
    pub brightness: u8,
    /// 0..=3, quarter turns.
    pub orientation: u8,
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, msg.into())
}

impl Kraken {
    pub fn new(io: Box<dyn Link>, model: &'static KrakenModel) -> Self {
        Kraken { io, bulk: None, model, firmware: None, brightness: 50, orientation: 0 }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let mut report = [0u8; REPORT_LEN];
        report[..data.len()].copy_from_slice(data);
        self.io.write(&report)
    }

    /// Throws away reports that queued up while nobody was reading.
    fn drain(&mut self) -> io::Result<()> {
        for _ in 0..64 {
            if self.io.read(Duration::ZERO)?.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Reads until a report starting with `prefix` arrives.
    fn read_until(&mut self, prefix: &[u8]) -> io::Result<Vec<u8>> {
        for _ in 0..READ_ATTEMPTS {
            match self.io.read(READ_TIMEOUT)? {
                Some(msg) if msg.starts_with(prefix) => return Ok(msg),
                Some(_) => continue,
                None => break,
            }
        }
        Err(err(format!("the cooler didn't answer (waiting for {prefix:02x?})")))
    }

    /// Reads the next report that isn't a periodic status report.
    fn read_reply(&mut self) -> io::Result<Vec<u8>> {
        for _ in 0..READ_ATTEMPTS {
            match self.io.read(READ_TIMEOUT)? {
                Some(msg) if msg.first() != Some(&0x75) => return Ok(msg),
                Some(_) => continue,
                None => break,
            }
        }
        Err(err("the cooler didn't answer"))
    }

    /// Must run after every power-up or reconnect before anything else.
    pub fn initialize(&mut self) -> io::Result<()> {
        self.drain()?;
        // Status reports every 0.5 s, then start.
        self.write(&[0x70, 0x02, 0x01, 0xB8, 0x01])?;
        self.write(&[0x70, 0x01])?;
        self.write(&[0x10, 0x01])?;
        let fw = self.read_until(&[0x11, 0x01])?;
        if fw.len() > 0x13 {
            self.firmware = Some((fw[0x11], fw[0x12], fw[0x13]));
        }
        if self.model.lcd.is_some() {
            self.read_screen_info()?;
        }
        Ok(())
    }

    fn read_screen_info(&mut self) -> io::Result<()> {
        self.write(&[0x30, 0x01])?;
        let info = self.read_until(&[0x31, 0x01])?;
        if info.len() > 0x1A {
            self.brightness = info[0x18].min(100);
            self.orientation = info[0x1A] & 3;
        }
        Ok(())
    }

    pub fn status(&mut self) -> io::Result<CoolerStatus> {
        self.drain()?;
        self.write(&[0x74, 0x01])?;
        let m = self.read_until(&[0x75])?;
        if m.len() < 26 {
            return Err(err("short status report"));
        }
        let fault = m[15] == 0xFF && m[16] == 0xFF;
        let has_fan = self.model.fan_id.is_some();
        Ok(CoolerStatus {
            liquid_c: (!fault).then(|| m[15] as f32 + m[16] as f32 / 10.0),
            pump_rpm: Some(u16::from_le_bytes([m[17], m[18]]) as u32),
            pump_duty: Some(m[19]),
            fan_rpm: has_fan.then(|| u16::from_le_bytes([m[23], m[24]]) as u32),
            fan_duty: has_fan.then_some(m[25]),
            fault,
            ..Default::default()
        })
    }

    /// Sends a full 20..=59 °C duty table. The cooler follows it on its own.
    pub fn set_curve(&mut self, channel: &str, table: &[u8; 40]) -> io::Result<()> {
        let id = match channel {
            "pump" => self.model.pump_id,
            "fan" => self.model.fan_id.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no fan channel"))?,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "unknown channel")),
        };
        let mut msg = vec![0x72, id[0], id[1], id[2]];
        msg.extend_from_slice(table);
        self.write(&msg)
    }

    fn write_screen_settings(&mut self) -> io::Result<()> {
        self.write(&[0x30, 0x02, 0x01, self.brightness, 0x00, 0x00, 0x01, self.orientation])
    }

    pub fn set_screen(&mut self, brightness: u8, rotation: u16) -> io::Result<()> {
        self.brightness = brightness.min(100);
        self.orientation = ((rotation / 90) % 4) as u8;
        self.write_screen_settings()
    }

    /// Writes a command and returns the next reply that isn't a status report.
    fn ask(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.write(data)?;
        self.read_reply()
    }

    fn delete_bucket(&mut self, i: u8) -> io::Result<bool> {
        self.write(&[0x32, 0x02, i])?;
        Ok(self.read_until(&[0x33, 0x02])?.get(14) == Some(&1))
    }

    /// A bucket that is free to write: deleted once if it was empty, twice if
    /// it held something, skipping buckets that refuse to be deleted.
    fn prepare_bucket(&mut self, mut i: u8, mut filled: bool) -> io::Result<u8> {
        loop {
            if i >= BUCKETS {
                return Err(err("no free picture slot on the screen"));
            }
            if !self.delete_bucket(i)? {
                i += 1;
                filled = true;
            } else if filled {
                filled = false;
            } else {
                return Ok(i);
            }
        }
    }

    fn switch_bucket(&mut self, i: u8, mode: u8) -> io::Result<bool> {
        Ok(self.ask(&[0x38, 0x01, mode, i])?.get(14) == Some(&1))
    }

    /// Stores `pixels` (already in the screen's layout) and shows them.
    pub fn show_picture(&mut self, pixels: &[u8]) -> io::Result<()> {
        if self.bulk.is_none() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "the screen's picture connection isn't open"));
        }
        self.drain()?;
        self.ask(&[0x36, 0x03])?;
        let mut buckets = Vec::with_capacity(BUCKETS as usize);
        for i in 0..BUCKETS {
            buckets.push(self.ask(&[0x30, 0x04, i])?);
        }
        let free = buckets.iter().position(|b| b.get(15..).is_none_or(|rest| rest.iter().all(|&x| x == 0)));
        let mut index = self.prepare_bucket(free.unwrap_or(0) as u8, free.is_none())?;

        let mut header = vec![0x12, 0xFA, 0x01, 0xE8, 0xAB, 0xCD, 0xEF, 0x98, 0x76, 0x54, 0x32, 0x10];
        // Picture type 2 (still image), then its length.
        header.extend([0x02, 0x00, 0x00, 0x00]);
        header.extend((pixels.len() as u32).to_le_bytes());
        let size_kib = (header.len() + pixels.len()).div_ceil(1024);
        let start = match bucket_memory_start(&buckets, index as usize, size_kib) {
            Some(s) => s,
            None => {
                // Full: back to the built-in screen and clear every slot.
                self.switch_bucket(0, 2)?;
                for i in 0..BUCKETS {
                    self.delete_bucket(i)?;
                }
                index = 0;
                0
            }
        };
        let start = (start as u16).to_le_bytes();
        let size = (size_kib as u16).to_le_bytes();
        let setup = self.ask(&[0x32, 0x01, index, index + 1, start[0], start[1], size[0], size[1], 0x01])?;
        if setup.get(14) != Some(&1) {
            return Err(err("the screen didn't accept the picture slot"));
        }
        self.ask(&[0x36, 0x01, index])?;
        let bulk = self.bulk.as_mut().expect("checked above");
        bulk.write(&header)?;
        for chunk in pixels.chunks(BULK_CHUNK) {
            bulk.write(chunk)?;
        }
        self.write(&[0x36, 0x02])?;
        if !self.switch_bucket(index, 0x04)? {
            return Err(err("the screen didn't switch to the new picture"));
        }
        Ok(())
    }

    /// Shows the cooler's built-in liquid temperature screen.
    pub fn show_liquid_temperature(&mut self) -> io::Result<bool> {
        self.write(&[0x38, 0x01, 0x02, 0x00])?;
        let reply = self.read_reply()?;
        Ok(reply.get(14) == Some(&1))
    }
}

impl Driver for Kraken {
    fn initialize(&mut self) -> io::Result<()> {
        Kraken::initialize(self)
    }

    fn firmware(&self) -> Option<String> {
        self.firmware.map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }

    fn status(&mut self) -> io::Result<CoolerStatus> {
        Kraken::status(self)
    }

    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()> {
        let min = if channel == "pump" { PUMP_MIN_DUTY } else { 0 };
        let table = curve::table(setting, channel, min);
        self.set_curve(channel, &table)
    }

    fn set_screen(&mut self, s: &ScreenSettings, pixels: Option<&[u8]>) -> io::Result<()> {
        if s.mode == ScreenMode::Unchanged {
            return Ok(());
        }
        Kraken::set_screen(self, s.brightness, s.rotation)?;
        match (s.mode, pixels) {
            (ScreenMode::Liquid, _) => {
                self.show_liquid_temperature()?;
            }
            (_, Some(px)) => self.show_picture(px)?,
            _ => {}
        }
        Ok(())
    }

    fn wants_bulk(&self) -> bool {
        self.model.lcd.is_some() && self.bulk.is_none()
    }

    fn attach_bulk(&mut self, bulk: Box<dyn Bulk>) {
        self.bulk = Some(bulk);
    }
}

/// Where in screen memory a picture of `size_kib` can go for `index`: its old
/// place if it fits and overlaps nothing, otherwise after everything stored,
/// otherwise at the very start. `None` means the memory has to be cleared.
fn bucket_memory_start(buckets: &[Vec<u8>], index: usize, size_kib: usize) -> Option<usize> {
    let field = |b: &Vec<u8>, at: usize| b.get(at..at + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]) as usize);
    let current = &buckets[index];
    let (cur_start, cur_size) = (field(current, 17), field(current, 19));
    if size_kib <= cur_size {
        return Some(cur_start);
    }
    let mut lowest = cur_start;
    let mut highest = 0;
    let mut overlaps = false;
    for (i, b) in buckets.iter().enumerate() {
        let (start, len) = (field(b, 17), field(b, 19));
        let end = start + len;
        highest = highest.max(end);
        lowest = lowest.min(start);
        let other = i != index && len > 0;
        if other && start < cur_start + size_kib && end > cur_start {
            overlaps = true;
        }
    }
    if !overlaps {
        return Some(cur_start);
    }
    if highest + size_kib < LCD_MEMORY_KIB {
        return Some(highest);
    }
    if size_kib < lowest {
        return Some(0);
    }
    None
}
