//! Raw USB access (libusb) for coolers that aren't HID devices, like the
//! Corsair Hydro Pro family.
//!
//! On Windows these coolers have no driver libusb can use by default. Lumora
//! uses the UsbDk filter driver instead of replacing the cooler's driver, so
//! iCUE keeps working whenever Lumora isn't running the cooler. Until UsbDk is
//! installed, coolers are listed with `needs_setup`.

use std::io;
use std::time::Duration;

use rusb::{Context, Device, DeviceHandle, UsbContext};

use super::hydro::{self, CORSAIR_VID};
use super::{Backend, Bulk, FoundCooler, Link};

const WRITE_ENDPOINT: u8 = 0x01;
const READ_ENDPOINT: u8 = 0x81;
const READ_LEN: usize = 32;
const TIMEOUT: Duration = Duration::from_millis(1000);

/// Vendor request that lets the cooler send data to the host (the USBXpress
/// "clear to send" flow control these coolers are built on).
const REQUEST_TYPE_VENDOR_OUT: u8 = 0x40;
const USBXPRESS_REQUEST: u8 = 0x02;
const USBXPRESS_CLEAR_TO_SEND: u16 = 0x02;

pub struct UsbBackend {
    /// A UsbDk context, once UsbDk is installed (Windows). Elsewhere a plain
    /// context, which can open devices directly. On Windows it only works in
    /// a process with admin rights (UsbDk answers no one else).
    access: Option<Context>,
    /// Key for the admin helper's relay (Windows), which reaches coolers
    /// for the Lumora window. `None` in the helper itself.
    relay_key: Option<String>,
    /// How each reachable cooler was reached in the last look.
    routes: std::collections::HashMap<String, Route>,
    /// Results of the one-time direct-open check, by cooler.
    plain: std::collections::HashMap<String, Result<(), String>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Route {
    Direct,
    Relay,
    /// Windows' own USB stack, when the cooler's driver is one libusb can
    /// use directly (WinUSB-based): no UsbDk, no admin rights needed.
    Plain,
    /// Corsair's own driver (Silicon Labs USBXpress, installed with iCUE),
    /// through the `SiUSBXp.dll` already on the PC; the number is its
    /// USBXpress device index.
    #[allow(dead_code)]
    UsbXpress(u32),
}

/// Opens a cooler through `ctx` and readies it to talk.
fn open_with(ctx: &Context, id: &str) -> io::Result<UsbLink> {
    let dev = list(ctx)
        .into_iter()
        .find(|(i, _, _)| i == id)
        .map(|(_, _, d)| d)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler isn't reachable yet"))?;
    let handle = dev.open().map_err(io_err)?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    if handle.claim_interface(0).is_err() {
        let _ = handle.set_active_configuration(1);
        handle.claim_interface(0).map_err(io_err)?;
    }
    handle
        .write_control(
            REQUEST_TYPE_VENDOR_OUT,
            USBXPRESS_REQUEST,
            USBXPRESS_CLEAR_TO_SEND,
            0,
            &[],
            TIMEOUT,
        )
        .map_err(io_err)?;
    Ok(UsbLink(handle))
}

/// Whether the cooler opens through Windows' own USB stack (checked once
/// per cooler per run: opening briefly takes it from its app).
fn plain_probe(id: &str) -> Result<(), String> {
    let ctx = Context::new().map_err(|e| e.to_string())?;
    open_with(&ctx, id).map(|_| ()).map_err(|e| e.to_string())
}

fn io_err(e: rusb::Error) -> io::Error {
    let kind = match e {
        rusb::Error::Timeout => io::ErrorKind::TimedOut,
        rusb::Error::Access => io::ErrorKind::PermissionDenied,
        rusb::Error::NoDevice | rusb::Error::NotFound => io::ErrorKind::NotFound,
        rusb::Error::Busy => io::ErrorKind::ResourceBusy,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, e.to_string())
}

/// A context that can open coolers: UsbDk on Windows (`None` until it's
/// installed), a plain one elsewhere.
///
/// libusb decides once per process whether UsbDk is there, when the first
/// context starts. So this must be tried while no other context is alive,
/// or a UsbDk installed meanwhile goes unnoticed until Lumora restarts.
fn access_context() -> Option<Context> {
    #[cfg(windows)]
    {
        Context::with_options(&[rusb::UsbOption::use_usbdk()]).ok()
    }
    #[cfg(not(windows))]
    {
        Context::new().ok()
    }
}

/// Why UsbDk can't see devices, when it's more than a pending restart.
fn usbdk_problem() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let st = crate::system::service_state("UsbDk")?;
    if st.running {
        return Some(
            "The UsbDk access driver is running but can't reach this cooler. Please send the troubleshooting report from Settings.".into(),
        );
    }
    Some(match st.exit_code {
        1275 | 577 => format!(
            "Windows blocked the UsbDk access driver from starting (error {}). On Windows 11 this is usually Memory integrity: Windows Security → Device security → Core isolation details lists it under incompatible drivers.",
            st.exit_code
        ),
        0 => "Windows installed the UsbDk access driver but hasn't started it. Restart your PC once.".into(),
        code => format!("Windows installed the UsbDk access driver but couldn't start it (error {code})."),
    })
}

/// Supported coolers a context can see, numbered per model in a stable order.
fn list(ctx: &Context) -> Vec<(String, u16, Device<Context>)> {
    let Ok(devices) = ctx.devices() else {
        return Vec::new();
    };
    let mut found: Vec<(u16, (u8, Vec<u8>), Device<Context>)> = devices
        .iter()
        .filter_map(|d| {
            let desc = d.device_descriptor().ok()?;
            (desc.vendor_id() == CORSAIR_VID && hydro::model(desc.product_id()).is_some()).then(
                || {
                    (
                        desc.product_id(),
                        (d.bus_number(), d.port_numbers().unwrap_or_default()),
                        d,
                    )
                },
            )
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = Vec::new();
    let mut last: Option<(u16, usize)> = None;
    for (pid, _, dev) in found {
        let n = match last {
            Some((p, n)) if p == pid => n + 1,
            _ => 0,
        };
        last = Some((pid, n));
        // Numbered rather than by port: ports read differently through UsbDk
        // and plain Windows USB, and the saved settings must still match.
        out.push((format!("usb:{CORSAIR_VID:04x}:{pid:04x}:{n}"), pid, dev));
    }
    out
}

impl UsbBackend {
    /// `relay_key`: see [`super::relay`]; pass `None` in the admin helper.
    pub fn new(relay_key: Option<String>) -> Self {
        UsbBackend {
            access: access_context(),
            relay_key,
            routes: Default::default(),
            plain: Default::default(),
        }
    }
}

/// What each way of looking at USB sees, for the troubleshooting report.
pub fn report() -> String {
    let seen = |ctx: Option<Context>| match ctx {
        None => "not available".to_string(),
        Some(c) => {
            let l = list(&c);
            if l.is_empty() {
                "no Corsair Hydro Pro".to_string()
            } else {
                l.iter()
                    .map(|(id, _, _)| id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
    };
    let access = seen(access_context());
    let plain = if cfg!(windows) {
        seen(Context::new().ok())
    } else {
        "same as above".into()
    };
    let windows = diagnose_windows();
    let service = match crate::system::service_state("UsbDk") {
        None => "not installed".to_string(),
        Some(st) => format!(
            "{} (exit code {})",
            if st.running { "running" } else { "stopped" },
            st.exit_code
        ),
    };
    format!("USB access (UsbDk on Windows): {access}\nUSB listing (Windows' own): {plain}\nUsbDk driver: {service}\n{windows}")
}

impl Default for UsbBackend {
    fn default() -> Self {
        Self::new(None)
    }
}

struct UsbLink(DeviceHandle<Context>);

impl Link for UsbLink {
    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        self.0
            .write_bulk(WRITE_ENDPOINT, msg, TIMEOUT)
            .map(|_| ())
            .map_err(io_err)
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut buf = [0u8; READ_LEN];
        match self.0.read_bulk(
            READ_ENDPOINT,
            &mut buf,
            timeout.max(Duration::from_millis(1)),
        ) {
            Ok(n) => Ok(Some(buf[..n].to_vec())),
            Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }
}

impl Drop for UsbLink {
    fn drop(&mut self) {
        let _ = self.0.release_interface(0);
    }
}

/// A detailed look through a fresh access context: every Corsair device it
/// lists, whether the cooler opens, and libusb's own messages on the way.
pub fn diagnose_access() -> String {
    use std::sync::{Arc, Mutex};
    let mut out = Vec::new();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut ctx = match access_context() {
        Some(c) => c,
        None => {
            return "access context: couldn't start (UsbDk not available to this process)".into()
        }
    };
    let sink = log.clone();
    ctx.set_log_level(rusb::LogLevel::Info);
    ctx.set_log_callback(
        Box::new(move |_level, msg| {
            if let Ok(mut l) = sink.lock() {
                if l.len() < 40 {
                    l.push(msg.trim().to_string());
                }
            }
        }),
        rusb::LogCallbackMode::Context,
    );
    match ctx.devices() {
        Err(e) => out.push(format!("device list failed: {e}")),
        Ok(list) => {
            out.push(format!("devices listed: {}", list.len()));
            let few = list.len() < 12;
            for d in list.iter() {
                if let Ok(desc) = d.device_descriptor() {
                    if few || desc.vendor_id() == CORSAIR_VID {
                        out.push(format!(
                            "device {:04x}:{:04x} class {:02x} bus {} port {}",
                            desc.vendor_id(),
                            desc.product_id(),
                            desc.class_code(),
                            d.bus_number(),
                            d.port_number()
                        ));
                    }
                }
            }
        }
    }
    for (id, _, dev) in list(&ctx) {
        let opened = dev.open().and_then(|h| {
            h.claim_interface(0).or_else(|_| {
                let _ = h.set_active_configuration(1);
                h.claim_interface(0)
            })?;
            h.write_control(
                REQUEST_TYPE_VENDOR_OUT,
                USBXPRESS_REQUEST,
                USBXPRESS_CLEAR_TO_SEND,
                0,
                &[],
                TIMEOUT,
            )?;
            Ok(())
        });
        out.push(format!(
            "open {id}: {}",
            opened
                .map(|_| "ok".to_string())
                .unwrap_or_else(|e| e.to_string())
        ));
    }
    if let Ok(l) = log.lock() {
        for m in l.iter().filter(|m| !m.is_empty()) {
            out.push(format!("libusb: {m}"));
        }
    }
    out.join("\n")
}

/// The cooler's Windows driver, UsbDk's place on the USB hubs, and whether
/// the cooler opens without UsbDk: for the troubleshooting report.
pub fn diagnose_windows() -> String {
    let mut out = Vec::new();
    for m in hydro::MODELS {
        for line in crate::system::usb_driver_info(CORSAIR_VID, m.pid) {
            out.push(format!("{} driver: {line}", m.name));
        }
    }
    out.push(format!(
        "USB hub filters: {}",
        crate::system::usb_class_filters().unwrap_or_else(|| "none".into())
    ));
    #[cfg(windows)]
    out.push(super::siusbxp::diagnose());
    if let Ok(ctx) = Context::new() {
        for (id, _, _) in list(&ctx) {
            out.push(format!(
                "open {id} without UsbDk: {}",
                plain_probe(&id)
                    .map(|_| "ok".to_string())
                    .unwrap_or_else(|e| e)
            ));
        }
    }
    out.join("\n")
}

impl Backend for UsbBackend {
    fn diagnose(&mut self) -> String {
        diagnose_access()
    }

    fn discover(&mut self) -> Vec<FoundCooler> {
        // Not set up yet: look for UsbDk afresh (no context alive, see
        // `access_context`).
        if self.access.is_none() {
            self.access = access_context();
        }
        self.routes.clear();
        for (id, _, _) in self.access.as_ref().map(list).unwrap_or_default() {
            self.routes.insert(id, Route::Direct);
        }
        // Through the admin helper (the Lumora window can't use UsbDk itself).
        let relay = self
            .relay_key
            .as_deref()
            .map(|k| super::relay::reachable(super::relay::PORT, k));
        if let Some(Ok(ids)) = &relay {
            for id in ids {
                self.routes.entry(id.clone()).or_insert(Route::Relay);
            }
        }
        // Windows-only: a cooler whose driver libusb can use directly needs
        // neither UsbDk nor the helper. Tried once per cooler.
        if cfg!(windows) {
            for (id, _, _) in Context::new().ok().map(|c| list(&c)).unwrap_or_default() {
                if self.routes.contains_key(&id) {
                    continue;
                }
                let ok = self
                    .plain
                    .entry(id.clone())
                    .or_insert_with(|| plain_probe(&id))
                    .is_ok();
                if ok {
                    self.routes.insert(id, Route::Plain);
                }
            }
        }
        // Corsair's own USBXpress driver (how iCUE installs these coolers).
        #[cfg(windows)]
        {
            if let Ok(api) = super::siusbxp::api() {
                if let Ok(devs) = api.devices() {
                    for (id, pid, _) in Context::new().ok().map(|c| list(&c)).unwrap_or_default() {
                        if self.routes.contains_key(&id) {
                            continue;
                        }
                        // The n-th cooler of this model is the n-th USBXpress one.
                        let n: usize = id
                            .rsplit(':')
                            .next()
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(0);
                        if let Some((index, _, _)) = devs
                            .iter()
                            .filter(|(_, v, p)| *v == CORSAIR_VID && *p == pid)
                            .nth(n)
                        {
                            self.routes.insert(id, Route::UsbXpress(*index));
                        }
                    }
                }
            }
        }
        let helper_up = matches!(relay, Some(Ok(_)));
        let helper_outdated =
            matches!(&relay, Some(Err(e)) if e.kind() == io::ErrorKind::InvalidData);
        let usbdk_installed = !cfg!(windows) || crate::system::service_state("UsbDk").is_some();

        // Windows' own view lists every cooler plugged in, set up or not.
        // (Elsewhere the access context already sees everything.)
        let plugged: Vec<(String, u16)> = if cfg!(windows) {
            Context::new()
                .ok()
                .map(|c| list(&c).into_iter().map(|(id, pid, _)| (id, pid)).collect())
                .unwrap_or_default()
        } else {
            self.access
                .as_ref()
                .map(|c| list(c).into_iter().map(|(id, pid, _)| (id, pid)).collect())
                .unwrap_or_default()
        };
        let mut out: Vec<FoundCooler> = plugged
            .iter()
            .map(|(id, pid)| {
                let ok = self.routes.contains_key(id);
                // Missing pieces are installed by the one-time setup: the UsbDk
                // driver and, in the window, the admin helper that uses it.
                let setup = !ok && (!usbdk_installed || (self.relay_key.is_some() && !helper_up));
                let waiting = !ok && !setup;
                FoundCooler {
                    id: id.clone(),
                    vid: CORSAIR_VID,
                    pid: *pid,
                    serial: String::new(),
                    needs_setup: setup,
                    // Everything is installed but the cooler isn't reachable
                    // yet: usually a restart after the driver install.
                    needs_restart: waiting,
                    problem: if setup && helper_outdated {
                        Some("Lumora's helper is from an older version. Run the setup once more to update it (one Windows prompt).".into())
                    } else {
                        waiting.then(usbdk_problem).flatten()
                    },
                }
            })
            .collect();
        // Reachable but not in Windows' list (shouldn't happen): never hide it.
        for (id, _) in self.routes.iter() {
            if !out.iter().any(|f| &f.id == id) {
                let pid = u16::from_str_radix(id.split(':').nth(2).unwrap_or("0"), 16).unwrap_or(0);
                out.push(FoundCooler {
                    id: id.clone(),
                    vid: CORSAIR_VID,
                    pid,
                    serial: String::new(),
                    needs_setup: false,
                    needs_restart: false,
                    problem: None,
                });
            }
        }
        out
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        match self.routes.get(&cooler.id) {
            Some(Route::Relay) => {
                let key = self.relay_key.as_deref().unwrap_or_default();
                return super::relay::Client::connect(super::relay::PORT, key)?.open(&cooler.id);
            }
            Some(Route::Plain) => {
                let ctx = Context::new().map_err(io_err)?;
                return Ok(Box::new(open_with(&ctx, &cooler.id)?));
            }
            #[cfg(windows)]
            Some(Route::UsbXpress(index)) => {
                let api = super::siusbxp::api().map_err(io::Error::other)?;
                return Ok(Box::new(api.open(*index)?));
            }
            _ => {}
        }
        let ctx = self.access.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "the USB access driver isn't set up",
            )
        })?;
        Ok(Box::new(open_with(ctx, &cooler.id)?))
    }
}

/// The bulk connection NZXT Kraken screens take pictures over: a second USB
/// interface next to the HID one, with a bulk OUT endpoint.
///
/// This uses libusb's normal Windows backend (WinUSB), never UsbDk: UsbDk
/// would take the whole cooler away from Windows, HID part included.
struct KrakenBulk {
    handle: DeviceHandle<Context>,
    iface: u8,
    endpoint: u8,
}

impl Bulk for KrakenBulk {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let mut done = 0;
        while done < data.len() {
            let n = self
                .handle
                .write_bulk(self.endpoint, &data[done..], Duration::from_secs(10))
                .map_err(io_err)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the screen stopped taking data",
                ));
            }
            done += n;
        }
        Ok(())
    }
}

impl Drop for KrakenBulk {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}

/// Opens the picture connection of the Kraken with this vendor, product and
/// serial number.
pub fn open_kraken_bulk(vid: u16, pid: u16, serial: &str) -> io::Result<Box<dyn Bulk>> {
    let ctx = Context::new().map_err(io_err)?;
    let devices = ctx.devices().map_err(io_err)?;
    let mut candidates: Vec<Device<Context>> = devices
        .iter()
        .filter(|d| {
            d.device_descriptor()
                .is_ok_and(|desc| desc.vendor_id() == vid && desc.product_id() == pid)
        })
        .collect();
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the cooler's picture connection wasn't found",
        ));
    }
    let mut last_err = io::Error::new(
        io::ErrorKind::NotFound,
        "the cooler's picture connection wasn't found",
    );
    // Several identical coolers: match the serial number when it's readable.
    let several = candidates.len() > 1;
    for dev in candidates.drain(..) {
        let Ok(config) = dev
            .active_config_descriptor()
            .or_else(|_| dev.config_descriptor(0))
        else {
            continue;
        };
        let found = config
            .interfaces()
            .flat_map(|i| i.descriptors())
            .find_map(|alt| {
                alt.endpoint_descriptors()
                    .find(|e| {
                        e.transfer_type() == rusb::TransferType::Bulk
                            && e.direction() == rusb::Direction::Out
                    })
                    .map(|e| (alt.interface_number(), e.address()))
            });
        let Some((iface, endpoint)) = found else {
            continue;
        };
        let handle = match dev.open() {
            Ok(h) => h,
            Err(e) => {
                last_err = io_err(e);
                continue;
            }
        };
        if several && !serial.is_empty() {
            let desc = dev.device_descriptor().map_err(io_err)?;
            if handle
                .read_serial_number_string_ascii(&desc)
                .ok()
                .as_deref()
                != Some(serial)
            {
                continue;
            }
        }
        let _ = handle.set_auto_detach_kernel_driver(true);
        if let Err(e) = handle.claim_interface(iface) {
            last_err = io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Windows wouldn't hand over the screen's picture connection ({e}). It needs the WinUSB driver NZXT CAM installs."),
            );
            continue;
        }
        return Ok(Box::new(KrakenBulk {
            handle,
            iface,
            endpoint,
        }));
    }
    Err(last_err)
}
