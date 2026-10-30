//! Pictures for a cooler's screen: colors drawn by Lumora or a picture the
//! user chose, turned into the pixel layout the screen takes.
//!
//! Kraken screens take one byte each of red, green and blue plus a zero byte
//! per pixel, rows top to bottom. They don't rotate uploaded pictures
//! themselves, so rotation is applied here.

use std::io;

use super::glyphs;
use super::{ScreenMode, ScreenSettings};

fn parse_hex(c: &str) -> [u8; 3] {
    let h = c.trim_start_matches('#');
    let v = u32::from_str_radix(h.get(..6).unwrap_or("000000"), 16).unwrap_or(0);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// RGBA pixels (alpha ignored) for the Color mode: one color, or a soft
/// diagonal blend through up to three.
pub fn render_colors(colors: &[String], w: usize, h: usize) -> Vec<u8> {
    let stops: Vec<[f32; 3]> = colors.iter().take(3).map(|c| parse_hex(c).map(|v| v as f32)).collect();
    let stops = if stops.is_empty() { vec![[124.0, 92.0, 255.0]] } else { stops };
    let mut out = vec![0u8; w * h * 4];
    let span = (w + h).saturating_sub(2).max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let t = (x + y) as f32 / span;
            let c = if stops.len() == 1 {
                stops[0]
            } else {
                let u = t * (stops.len() - 1) as f32;
                let i = (u.floor() as usize).min(stops.len() - 2);
                let f = u - i as f32;
                let f = f * f * (3.0 - 2.0 * f);
                let (a, b) = (stops[i], stops[i + 1]);
                [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
            };
            let o = (y * w + x) * 4;
            out[o] = c[0].round() as u8;
            out[o + 1] = c[1].round() as u8;
            out[o + 2] = c[2].round() as u8;
            out[o + 3] = 255;
        }
    }
    out
}

/// Draws `text` onto RGBA `img` (`size` wide), its top-left at `(x, y)`,
/// `h` pixels tall, in `color`.
fn draw_text(img: &mut [u8], size: usize, text: &str, x: f32, y: f32, h: f32, gap: f32, color: [u8; 3], alpha: f32) {
    let stroke = (h * 0.13).max(1.5);
    let mut segs: Vec<((f32, f32), (f32, f32))> = Vec::new();
    let mut pen = x;
    for c in text.chars() {
        let (strokes, w) = glyphs::glyph(c);
        for pl in strokes {
            for pair in pl.windows(2) {
                let p = |q: (f32, f32)| (pen + q.0 * h, y + q.1 * h);
                segs.push((p(pair[0]), p(pair[1])));
            }
        }
        pen += (w + gap) * h;
    }
    if segs.is_empty() {
        return;
    }
    let half = stroke / 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &((ax, ay), (bx, by)) in &segs {
        x0 = x0.min(ax.min(bx));
        y0 = y0.min(ay.min(by));
        x1 = x1.max(ax.max(bx));
        y1 = y1.max(ay.max(by));
    }
    let clamp = |v: f32| (v.max(0.0) as usize).min(size.saturating_sub(1));
    for py in clamp(y0 - half - 1.0)..=clamp(y1 + half + 1.0) {
        for px in clamp(x0 - half - 1.0)..=clamp(x1 + half + 1.0) {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let mut d = f32::MAX;
            for &((ax, ay), (bx, by)) in &segs {
                let (vx, vy) = (bx - ax, by - ay);
                let len = vx * vx + vy * vy;
                let t = if len > 0.0 { (((fx - ax) * vx + (fy - ay) * vy) / len).clamp(0.0, 1.0) } else { 0.0 };
                let (dx, dy) = (fx - (ax + t * vx), fy - (ay + t * vy));
                d = d.min(dx * dx + dy * dy);
            }
            let cover = (half - d.sqrt() + 0.5).clamp(0.0, 1.0) * alpha;
            if cover > 0.0 {
                let o = (py * size + px) * 4;
                for k in 0..3 {
                    img[o + k] = (img[o + k] as f32 * (1.0 - cover) + color[k] as f32 * cover).round() as u8;
                }
            }
        }
    }
}

/// Width of `text` drawn `h` pixels tall.
fn text_width(text: &str, h: f32, gap: f32) -> f32 {
    let n = text.chars().count() as f32;
    text.chars().map(|c| glyphs::glyph(c).1).sum::<f32>() * h + gap * h * (n - 1.0).max(0.0)
}

fn label(source: &str) -> &'static str {
    match source {
        "gpu" => "GPU",
        "cpu" => "CPU",
        _ => "LIQUID",
    }
}

/// Draws the readings (one big, or two stacked) in the text color.
pub fn draw_readings(img: &mut [u8], s: &ScreenSettings, values: &[Option<i32>], size: usize) {
    let color = parse_hex(&s.text_color);
    let f = size as f32;
    let sources = s.sources();
    let n = sources.len().clamp(1, 2);
    let (h, lh) = if n == 1 { (f * 0.30, f * 0.055) } else { (f * 0.19, f * 0.042) };
    let gap = 0.22;
    let block = h + lh * 2.2;
    let spacing = f * 0.05;
    let total = block * n as f32 + spacing * (n as f32 - 1.0);
    let mut top = (f - total) / 2.0;
    for (i, src) in sources.iter().take(n).enumerate() {
        let digits = values.get(i).copied().flatten().map_or("--".to_string(), |v| v.clamp(-99, 199).to_string());
        let w = text_width(&digits, h, gap);
        let deg_h = h * 0.32;
        let width = w + deg_h * 0.35 + text_width("°C", deg_h, 0.18);
        let x = (f - width) / 2.0;
        draw_text(img, size, &digits, x, top, h, gap, color, 1.0);
        draw_text(img, size, "°C", x + w + deg_h * 0.35, top + h * 0.02, deg_h, 0.18, color, 0.85);
        let text = label(src);
        let lw = text_width(text, lh, 0.55);
        draw_text(img, size, text, (f - lw) / 2.0, top + h + lh * 0.9, lh, 0.55, color, 0.7);
        top += block + spacing;
    }
}

/// Lumora's own temperature screen: readings over the background colors.
pub fn render_temperature(s: &ScreenSettings, values: &[Option<i32>], size: usize) -> Vec<u8> {
    let mut img = render_colors(&s.background, size, size);
    draw_readings(&mut img, s, values, size);
    img
}

/// Turns RGBA pixels into the screen's layout, rotated clockwise by
/// `rotation` degrees (0, 90, 180 or 270). The picture must be square.
pub fn to_device(rgba: &[u8], size: usize, rotation: u16) -> Vec<u8> {
    let mut out = vec![0u8; size * size * 4];
    let turns = (rotation / 90) % 4;
    for y in 0..size {
        for x in 0..size {
            // Where this output pixel comes from in the source.
            let (sx, sy) = match turns {
                0 => (x, y),
                1 => (y, size - 1 - x),
                2 => (size - 1 - x, size - 1 - y),
                _ => (size - 1 - y, x),
            };
            let i = (sy * size + sx) * 4;
            let o = (y * size + x) * 4;
            out[o..o + 3].copy_from_slice(&rgba[i..i + 3]);
            out[o + 3] = 0;
        }
    }
    out
}

/// The pixels to upload for these settings, or `None` when the screen shows
/// something built in.
pub fn pixels(s: &ScreenSettings, size: usize, values: &[Option<i32>]) -> io::Result<Option<Vec<u8>>> {
    let mut rgba = match s.mode {
        ScreenMode::Color => render_colors(&s.colors, size, size),
        ScreenMode::Temperature => render_colors(&s.background, size, size),
        ScreenMode::Image => {
            let path = s.image.as_deref().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no picture chosen"))?;
            let data = std::fs::read(path)?;
            if data.len() != size * size * 4 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "the saved picture doesn't match this screen; choose it again"));
            }
            data
        }
        ScreenMode::Liquid | ScreenMode::Unchanged => return Ok(None),
    };
    if s.shows_readings() {
        draw_readings(&mut rgba, s, values, size);
    }
    Ok(Some(to_device(&rgba, size, s.rotation)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_blend_corner_to_corner() {
        let px = render_colors(&["#ff0000".into(), "#0000ff".into()], 4, 4);
        assert_eq!(&px[0..3], &[255, 0, 0]);
        let last = (4 * 4 - 1) * 4;
        assert_eq!(&px[last..last + 3], &[0, 0, 255]);
    }

    #[test]
    fn rotation_turns_clockwise() {
        // 2x2: top-left red, top-right green, bottom-left blue, bottom-right white.
        let src = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        let r = to_device(&src, 2, 90);
        // Turned clockwise, blue moves to the top-left and red to the top-right.
        assert_eq!(&r[0..4], &[0, 0, 255, 0]);
        assert_eq!(&r[4..8], &[255, 0, 0, 0]);
        let same = to_device(&src, 2, 0);
        assert_eq!(&same[0..4], &[255, 0, 0, 0]);
        assert_eq!(to_device(&to_device(&src, 2, 180), 2, 180)[12..15], [255, 255, 255]);
    }

    #[test]
    fn temperature_screen_draws_digits_in_the_text_color() {
        let s = ScreenSettings {
            mode: ScreenMode::Temperature,
            text_color: "#00ff00".into(),
            background: vec!["#000000".into()],
            ..Default::default()
        };
        let img = render_temperature(&s, &[Some(36)], 200);
        let green = img.chunks(4).filter(|p| p[1] > 200 && p[0] < 40).count();
        assert!(green > 400, "digits drawn ({green} px)");
        // Corners stay background.
        assert_eq!(&img[0..3], &[0, 0, 0]);
        // The middle column of a "36" is between the digits, mostly empty.
        let blank = render_temperature(&s, &[None], 200);
        assert_ne!(img, blank);
    }
}
