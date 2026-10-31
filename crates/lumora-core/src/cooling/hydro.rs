//! Corsair Hydro H100i Pro, H115i Pro and H150i Pro (Asetek's sixth
//! generation) over raw USB.
//!
//! Each command is one short bulk message; the cooler answers with a message
//! that starts with the same command byte. The pump has three built-in modes,
//! each fan follows a curve of up to seven points that the cooler interpolates
//! itself, and the pump logo has a few built-in lighting modes.

use std::io;
use std::time::Duration;

use super::curve::{self, Preset, SpeedSetting};
use super::family::Driver;
use super::{CoolerStatus, LightMode, LightSettings, LightSpeed, Link};

pub const CORSAIR_VID: u16 = 0x1B1C;

#[derive(Debug, PartialEq)]
pub struct HydroModel {
    pub pid: u16,
    pub name: &'static str,
    pub fans: u8,
}

pub const MODELS: &[HydroModel] = &[
    HydroModel {
        pid: 0x0C12,
        name: "H150i Pro",
        fans: 3,
    },
    HydroModel {
        pid: 0x0C13,
        name: "H115i Pro",
        fans: 2,
    },
    HydroModel {
        pid: 0x0C15,
        name: "H100i Pro",
        fans: 2,
    },
];

pub fn model(pid: u16) -> Option<&'static HydroModel> {
    MODELS.iter().find(|m| m.pid == pid)
}

const READ_TIMEOUT: Duration = Duration::from_millis(1000);
const MAX_CURVE_POINTS: usize = 7;
/// Unused curve slots are filled with this point; padding with zeros has been
/// seen to confuse the firmware.
const PAD_POINT: (u8, u8) = (60, 100);

const CMD_PUMP_SPEED: u8 = 0x31;
const CMD_SET_PUMP_MODE: u8 = 0x32;
const CMD_PUMP_MODE: u8 = 0x33;
const CMD_FAN_CURVE: u8 = 0x40;
const CMD_FAN_SPEED: u8 = 0x41;
const CMD_FAN_FIXED: u8 = 0x42;
const CMD_LIGHT_PULSE: u8 = 0x52;
const CMD_LIGHT_SPEED: u8 = 0x53;
const CMD_LIGHT_SHIFT: u8 = 0x55;
const CMD_LIGHT_COLORS: u8 = 0x56;
const CMD_LIGHT_BLINK: u8 = 0x58;
const CMD_LIQUID: u8 = 0xA9;
const CMD_FIRMWARE: u8 = 0xAA;

const PUMP_MODES: [&str; 3] = ["quiet", "balanced", "performance"];

pub struct HydroPro {
    link: Box<dyn Link>,
    pub model: &'static HydroModel,
    firmware: Option<String>,
}

fn timeout(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("the cooler didn't answer ({what})"),
    )
}

/// Pump mode index for a speed setting. Only the presets map exactly; other
/// settings pick the closest mode.
pub fn pump_mode(setting: &SpeedSetting) -> u8 {
    match setting {
        SpeedSetting::Preset {
            preset: Preset::Silent,
        } => 0,
        SpeedSetting::Preset {
            preset: Preset::Balanced,
        } => 1,
        SpeedSetting::Preset {
            preset: Preset::Performance,
        } => 2,
        SpeedSetting::Fixed { duty } if *duty < 50 => 0,
        SpeedSetting::Fixed { duty } if *duty < 80 => 1,
        SpeedSetting::Fixed { .. } => 2,
        SpeedSetting::Custom { .. } => 1,
    }
}

/// The seven `(°C, duty)` points the cooler stores for a fan.
pub fn fan_points(setting: &SpeedSetting) -> [(u8, u8); MAX_CURVE_POINTS] {
    let mut pts = curve::normalize(&setting.points("fan"));
    if pts.len() > MAX_CURVE_POINTS {
        // Keep the ends and an even spread between them.
        let n = pts.len();
        pts = (0..MAX_CURVE_POINTS)
            .map(|i| pts[i * (n - 1) / (MAX_CURVE_POINTS - 1)])
            .collect();
    }
    let mut out = [PAD_POINT; MAX_CURVE_POINTS];
    out[..pts.len()].copy_from_slice(&pts);
    out
}

fn parse_hex(c: &str) -> [u8; 3] {
    let h = c.trim_start_matches('#');
    let v = u32::from_str_radix(h.get(..6).unwrap_or("000000"), 16).unwrap_or(0);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// The messages that set the logo lighting, in order.
pub fn light_messages(s: &LightSettings) -> Vec<Vec<u8>> {
    let mut colors: Vec<[u8; 3]> = s.colors.iter().take(4).map(|c| parse_hex(c)).collect();
    if colors.is_empty() {
        colors.push([0x7c, 0x5c, 0xff]);
    }
    let speed = |vals: [u8; 3]| match s.speed {
        LightSpeed::Slower => vals[0],
        LightSpeed::Normal => vals[1],
        LightSpeed::Faster => vals[2],
    };
    let (colors, speed_value, mode_msg) = match s.mode {
        // One color is sent as a two-color shift between the same color.
        // Sync arrives here already resolved to the current color.
        LightMode::Static | LightMode::Sync => (
            vec![colors[0], colors[0]],
            None,
            vec![CMD_LIGHT_SHIFT, 0x01],
        ),
        LightMode::Shift => {
            if colors.len() < 2 {
                colors.push(colors[0]);
            }
            (
                colors,
                Some(speed([0x46, 0x28, 0x0F])),
                vec![CMD_LIGHT_SHIFT, 0x01],
            )
        }
        LightMode::Pulse => (
            colors,
            Some(speed([0x50, 0x37, 0x1E])),
            vec![CMD_LIGHT_PULSE, 0x01],
        ),
        LightMode::Blink => (
            colors,
            Some(speed([0x0F, 0x0A, 0x05])),
            vec![CMD_LIGHT_BLINK, 0x01],
        ),
    };
    let mut list = vec![CMD_LIGHT_COLORS, colors.len() as u8];
    list.extend(colors.iter().flatten());
    let mut out = vec![list];
    if let Some(v) = speed_value {
        out.push(vec![CMD_LIGHT_SPEED, v]);
    }
    out.push(mode_msg);
    out
}

impl HydroPro {
    pub fn new(link: Box<dyn Link>, model: &'static HydroModel) -> Self {
        HydroPro {
            link,
            model,
            firmware: None,
        }
    }

    /// Sends one command and returns the cooler's answer to it.
    fn post(&mut self, msg: &[u8]) -> io::Result<Vec<u8>> {
        self.link.write(msg)?;
        // Replies echo the command byte. If one doesn't, it may be a leftover
        // from earlier, so give the real answer a moment before settling.
        let Some(first) = self.link.read(READ_TIMEOUT)? else {
            return Err(timeout(&format!("command {:02x}", msg[0])));
        };
        if first.first() == msg.first() {
            return Ok(first);
        }
        match self.link.read(Duration::from_millis(150))? {
            Some(reply) if reply.first() == msg.first() => Ok(reply),
            _ => Ok(first),
        }
    }
}

impl Driver for HydroPro {
    fn initialize(&mut self) -> io::Result<()> {
        let m = self.post(&[CMD_FIRMWARE])?;
        if m.len() >= 7 {
            self.firmware = Some(format!("{}.{}.{}.{}", m[3], m[4], m[5], m[6]));
        }
        Ok(())
    }

    fn firmware(&self) -> Option<String> {
        self.firmware.clone()
    }

    fn status(&mut self) -> io::Result<CoolerStatus> {
        let m = self.post(&[CMD_LIQUID])?;
        if m.len() < 5 {
            return Err(timeout("short temperature reply"));
        }
        let liquid = m[3] as f32 + m[4] as f32 / 10.0;

        let mut fans = Vec::new();
        for i in 0..self.model.fans {
            let m = self.post(&[CMD_FAN_SPEED, i])?;
            if m.len() >= 6 && m[3] == i {
                fans.push(u16::from_be_bytes([m[4], m[5]]) as u32);
            }
        }

        let m = self.post(&[CMD_PUMP_MODE])?;
        let mode = m
            .get(3)
            .and_then(|&i| PUMP_MODES.get(i as usize))
            .map(|s| s.to_string());
        let m = self.post(&[CMD_PUMP_SPEED])?;
        let pump = (m.len() >= 5).then(|| u16::from_be_bytes([m[3], m[4]]) as u32);

        let avg = (!fans.is_empty()).then(|| fans.iter().sum::<u32>() / fans.len() as u32);
        Ok(CoolerStatus {
            liquid_c: Some(liquid),
            pump_rpm: pump,
            pump_duty: None,
            fan_rpm: avg,
            fan_duty: None,
            fan_rpms: fans,
            pump_mode: mode,
            fault: false,
        })
    }

    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()> {
        match channel {
            "pump" => {
                self.post(&[CMD_SET_PUMP_MODE, pump_mode(setting)])?;
            }
            "fan" => {
                for i in 0..self.model.fans {
                    if let SpeedSetting::Fixed { duty } = setting {
                        self.post(&[CMD_FAN_FIXED, i, (*duty).min(100)])?;
                    } else {
                        let pts = fan_points(setting);
                        let mut msg = vec![CMD_FAN_CURVE, i];
                        msg.extend(pts.iter().map(|p| p.0));
                        msg.extend(pts.iter().map(|p| p.1));
                        self.post(&msg)?;
                    }
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown channel",
                ))
            }
        }
        Ok(())
    }

    fn set_light(&mut self, settings: &LightSettings) -> io::Result<()> {
        for msg in light_messages(settings) {
            self.post(&msg)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_points_are_padded_and_capped() {
        let p = fan_points(&SpeedSetting::Preset {
            preset: Preset::Balanced,
        });
        assert_eq!(p[0], (20, 30));
        assert_eq!(p[4], (48, 100));
        assert_eq!(p[5], PAD_POINT);
        assert_eq!(p[6], PAD_POINT);

        let many: Vec<(u8, u8)> = (0..12).map(|i| (20 + i * 3, 10 + i * 5)).collect();
        let p = fan_points(&SpeedSetting::Custom { points: many });
        assert_eq!(p[0], (20, 10));
        assert!(p.windows(2).all(|w| w[0].0 < w[1].0 || w[1] == PAD_POINT));
        assert_eq!(p.iter().map(|x| x.1).max(), Some(100), "failsafe kept");
    }

    #[test]
    fn light_messages_follow_mode() {
        let s = LightSettings {
            mode: LightMode::Static,
            colors: vec!["#ff8000".into()],
            speed: LightSpeed::Normal,
        };
        assert_eq!(
            light_messages(&s),
            vec![
                vec![0x56, 2, 0xff, 0x80, 0, 0xff, 0x80, 0],
                vec![0x55, 0x01]
            ]
        );
        let s = LightSettings {
            mode: LightMode::Pulse,
            colors: vec!["#010203".into(), "#040506".into()],
            speed: LightSpeed::Faster,
        };
        assert_eq!(
            light_messages(&s),
            vec![
                vec![0x56, 2, 1, 2, 3, 4, 5, 6],
                vec![0x53, 0x1E],
                vec![0x52, 0x01]
            ]
        );
    }

    #[test]
    fn pump_modes_map_from_presets() {
        assert_eq!(
            pump_mode(&SpeedSetting::Preset {
                preset: Preset::Silent
            }),
            0
        );
        assert_eq!(
            pump_mode(&SpeedSetting::Preset {
                preset: Preset::Performance
            }),
            2
        );
        assert_eq!(pump_mode(&SpeedSetting::Fixed { duty: 60 }), 1);
    }
}
