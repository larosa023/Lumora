use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An 8-bit RGB color. Serialized as a `#rrggbb` string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Parses `#rgb`, `#rrggbb` (the `#` is optional).
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        let parse = |h: &str| u8::from_str_radix(h, 16).ok();
        match s.len() {
            6 => Some(Rgb::new(
                parse(&s[0..2])?,
                parse(&s[2..4])?,
                parse(&s[4..6])?,
            )),
            3 => {
                let c = |i: usize| parse(&s[i..i + 1]).map(|v| v * 17);
                Some(Rgb::new(c(0)?, c(1)?, c(2)?))
            }
            _ => None,
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// `h` in degrees (any value, wraps), `s` and `v` in 0..=1.
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Self {
        let h = h.rem_euclid(360.0) / 60.0;
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let c = v * s;
        let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        Rgb::from_f32(r + m, g + m, b + m)
    }

    pub fn from_f32(r: f32, g: f32, b: f32) -> Self {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        Rgb::new(q(r), q(g), q(b))
    }

    /// Linear blend: `t = 0` gives `self`, `t = 1` gives `other`.
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t + 0.5) as u8;
        Rgb::new(m(self.r, other.r), m(self.g, other.g), m(self.b, other.b))
    }

    pub fn scale(self, f: f32) -> Rgb {
        let f = f.clamp(0.0, 1.0);
        let m = |a: u8| (a as f32 * f + 0.5) as u8;
        Rgb::new(m(self.r), m(self.g), m(self.b))
    }

    /// Adds two colors with saturation (for layering glows).
    pub fn add(self, other: Rgb) -> Rgb {
        Rgb::new(
            self.r.saturating_add(other.r),
            self.g.saturating_add(other.g),
            self.b.saturating_add(other.b),
        )
    }

    /// The OpenRGB wire format: little-endian `r, g, b, 0`.
    pub fn to_openrgb(self) -> u32 {
        self.r as u32 | (self.g as u32) << 8 | (self.b as u32) << 16
    }

    pub fn from_openrgb(v: u32) -> Rgb {
        Rgb::new(v as u8, (v >> 8) as u8, (v >> 16) as u8)
    }
}

/// Samples a gradient built from `colors` at `t` in 0..=1.
/// With `wrap`, the last color blends back into the first so the gradient can loop.
pub fn sample(colors: &[Rgb], t: f32, wrap: bool) -> Rgb {
    match colors.len() {
        0 => Rgb::BLACK,
        1 => colors[0],
        n => {
            if wrap {
                let t = t.rem_euclid(1.0) * n as f32;
                let i = (t as usize) % n;
                colors[i].lerp(colors[(i + 1) % n], smooth(t.fract()))
            } else {
                let t = t.clamp(0.0, 1.0) * (n - 1) as f32;
                let i = (t as usize).min(n - 2);
                colors[i].lerp(colors[i + 1], t - i as f32)
            }
        }
    }
}

/// Smoothstep easing.
pub fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::from_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color `{s}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(Rgb::from_hex("#7c5cff"), Some(Rgb::new(0x7c, 0x5c, 0xff)));
        assert_eq!(Rgb::from_hex("fff"), Some(Rgb::WHITE));
        assert_eq!(Rgb::from_hex("#12"), None);
        assert_eq!(Rgb::new(1, 2, 255).to_hex(), "#0102ff");
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(Rgb::from_hsv(0.0, 1.0, 1.0), Rgb::new(255, 0, 0));
        assert_eq!(Rgb::from_hsv(120.0, 1.0, 1.0), Rgb::new(0, 255, 0));
        assert_eq!(Rgb::from_hsv(240.0, 1.0, 1.0), Rgb::new(0, 0, 255));
        assert_eq!(Rgb::from_hsv(360.0, 1.0, 1.0), Rgb::new(255, 0, 0));
    }

    #[test]
    fn wire_format() {
        let c = Rgb::new(0x11, 0x22, 0x33);
        assert_eq!(c.to_openrgb(), 0x00332211);
        assert_eq!(Rgb::from_openrgb(c.to_openrgb()), c);
    }

    #[test]
    fn gradient_ends() {
        let g = [Rgb::BLACK, Rgb::WHITE];
        assert_eq!(sample(&g, 0.0, false), Rgb::BLACK);
        assert_eq!(sample(&g, 1.0, false), Rgb::WHITE);
        assert_eq!(sample(&g, 0.0, true), Rgb::BLACK);
    }
}
