//! The last 24 hours of readings: one every few seconds, kept in memory for
//! the charts and appended to a CSV log per day (UTC) for keeping. Logs
//! older than 30 days are deleted. On start, today's and yesterday's logs
//! fill the charts back in, so a restart loses nothing.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

/// The readings kept, in this order.
pub const FIELDS: [&str; 10] = [
    "liquid",
    "cpu",
    "gpu",
    "cpu_usage",
    "gpu_usage",
    "ram",
    "cpu_power",
    "gpu_power",
    "net_down",
    "net_up",
];
const HEADER: &str = "unix_ms,time_utc,liquid_c,cpu_c,gpu_c,cpu_pct,gpu_pct,ram_pct,cpu_w,gpu_w,down_bytes_per_s,up_bytes_per_s";
pub const KEEP: Duration = Duration::from_secs(24 * 3600);
const KEEP_DAYS: u64 = 30;
const FLUSH_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Point {
    pub at: u64,
    /// NaN where there was no reading (serialized as null).
    #[serde(serialize_with = "nan_as_null")]
    pub v: [f32; 10],
}

fn nan_as_null<S: serde::Serializer>(v: &[f32; 10], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(v.len()))?;
    for x in v {
        if x.is_finite() {
            seq.serialize_element(&((x * 10.0).round() / 10.0))?;
        } else {
            seq.serialize_element(&Option::<f32>::None)?;
        }
    }
    seq.end()
}

impl Point {
    pub fn empty(at: u64) -> Self {
        Point {
            at,
            v: [f32::NAN; 10],
        }
    }
    pub fn set(&mut self, field: &str, value: Option<f32>) {
        if let (Some(i), Some(v)) = (FIELDS.iter().position(|f| *f == field), value) {
            self.v[i] = v;
        }
    }
    pub fn get(&self, field: &str) -> Option<f32> {
        FIELDS
            .iter()
            .position(|f| *f == field)
            .map(|i| self.v[i])
            .filter(|v| v.is_finite())
    }
}

/// Days since 1970 → (year, month, day), UTC.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

pub fn day_name(ms: u64) -> String {
    let (y, m, d) = civil((ms / 86_400_000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn time_name(ms: u64) -> String {
    let s = ms / 1000;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        day_name(ms),
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60
    )
}

pub fn csv_line(p: &Point) -> String {
    let mut line = format!("{},{}", p.at, time_name(p.at));
    for v in p.v {
        line.push(',');
        if v.is_finite() {
            line.push_str(&format!("{:.1}", v));
        }
    }
    line
}

pub fn parse_line(line: &str) -> Option<Point> {
    let mut parts = line.split(',');
    let at = parts.next()?.parse::<u64>().ok()?;
    parts.next()?;
    let mut p = Point::empty(at);
    for (i, x) in parts.take(FIELDS.len()).enumerate() {
        if let Ok(v) = x.parse::<f32>() {
            p.v[i] = v;
        }
    }
    Some(p)
}

pub struct History {
    points: VecDeque<Point>,
    dir: Option<PathBuf>,
    pending: Vec<String>,
    flushed: Instant,
}

impl History {
    /// `dir`: where the daily logs go (`None` keeps memory only).
    pub fn new(dir: Option<PathBuf>, now_ms: u64) -> Self {
        let mut h = History {
            points: VecDeque::new(),
            dir,
            pending: Vec::new(),
            flushed: Instant::now(),
        };
        if let Some(d) = h.dir.clone() {
            let _ = std::fs::create_dir_all(&d);
            h.prune(&d, now_ms);
            let since = now_ms.saturating_sub(KEEP.as_millis() as u64);
            for day in [now_ms.saturating_sub(86_400_000), now_ms] {
                if let Ok(text) = std::fs::read_to_string(d.join(format!("{}.csv", day_name(day))))
                {
                    h.points.extend(
                        text.lines()
                            .filter_map(parse_line)
                            .filter(|p| p.at >= since && p.at <= now_ms),
                    );
                }
            }
        }
        h
    }

    fn prune(&self, dir: &Path, now_ms: u64) {
        let oldest = day_name(now_ms.saturating_sub(KEEP_DAYS * 86_400_000));
        if let Ok(it) = std::fs::read_dir(dir) {
            for e in it.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".csv") && name.len() == 14 && name[..10] < *oldest {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    pub fn push(&mut self, p: Point) {
        let oldest = p.at.saturating_sub(KEEP.as_millis() as u64);
        while self.points.front().is_some_and(|x| x.at < oldest) {
            self.points.pop_front();
        }
        self.points.push_back(p);
        if self.dir.is_some() {
            self.pending.push(csv_line(&p));
            if self.flushed.elapsed() >= FLUSH_EVERY {
                self.flush();
            }
        }
    }

    /// Writes buffered lines to the day's log.
    pub fn flush(&mut self) {
        self.flushed = Instant::now();
        let Some(dir) = &self.dir else { return };
        let mut by_day: Vec<(String, Vec<String>)> = Vec::new();
        for line in self.pending.drain(..) {
            let at: u64 = line
                .split(',')
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let day = day_name(at);
            match by_day.last_mut() {
                Some((d, v)) if *d == day => v.push(line),
                _ => by_day.push((day, vec![line])),
            }
        }
        for (day, lines) in by_day {
            let path = dir.join(format!("{day}.csv"));
            let new = !path.exists();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                if new {
                    let _ = writeln!(f, "{HEADER}");
                }
                for l in lines {
                    let _ = writeln!(f, "{l}");
                }
            }
        }
    }

    pub fn latest(&self) -> Option<&Point> {
        self.points.back()
    }

    /// Readings since `since_ms`, averaged into at most `max` points.
    pub fn range(&self, since_ms: u64, max: usize) -> Vec<Point> {
        let pts: Vec<&Point> = self.points.iter().filter(|p| p.at >= since_ms).collect();
        let max = max.max(1);
        if pts.len() <= max {
            return pts.into_iter().copied().collect();
        }
        let per = pts.len().div_ceil(max);
        pts.chunks(per)
            .map(|c| {
                let mut out = Point::empty(c[c.len() / 2].at);
                for i in 0..FIELDS.len() {
                    let vals: Vec<f32> =
                        c.iter().map(|p| p.v[i]).filter(|v| v.is_finite()).collect();
                    if !vals.is_empty() {
                        out.v[i] = vals.iter().sum::<f32>() / vals.len() as f32;
                    }
                }
                out
            })
            .collect()
    }

    /// The readings since `since_ms` as a CSV file's text.
    pub fn export(&self, since_ms: u64) -> String {
        let mut s = String::from(HEADER);
        s.push('\n');
        for p in self.points.iter().filter(|p| p.at >= since_ms) {
            s.push_str(&csv_line(p));
            s.push('\n');
        }
        s
    }
}

impl Drop for History {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(day_name(0), "1970-01-01");
        // 2026-11-21 09:30:05 UTC
        let ms = 1_795_253_405_000;
        assert_eq!(time_name(ms), "2026-11-21T09:30:05Z");
        assert_eq!(day_name(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn lines_round_trip_and_gaps_stay_empty() {
        let mut p = Point::empty(1_795_253_405_000);
        p.set("cpu", Some(61.26));
        p.set("net_down", Some(1234.0));
        let line = csv_line(&p);
        assert!(line.starts_with("1795253405000,2026-11-21T09:30:05Z,,61.3,"));
        let back = parse_line(&line).unwrap();
        assert_eq!(back.get("cpu"), Some(61.3));
        assert_eq!(back.get("liquid"), None);
        assert_eq!(back.get("net_down"), Some(1234.0));
    }

    #[test]
    fn keeps_a_day_logs_to_disk_and_reloads() {
        let dir = std::env::temp_dir().join(format!("lumora-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = 1_795_253_405_000u64;
        {
            let mut h = History::new(Some(dir.clone()), now);
            for i in 0..100u64 {
                let mut p = Point::empty(now - (99 - i) * 5000);
                p.set("cpu", Some(i as f32));
                h.push(p);
            }
            // An old reading falls out of memory.
            assert_eq!(h.range(0, 1000).len(), 100);
            let r = h.range(0, 10);
            assert_eq!(r.len(), 10);
            assert!((r[0].get("cpu").unwrap() - 4.5).abs() < 0.01);
            assert!(h.export(now - 10_000).lines().count() == 4);
        }
        let h = History::new(Some(dir.clone()), now + 1000);
        assert_eq!(h.range(0, 1000).len(), 100, "reloaded from the log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nan_serializes_as_null() {
        let mut p = Point::empty(5);
        p.set("cpu", Some(40.04));
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(
            j,
            r#"{"at":5,"v":[null,40.0,null,null,null,null,null,null,null,null]}"#
        );
    }
}
