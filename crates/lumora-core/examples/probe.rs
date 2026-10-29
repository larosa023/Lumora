//! Connects to an OpenRGB SDK server and lists what it reports:
//!   cargo run -p lumora-core --example probe -- [host] [port]
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".into());
    let port = args.next().and_then(|p| p.parse().ok()).unwrap_or(6742);
    // The server may still be starting; keep trying for a while.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut client = loop {
        match lumora_core::openrgb::Client::connect(&host, port, Duration::from_secs(1)) {
            Ok(c) => break c,
            Err(e) if Instant::now() < deadline => {
                eprintln!("waiting for {host}:{port} ({e})");
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("could not connect: {e}");
                std::process::exit(1);
            }
        }
    };
    println!("connected, protocol {}", client.protocol);
    match client.controllers() {
        Ok(list) => {
            println!("{} controllers", list.len());
            for c in list {
                println!("  {} {} ({} LEDs, direct: {})", c.vendor, c.name, c.led_count(), c.supports_direct());
            }
        }
        Err(e) => {
            eprintln!("could not read controllers: {e}");
            std::process::exit(1);
        }
    }
}
