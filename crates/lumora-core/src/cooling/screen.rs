//! Pictures for a cooler's screen: colors drawn by Lumora or a picture the
//! user chose, turned into the pixel layout the screen takes.
//!
//! Kraken screens take one byte each of red, green and blue plus a zero byte
//! per pixel, rows top to bottom. They don't rotate uploaded pictures
//! themselves, so rotation is applied here.

use std::io;

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
pub fn pixels(s: &ScreenSettings, size: usize) -> io::Result<Option<Vec<u8>>> {
    let rgba = match s.mode {
        ScreenMode::Color => render_colors(&s.colors, size, size),
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
}
