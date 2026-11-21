//! Alerts: a notification when the CPU or GPU stays too hot, memory stays
//! nearly full, or a drive runs low on space. Each alert waits a while
//! before repeating.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::storage::Volume;
use super::Sample;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertSettings {
    pub enabled: bool,
    /// °C, sustained.
    pub cpu_temp: f32,
    pub gpu_temp: f32,
    /// Percent of memory in use, sustained.
    pub ram: f32,
    /// Percent of a drive left free.
    pub disk_free: f32,
    /// How long a limit must be passed before alerting, seconds.
    pub sustain_s: u32,
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            enabled: true,
            cpu_temp: 90.0,
            gpu_temp: 85.0,
            ram: 95.0,
            disk_free: 10.0,
            sustain_s: 30,
        }
    }
}

/// How long before the same alert can show again.
const REPEAT_MS: u64 = 15 * 60 * 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub key: String,
    pub title: String,
    pub body: String,
}

#[derive(Default)]
pub struct Alerts {
    over_since: HashMap<String, u64>,
    sent: HashMap<String, u64>,
}

impl Alerts {
    fn sustained(&mut self, key: &str, over: bool, now: u64, need_ms: u64) -> bool {
        if !over {
            self.over_since.remove(key);
            return false;
        }
        let since = *self.over_since.entry(key.to_string()).or_insert(now);
        now.saturating_sub(since) >= need_ms
    }

    fn fire(&mut self, out: &mut Vec<Alert>, key: &str, title: String, body: String, now: u64) {
        if self
            .sent
            .get(key)
            .is_some_and(|t| now.saturating_sub(*t) < REPEAT_MS)
        {
            return;
        }
        self.sent.insert(key.to_string(), now);
        out.push(Alert {
            key: key.into(),
            title,
            body,
        });
    }

    /// The alerts to show now. `volumes`: drives to check (pass an empty
    /// list between disk checks).
    pub fn check(
        &mut self,
        s: &AlertSettings,
        sample: &Sample,
        volumes: &[Volume],
        now: u64,
    ) -> Vec<Alert> {
        let mut out = Vec::new();
        if !s.enabled {
            return out;
        }
        let need = s.sustain_s as u64 * 1000;
        if let Some(t) = sample.cpu.temp {
            if self.sustained("cpu", t >= s.cpu_temp, now, need) {
                self.fire(
                    &mut out,
                    "cpu",
                    "CPU is running hot".into(),
                    format!(
                        "{t:.0} °C for over {} seconds (your limit is {:.0} °C).",
                        s.sustain_s, s.cpu_temp
                    ),
                    now,
                );
            }
        }
        if let Some(t) = sample.gpu.as_ref().and_then(|g| g.temp) {
            if self.sustained("gpu", t >= s.gpu_temp, now, need) {
                self.fire(
                    &mut out,
                    "gpu",
                    "Graphics card is running hot".into(),
                    format!(
                        "{t:.0} °C for over {} seconds (your limit is {:.0} °C).",
                        s.sustain_s, s.gpu_temp
                    ),
                    now,
                );
            }
        }
        let ram = sample.memory.percent;
        if sample.memory.total > 0 && self.sustained("ram", ram >= s.ram, now, need.max(60_000)) {
            self.fire(
                &mut out,
                "ram",
                "Memory is nearly full".into(),
                format!("{ram:.0} % of your RAM is in use. Closing a few apps frees some up."),
                now,
            );
        }
        for v in volumes.iter().filter(|v| v.kind == "fixed" && v.total > 0) {
            let free = v.free as f64 / v.total as f64 * 100.0;
            let key = format!("disk:{}", v.letter);
            if free < s.disk_free as f64 {
                self.fire(
                    &mut out,
                    &key,
                    format!("{} is running low on space", v.letter),
                    format!("{:.1} GB free ({free:.0} %).", v.free as f64 / 1e9),
                    now,
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hot(temp: f32) -> Sample {
        let mut s = Sample::default();
        s.cpu.temp = Some(temp);
        s
    }

    #[test]
    fn waits_for_a_sustained_limit_then_holds_off() {
        let set = AlertSettings::default();
        let mut a = Alerts::default();
        assert!(a.check(&set, &hot(95.0), &[], 0).is_empty());
        assert!(a.check(&set, &hot(95.0), &[], 20_000).is_empty());
        let fired = a.check(&set, &hot(95.0), &[], 31_000);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].key, "cpu");
        // No repeat for a while.
        assert!(a.check(&set, &hot(95.0), &[], 120_000).is_empty());
        // Dipping below resets the timer.
        assert!(a.check(&set, &hot(70.0), &[], 130_000).is_empty());
        assert!(a.check(&set, &hot(95.0), &[], 2_000_000).is_empty());
        assert_eq!(a.check(&set, &hot(95.0), &[], 2_040_000).len(), 1);
    }

    #[test]
    fn low_disk_space() {
        let set = AlertSettings::default();
        let mut a = Alerts::default();
        let v = Volume {
            letter: "C:".into(),
            label: String::new(),
            filesystem: "NTFS".into(),
            total: 1_000_000_000_000,
            free: 50_000_000_000,
            kind: "fixed",
            disk: Some(0),
        };
        let fired = a.check(&set, &Sample::default(), &[v], 0);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].title.contains("C:"));
        let off = AlertSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(Alerts::default()
            .check(&off, &hot(99.0), &[], 99_000)
            .is_empty());
    }
}
