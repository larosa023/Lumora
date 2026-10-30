//! The color Lumora's main effect is showing right now, for lights that can
//! only take one color at a time (like the Corsair Hydro Pro's pump logo).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::Rgb;

const STALE_AFTER: Duration = Duration::from_secs(3);

fn slot() -> &'static Mutex<Option<(Rgb, Instant)>> {
    static S: OnceLock<Mutex<Option<(Rgb, Instant)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

pub fn set(color: Rgb) {
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Some((color, Instant::now()));
}

/// The current color, or `None` when the engine isn't running.
pub fn get() -> Option<Rgb> {
    slot().lock().unwrap_or_else(|e| e.into_inner()).filter(|(_, at)| at.elapsed() < STALE_AFTER).map(|(c, _)| c)
}
