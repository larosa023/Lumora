use serde::Serialize;

use crate::openrgb::{self, Controller, NO_LED};

/// What a device is, as shown in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Motherboard,
    Ram,
    Gpu,
    Cooler,
    LedStrip,
    Keyboard,
    Mouse,
    Mousemat,
    Headset,
    HeadsetStand,
    Gamepad,
    Light,
    Speaker,
    Storage,
    Case,
    Microphone,
    Accessory,
    Keypad,
    Other,
}

impl DeviceKind {
    /// Maps OpenRGB's `device_type` enum.
    pub fn from_openrgb(v: i32) -> Self {
        use DeviceKind::*;
        match v {
            0 => Motherboard,
            1 => Ram,
            2 => Gpu,
            3 => Cooler,
            4 => LedStrip,
            5 => Keyboard,
            6 => Mouse,
            7 => Mousemat,
            8 => Headset,
            9 => HeadsetStand,
            10 => Gamepad,
            11 => Light,
            12 => Speaker,
            14 => Storage,
            15 => Case,
            16 => Microphone,
            17 => Accessory,
            18 => Keypad,
            _ => Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    Single,
    Linear,
    Matrix,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ZoneInfo {
    pub name: String,
    pub kind: ZoneKind,
    /// Index of this zone's first LED in the device's LED array.
    pub start: usize,
    pub count: usize,
    /// Matrix zones: `width`, `height` and a row-major map of zone-relative LED
    /// indices (`null` for empty cells).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Matrix>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Matrix {
    pub width: usize,
    pub height: usize,
    pub map: Vec<Option<usize>>,
}

/// Everything the UI needs to know about a device.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceInfo {
    /// Stable across restarts (built from name, location and serial), used for
    /// per-device settings and profiles.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub kind: DeviceKind,
    pub location: String,
    pub led_count: usize,
    pub zones: Vec<ZoneInfo>,
    /// `false` when the device has no direct/per-LED mode, so Lumora can't animate it.
    pub controllable: bool,
    pub virtual_device: bool,
}

/// Where an LED sits, normalized to 0..=1 within its device.
///
/// For strips and rings, `x` runs along the zone. For matrices it's the real
/// column/row, so waves sweep across a keyboard the way you'd expect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LedPos {
    pub x: f32,
    pub y: f32,
}

impl DeviceInfo {
    pub fn from_controller(c: &Controller, id: String) -> Self {
        let mut start = 0;
        let zones = c
            .zones
            .iter()
            .map(|z| {
                let count = z.leds_count as usize;
                let kind = match z.kind {
                    openrgb::ZONE_SINGLE => ZoneKind::Single,
                    openrgb::ZONE_MATRIX if z.matrix.is_some() => ZoneKind::Matrix,
                    _ => ZoneKind::Linear,
                };
                let matrix = z.matrix.as_ref().filter(|_| kind == ZoneKind::Matrix).map(|(h, w, map)| Matrix {
                    width: *w as usize,
                    height: *h as usize,
                    map: map
                        .iter()
                        .map(|&v| (v != NO_LED && (v as usize) < count).then_some(v as usize))
                        .collect(),
                });
                let zi = ZoneInfo { name: z.name.clone(), kind, start, count, matrix };
                start += count;
                zi
            })
            .collect();
        let mut name = c.name.clone();
        if !c.vendor.is_empty() && name.to_lowercase().starts_with(&c.vendor.to_lowercase()) {
            name = name[c.vendor.len()..].trim().to_string();
            if name.is_empty() {
                name = c.name.clone();
            }
        }
        DeviceInfo {
            id,
            name,
            vendor: c.vendor.clone(),
            kind: DeviceKind::from_openrgb(c.kind),
            location: c.location.clone(),
            led_count: start,
            zones,
            controllable: c.supports_direct() && start > 0,
            virtual_device: false,
        }
    }

    /// Normalized positions for every LED.
    pub fn led_positions(&self) -> Vec<LedPos> {
        let mut out = vec![LedPos { x: 0.5, y: 0.5 }; self.led_count];
        let linear_zones = self.zones.iter().filter(|z| z.kind != ZoneKind::Matrix).count().max(1);
        let mut row = 0;
        for z in &self.zones {
            match (&z.matrix, z.kind) {
                (Some(m), ZoneKind::Matrix) => {
                    let fx = |c: usize| if m.width > 1 { c as f32 / (m.width - 1) as f32 } else { 0.5 };
                    let fy = |r: usize| if m.height > 1 { r as f32 / (m.height - 1) as f32 } else { 0.5 };
                    for (cell, led) in m.map.iter().enumerate() {
                        if let Some(i) = led {
                            if let Some(p) = out.get_mut(z.start + i) {
                                *p = LedPos { x: fx(cell % m.width), y: fy(cell / m.width) };
                            }
                        }
                    }
                }
                _ => {
                    let y = if linear_zones > 1 { row as f32 / (linear_zones - 1) as f32 } else { 0.5 };
                    for i in 0..z.count {
                        let x = if z.count > 1 { i as f32 / (z.count - 1) as f32 } else { 0.5 };
                        out[z.start + i] = LedPos { x, y };
                    }
                    row += 1;
                }
            }
        }
        out
    }
}

/// Builds stable ids for a set of controllers, de-duplicating identical ones.
pub fn stable_ids(controllers: &[Controller]) -> Vec<String> {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    controllers
        .iter()
        .map(|c| {
            let base = format!("orgb:{}|{}|{}|{}", c.vendor, c.name, c.location, c.serial);
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base}#{n}")
            }
        })
        .collect()
}
