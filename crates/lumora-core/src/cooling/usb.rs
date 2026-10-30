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
    ctx: Option<Context>,
    /// The context can see coolers but can't open them yet.
    needs_setup: bool,
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

/// A context that can open coolers, and whether it needed a fallback.
fn new_context() -> (Option<Context>, bool) {
    #[cfg(windows)]
    {
        if let Ok(ctx) = Context::with_options(&[rusb::UsbOption::use_usbdk()]) {
            return (Some(ctx), false);
        }
        (Context::new().ok(), true)
    }
    #[cfg(not(windows))]
    {
        (Context::new().ok(), false)
    }
}

impl UsbBackend {
    pub fn new() -> Self {
        let (ctx, needs_setup) = new_context();
        UsbBackend { ctx, needs_setup }
    }

    /// Supported coolers, numbered per model in a stable order.
    fn coolers(&self) -> Vec<(FoundCooler, Device<Context>)> {
        let Some(ctx) = &self.ctx else { return Vec::new() };
        let Ok(list) = ctx.devices() else { return Vec::new() };
        let mut found: Vec<(u16, (u8, Vec<u8>), Device<Context>)> = list
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
            // Numbered rather than by port: ports read differently before and
            // after UsbDk is installed, and the saved settings must still match.
            let id = format!("usb:{CORSAIR_VID:04x}:{pid:04x}:{n}");
            out.push((FoundCooler { id, vid: CORSAIR_VID, pid, serial: String::new(), needs_setup: self.needs_setup }, dev));
        }
        out
    }
}

impl Default for UsbBackend {
    fn default() -> Self {
        Self::new()
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
        let mut found = self.coolers();
        // UsbDk may have been installed since the last look.
        if self.needs_setup && !found.is_empty() {
            let (ctx, needs_setup) = new_context();
            if !needs_setup {
                self.ctx = ctx;
                self.needs_setup = false;
                found = self.coolers();
            }
        }
        found.into_iter().map(|(f, _)| f).collect()
    }

    fn open(&mut self, cooler: &FoundCooler) -> io::Result<Box<dyn Link>> {
        let dev = self
            .coolers()
            .into_iter()
            .find(|(f, _)| f.id == cooler.id)
            .map(|(_, d)| d)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the cooler was unplugged"))?;
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
