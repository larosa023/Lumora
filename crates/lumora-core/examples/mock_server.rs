//! Runs a fake OpenRGB SDK server with a sample rig, for trying Lumora without
//! hardware:
//!
//! ```sh
//! cargo run -p lumora-core --example mock_server   # listens on 127.0.0.1:6742
//! ```

fn main() {
    let port = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(6742);
    let server = lumora_core::mock::MockServer::start(port, lumora_core::mock::sample_rig(), 4)
        .expect("bind port");
    println!(
        "Mock OpenRGB server on 127.0.0.1:{} – Ctrl+C to stop",
        server.port
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let r = server.received.lock().unwrap();
        println!("{} LED updates received", r.led_updates);
    }
}
