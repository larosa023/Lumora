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
    Preset {
        preset: Preset,
    },
    Fixed {
        duty: u8,
    },
    Custom {
        points: Curve,
    },
    /// The device's own control (a motherboard header's BIOS curve): Lumora
    /// doesn't set this one. Devices that can't hand back run it as balanced.
    Auto,
}

impl Default for SpeedSetting {
    fn default() -> Self {
        SpeedSetting::Preset {
            preset: Preset::Balanced,
        }
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

/// The temperature a curve follows: the cooler's liquid (the cooler runs the
/// curve itself) or a reading Lumora feeds it ("gpu", "cpu").
pub fn is_liquid(source: &str) -> bool {
    !matches!(source, "gpu" | "cpu")
}

/// `(first °C, full speed °C)` of a curve following `source`.
pub fn range(source: &str) -> (u8, u8) {
    match source {
        "gpu" => (30, 89),
        "cpu" => (35, 94),
        _ => (CURVE_START, CRITICAL),
    }
}

/// Built-in curves for a channel following `source`.
pub fn preset_curve_for(channel: &str, preset: Preset, source: &str) -> Curve {
    if is_liquid(source) {
        return preset_curve(channel, preset);
    }
    // Shaped for GPU/CPU readings, which run much hotter than liquid; CPU
    // curves sit five degrees above GPU ones.
    let shift = if source == "cpu" { 5 } else { 0 };
    let pump = channel == "pump";
    let pts: Curve = match (pump, preset) {
        (true, Preset::Silent) => vec![(40, 50), (60, 60), (75, 80), (85, 100)],
        (true, Preset::Balanced) => vec![(40, 60), (60, 70), (72, 90), (82, 100)],
        (true, Preset::Performance) => vec![(40, 80), (55, 90), (70, 100)],
        (false, Preset::Silent) => vec![(40, 25), (55, 30), (65, 45), (75, 70), (85, 100)],
        (false, Preset::Balanced) => vec![(40, 30), (55, 40), (65, 60), (75, 85), (83, 100)],
        (false, Preset::Performance) => vec![(40, 50), (55, 65), (65, 85), (72, 100)],
    };
    pts.into_iter().map(|(t, d)| (t + shift, d)).collect()
}

impl SpeedSetting {
    /// The points this setting describes for a curve following `source`.
    pub fn points_for(&self, channel: &str, source: &str) -> Curve {
        let (start, end) = range(source);
        match self {
            SpeedSetting::Preset { preset } => preset_curve_for(channel, *preset, source),
            SpeedSetting::Fixed { duty } => vec![(start, *duty), (end - 1, *duty)],
            SpeedSetting::Custom { points } => points.clone(),
            SpeedSetting::Auto => preset_curve_for(channel, Preset::Balanced, source),
        }
    }

    /// The points this setting describes (before clamping to the channel).
    pub fn points(&self, channel: &str) -> Curve {
        match self {
            SpeedSetting::Preset { preset } => preset_curve(channel, *preset),
            SpeedSetting::Fixed { duty } => vec![(CURVE_START, *duty), (CURVE_END - 1, *duty)],
            SpeedSetting::Custom { points } => points.clone(),
            SpeedSetting::Auto => preset_curve(channel, Preset::Balanced),
        }
    }
}

/// Sorts the points, makes duty never drop as temperature rises, and adds a
/// 100 % failsafe at the critical temperature.
pub fn normalize(points: &[(u8, u8)]) -> Curve {
    normalize_to(points, CRITICAL)
}

/// [`normalize`] with full speed from `critical` °C.
pub fn normalize_to(points: &[(u8, u8)], critical: u8) -> Curve {
    let mut pts: Vec<(u8, u8)> = points.iter().map(|&(t, d)| (t, d.min(100))).collect();
    pts.push((critical, 100));
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
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100;
    };
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

/// Duty for every degree of `source`'s range, as the UI draws it.
pub fn table_for(setting: &SpeedSetting, channel: &str, min_duty: u8, source: &str) -> Vec<u8> {
    let (start, end) = range(source);
    let norm = normalize_to(&setting.points_for(channel, source), end);
    (start..=end)
        .map(|t| interpolate(&norm, t).clamp(min_duty, 100))
        .collect()
}

/// Duty right now for a curve following `source` at `celsius`.
pub fn duty_at(
    setting: &SpeedSetting,
    channel: &str,
    min_duty: u8,
    source: &str,
    celsius: f32,
) -> u8 {
    let (_, end) = range(source);
    let norm = normalize_to(&setting.points_for(channel, source), end);
    // The exact reading, not rounded to a degree: on a steep curve a reading
    // sitting at x.5 °C would otherwise flip between two speeds.
    (interpolate_f(&norm, celsius).round() as u8).clamp(min_duty, 100)
}

/// [`interpolate`] at a fractional temperature.
pub fn interpolate_f(points: &[(u8, u8)], temp: f32) -> f32 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100.0;
    };
    if temp <= first.0 as f32 {
        return first.1 as f32;
    }
    if temp >= last.0 as f32 {
        return last.1 as f32;
    }
    for w in points.windows(2) {
        let ((t0, d0), (t1, d1)) = (w[0], w[1]);
        let (t0, t1) = (t0 as f32, t1 as f32);
        if temp >= t0 && temp <= t1 {
            return d0 as f32 + (temp - t0) / (t1 - t0) * (d1 as f32 - d0 as f32);
        }
    }
    last.1 as f32
}

#[cfg(test)]
mod tests {
    #[test]
    fn steep_curves_are_read_at_the_exact_temperature() {
        let s = SpeedSetting::Custom {
            points: vec![(30, 25), (33, 100)],
        };
        let a = duty_at(&s, "fan", 0, "liquid", 31.4);
        let b = duty_at(&s, "fan", 0, "liquid", 31.5);
        assert!(b - a <= 3, "{a} vs {b}");
    }

    #[test]
    fn gpu_curves_run_on_their_own_range() {
        let s = SpeedSetting::default();
        let t = table_for(&s, "fan", 0, "gpu");
        assert_eq!(t.len(), 60);
        assert_eq!(*t.last().unwrap(), 100);
        assert_eq!(duty_at(&s, "fan", 0, "gpu", 40.0), 30);
        assert_eq!(duty_at(&s, "fan", 0, "gpu", 95.0), 100);
        assert_eq!(duty_at(&s, "pump", 20, "cpu", 30.0), 60);
        // Liquid tables are unchanged.
        assert_eq!(
            table_for(&s, "fan", 0, "liquid"),
            table(&s, "fan", 0).to_vec()
        );
    }

    use super::*;

    #[test]
    fn normalizes_like_a_cooler_expects() {
        // Unsorted, a dip, and a duplicate temperature.
        let n = normalize(&[(30, 40), (25, 25), (35, 30), (40, 35), (40, 80)]);
        assert_eq!(n, vec![(25, 25), (30, 40), (35, 40), (40, 80), (59, 100)]);
        // Stops at the first 100 %.
        assert_eq!(
            normalize(&[(30, 40), (35, 100), (40, 100)]),
            vec![(30, 40), (35, 100)]
        );
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
        let s: SpeedSetting =
            serde_json::from_str(r#"{"mode":"custom","points":[[20,30],[40,70]]}"#).unwrap();
        assert_eq!(
            s,
            SpeedSetting::Custom {
                points: vec![(20, 30), (40, 70)]
            }
        );
        let s: SpeedSetting =
            serde_json::from_str(r#"{"mode":"preset","preset":"silent"}"#).unwrap();
        assert_eq!(
            s,
            SpeedSetting::Preset {
                preset: Preset::Silent
            }
        );
    }
}
