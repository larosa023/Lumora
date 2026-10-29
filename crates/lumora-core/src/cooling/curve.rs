//! Speed settings and temperature curves.
//!
//! Coolers store a curve of duty per liquid temperature and follow it on
//! their own, so a cooler keeps doing the right thing even if Lumora closes.

use serde::{Deserialize, Serialize};

/// Points of `(liquid °C, duty %)`.
pub type Curve = Vec<(u8, u8)>;

/// Coolers like the Kraken take one duty value for each degree from 20 to 59 °C.
pub const CURVE_START: u8 = 20;
pub const CURVE_END: u8 = 59;
/// Above this the cooler runs flat out no matter what (firmware enforces 60 °C).
pub const CRITICAL: u8 = 59;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Silent,
    Balanced,
    Performance,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum SpeedSetting {
    Preset { preset: Preset },
    Fixed { duty: u8 },
    Custom { points: Curve },
}

impl Default for SpeedSetting {
    fn default() -> Self {
        SpeedSetting::Preset { preset: Preset::Balanced }
    }
}

/// Built-in curves, by channel kind.
pub fn preset_curve(channel: &str, preset: Preset) -> Curve {
    let pump = channel == "pump";
    match (pump, preset) {
        (true, Preset::Silent) => vec![(20, 50), (35, 60), (45, 80), (50, 100)],
        (true, Preset::Balanced) => vec![(20, 60), (35, 70), (45, 90), (50, 100)],
        (true, Preset::Performance) => vec![(20, 80), (30, 90), (40, 100)],
        (false, Preset::Silent) => vec![(20, 25), (30, 30), (36, 45), (42, 70), (48, 100)],
        (false, Preset::Balanced) => vec![(20, 30), (30, 40), (36, 60), (42, 85), (48, 100)],
        (false, Preset::Performance) => vec![(20, 50), (30, 65), (36, 85), (40, 100)],
    }
}

impl SpeedSetting {
    /// The points this setting describes (before clamping to the channel).
    pub fn points(&self, channel: &str) -> Curve {
        match self {
            SpeedSetting::Preset { preset } => preset_curve(channel, *preset),
            SpeedSetting::Fixed { duty } => vec![(CURVE_START, *duty), (CURVE_END - 1, *duty)],
            SpeedSetting::Custom { points } => points.clone(),
        }
    }
}

/// Sorts the points, makes duty never drop as temperature rises, and adds a
/// 100 % failsafe at the critical temperature.
pub fn normalize(points: &[(u8, u8)]) -> Curve {
    let mut pts: Vec<(u8, u8)> = points.iter().map(|&(t, d)| (t, d.min(100))).collect();
    pts.push((CRITICAL, 100));
    pts.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out: Curve = Vec::with_capacity(pts.len());
    for (t, d) in pts {
        if let Some(&(lt, ld)) = out.last() {
            if t == lt {
                continue;
            }
            if ld >= 100 {
                break;
            }
            out.push((t, d.max(ld)));
        } else {
            out.push((t, d));
        }
    }
    out
}

/// Duty at `temp` by straight lines between points (flat beyond the ends).
pub fn interpolate(points: &[(u8, u8)], temp: u8) -> u8 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return 100 };
    if temp <= first.0 {
        return first.1;
    }
    if temp >= last.0 {
        return last.1;
    }
    for w in points.windows(2) {
        let ((t0, d0), (t1, d1)) = (w[0], w[1]);
        if temp >= t0 && temp <= t1 {
            let f = (temp - t0) as f32 / (t1 - t0) as f32;
            return (d0 as f32 + f * (d1 as f32 - d0 as f32)).round() as u8;
        }
    }
    last.1
}

/// The 40 duty values (20..=59 °C) a Kraken-style cooler wants, clamped to the
/// channel's allowed range.
pub fn table(setting: &SpeedSetting, channel: &str, min_duty: u8) -> [u8; 40] {
    let norm = normalize(&setting.points(channel));
    let mut out = [0u8; 40];
    for (i, temp) in (CURVE_START..=CURVE_END).enumerate() {
        out[i] = interpolate(&norm, temp).clamp(min_duty, 100);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_a_cooler_expects() {
        // Unsorted, a dip, and a duplicate temperature.
        let n = normalize(&[(30, 40), (25, 25), (35, 30), (40, 35), (40, 80)]);
        assert_eq!(n, vec![(25, 25), (30, 40), (35, 40), (40, 80), (59, 100)]);
        // Stops at the first 100 %.
        assert_eq!(normalize(&[(30, 40), (35, 100), (40, 100)]), vec![(30, 40), (35, 100)]);
        assert_eq!(normalize(&[]), vec![(59, 100)]);
    }

    #[test]
    fn interpolates() {
        let p = [(20, 50), (50, 70), (59, 100)];
        assert_eq!(interpolate(&p, 19), 50);
        assert_eq!(interpolate(&p, 35), 60);
        assert_eq!(interpolate(&p, 59), 100);
    }

    #[test]
    fn tables_respect_minimum_and_failsafe() {
        let t = table(&SpeedSetting::Fixed { duty: 0 }, "pump", 20);
        assert!(t.iter().all(|&d| d >= 20), "pump never below its minimum");
        let t = table(&SpeedSetting::Fixed { duty: 30 }, "fan", 0);
        assert_eq!(t[0], 30);
        assert_eq!(t[39], 100, "59 °C is always full speed");
        for preset in [Preset::Silent, Preset::Balanced, Preset::Performance] {
            for ch in ["pump", "fan"] {
                let t = table(&SpeedSetting::Preset { preset }, ch, 0);
                assert!(t.windows(2).all(|w| w[1] >= w[0]), "{ch} {preset:?} rises");
                assert_eq!(t[39], 100);
            }
        }
    }

    #[test]
    fn settings_round_trip_as_json() {
        let s: SpeedSetting = serde_json::from_str(r#"{"mode":"custom","points":[[20,30],[40,70]]}"#).unwrap();
        assert_eq!(s, SpeedSetting::Custom { points: vec![(20, 30), (40, 70)] });
        let s: SpeedSetting = serde_json::from_str(r#"{"mode":"preset","preset":"silent"}"#).unwrap();
        assert_eq!(s, SpeedSetting::Preset { preset: Preset::Silent });
    }
}
