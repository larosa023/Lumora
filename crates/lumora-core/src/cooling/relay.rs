//! A small relay for coolers that only an admin process can reach.
//!
//! On Windows, the UsbDk access driver Lumora uses for Corsair Hydro Pro
//! coolers only answers programs with admin rights, and the Lumora window
//! runs without them. So Lumora's admin helper (`lumora-sensors`) opens the
//! cooler and relays its messages over `127.0.0.1:6745`, one line each way:
//!
//! ```text
//! → auth <key>          ← ok | err <why>
//! → list                ← ok <id> <id>…
//! → open <id>           ← ok | err <why>
//! → write <hex>         ← ok | err <why>
//! → read <ms>           ← ok [<hex>] | err <why>     (no hex: timed out)
//! → diag                ← ok <details, " ¦ " between lines>
//! → version             ← ok <Lumora version>
//! ```
//!
//! The key is a random value Lumora writes into its own settings folder (the
//! user's profile) at every start; the relay reads it from there. Only the
//! coolers its backend supports are listed or opened, and only one at a time
//! per connection.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Backend, FoundCooler, Link};

pub const PORT: u16 = 6745;

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> io::Result<Vec<u8>> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad hex");
    if s.len() % 2 != 0 {
        return Err(bad());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).ok_or_else(bad)?, 16).map_err(|_| bad()))
        .collect()
}

/// Lumora's settings folder as seen from the same user's admin helper.
pub fn default_key_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("app.lumora.rgb").join("relay.key"))
}

/// Writes a fresh random key for the relay and returns it.
pub fn new_key(file: &Path) -> io::Result<String> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut key = String::new();
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        key.push_str(&format!("{:016x}", h.finish()));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, &key)?;
    Ok(key)
}

// --- Relay (runs in the admin helper) ----------------------------------------

/// Serves one connection until it closes. The backend is shared between
/// connections (each keeps its own open cooler), so a quick "list" never
/// waits behind a cooler that's open.
pub fn serve(stream: TcpStream, backend: &Mutex<Box<dyn Backend>>, key_file: &Path) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut out = stream;
    let mut lines = BufReader::new(read_half).lines();
    let mut authed = false;
    let mut link: Option<Box<dyn Link>> = None;
    let reply =
        |out: &mut TcpStream, text: String| out.write_all(format!("{text}\n").as_bytes()).is_ok();
    let lock = || backend.lock().unwrap_or_else(|e| e.into_inner());

    while let Some(Ok(line)) = lines.next() {
        let (cmd, arg) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let answer = if !authed {
            match (cmd, std::fs::read_to_string(key_file)) {
                ("auth", Ok(key)) if !key.trim().is_empty() && key.trim() == arg => {
                    authed = true;
                    "ok".to_string()
                }
                ("auth", _) => "err wrong key".into(),
                _ => "err sign in first".into(),
            }
        } else {
            match cmd {
                // Lumora checks this to know when the helper needs updating.
                "version" => format!("ok {}", env!("CARGO_PKG_VERSION")),
                "list" => {
                    let ids: Vec<String> = lock()
                        .discover()
                        .into_iter()
                        .filter(|f| !f.needs_setup && !f.needs_restart)
                        .map(|f| f.id)
                        .collect();
                    format!("ok {}", ids.join(" ")).trim_end().to_string()
                }
                // Details for the troubleshooting report, on one line.
                "diag" => format!("ok {}", lock().diagnose().replace('\n', " ¦ ")),
                "open" => {
                    link = None;
                    let mut b = lock();
                    match b.discover().into_iter().find(|f| f.id == arg) {
                        None => "err no such cooler".into(),
                        Some(f) => match b.open(&f) {
                            Ok(l) => {
                                link = Some(l);
                                "ok".into()
                            }
                            Err(e) => format!("err {e}"),
                        },
                    }
                }
                "write" => match (link.as_mut(), unhex(arg)) {
                    (None, _) => "err nothing open".into(),
                    (_, Err(e)) => format!("err {e}"),
                    (Some(l), Ok(data)) => match l.write(&data) {
                        Ok(()) => "ok".into(),
                        Err(e) => format!("err {e}"),
                    },
                },
                "read" => match link.as_mut() {
                    None => "err nothing open".into(),
                    Some(l) => {
                        let ms = arg.parse::<u64>().unwrap_or(1000).min(10_000);
                        match l.read(Duration::from_millis(ms)) {
                            Ok(Some(data)) => format!("ok {}", hex(&data)),
                            Ok(None) => "ok".into(),
                            Err(e) => format!("err {e}"),
                        }
                    }
                },
                _ => "err unknown command".into(),
            }
        };
        if !reply(&mut out, answer) {
            break;
        }
    }
}

/// Runs the relay on `127.0.0.1:6745`.
pub fn run(backend: Box<dyn Backend>, key_file: PathBuf) {
    run_on(PORT, backend, key_file)
}

/// Where Lumora's 32-bit companion relays USBXpress coolers.
pub const USBXPRESS_PORT: u16 = 6746;

/// Runs a relay on `127.0.0.1:<port>`; returns at once if the port is taken
/// (another copy already serves it).
pub fn run_on(port: u16, backend: Box<dyn Backend>, key_file: PathBuf) {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else {
        return;
    };
    let shared = Arc::new(Mutex::new(backend));
    for stream in listener.incoming().flatten() {
        let (backend, key_file) = (shared.clone(), key_file.clone());
        std::thread::spawn(move || serve(stream, &backend, &key_file));
    }
}

// --- Client (runs in Lumora) -------------------------------------------------

/// A connection to the relay, signed in.
pub struct Client {
    out: TcpStream,
    lines: std::io::Lines<BufReader<TcpStream>>,
}

impl Client {
    pub fn connect(port: u16, key: &str) -> io::Result<Client> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let out = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
        out.set_read_timeout(Some(Duration::from_secs(5)))?;
        let _ = out.set_nodelay(true);
        let lines = BufReader::new(out.try_clone()?).lines();
        let mut c = Client { out, lines };
        c.ask(&format!("auth {key}"))?;
        Ok(c)
    }

    /// Sends one line and returns what follows "ok".
    fn ask(&mut self, line: &str) -> io::Result<String> {
        self.out.write_all(format!("{line}\n").as_bytes())?;
        let reply = self.lines.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the helper closed the connection",
            )
        })??;
        match reply.split_once(' ').unwrap_or((reply.as_str(), "")) {
            ("ok", rest) => Ok(rest.to_string()),
            ("err", why) => Err(io::Error::other(why.to_string())),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected reply from the helper",
            )),
        }
    }

    /// Coolers the helper can reach.
    pub fn list(&mut self) -> io::Result<Vec<String>> {
        Ok(self
            .ask("list")?
            .split_whitespace()
            .map(String::from)
            .collect())
    }

    /// Opens a cooler; this connection then carries its messages.
    pub fn open(mut self, id: &str) -> io::Result<Box<dyn Link>> {
        self.ask(&format!("open {id}"))?;
        Ok(Box::new(self))
    }
}

impl Link for Client {
    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        self.ask(&format!("write {}", hex(msg))).map(|_| ())
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let _ = self
            .out
            .set_read_timeout(Some(timeout + Duration::from_secs(5)));
        let rest = self.ask(&format!("read {}", timeout.as_millis()))?;
        if rest.is_empty() {
            Ok(None)
        } else {
            unhex(&rest).map(Some)
        }
    }
}

/// What the helper sees, for the troubleshooting report.
pub fn diagnose(port: u16, key: &str) -> String {
    match Client::connect(port, key) {
        Err(e) => format!("helper relay: not reachable ({e})"),
        Ok(mut c) => match c.ask("diag") {
            Ok(text) => format!("helper relay: connected\n  {}", text.replace(" ¦ ", "\n  ")),
            Err(e) => format!("helper relay: connected, but it couldn't diagnose ({e}). It may be an older helper: run the cooler setup again."),
        },
    }
}

/// What a helper relay offers, for a backend on the Lumora side. A helper
/// from another Lumora version counts as out of date
/// (`ErrorKind::InvalidData`): the setup installs the matching one.
pub fn reachable(port: u16, key: &str) -> io::Result<Vec<String>> {
    let mut c = Client::connect(port, key)?;
    match c.ask("version") {
        Ok(v) if v == env!("CARGO_PKG_VERSION") => c.list(),
        Ok(v) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the helper is version {v}"),
        )),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the helper is from an older Lumora",
        )),
    }
}

/// Turns relay ids into coolers (for tests and simple backends).
pub fn found(id: &str, vid: u16, pid: u16) -> FoundCooler {
    FoundCooler {
        id: id.into(),
        vid,
        pid,
        serial: String::new(),
        needs_setup: false,
        needs_restart: false,
        problem: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeBackend;
    use super::super::family::Model;
    use super::*;

    #[test]
    fn relays_a_corsair_cooler_with_the_right_key_only() {
        let dir = std::env::temp_dir().join(format!("lumora-relay-{}", std::process::id()));
        let key_file = dir.join("relay.key");
        let key = new_key(&key_file).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (backend, dev) = FakeBackend::with_hydro(0x0C13);
        let shared: Arc<Mutex<Box<dyn Backend>>> = Arc::new(Mutex::new(Box::new(backend)));
        let kf = key_file.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let (b, kf) = (shared.clone(), kf.clone());
                std::thread::spawn(move || serve(s, &b, &kf));
            }
        });

        assert!(
            Client::connect(port, "wrong").is_err(),
            "a wrong key is refused"
        );
        let ids = reachable(port, &key).unwrap();
        assert_eq!(ids, vec!["usb:1b1c:0c13:1-4".to_string()]);

        // The real driver runs over the relay.
        let link = Client::connect(port, &key).unwrap().open(&ids[0]).unwrap();
        let Some(Model::Hydro(m)) = Model::find(0x1B1C, 0x0C13) else {
            panic!()
        };
        let mut d = super::super::hydro::HydroPro::new(link, m);
        use super::super::family::Driver;
        d.initialize().unwrap();
        assert_eq!(d.firmware().as_deref(), Some("1.1.15.4"));
        let st = d.status().unwrap();
        assert!((st.liquid_c.unwrap() - 31.7).abs() < 0.01);
        assert_eq!(st.fan_rpms, vec![900, 920]);
        assert!(dev.lock().unwrap().writes > 3);
        // Listing still answers while the cooler is open on another connection.
        assert_eq!(reachable(port, &key).unwrap().len(), 1);
        d.status().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
