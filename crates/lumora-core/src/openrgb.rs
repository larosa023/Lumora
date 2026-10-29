//! A minimal client for the OpenRGB SDK network protocol.
//!
//! OpenRGB already speaks the vendor-specific protocols for hundreds of RGB
//! controllers. Its SDK server (default port 6742) exposes them all through one
//! simple binary protocol, which is what this module implements.
//!
//! Every packet starts with a 16-byte header: the magic `ORGB`, then the
//! device index, the packet id and the payload size (all little-endian `u32`).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::color::Rgb;

pub const DEFAULT_PORT: u16 = 6742;
pub const MAGIC: &[u8; 4] = b"ORGB";
/// Highest protocol version Lumora understands. The server talks to each
/// client at `min(client, server)`, so asking for 4 keeps the format stable
/// even against newer servers (v5 adds zone segments).
pub const CLIENT_PROTOCOL: u32 = 4;
const CLIENT_NAME: &str = "Lumora";

pub mod packet {
    pub const REQUEST_CONTROLLER_COUNT: u32 = 0;
    pub const REQUEST_CONTROLLER_DATA: u32 = 1;
    pub const REQUEST_PROTOCOL_VERSION: u32 = 40;
    pub const SET_CLIENT_NAME: u32 = 50;
    pub const DEVICE_LIST_UPDATED: u32 = 100;
    pub const UPDATE_LEDS: u32 = 1050;
    pub const UPDATE_ZONE_LEDS: u32 = 1051;
    pub const SET_CUSTOM_MODE: u32 = 1100;
}

pub const ZONE_SINGLE: i32 = 0;
pub const ZONE_LINEAR: i32 = 1;
pub const ZONE_MATRIX: i32 = 2;
/// Matrix cells without an LED.
pub const NO_LED: u32 = 0xFFFF_FFFF;
/// Mode flag: the mode accepts per-LED colors.
pub const MODE_FLAG_HAS_PER_LED_COLOR: u32 = 1 << 5;

#[derive(Debug, Clone, PartialEq)]
pub struct Controller {
    pub kind: i32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub version: String,
    pub serial: String,
    pub location: String,
    pub active_mode: i32,
    pub modes: Vec<Mode>,
    pub zones: Vec<Zone>,
    pub leds: Vec<String>,
    pub colors: Vec<Rgb>,
}

impl Controller {
    /// Whether the device can be driven LED-by-LED (a "Direct" style mode).
    pub fn supports_direct(&self) -> bool {
        self.modes.iter().any(|m| {
            let n = m.name.to_ascii_lowercase();
            n == "direct" || (m.flags & MODE_FLAG_HAS_PER_LED_COLOR != 0 && n.contains("custom"))
        })
    }

    pub fn led_count(&self) -> usize {
        self.zones.iter().map(|z| z.leds_count as usize).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub name: String,
    pub value: i32,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub name: String,
    pub kind: i32,
    pub leds_count: u32,
    /// `(height, width, map)` – map holds zone-relative LED indices or [`NO_LED`].
    pub matrix: Option<(u32, u32, Vec<u32>)>,
}

struct Packet {
    device: u32,
    id: u32,
    data: Vec<u8>,
}

/// A live connection to an OpenRGB SDK server.
pub struct Client {
    stream: TcpStream,
    rx: Receiver<Packet>,
    list_updated: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    pub protocol: u32,
    pub server: String,
}

impl Client {
    pub fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<Client> {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve host"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;

        let (tx, rx) = mpsc::channel();
        let list_updated = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        {
            let mut reader = stream.try_clone()?;
            let list_updated = list_updated.clone();
            let alive = alive.clone();
            thread::Builder::new()
                .name("openrgb-reader".into())
                .spawn(move || {
                    while let Ok(p) = read_packet(&mut reader) {
                        if p.id == packet::DEVICE_LIST_UPDATED {
                            list_updated.store(true, Ordering::SeqCst);
                        } else if tx.send(p).is_err() {
                            break;
                        }
                    }
                    alive.store(false, Ordering::SeqCst);
                })?;
        }

        let mut client = Client {
            stream,
            rx,
            list_updated,
            alive,
            protocol: 0,
            server: format!("{host}:{port}"),
        };

        let mut name = CLIENT_NAME.as_bytes().to_vec();
        name.push(0);
        client.send(0, packet::SET_CLIENT_NAME, &name)?;

        // Servers older than protocol 1 never answer this request.
        client.protocol = match client.request(
            0,
            packet::REQUEST_PROTOCOL_VERSION,
            &CLIENT_PROTOCOL.to_le_bytes(),
            Duration::from_millis(1500),
        ) {
            Ok(data) if data.len() >= 4 => {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]).min(CLIENT_PROTOCOL)
            }
            Ok(_) => 0,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(e),
        };
        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// True once after the server reports that devices were added or removed.
    pub fn take_list_updated(&self) -> bool {
        self.list_updated.swap(false, Ordering::SeqCst)
    }

    fn send(&mut self, device: u32, id: u32, data: &[u8]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(16 + data.len());
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&device.to_le_bytes());
        buf.extend_from_slice(&id.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(data);
        let r = self.stream.write_all(&buf);
        if r.is_err() {
            self.alive.store(false, Ordering::SeqCst);
        }
        r
    }

    fn request(&mut self, device: u32, id: u32, data: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        // Drop anything stale left from an earlier timed-out request.
        while self.rx.try_recv().is_ok() {}
        self.send(device, id, data)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(p) if p.id == id && p.device == device => return Ok(p.data),
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "OpenRGB did not reply"))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "OpenRGB closed the connection"))
                }
            }
        }
    }

    pub fn controller_count(&mut self) -> io::Result<u32> {
        let d = self.request(0, packet::REQUEST_CONTROLLER_COUNT, &[], Duration::from_secs(3))?;
        Reader::new(&d).u32()
    }

    pub fn controller(&mut self, index: u32) -> io::Result<Controller> {
        let payload = if self.protocol >= 1 { self.protocol.to_le_bytes().to_vec() } else { vec![] };
        // Slow buses (SMBus RAM) can take a moment to report.
        let d = self.request(index, packet::REQUEST_CONTROLLER_DATA, &payload, Duration::from_secs(10))?;
        parse_controller(&d, self.protocol)
    }

    pub fn controllers(&mut self) -> io::Result<Vec<Controller>> {
        let n = self.controller_count()?;
        (0..n).map(|i| self.controller(i)).collect()
    }

    /// Switches the controller into its direct/custom mode so it accepts LED updates.
    pub fn set_custom_mode(&mut self, index: u32) -> io::Result<()> {
        self.send(index, packet::SET_CUSTOM_MODE, &[])
    }

    pub fn update_leds(&mut self, index: u32, colors: &[Rgb]) -> io::Result<()> {
        self.send(index, packet::UPDATE_LEDS, &encode_leds(colors))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Payload for `UPDATE_LEDS`: size, count, then one `u32` per LED.
pub fn encode_leds(colors: &[Rgb]) -> Vec<u8> {
    let size = 4 + 2 + 4 * colors.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.extend_from_slice(&(colors.len() as u16).to_le_bytes());
    for c in colors {
        buf.extend_from_slice(&c.to_openrgb().to_le_bytes());
    }
    buf
}

fn read_packet(r: &mut impl Read) -> io::Result<Packet> {
    let mut h = [0u8; 16];
    r.read_exact(&mut h)?;
    if &h[0..4] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad packet magic"));
    }
    let le = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let (device, id, size) = (le(4), le(8), le(12) as usize);
    if size > 64 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "packet too large"));
    }
    let mut data = vec![0; size];
    r.read_exact(&mut data)?;
    Ok(Packet { device, id, data })
}

/// Bounds-checked little-endian reader.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated controller data"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> io::Result<i32> {
        Ok(self.u32()? as i32)
    }

    /// `u16` length (including the trailing NUL), then the bytes.
    fn string(&mut self) -> io::Result<String> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        let b = b.strip_suffix(&[0]).unwrap_or(b);
        Ok(String::from_utf8_lossy(b).trim().to_string())
    }
}

pub fn parse_controller(data: &[u8], protocol: u32) -> io::Result<Controller> {
    let mut r = Reader::new(data);
    let _size = r.u32()?;
    let kind = r.i32()?;
    let name = r.string()?;
    let vendor = if protocol >= 1 { r.string()? } else { String::new() };
    let description = r.string()?;
    let version = r.string()?;
    let serial = r.string()?;
    let location = r.string()?;

    let num_modes = r.u16()?;
    let active_mode = r.i32()?;
    let mut modes = Vec::with_capacity(num_modes as usize);
    for _ in 0..num_modes {
        let name = r.string()?;
        let value = r.i32()?;
        let flags = r.u32()?;
        let _speed_min = r.u32()?;
        let _speed_max = r.u32()?;
        if protocol >= 3 {
            let _brightness_min = r.u32()?;
            let _brightness_max = r.u32()?;
        }
        let _colors_min = r.u32()?;
        let _colors_max = r.u32()?;
        let _speed = r.u32()?;
        if protocol >= 3 {
            let _brightness = r.u32()?;
        }
        let _direction = r.u32()?;
        let _color_mode = r.u32()?;
        let n = r.u16()?;
        r.take(4 * n as usize)?;
        modes.push(Mode { name, value, flags });
    }

    let num_zones = r.u16()?;
    let mut zones = Vec::with_capacity(num_zones as usize);
    for _ in 0..num_zones {
        let name = r.string()?;
        let kind = r.i32()?;
        let _leds_min = r.u32()?;
        let _leds_max = r.u32()?;
        let leds_count = r.u32()?;
        let matrix_len = r.u16()? as usize;
        let matrix = if matrix_len >= 8 {
            let h = r.u32()?;
            let w = r.u32()?;
            let cells = (matrix_len - 8) / 4;
            let mut map = Vec::with_capacity(cells);
            for _ in 0..cells {
                map.push(r.u32()?);
            }
            (h as usize * w as usize == cells && cells > 0).then_some((h, w, map))
        } else {
            r.take(matrix_len)?;
            None
        };
        zones.push(Zone { name, kind, leds_count, matrix });
    }

    let num_leds = r.u16()?;
    let mut leds = Vec::with_capacity(num_leds as usize);
    for _ in 0..num_leds {
        leds.push(r.string()?);
        let _value = r.u32()?;
    }

    let num_colors = r.u16()?;
    let mut colors = Vec::with_capacity(num_colors as usize);
    for _ in 0..num_colors {
        colors.push(Rgb::from_openrgb(r.u32()?));
    }

    Ok(Controller {
        kind,
        name,
        vendor,
        description,
        version,
        serial,
        location,
        active_mode,
        modes,
        zones,
        leds,
        colors,
    })
}
