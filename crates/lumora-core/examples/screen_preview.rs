//! Writes Lumora's temperature screen to a PPM file, to check how it looks:
//! `cargo run -p lumora-core --example screen_preview -- out.ppm 36 "#ff3d8b" "#000000,#1a1040"`
use lumora_core::cooling::screen::render_temperature;
use lumora_core::cooling::{ScreenMode, ScreenSettings};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "screen.ppm".into());
    let value = args.get(2).and_then(|v| v.parse().ok());
    let s = ScreenSettings {
        mode: ScreenMode::Temperature,
        text_color: args.get(3).cloned().unwrap_or_else(|| "#ffffff".into()),
        background: args.get(4).map(|b| b.split(',').map(String::from).collect()).unwrap_or_else(|| vec!["#000000".into()]),
        source: args.get(5).cloned().unwrap_or_else(|| "liquid".into()),
        ..Default::default()
    };
    let size = 640;
    let img = render_temperature(&s, value, size);
    let mut ppm = format!("P6 {size} {size} 255\n").into_bytes();
    for px in img.chunks(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    std::fs::write(&out, ppm).unwrap();
}
