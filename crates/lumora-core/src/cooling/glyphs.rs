//! A tiny stroke font for the screen: digits, a few capitals, `°`, `-`.
//!
//! Each glyph is a set of polylines in a box 0.6 wide and 1.0 tall (y down),
//! drawn with round, anti-aliased strokes.

use std::f32::consts::PI;

pub type Polyline = Vec<(f32, f32)>;

pub const WIDTH: f32 = 0.6;

/// Points along an ellipse from `a0` to `a1` degrees (0° = right, 90° = down).
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) -> Polyline {
    let steps = (((a1 - a0).abs() / 6.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / steps as f32) * PI / 180.0;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn line(p: &[(f32, f32)]) -> Polyline {
    p.to_vec()
}

fn join(mut a: Polyline, b: Polyline) -> Polyline {
    a.extend(b);
    a
}

/// The strokes of `c`, and how wide it is.
pub fn glyph(c: char) -> (Vec<Polyline>, f32) {
    let w = WIDTH;
    let strokes = match c {
        '0' => vec![arc(0.3, 0.5, 0.3, 0.5, 0.0, 360.0)],
        '1' => vec![line(&[(0.1, 0.18), (0.34, 0.0), (0.34, 1.0)])],
        '2' => vec![join(arc(0.3, 0.29, 0.29, 0.29, 195.0, 360.0 + 35.0), line(&[(0.0, 1.0), (0.6, 1.0)]))],
        '3' => vec![arc(0.3, 0.26, 0.27, 0.26, 200.0, 450.0), arc(0.3, 0.74, 0.3, 0.26, 270.0, 520.0)],
        '4' => vec![line(&[(0.46, 1.0), (0.46, 0.0), (0.0, 0.7), (0.6, 0.7)])],
        '5' => vec![join(line(&[(0.56, 0.0), (0.1, 0.0), (0.06, 0.45)]), arc(0.3, 0.68, 0.3, 0.32, 230.0, 480.0))],
        '6' => vec![line(&[(0.48, 0.0), (0.06, 0.58)]), arc(0.3, 0.7, 0.29, 0.3, 0.0, 360.0)],
        '7' => vec![line(&[(0.0, 0.0), (0.6, 0.0), (0.2, 1.0)])],
        '8' => vec![arc(0.3, 0.235, 0.24, 0.235, 0.0, 360.0), arc(0.3, 0.735, 0.3, 0.265, 0.0, 360.0)],
        '9' => vec![arc(0.3, 0.3, 0.29, 0.3, 0.0, 360.0), line(&[(0.54, 0.42), (0.12, 1.0)])],
        '-' => vec![line(&[(0.1, 0.55), (0.5, 0.55)])],
        '°' => return (vec![arc(0.14, 0.14, 0.13, 0.13, 0.0, 360.0)], 0.3),
        'C' => vec![arc(0.32, 0.5, 0.32, 0.5, 40.0, 320.0)],
        'D' => vec![join(line(&[(0.08, 0.0), (0.0, 0.0), (0.0, 1.0), (0.08, 1.0)]), arc(0.08, 0.5, 0.5, 0.5, 90.0, -90.0))],
        'G' => vec![join(line(&[(0.36, 0.56), (0.635, 0.56)]), arc(0.32, 0.5, 0.32, 0.5, 10.0, 320.0))],
        'I' => return (vec![line(&[(0.05, 0.0), (0.05, 1.0)])], 0.1),
        'L' => vec![line(&[(0.0, 0.0), (0.0, 1.0), (0.5, 1.0)])],
        'P' => vec![join(line(&[(0.0, 1.0), (0.0, 0.0), (0.28, 0.0)]), join(arc(0.28, 0.27, 0.27, 0.27, -90.0, 90.0), line(&[(0.0, 0.54)])))],
        'Q' => vec![arc(0.3, 0.5, 0.3, 0.5, 0.0, 360.0), line(&[(0.38, 0.74), (0.62, 1.04)])],
        'U' => vec![join(line(&[(0.0, 0.0), (0.0, 0.7)]), join(arc(0.3, 0.7, 0.3, 0.3, 180.0, 0.0), line(&[(0.6, 0.0)])))],
        _ => vec![],
    };
    (strokes, w)
}
