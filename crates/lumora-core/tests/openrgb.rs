use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use lumora_core::engine::EngineEvent;
use lumora_core::mock::{sample_rig, MockServer};
use lumora_core::openrgb::Client;
use lumora_core::store::{DemoMode, Session, Settings};
use lumora_core::{ConnectionStatus, DeviceKind, EffectConfig, Engine, Rgb};

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn client_reads_controllers_at_every_protocol() {
    for proto in [0, 1, 2, 3, 4, 5] {
        let server = MockServer::start(0, sample_rig(), proto).unwrap();
        let mut c = Client::connect("127.0.0.1", server.port, Duration::from_secs(1)).unwrap();
        assert_eq!(c.protocol, proto.min(4), "negotiated protocol");
        let controllers = c.controllers().unwrap();
        assert_eq!(controllers, sample_rig().into_iter().map(|mut x| {
            if proto == 0 {
                x.vendor.clear();
            }
            if proto < 4 {
                // Segments arrived with protocol 4.
                x.zones.iter_mut().for_each(|z| z.segments.clear());
            }
            x
        }).collect::<Vec<_>>());
        assert!(controllers.iter().all(|c| c.supports_direct()));
        assert_eq!(server.received.lock().unwrap().client_name.as_deref(), Some("Lumora"));
    }
}

#[test]
fn engine_drives_real_devices() {
    let server = MockServer::start(0, sample_rig(), 4).unwrap();
    let changes = Arc::new(AtomicUsize::new(0));
    let c2 = changes.clone();
    let settings = Settings { port: server.port, demo_devices: DemoMode::Auto, ..Default::default() };
    let engine = Engine::new(
        settings,
        Session::default(),
        Arc::new(move |e| {
            if let EngineEvent::Changed = e {
                c2.fetch_add(1, Ordering::SeqCst);
            }
        }),
    );

    wait_for("connection", || engine.snapshot().connection.status == ConnectionStatus::Connected);
    let snap = engine.snapshot();
    assert_eq!(snap.devices.len(), 7, "demo devices hide once real ones appear");
    assert!(snap.devices.iter().all(|d| !d.virtual_device));
    assert_eq!(snap.devices[0].kind, DeviceKind::Ram);
    assert_ne!(snap.devices[0].id, snap.devices[1].id, "identical sticks get distinct ids");
    let kbd = snap.devices.iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap();
    assert_eq!(kbd.led_count, 9);
    assert!(kbd.zones[0].matrix.is_some());
    assert!(changes.load(Ordering::SeqCst) > 0);
    wait_for("direct mode", || server.received.lock().unwrap().custom_mode.len() == 6);

    // A static color reaches every device…
    let teal = Rgb::new(0, 200, 180);
    engine.sync_all(EffectConfig { effect: "static".into(), colors: vec![teal], ..Default::default() });
    wait_for("static frame", || {
        let r = server.received.lock().unwrap();
        (0..7).filter(|&i| i != 3).all(|i| r.leds.get(&i).is_some_and(|l| !l.is_empty() && l.iter().all(|c| *c == teal)))
    });

    // The NZXT cooler is left to NZXT CAM by default: never touched…
    let kraken = snap.devices.iter().find(|d| d.vendor == "NZXT").unwrap().clone();
    assert!(!kraken.managed);
    assert!(!server.received.lock().unwrap().custom_mode.contains(&3));
    assert!(server.received.lock().unwrap().leds.get(&3).is_none());
    // …until the user hands it to Lumora.
    let mut settings = Settings { port: server.port, ..Default::default() };
    settings.device_control.insert(kraken.id.clone(), true);
    engine.apply_settings(settings.clone());
    wait_for("kraken taken over", || {
        let r = server.received.lock().unwrap();
        r.custom_mode.contains(&3) && r.leds.get(&3).is_some_and(|l| l.iter().all(|c| *c == teal))
    });
    assert!(engine.snapshot().devices.iter().find(|d| d.id == kraken.id).unwrap().managed);

    // …and then isn't re-sent while nothing changes.
    let before = server.received.lock().unwrap().led_updates;
    sleep(Duration::from_millis(600));
    assert_eq!(server.received.lock().unwrap().led_updates, before, "static effects are sent once");

    // Per-device override, brightness and power.
    let gpu = snap.devices[2].id.clone();
    engine.set_effect(Some(&gpu), EffectConfig { effect: "static".into(), colors: vec![Rgb::new(255, 0, 0)], ..Default::default() });
    wait_for("override", || server.received.lock().unwrap().leds.get(&2).is_some_and(|l| l[0] == Rgb::new(255, 0, 0)));
    engine.set_brightness(50);
    wait_for("dimmed", || server.received.lock().unwrap().leds.get(&2).is_some_and(|l| l[0] == Rgb::new(64, 0, 0)));
    engine.set_enabled(false);
    wait_for("off", || server.received.lock().unwrap().leds.get(&2).is_some_and(|l| l[0] == Rgb::BLACK));

    // Device-list changes are picked up without dropping the connection.
    server.notify_list_updated();
    sleep(Duration::from_millis(300));
    assert_eq!(engine.snapshot().connection.status, ConnectionStatus::Connected);
    assert_eq!(engine.snapshot().devices.len(), 7);

    // An empty ARGB header can be sized, and the new LEDs light up.
    let board = engine.snapshot().devices.into_iter().find(|d| d.kind == DeviceKind::Motherboard).unwrap();
    assert_eq!(board.led_count, 3);
    assert_eq!(board.zones[1].resizable, Some((0, 120)));
    engine.set_enabled(true);
    engine.resize_zone(&board.id, 1, 16);
    wait_for("resized zone", || {
        engine.snapshot().devices.iter().any(|d| d.id == board.id && d.led_count == 19 && d.zones[1].count == 16)
    });
    wait_for("leds on the new zone", || server.received.lock().unwrap().leds.get(&6).is_some_and(|l| l.len() == 19));

    // Rescans go to the server.
    engine.rescan();
    wait_for("rescan", || server.received.lock().unwrap().rescans == 1);
    assert_eq!(engine.snapshot().connection.status, ConnectionStatus::Connected);
}

#[test]
fn engine_falls_back_to_demo_devices() {
    // Nothing listens on this port.
    let settings = Settings { port: 1, ..Default::default() };
    let frames = Arc::new(AtomicUsize::new(0));
    let f2 = frames.clone();
    let engine = Engine::new(
        settings,
        Session::default(),
        Arc::new(move |e| {
            if let EngineEvent::Frames(f) = e {
                f2.fetch_add(f.len(), Ordering::SeqCst);
            }
        }),
    );
    wait_for("offline", || engine.snapshot().connection.status == ConnectionStatus::Offline);
    let snap = engine.snapshot();
    assert!(!snap.devices.is_empty());
    assert!(snap.devices.iter().all(|d| d.virtual_device));
    engine.set_preview(true);
    wait_for("preview frames", || frames.load(Ordering::SeqCst) > 10);
}
