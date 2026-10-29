//! Lumora core: everything that isn't UI.
//!
//! * [`openrgb`] – a small, dependency-free client for the OpenRGB SDK protocol,
//!   which is how Lumora reaches RAM, motherboards, GPUs, coolers, fans,
//!   keyboards, mice and the rest.
//! * [`effects`] – the lighting effects, rendered per LED from a shared clock so
//!   every device stays in sync.
//! * [`engine`] – the render loop that ties devices, effects and output together.
//! * [`store`] – settings, profiles and the last session, saved as JSON.

pub mod color;
pub mod cooling;
pub mod demo;
pub mod device;
pub mod effects;
pub mod engine;
pub mod mock;
pub mod openrgb;
pub mod store;
pub mod temps;

pub use color::Rgb;
pub use device::{DeviceInfo, DeviceKind};
pub use effects::{Direction, EffectConfig, EffectDescriptor};
pub use engine::{ConnectionInfo, ConnectionStatus, Engine, EngineEvent, Snapshot};
pub use store::{DemoMode, Profile, Settings, Store};
