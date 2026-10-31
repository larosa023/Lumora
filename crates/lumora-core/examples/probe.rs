//! Diagnoses a connection to an OpenRGB SDK server, step by step:
//!   cargo run -p lumora-core --example probe -- [host] [port]
//! First the raw handshake (hex dumps), then the normal client.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

fn packet(device: u32, id: u32, data: &[u8]) -> Vec<u8> {
    let mut b = b"ORGB".to_vec();
    for v in [device, id, data.len() as u32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(data);
    b
}

fn read_reply(s: &mut TcpStream) -> std::io::Result<(u32, u32, Vec<u8>)> {
    let mut h = [0u8; 16];
    s.read_exact(&mut h)?;
    let le = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let mut data = vec![0; le(12) as usize];
    s.read_exact(&mut data)?;
    println!(
        "  <- magic={:?} device={} id={} size={}",
        String::from_utf8_lossy(&h[..4]),
        le(4),
        le(8),
        data.len()
    );
    Ok((le(4), le(8), data))
}

fn hex(d: &[u8]) -> String {
    d.iter()
        .take(96)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(6742);

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut s = loop {
        match TcpStream::connect((host.as_str(), port)) {
            Ok(s) => break s,
            Err(e) if Instant::now() < deadline => {
                println!("waiting for {host}:{port} ({e})");
                std::thread::sleep(Duration::from_secs(3));
            }
            Err(e) => {
                println!("FAIL could not connect: {e}");
                std::process::exit(1);
            }
        }
    };
    println!("raw: connected");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&packet(0, 50, b"Lumora probe\0")).unwrap();
    println!("raw: -> SET_CLIENT_NAME");
    s.write_all(&packet(0, 40, &4u32.to_le_bytes())).unwrap();
    println!("raw: -> REQUEST_PROTOCOL_VERSION 4");
    match read_reply(&mut s) {
        Ok((_, _, d)) => println!("raw: protocol reply {}", hex(&d)),
        Err(e) => println!("raw: no protocol reply: {e}"),
    }
    s.write_all(&packet(0, 0, &[])).unwrap();
    println!("raw: -> REQUEST_CONTROLLER_COUNT");
    let count = match read_reply(&mut s) {
        Ok((_, _, d)) => {
            println!("raw: count reply {}", hex(&d));
            d.get(..4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .unwrap_or(0)
        }
        Err(e) => {
            println!("raw: no count reply: {e}");
            0
        }
    };
    for i in 0..count.min(3) {
        s.write_all(&packet(i, 1, &4u32.to_le_bytes())).unwrap();
        match read_reply(&mut s) {
            Ok((_, _, d)) => {
                println!("raw: controller {i} first bytes {}", hex(&d));
                match lumora_core::openrgb::parse_controller(&d, 4) {
                    Ok(c) => println!(
                        "raw: parsed {} {} ({} LEDs)",
                        c.vendor,
                        c.name,
                        c.led_count()
                    ),
                    Err(e) => println!("raw: PARSE ERROR {e}"),
                }
            }
            Err(e) => println!("raw: controller {i} no reply: {e}"),
        }
    }
    drop(s);

    println!("client: connecting");
    let mut client =
        match lumora_core::openrgb::Client::connect(&host, port, Duration::from_secs(2)) {
            Ok(c) => c,
            Err(e) => {
                println!("FAIL client connect: {e}");
                std::process::exit(1);
            }
        };
    println!("client: protocol {}", client.protocol);
    match client.controllers() {
        Ok(list) => {
            println!("OK {} controllers", list.len());
            for c in list {
                println!(
                    "  {} {} ({} LEDs, direct: {})",
                    c.vendor,
                    c.name,
                    c.led_count(),
                    c.supports_direct()
                );
            }
        }
        Err(e) => {
            println!("FAIL controllers: {e}");
            std::process::exit(1);
        }
    }
}
