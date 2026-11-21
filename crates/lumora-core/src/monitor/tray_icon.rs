//! The live tray icon: a number (a temperature or a percentage) drawn on a
//! small rounded tile, in a color that warms up as the number does.

use crate::cooling::screen::{draw_text, text_width};

pub const SIZE: usize = 32;

/// Green → amber → red as a reading passes `warm` and `hot`.
pub fn color_for(value: f32, warm: f32, hot: f32) -> [u8; 3] {
    if value >= hot {
        [255, 92, 92]
    } else if value >= warm {
        [255, 184, 77]
    } else {
        [120, 230, 160]
    }
}

/// RGBA pixels, `SIZE` square.
pub fn render(text: &str, color: [u8; 3]) -> Vec<u8> {
    let n = SIZE;
    let mut img = vec![0u8; n * n * 4];
    // A rounded dark tile so the number reads on light and dark taskbars.
    let r = 7.0f32;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, n as f32 - r);
            let cy = fy.clamp(r, n as f32 - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a > 0.0 {
                let o = (y * n + x) * 4;
                img[o..o + 4].copy_from_slice(&[22, 19, 31, (a * 255.0) as u8]);
            }
        }
    }
    let gap = 0.16;
    let mut h = 20.0f32;
    while h > 8.0 && text_width(text, h, gap) > n as f32 - 5.0 {
        h -= 1.0;
    }
    let w = text_width(text, h, gap);
    draw_text(
        &mut img,
        n,
        text,
        (n as f32 - w) / 2.0,
        (n as f32 - h) / 2.0,
        h,
        gap,
        color,
        1.0,
    );
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_a_readable_number() {
        let img = render("64", [255, 255, 255]);
        assert_eq!(img.len(), SIZE * SIZE * 4);
        let lit = img.chunks(4).filter(|p| p[0] > 200 && p[3] == 255).count();
        assert!(lit > 60, "digits drawn ({lit} px)");
        assert_eq!(img[3], 0, "corner stays transparent");
        let wide = render("100", [255, 255, 255]);
        assert!(wide.chunks(4).filter(|p| p[0] > 200).count() > 40);
        assert_eq!(color_for(50.0, 70.0, 85.0), [120, 230, 160]);
        assert_eq!(color_for(90.0, 70.0, 85.0), [255, 92, 92]);
    }
}
