//! Virtual devices shown while no OpenRGB server is reachable, so the app is
//! never an empty screen and effects can be tried out before any setup.

use crate::device::{DeviceInfo, DeviceKind, Matrix, ZoneInfo, ZoneKind};

fn linear(zones: &[(&str, usize)]) -> Vec<ZoneInfo> {
    let mut start = 0;
    zones
        .iter()
        .map(|&(name, count)| {
            let kind = if count == 1 {
                ZoneKind::Single
            } else {
                ZoneKind::Linear
            };
            let z = ZoneInfo {
                name: name.into(),
                kind,
                start,
                count,
                resizable: None,
                matrix: None,
            };
            start += count;
            z
        })
        .collect()
}

fn device(
    id: &str,
    name: &str,
    vendor: &str,
    kind: DeviceKind,
    zones: Vec<ZoneInfo>,
) -> DeviceInfo {
    DeviceInfo {
        id: format!("demo:{id}"),
        name: name.into(),
        vendor: vendor.into(),
        kind,
        location: "Demo device".into(),
        led_count: zones.iter().map(|z| z.count).sum(),
        zones,
        controllable: true,
        virtual_device: true,
        managed: true,
    }
}

/// A 75% keyboard: 6 rows × 16 columns, gaps where wide keys sit.
fn keyboard_75() -> ZoneInfo {
    // 1 = key, 0 = gap (the key to its left is wider).
    const ROWS: [&str; 6] = [
        "1111111111111111",
        "1111111111111011",
        "1111111111111111",
        "1111111111110101",
        "1111111111110111",
        "1110001001111111",
    ];
    let mut map = Vec::with_capacity(96);
    let mut n = 0;
    for row in ROWS {
        for c in row.chars() {
            if c == '1' {
                map.push(Some(n));
                n += 1;
            } else {
                map.push(None);
            }
        }
    }
    ZoneInfo {
        name: "Keyboard".into(),
        kind: ZoneKind::Matrix,
        start: 0,
        count: n,
        resizable: None,
        matrix: Some(Matrix {
            width: 16,
            height: 6,
            map,
        }),
    }
}

pub fn devices() -> Vec<DeviceInfo> {
    vec![
        device(
            "kbd",
            "Demo Keyboard 75%",
            "Lumora",
            DeviceKind::Keyboard,
            vec![keyboard_75()],
        ),
        device(
            "mouse",
            "Demo Mouse",
            "Lumora",
            DeviceKind::Mouse,
            linear(&[("Logo", 1), ("Scroll Wheel", 1), ("Underglow", 10)]),
        ),
        device(
            "ram1",
            "Demo DDR5 Stick 1",
            "Lumora",
            DeviceKind::Ram,
            linear(&[("DRAM", 12)]),
        ),
        device(
            "ram2",
            "Demo DDR5 Stick 2",
            "Lumora",
            DeviceKind::Ram,
            linear(&[("DRAM", 12)]),
        ),
        device(
            "gpu",
            "Demo Graphics Card",
            "Lumora",
            DeviceKind::Gpu,
            linear(&[("Light Bar", 18)]),
        ),
        device(
            "aio",
            "Demo AIO Cooler",
            "Lumora",
            DeviceKind::Cooler,
            linear(&[("Pump", 16), ("Fan 1", 12), ("Fan 2", 12), ("Fan 3", 12)]),
        ),
        device(
            "strip",
            "Demo Case Strip",
            "Lumora",
            DeviceKind::LedStrip,
            linear(&[("Strip", 40)]),
        ),
    ]
}
