//! Writes Lumora's temperature screen to a PPM file, to check how it looks:
//! `cargo run -p lumora-core --example screen_preview -- out.ppm 36 "#ff3d8b" "#000000,#1a1040"`
use lumora_core::cooling::screen::render_temperature;
use lumora_core::cooling::{ScreenMode, ScreenSettings};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "screen.ppm".into());
    // One or two readings: "36" or "52,61".
    let value: Vec<Option<i32>> = args.get(2).map(|v| v.split(',').map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let s = ScreenSettings {
        mode: ScreenMode::Temperature,
        text_color: args.get(3).cloned().unwrap_or_else(|| "#ffffff".into()),
        background: args.get(4).map(|b| b.split(',').map(String::from).collect()).unwrap_or_else(|| vec!["#000000".into()]),
        readings: args.get(5).map(|r| r.split(',').map(String::from).collect()).unwrap_or_else(|| vec!["liquid".into()]),
        ..Default::default()
    };
    let size = 640;
    let img = render_temperature(&s, &value, size);
    let mut ppm = format!("P6 {size} {size} 255\n").into_bytes();
    for px in img.chunks(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    std::fs::write(&out, ppm).unwrap();
}
