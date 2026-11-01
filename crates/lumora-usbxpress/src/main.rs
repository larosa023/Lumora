//! Lumora's 32-bit companion for Corsair Hydro Pro coolers.
//!
//! iCUE installs these coolers with Silicon Labs' USBXpress driver and a
//! 32-bit `SiUSBXp.dll` to talk through it. A 64-bit program (Lumora) can't
//! load a 32-bit DLL, so this small 32-bit program does, and relays the
//! cooler to Lumora on `127.0.0.1:6746` (see `lumora_core::cooling::relay`):
//! only Lumora can sign in (by the key in its settings folder), and only
//! supported coolers are reachable. It needs no admin rights; Lumora starts
//! it when needed and stops it on exit.
//!
//! Usage: `lumora-usbxpress --key-file <path> [--parent <Lumora's process id>]`

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        let key_file = args
            .iter()
            .position(|a| a == "--key-file")
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from)
            .or_else(lumora_core::cooling::relay::default_key_file);
        let Some(key_file) = key_file else { return };
        // Leave when Lumora does (it passes its process id), so no copy is
        // left holding the cooler or blocking an update's installer.
        if let Some(parent) = args.iter().position(|a| a == "--parent").and_then(|i| args.get(i + 1)).and_then(|p| p.parse::<u32>().ok()) {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if !lumora_core::system::processes().iter().any(|p| p.pid == parent) {
                    std::process::exit(0);
                }
            });
        }
        lumora_core::cooling::relay::run_on(
            lumora_core::cooling::relay::USBXPRESS_PORT,
            Box::new(lumora_core::cooling::siusbxp::UsbXpressBackend),
            key_file,
        );
    }
}
