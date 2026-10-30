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
}

#[derive(Clone, Copy, PartialEq)]
enum Route {
    Direct,
    Relay,
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
    let Ok(devices) = ctx.devices() else { return Vec::new() };
    let mut found: Vec<(u16, (u8, Vec<u8>), Device<Context>)> = devices
        .iter()
        .filter_map(|d| {
            let desc = d.device_descriptor().ok()?;
            (desc.vendor_id() == CORSAIR_VID && hydro::model(desc.product_id()).is_some())
                .then(|| (desc.product_id(), (d.bus_number(), d.port_numbers().unwrap_or_default()), d))
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
        UsbBackend { access: access_context(), relay_key, routes: Default::default() }
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
                l.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>().join(", ")
            }
        }
    };
    let access = seen(access_context());
    let plain = if cfg!(windows) { seen(Context::new().ok()) } else { "same as above".into() };
    let service = match crate::system::service_state("UsbDk") {
        None => "not installed".to_string(),
        Some(st) => format!("{} (exit code {})", if st.running { "running" } else { "stopped" }, st.exit_code),
    };
    format!("USB access (UsbDk on Windows): {access}\nUSB listing (Windows' own): {plain}\nUsbDk driver: {service}")
}

impl Default for UsbBackend {
    fn default() -> Self {
        Self::new(None)
    }
}

struct UsbLink(DeviceHandle<Context>);

impl Link for UsbLink {
    fn write(&mut self, msg: &[u8]) -> io::Result<()> {
        self.0.write_bulk(WRITE_ENDPOINT, msg, TIMEOUT).map(|_| ()).map_err(io_err)
    }

    fn read(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut buf = [0u8; READ_LEN];
        match self.0.read_bulk(READ_ENDPOINT, &mut buf, timeout.max(Duration::from_millis(1))) {
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

impl Backend for UsbBackend {
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
        let relay = self.relay_key.as_deref().map(|k| super::relay::reachable(super::relay::PORT, k));
        if let Some(Ok(ids)) = &relay {
            for id in ids {
                self.routes.entry(id.clone()).or_insert(Route::Relay);
            }
        }
        let helper_up = matches!(relay, Some(Ok(_)));
        let usbdk_installed = !cfg!(windows) || crate::system::service_state("UsbDk").is_some();

        // Windows' own view lists every cooler plugged in, set up or not.
        // (Elsewhere the access context already sees everything.)
        let plugged: Vec<(String, u16)> = if cfg!(windows) {
            Context::new().ok().map(|c| list(&c).into_iter().map(|(id, pid, _)| (id, pid)).collect()).unwrap_or_default()
        } else {
            self.access.as_ref().map(|c| list(c).into_iter().map(|(id, pid, _)| (id, pid)).collect()).unwrap_or_default()
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
                    problem: waiting.then(usbdk_problem).flatten(),
                }
            })
            .collect();
        // Reachable but not in Windows' list (shouldn't happen): never hide it.
        for (id, _) in self.routes.iter() {
            if !out.iter().any(|f| &f.id == id) {
                let pid = u16::from_str_radix(id.split(':').nth(2).unwrap_or("0"), 16).unwrap_or(0);
                out.push(FoundCooler { id: id.clone(), vid: CORSAIR_VID, pid, serial: String::new(), needs_setup: false, needs_restart: false, problem: None });
            }
        }
        out
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        if self.routes.get(&cooler.id) == Some(&Route::Relay) {
            let key = self.relay_key.as_deref().unwrap_or_default();
            return super::relay::Client::connect(super::relay::PORT, key)?.open(&cooler.id);
        }
        let ctx = self.access.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "the USB access driver isn't set up"))?;
        let dev = list(ctx)
            .into_iter()
            .find(|(id, _, _)| *id == cooler.id)
            .map(|(_, _, d)| d)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler isn't reachable yet"))?;
        let handle = dev.open().map_err(io_err)?;
        let _ = handle.set_auto_detach_kernel_driver(true);
        if handle.claim_interface(0).is_err() {
            let _ = handle.set_active_configuration(1);
            handle.claim_interface(0).map_err(io_err)?;
        }
        handle
            .write_control(REQUEST_TYPE_VENDOR_OUT, USBXPRESS_REQUEST, USBXPRESS_CLEAR_TO_SEND, 0, &[], TIMEOUT)
            .map_err(io_err)?;
        Ok(Box::new(UsbLink(handle)))
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
            let n = self.handle.write_bulk(self.endpoint, &data[done..], Duration::from_secs(10)).map_err(io_err)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "the screen stopped taking data"));
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
        .filter(|d| d.device_descriptor().is_ok_and(|desc| desc.vendor_id() == vid && desc.product_id() == pid))
        .collect();
    if candidates.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the cooler's picture connection wasn't found"));
    }
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "the cooler's picture connection wasn't found");
    // Several identical coolers: match the serial number when it's readable.
    let several = candidates.len() > 1;
    for dev in candidates.drain(..) {
        let Ok(config) = dev.active_config_descriptor().or_else(|_| dev.config_descriptor(0)) else { continue };
        let found = config.interfaces().flat_map(|i| i.descriptors()).find_map(|alt| {
            alt.endpoint_descriptors()
                .find(|e| e.transfer_type() == rusb::TransferType::Bulk && e.direction() == rusb::Direction::Out)
                .map(|e| (alt.interface_number(), e.address()))
        });
        let Some((iface, endpoint)) = found else { continue };
        let handle = match dev.open() {
            Ok(h) => h,
            Err(e) => {
                last_err = io_err(e);
                continue;
            }
        };
        if several && !serial.is_empty() {
            let desc = dev.device_descriptor().map_err(io_err)?;
            if handle.read_serial_number_string_ascii(&desc).ok().as_deref() != Some(serial) {
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
        return Ok(Box::new(KrakenBulk { handle, iface, endpoint }));
    }
    Err(last_err)
}
