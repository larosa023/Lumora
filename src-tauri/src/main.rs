// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The elevated helper (RAM & motherboard setup) runs without any window.
    if let Some(code) = lumora_lib::run_elevated_helper() {
        std::process::exit(code);
    }
    lumora_lib::run()
}
