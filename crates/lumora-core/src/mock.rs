//! A tiny fake OpenRGB SDK server, used by the tests and by
//! `cargo run -p lumora-core --example mock_server` so the full app can be
//! exercised without any RGB hardware.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::color::Rgb;
use crate::openrgb::{packet, Controller, Mode, Zone, MAGIC, MODE_FLAG_HAS_PER_LED_COLOR, NO_LED, ZONE_LINEAR, ZONE_MATRIX, ZONE_SINGLE};

/// What the mock server has received, for assertions.
#[derive(Default, Debug)]
pub struct Received {
    pub client_name: Option<String>,
    pub custom_mode: Vec<u32>,
    pub leds: HashMap<u32, Vec<Rgb>>,
    pub led_updates: usize,
    pub resizes: usize,
    pub rescans: usize,
}

pub struct MockServer {
    pub port: u16,
    pub received: Arc<Mutex<Received>>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
}

impl MockServer {
    /// Starts listening on an ephemeral local port (or `port` if non-zero).
    pub fn start(port: u16, controllers: Vec<Controller>, protocol: u32) -> io::Result<MockServer> {
        let controllers = Arc::new(Mutex::new(controllers));
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        let received = Arc::new(Mutex::new(Received::default()));
        let clients = Arc::new(Mutex::new(Vec::new()));
        {
            let received = received.clone();
            let clients = clients.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    if let Ok(c) = stream.try_clone() {
                        clients.lock().unwrap().push(c);
                    }
                    let received = received.clone();
                    let controllers = controllers.clone();
                    let clients = clients.clone();
                    thread::spawn(move || {
                        let _ = serve(stream, &controllers, protocol, &received, &clients);
                    });
                }
            });
        }
        Ok(MockServer { port, received, clients })
    }

    /// Tells every connected client that the device list changed.
    pub fn notify_list_updated(&self) {
        for c in self.clients.lock().unwrap().iter_mut() {
            let _ = write_packet(c, 0, packet::DEVICE_LIST_UPDATED, &[]);
        }
    }
}

fn serve(
    mut s: TcpStream,
    controllers: &Mutex<Vec<Controller>>,
    server_protocol: u32,
    rec: &Mutex<Received>,
    clients: &Mutex<Vec<TcpStream>>,
) -> io::Result<()> {
    loop {
        let mut h = [0u8; 16];
        s.read_exact(&mut h)?;
        if &h[0..4] != MAGIC {
            return Ok(());
        }
        let le = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let (device, id, size) = (le(4), le(8), le(12) as usize);
        let mut data = vec![0; size];
        s.read_exact(&mut data)?;
        match id {
            packet::REQUEST_PROTOCOL_VERSION => {
                write_packet(&mut s, 0, id, &server_protocol.to_le_bytes())?;
            }
            packet::SET_CLIENT_NAME => {
                let name = String::from_utf8_lossy(data.strip_suffix(&[0]).unwrap_or(&data)).to_string();
                rec.lock().unwrap().client_name = Some(name);
            }
            packet::REQUEST_CONTROLLER_COUNT => {
                let n = controllers.lock().unwrap().len() as u32;
                write_packet(&mut s, 0, id, &n.to_le_bytes())?;
            }
            packet::REQUEST_CONTROLLER_DATA => {
                let p = if data.len() >= 4 { u32::from_le_bytes(data[..4].try_into().unwrap()) } else { 0 };
                let data = controllers.lock().unwrap().get(device as usize).map(|c| encode_controller(c, p.min(server_protocol)));
                if let Some(d) = data {
                    write_packet(&mut s, device, id, &d)?;
                }
            }
            packet::SET_CUSTOM_MODE => rec.lock().unwrap().custom_mode.push(device),
            packet::RESIZE_ZONE => {
                let zone = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
                let size = u32::from_le_bytes(data[4..8].try_into().unwrap());
                if let Some(c) = controllers.lock().unwrap().get_mut(device as usize) {
                    if let Some(z) = c.zones.get_mut(zone) {
                        z.leds_count = size.clamp(z.leds_min, z.leds_max);
                    }
                    let n: u32 = c.zones.iter().map(|z| z.leds_count).sum();
                    c.leds = (0..n).map(|i| format!("LED {}", i + 1)).collect();
                    c.colors = vec![Rgb::BLACK; n as usize];
                }
                rec.lock().unwrap().resizes += 1;
            }
            packet::REQUEST_RESCAN_DEVICES => {
                rec.lock().unwrap().rescans += 1;
                for c in clients.lock().unwrap().iter_mut() {
                    let _ = write_packet(c, 0, packet::DEVICE_LIST_UPDATED, &[]);
                }
            }
            packet::UPDATE_LEDS => {
                let n = u16::from_le_bytes([data[4], data[5]]) as usize;
                let colors = (0..n)
                    .map(|i| {
                        let o = 6 + i * 4;
                        Rgb::from_openrgb(u32::from_le_bytes(data[o..o + 4].try_into().unwrap()))
                    })
                    .collect();
                let mut r = rec.lock().unwrap();
                r.leds.insert(device, colors);
                r.led_updates += 1;
            }
            _ => {}
        }
    }
}

fn write_packet(s: &mut TcpStream, device: u32, id: u32, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(16 + data.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&device.to_le_bytes());
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    s.write_all(&buf)
}

/// Serializes a controller exactly the way the OpenRGB server does.
pub fn encode_controller(c: &Controller, protocol: u32) -> Vec<u8> {
    fn s(b: &mut Vec<u8>, v: &str) {
        b.extend_from_slice(&((v.len() + 1) as u16).to_le_bytes());
        b.extend_from_slice(v.as_bytes());
        b.push(0);
    }
    fn u(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let mut b = Vec::new();
    u(&mut b, 0); // size, patched below
    u(&mut b, c.kind as u32);
    s(&mut b, &c.name);
    if protocol >= 1 {
        s(&mut b, &c.vendor);
    }
    s(&mut b, &c.description);
    s(&mut b, &c.version);
    s(&mut b, &c.serial);
    s(&mut b, &c.location);
    b.extend_from_slice(&(c.modes.len() as u16).to_le_bytes());
    u(&mut b, c.active_mode as u32);
    for m in &c.modes {
        s(&mut b, &m.name);
        u(&mut b, m.value as u32);
        u(&mut b, m.flags);
        u(&mut b, 0); // speed min
        u(&mut b, 100); // speed max
        if protocol >= 3 {
            u(&mut b, 0);
            u(&mut b, 100);
        }
        u(&mut b, 0); // colors min
        u(&mut b, 1); // colors max
        u(&mut b, 50); // speed
        if protocol >= 3 {
            u(&mut b, 100);
        }
        u(&mut b, 0); // direction
        u(&mut b, 1); // color mode
        b.extend_from_slice(&1u16.to_le_bytes());
        u(&mut b, 0x00ff00ff);
    }
    b.extend_from_slice(&(c.zones.len() as u16).to_le_bytes());
    for z in &c.zones {
        s(&mut b, &z.name);
        u(&mut b, z.kind as u32);
        u(&mut b, z.leds_min);
        u(&mut b, z.leds_max);
        u(&mut b, z.leds_count);
        match &z.matrix {
            Some((h, w, map)) => {
                b.extend_from_slice(&((8 + 4 * map.len()) as u16).to_le_bytes());
                u(&mut b, *h);
                u(&mut b, *w);
                for v in map {
                    u(&mut b, *v);
                }
            }
            None => b.extend_from_slice(&0u16.to_le_bytes()),
        }
    }
    b.extend_from_slice(&(c.leds.len() as u16).to_le_bytes());
    for (i, l) in c.leds.iter().enumerate() {
        s(&mut b, l);
        u(&mut b, i as u32);
    }
    b.extend_from_slice(&(c.colors.len() as u16).to_le_bytes());
    for col in &c.colors {
        u(&mut b, col.to_openrgb());
    }
    let size = b.len() as u32;
    b[0..4].copy_from_slice(&size.to_le_bytes());
    b
}

fn controller(kind: i32, name: &str, vendor: &str, location: &str, zones: Vec<Zone>) -> Controller {
    let n: u32 = zones.iter().map(|z| z.leds_count).sum();
    Controller {
        kind,
        name: name.into(),
        vendor: vendor.into(),
        description: format!("{vendor} device"),
        version: "1.0".into(),
        serial: String::new(),
        location: location.into(),
        active_mode: 0,
        modes: vec![
            Mode { name: "Direct".into(), value: 0, flags: MODE_FLAG_HAS_PER_LED_COLOR },
            Mode { name: "Rainbow Wave".into(), value: 1, flags: 1 },
        ],
        zones,
        leds: (0..n).map(|i| format!("LED {}", i + 1)).collect(),
        colors: vec![Rgb::BLACK; n as usize],
    }
}

fn zone(name: &str, kind: i32, n: u32) -> Zone {
    Zone { name: name.into(), kind, leds_min: n, leds_max: n, leds_count: n, matrix: None }
}

/// An addressable header: empty until told how many LEDs are on it.
fn header(name: &str) -> Zone {
    Zone { name: name.into(), kind: ZONE_LINEAR, leds_min: 0, leds_max: 120, leds_count: 0, matrix: None }
}

/// A believable desktop: two RAM sticks, a GPU, an AIO with fans, a keyboard and a mouse.
pub fn sample_rig() -> Vec<Controller> {
    let (h, w) = (2u32, 5u32);
    let map: Vec<u32> = vec![0, 1, 2, 3, 4, 5, 6, NO_LED, 7, 8];
    vec![
        controller(1, "Vengeance RGB Pro", "Corsair", "I2C: SMBus 0x58", vec![zone("DRAM", ZONE_LINEAR, 10)]),
        controller(1, "Vengeance RGB Pro", "Corsair", "I2C: SMBus 0x59", vec![zone("DRAM", ZONE_LINEAR, 10)]),
        controller(2, "GeForce RTX 4070 Ti", "ASUS", "I2C: NVIDIA 0x67", vec![zone("Logo", ZONE_LINEAR, 14)]),
        controller(
            3,
            "Kraken Elite",
            "NZXT",
            "HID: /dev/hidraw3",
            vec![zone("Pump Ring", ZONE_LINEAR, 24), zone("Fan 1", ZONE_LINEAR, 8), zone("Fan 2", ZONE_LINEAR, 8)],
        ),
        controller(
            5,
            "Mini Board",
            "Lumora Labs",
            "HID: /dev/hidraw5",
            vec![Zone { name: "Keyboard".into(), kind: ZONE_MATRIX, leds_min: 9, leds_max: 9, leds_count: 9, matrix: Some((h, w, map)) }],
        ),
        controller(6, "Viper V3", "Razer", "HID: /dev/hidraw7", vec![zone("Logo", ZONE_SINGLE, 1), zone("Underglow", ZONE_LINEAR, 6)]),
        controller(
            0,
            "ROG STRIX B650E-F GAMING",
            "ASUS",
            "HID: /dev/hidraw9",
            vec![zone("Aura Mainboard", ZONE_LINEAR, 3), header("Aura Addressable 1"), header("Aura Addressable 2")],
        ),
    ]
}
