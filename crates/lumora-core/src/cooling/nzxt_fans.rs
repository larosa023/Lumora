//! NZXT RGB & Fan Controller and Smart Device V2: case fan speeds over HID.
//!
//! The controller only takes a fixed speed per fan (it stores no curves), so
//! Lumora runs the curves itself and sends the speed live. Once asked, it
//! reports every fan's speed and duty about once a second. The lighting
//! side of the same controller is left to the lighting engine.
//!
//! Protocol as in liquidctl's `SmartDevice2` driver: 64-byte reports.

use std::io;
use std::time::{Duration, Instant};

use super::curve::SpeedSetting;
use super::family::Driver;
use super::{CoolerStatus, Link};

pub const NZXT_VID: u16 = 0x1E71;

#[derive(Debug, PartialEq)]
pub struct FanControllerModel {
    pub pid: u16,
    pub name: &'static str,
    pub fans: u8,
}

const fn m(pid: u16, name: &'static str) -> FanControllerModel {
    FanControllerModel { pid, name, fans: 3 }
}

pub const MODELS: &[FanControllerModel] = &[
    m(0x2006, "Smart Device V2"),
    m(0x200D, "Smart Device V2"),
    m(0x200F, "Smart Device V2"),
    m(0x2009, "RGB & Fan Controller"),
    m(0x200E, "RGB & Fan Controller"),
    m(0x2010, "RGB & Fan Controller"),
    m(0x2011, "RGB & Fan Controller"),
    m(0x2019, "RGB & Fan Controller"),
    m(0x201F, "RGB & Fan Controller"),
    m(0x2020, "RGB & Fan Controller"),
];

pub fn model(pid: u16) -> Option<&'static FanControllerModel> {
    MODELS.iter().find(|m| m.pid == pid)
}

/// Channel ids, one per fan port.
pub const CHANNELS: [&str; 3] = ["fan1", "fan2", "fan3"];
pub const CHANNEL_NAMES: [&str; 3] = ["Fan 1", "Fan 2", "Fan 3"];

/// Speed left on the fans when Lumora isn't driving them live any more
/// (closed, switched off, or no temperature to follow): never stuck slow.
pub const SAFE_DUTY: u8 = 60;
/// Lowest speed Lumora sets; many fans stall below this.
pub const MIN_DUTY: u8 = 20;

const REPORT: usize = 64;
const MAX_READS: usize = 12;

fn padded(msg: &[u8]) -> Vec<u8> {
    let mut v = msg.to_vec();
    v.resize(REPORT, 0);
    v
}

/// The message setting fan `index` (0-based) to `duty` percent.
pub fn duty_message(index: usize, duty: u8) -> Vec<u8> {
    let mut msg = vec![0x62, 0x01, 1 << index, 0, 0, 0];
    msg[3 + index] = duty.min(100);
    padded(&msg)
}

/// Fan readings from a `67 02` report: (rpm, duty, connected) per fan.
pub fn parse_fans(msg: &[u8], fans: usize) -> Vec<(u32, u8, bool)> {
    (0..fans)
        .map(|i| {
            let rpm = u16::from_le_bytes([
                msg.get(24 + 2 * i).copied().unwrap_or(0),
                msg.get(25 + 2 * i).copied().unwrap_or(0),
            ]) as u32;
            let duty = msg.get(40 + i).copied().unwrap_or(0);
            let connected = msg.get(16 + i).copied().unwrap_or(0) != 0;
            (rpm, duty, connected)
        })
        .collect()
}

pub struct FanController {
    link: Box<dyn Link>,
    model: &'static FanControllerModel,
    firmware: Option<String>,
}

impl FanController {
    pub fn new(link: Box<dyn Link>, model: &'static FanControllerModel) -> Self {
        FanController {
            link,
            model,
            firmware: None,
        }
    }

    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        self.link.write(&padded(msg))
    }

    /// Reads until a report starting with `prefix` arrives.
    fn read_until(&mut self, prefix: [u8; 2]) -> io::Result<Option<Vec<u8>>> {
        let start = Instant::now();
        for _ in 0..MAX_READS {
            if start.elapsed() > Duration::from_millis(2500) {
                break;
            }
            match self.link.read(Duration::from_millis(1200))? {
                Some(m) if m.len() >= 2 && m[..2] == prefix => return Ok(Some(m)),
                Some(_) => continue,
                None => break,
            }
        }
        Ok(None)
    }
}

impl Driver for FanController {
    fn initialize(&mut self) -> io::Result<()> {
        // Ask for fan reports once a second.
        self.write(&[0x60, 0x02, 0x01, 0xE8, 0x01, 0x01, 0xE8, 0x01])?;
        self.write(&[0x60, 0x03])?;
        self.write(&[0x10, 0x01])?;
        if let Some(m) = self.read_until([0x11, 0x01])? {
            if m.len() > 0x13 {
                self.firmware = Some(format!("{}.{}.{}", m[0x11], m[0x12], m[0x13]));
            }
        }
        Ok(())
    }

    fn firmware(&self) -> Option<String> {
        self.firmware.clone()
    }

    fn status(&mut self) -> io::Result<CoolerStatus> {
        let Some(m) = self.read_until([0x67, 0x02])? else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the fan controller didn't report its fans",
            ));
        };
        let fans = parse_fans(&m, self.model.fans as usize);
        let on: Vec<&(u32, u8, bool)> = fans.iter().filter(|f| f.2).collect();
        let avg = |v: Vec<u32>| (!v.is_empty()).then(|| v.iter().sum::<u32>() / v.len() as u32);
        Ok(CoolerStatus {
            fan_rpm: avg(on.iter().map(|f| f.0).collect()),
            fan_duty: avg(on.iter().map(|f| f.1 as u32).collect()).map(|d| d as u8),
            fan_rpms: fans.iter().map(|f| f.0).collect(),
            fan_duties: fans.iter().map(|f| f.1).collect(),
            fans_connected: fans.iter().map(|f| f.2).collect(),
            ..Default::default()
        })
    }

    /// A fixed duty is set as is; anything else (a curve Lumora can't follow
    /// right now) leaves the fan at a safe speed.
    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()> {
        let index = CHANNELS
            .iter()
            .position(|c| *c == channel)
            .filter(|i| *i < self.model.fans as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown channel"))?;
        let duty = match setting {
            SpeedSetting::Fixed { duty } => (*duty).max(MIN_DUTY),
            _ => SAFE_DUTY,
        };
        self.link.write(&duty_message(index, duty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duty_messages_target_one_fan() {
        let m = duty_message(1, 55);
        assert_eq!(&m[..6], &[0x62, 0x01, 0x02, 0, 55, 0]);
        assert_eq!(m.len(), 64);
        assert_eq!(duty_message(2, 140)[5], 100);
    }

    #[test]
    fn fan_reports_parse() {
        let mut r = vec![0u8; 64];
        r[0] = 0x67;
        r[1] = 0x02;
        r[16] = 2; // fan 1 PWM
        r[24] = 0x84;
        r[25] = 0x03; // 900 rpm
        r[40] = 45;
        let f = parse_fans(&r, 3);
        assert_eq!(f[0], (900, 45, true));
        assert_eq!(f[1], (0, 0, false));
    }
}
