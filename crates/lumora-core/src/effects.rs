//! Lighting effects.
//!
//! Effects are pure functions of a shared clock plus a little per-device state
//! (for things like twinkles), so every device on every bus stays in phase.
//! The UI ships a matching JavaScript port for its browser-only dev preview;
//! keep `ui/js/effects.js` in step when changing the maths here.

use serde::{Deserialize, Serialize};

use crate::color::{sample, smooth, Rgb};
use crate::device::LedPos;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    #[default]
    Right,
    Up,
    Down,
    /// Toward the center.
    In,
    /// Away from the center.
    Out,
}

/// The user-facing settings of an effect. Every effect reads the same small
/// set of knobs; [`EffectDescriptor`] says which ones it actually uses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectConfig {
    pub effect: String,
    pub colors: Vec<Rgb>,
    /// 0..=100
    pub speed: u8,
    /// 0..=100: wavelength, tail length, density… depending on the effect.
    pub size: u8,
    pub direction: Direction,
}

impl Default for EffectConfig {
    fn default() -> Self {
        EffectConfig {
            effect: "color_wave".into(),
            colors: vec![Rgb::new(0x7c, 0x5c, 0xff), Rgb::new(0x00, 0xd4, 0xff), Rgb::new(0xff, 0x3d, 0xa8)],
            speed: 40,
            size: 50,
            direction: Direction::Right,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub min_colors: u8,
    pub max_colors: u8,
    pub uses_speed: bool,
    pub uses_size: bool,
    pub uses_direction: bool,
    /// Label for the size slider in this effect's context.
    pub size_label: &'static str,
    pub default_colors: Vec<Rgb>,
}

fn hex(s: &[&str]) -> Vec<Rgb> {
    s.iter().filter_map(|h| Rgb::from_hex(h)).collect()
}

#[allow(clippy::too_many_arguments)]
fn d(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    colors: (u8, u8),
    speed: bool,
    size: Option<&'static str>,
    direction: bool,
    default_colors: &[&str],
) -> EffectDescriptor {
    EffectDescriptor {
        id,
        name,
        description,
        category,
        min_colors: colors.0,
        max_colors: colors.1,
        uses_speed: speed,
        uses_size: size.is_some(),
        uses_direction: direction,
        size_label: size.unwrap_or(""),
        default_colors: hex(default_colors),
    }
}

/// Every built-in effect, in display order.
pub fn catalog() -> Vec<EffectDescriptor> {
    vec![
        d("static", "Static", "One solid color", "Solid", (1, 1), false, None, false, &["#7c5cff"]),
        d("gradient", "Gradient", "A still blend across your LEDs", "Solid", (2, 6), false, None, true, &["#ff3d7f", "#7c5cff", "#00d4ff"]),
        d("breathing", "Breathing", "Slow fade in and out, cycling colors", "Calm", (1, 6), true, None, false, &["#00d4ff", "#7c5cff"]),
        d("spectrum", "Spectrum", "Everything drifts through the rainbow", "Calm", (0, 0), true, None, false, &[]),
        d("aurora", "Aurora", "Soft, shifting northern lights", "Calm", (2, 6), true, Some("Scale"), false, &["#00ffa3", "#00b3ff", "#8a5cff", "#ff4fd8"]),
        d("rainbow_wave", "Rainbow", "The classic rolling rainbow", "Wave", (0, 0), true, Some("Wavelength"), true, &[]),
        d("color_wave", "Color Wave", "Your colors rolling across", "Wave", (2, 6), true, Some("Wavelength"), true, &["#7c5cff", "#00d4ff", "#ff3da8"]),
        d("ripple", "Ripple", "Rings pulsing out from the center", "Wave", (1, 6), true, Some("Ring width"), true, &["#00d4ff", "#ff3da8"]),
        d("comet", "Comet", "A bright head with a fading tail", "Motion", (1, 2), true, Some("Tail length"), true, &["#ffffff", "#2a0f5c"]),
        d("marquee", "Marquee", "Theater-style chasing dashes", "Motion", (1, 4), true, Some("Dash size"), true, &["#ffb000", "#ff3d7f"]),
        d("twinkle", "Starfield", "Random sparkles fading in and out", "Motion", (1, 6), true, Some("Density"), false, &["#ffffff", "#9ad7ff", "#c7a6ff"]),
        d("fire", "Fire", "Flickering flames", "Motion", (3, 3), true, Some("Flame height"), true, &["#ff1e00", "#ff8a00", "#ffe066"]),
    ]
}

pub fn descriptor(id: &str) -> Option<EffectDescriptor> {
    catalog().into_iter().find(|e| e.id == id)
}

/// Per-frame inputs.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Seconds since the engine started (shared by all devices).
    pub t: f32,
    /// Seconds since the previous frame.
    pub dt: f32,
}

pub trait Effect: Send {
    fn render(&mut self, frame: &Frame, leds: &[LedPos], out: &mut [Rgb]);
    /// Static effects are only re-sent when something changes.
    fn is_static(&self) -> bool {
        false
    }
}

/// Speed knob → rate multiplier (0.15× … 3×), exponential so the slider feels even.
pub fn speed_factor(speed: u8) -> f32 {
    0.15 * 20f32.powf(speed.min(100) as f32 / 100.0)
}

/// Size knob → scale multiplier (0.25× … 4×), 1× at the midpoint.
pub fn size_factor(size: u8) -> f32 {
    0.25 * 16f32.powf(size.min(100) as f32 / 100.0)
}

/// Position of an LED along the effect's direction, 0..=1.
pub fn along(p: LedPos, dir: Direction) -> f32 {
    let center = || (((p.x - 0.5).powi(2) + (p.y - 0.5).powi(2)).sqrt() / 0.7071).min(1.0);
    match dir {
        Direction::Right => p.x,
        Direction::Left => 1.0 - p.x,
        Direction::Down => p.y,
        Direction::Up => 1.0 - p.y,
        Direction::Out => center(),
        Direction::In => 1.0 - center(),
    }
}

/// Builds a renderer for `cfg`. Unknown effects fall back to static.
pub fn build(cfg: &EffectConfig, led_count: usize) -> Box<dyn Effect> {
    let mut cfg = cfg.clone();
    if let Some(desc) = descriptor(&cfg.effect) {
        if cfg.colors.len() < desc.min_colors as usize {
            let mut fill = desc.default_colors.clone();
            fill.retain(|c| !cfg.colors.contains(c));
            cfg.colors.extend(fill.into_iter().take(desc.min_colors as usize - cfg.colors.len()));
        }
        if desc.max_colors > 0 {
            cfg.colors.truncate(desc.max_colors as usize);
        }
    }
    if cfg.colors.is_empty() {
        cfg.colors.push(Rgb::WHITE);
    }
    let speed = speed_factor(cfg.speed);
    let size = size_factor(cfg.size);
    let dir = cfg.direction;
    let colors = cfg.colors;
    match cfg.effect.as_str() {
        "gradient" => Box::new(Gradient { colors, dir }),
        "breathing" => Box::new(Breathing { colors, speed }),
        "spectrum" => Box::new(Spectrum { speed }),
        "aurora" => Box::new(Aurora { colors, speed, size }),
        "rainbow_wave" => Box::new(RainbowWave { speed, size, dir }),
        "color_wave" => Box::new(ColorWave { colors, speed, size, dir }),
        "ripple" => Box::new(Ripple { colors, speed, size, dir }),
        "comet" => Box::new(Comet { colors, speed, size, dir }),
        "marquee" => Box::new(Marquee { colors, speed, size, dir }),
        "twinkle" => Box::new(Twinkle::new(colors, speed, size, led_count)),
        "fire" => Box::new(Fire { colors, speed, size, dir }),
        _ => Box::new(Static { color: colors[0] }),
    }
}

struct Static {
    color: Rgb,
}
impl Effect for Static {
    fn render(&mut self, _: &Frame, _: &[LedPos], out: &mut [Rgb]) {
        out.fill(self.color);
    }
    fn is_static(&self) -> bool {
        true
    }
}

struct Gradient {
    colors: Vec<Rgb>,
    dir: Direction,
}
impl Effect for Gradient {
    fn render(&mut self, _: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        for (o, p) in out.iter_mut().zip(leds) {
            *o = sample(&self.colors, along(*p, self.dir), false);
        }
    }
    fn is_static(&self) -> bool {
        true
    }
}

struct Breathing {
    colors: Vec<Rgb>,
    speed: f32,
}
impl Effect for Breathing {
    fn render(&mut self, f: &Frame, _: &[LedPos], out: &mut [Rgb]) {
        let phase = f.t * self.speed * 0.5;
        let i = (phase.floor() as i64).rem_euclid(self.colors.len() as i64) as usize;
        let level = (0.5 - 0.5 * (phase.fract() * std::f32::consts::TAU).cos()).powf(1.6);
        out.fill(self.colors[i].scale(level));
    }
}

struct Spectrum {
    speed: f32,
}
impl Effect for Spectrum {
    fn render(&mut self, f: &Frame, _: &[LedPos], out: &mut [Rgb]) {
        out.fill(Rgb::from_hsv(f.t * self.speed * 40.0, 1.0, 1.0));
    }
}

struct RainbowWave {
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for RainbowWave {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        let shift = f.t * self.speed * 0.5;
        for (o, p) in out.iter_mut().zip(leds) {
            let hue = (along(*p, self.dir) / self.size - shift) * 360.0;
            *o = Rgb::from_hsv(hue, 1.0, 1.0);
        }
    }
}

struct ColorWave {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for ColorWave {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        let shift = f.t * self.speed * 0.4;
        for (o, p) in out.iter_mut().zip(leds) {
            *o = sample(&self.colors, along(*p, self.dir) / self.size - shift, true);
        }
    }
}

struct Ripple {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for Ripple {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        // Ripples default to radiating outward; In/Out pick the sense.
        let dir = if self.dir == Direction::In { Direction::In } else { Direction::Out };
        let n = self.colors.len() as i64;
        for (o, p) in out.iter_mut().zip(leds) {
            let v = along(*p, dir) * 2.0 / self.size - f.t * self.speed * 0.8;
            let ring = v.floor() as i64;
            let w = (v.fract() * std::f32::consts::TAU).sin().max(0.0).powf(1.5);
            *o = self.colors[ring.rem_euclid(n) as usize].scale(w);
        }
    }
}

struct Comet {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for Comet {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        let head = (f.t * self.speed * 0.6).rem_euclid(1.0);
        let tail = (0.18 * self.size).clamp(0.04, 0.95);
        let bg = self.colors.get(1).copied().unwrap_or(Rgb::BLACK);
        for (o, p) in out.iter_mut().zip(leds) {
            let dist = (head - along(*p, self.dir)).rem_euclid(1.0);
            let k = if dist < tail { (1.0 - dist / tail).powi(2) } else { 0.0 };
            *o = bg.lerp(self.colors[0], k);
        }
    }
}

struct Marquee {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for Marquee {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        let dashes = (6.0 / self.size).max(1.0);
        let n = self.colors.len() as i64;
        for (o, p) in out.iter_mut().zip(leds) {
            let v = along(*p, self.dir) * dashes - f.t * self.speed * 1.5;
            let k = v.floor() as i64;
            let on = smooth(1.0 - ((v.fract() - 0.25).abs() * 4.0 - 0.6).max(0.0) * 2.5);
            *o = self.colors[k.rem_euclid(n) as usize].scale(on);
        }
    }
}

/// A tiny deterministic PRNG (xorshift32) so effects don't need a dependency.
#[derive(Clone)]
pub struct Rng(u32);
impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.max(1))
    }
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

struct Twinkle {
    colors: Vec<Rgb>,
    speed: f32,
    density: f32,
    level: Vec<f32>,
    rising: Vec<bool>,
    pick: Vec<usize>,
    rng: Rng,
}
impl Twinkle {
    fn new(colors: Vec<Rgb>, speed: f32, size: f32, n: usize) -> Self {
        Twinkle {
            colors,
            speed,
            density: size,
            level: vec![0.0; n],
            rising: vec![false; n],
            pick: vec![0; n],
            rng: Rng::new(0x9E37_79B9 ^ n as u32),
        }
    }
}
impl Effect for Twinkle {
    fn render(&mut self, f: &Frame, _: &[LedPos], out: &mut [Rgb]) {
        let rate = self.speed * 2.5;
        let spawn = (0.35 * self.density * self.speed * f.dt).min(1.0);
        for i in 0..out.len().min(self.level.len()) {
            if self.rising[i] {
                self.level[i] += rate * f.dt;
                if self.level[i] >= 1.0 {
                    self.level[i] = 1.0;
                    self.rising[i] = false;
                }
            } else if self.level[i] > 0.0 {
                self.level[i] = (self.level[i] - rate * 0.6 * f.dt).max(0.0);
            } else if self.rng.next_f32() < spawn {
                self.rising[i] = true;
                self.pick[i] = (self.rng.next_f32() * self.colors.len() as f32) as usize % self.colors.len();
            }
            out[i] = self.colors[self.pick[i]].scale(smooth(self.level[i]));
        }
    }
}

/// Smooth 2D value noise in roughly 0..=1.
pub fn noise(x: f32, y: f32) -> f32 {
    fn hash(x: i32, y: i32) -> f32 {
        let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1);
        h = (h ^ (h >> 15)).wrapping_mul(0x85eb_ca6b);
        h ^= h >> 13;
        (h & 0xffff) as f32 / 65535.0
    }
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (xf, yf) = (smooth(x - xi as f32), smooth(y - yi as f32));
    let a = hash(xi, yi) + (hash(xi + 1, yi) - hash(xi, yi)) * xf;
    let b = hash(xi, yi + 1) + (hash(xi + 1, yi + 1) - hash(xi, yi + 1)) * xf;
    a + (b - a) * yf
}

struct Aurora {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
}
impl Effect for Aurora {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        let t = f.t * self.speed * 0.35;
        for (o, p) in out.iter_mut().zip(leds) {
            let x = p.x / self.size * 2.0;
            let hue = noise(x + t * 0.6, p.y * 1.5 + t * 0.2) * 1.4 + t * 0.05;
            let glow = 0.35 + 0.65 * smooth(noise(x * 1.7 - t, p.y * 2.0 + 7.0));
            *o = sample(&self.colors, hue, true).scale(glow);
        }
    }
}

struct Fire {
    colors: Vec<Rgb>,
    speed: f32,
    size: f32,
    dir: Direction,
}
impl Effect for Fire {
    fn render(&mut self, f: &Frame, leds: &[LedPos], out: &mut [Rgb]) {
        // Flames rise *against* the chosen direction's travel: "Up" means the
        // base is at the bottom of the device.
        let t = f.t * self.speed * 1.6;
        let palette = [Rgb::BLACK, self.colors[0], self.colors[1 % self.colors.len()], self.colors[2 % self.colors.len()]];
        for (i, (o, p)) in out.iter_mut().zip(leds).enumerate() {
            let height = along(*p, self.dir);
            let across = if matches!(self.dir, Direction::Up | Direction::Down) { p.x } else { p.y };
            let flicker = noise(across * 6.0 + i as f32 * 0.37, t - height * 2.0);
            let flicker = flicker * 0.7 + 0.3 * noise(across * 13.0, t * 2.3);
            let reach = 0.35 + 0.45 * self.size.min(2.0);
            let heat = (flicker * 1.25 - height / reach).clamp(0.0, 1.0);
            *o = sample(&palette, heat, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(n: usize) -> Vec<LedPos> {
        (0..n).map(|i| LedPos { x: i as f32 / (n - 1) as f32, y: 0.5 }).collect()
    }

    #[test]
    fn every_effect_renders() {
        let leds = strip(30);
        for e in catalog() {
            let cfg = EffectConfig { effect: e.id.into(), colors: e.default_colors.clone(), ..Default::default() };
            let mut fx = build(&cfg, leds.len());
            let mut out = vec![Rgb::BLACK; leds.len()];
            for k in 0..60 {
                fx.render(&Frame { t: k as f32 / 30.0, dt: 1.0 / 30.0 }, &leds, &mut out);
            }
            assert_eq!(out.len(), 30, "{}", e.id);
        }
    }

    #[test]
    fn static_is_static() {
        let mut fx = build(&EffectConfig { effect: "static".into(), colors: vec![Rgb::new(1, 2, 3)], ..Default::default() }, 3);
        let mut out = vec![Rgb::BLACK; 3];
        fx.render(&Frame { t: 0.0, dt: 0.0 }, &strip(3), &mut out);
        assert!(fx.is_static());
        assert_eq!(out, vec![Rgb::new(1, 2, 3); 3]);
    }

    #[test]
    fn missing_colors_are_filled() {
        // Fire needs three colors; give it none and it should still render.
        let mut fx = build(&EffectConfig { effect: "fire".into(), colors: vec![], ..Default::default() }, 10);
        let mut out = vec![Rgb::BLACK; 10];
        fx.render(&Frame { t: 1.0, dt: 0.03 }, &strip(10), &mut out);
    }

    #[test]
    fn wave_moves() {
        let leds = strip(10);
        let mut fx = build(&EffectConfig { effect: "rainbow_wave".into(), ..Default::default() }, 10);
        let (mut a, mut b) = (vec![Rgb::BLACK; 10], vec![Rgb::BLACK; 10]);
        fx.render(&Frame { t: 0.0, dt: 0.0 }, &leds, &mut a);
        fx.render(&Frame { t: 0.5, dt: 0.5 }, &leds, &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn config_json_is_friendly() {
        let cfg: EffectConfig = serde_json::from_str(r##"{"effect":"comet","colors":["#ff0000"],"direction":"left"}"##).unwrap();
        assert_eq!(cfg.speed, EffectConfig::default().speed);
        assert_eq!(cfg.direction, Direction::Left);
        assert!(serde_json::to_string(&cfg).unwrap().contains("\"#ff0000\""));
    }
}
