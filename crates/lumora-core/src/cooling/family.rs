//! What each supported cooler can do, and one interface to drive any of them.

use std::io;

use serde::Serialize;

use super::curve::SpeedSetting;
use super::gpu::{self, GpuFans, GpuModel};
use super::hydro::{self, HydroModel, HydroPro};
use super::kraken::{self, Kraken, KrakenModel, PUMP_MIN_DUTY};
use super::nzxt_fans::{self, FanController, FanControllerModel};
use super::{Bulk, CoolerStatus, LightSettings, Link, ScreenSettings};

/// How a channel's speed is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// Presets, a fixed duty or any curve.
    Curve,
    /// Only the three built-in modes (Silent, Balanced, Performance).
    Modes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub min_duty: u8,
    pub kind: ChannelKind,
}

/// A supported cooler model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    Kraken(&'static KrakenModel),
    Hydro(&'static HydroModel),
    /// A graphics card's own fans.
    Gpu(&'static GpuModel),
    /// A case fan controller (no pump, no curves of its own).
    Fans(&'static FanControllerModel),
}

impl Model {
    pub fn find(vid: u16, pid: u16) -> Option<Model> {
        match vid {
            kraken::NZXT_VID => kraken::model(pid)
                .map(Model::Kraken)
                .or_else(|| nzxt_fans::model(pid).map(Model::Fans)),
            hydro::CORSAIR_VID => hydro::model(pid).map(Model::Hydro),
            gpu::NVIDIA_VID => gpu::model(pid).map(Model::Gpu),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Model::Kraken(m) => m.name,
            Model::Hydro(m) => m.name,
            Model::Gpu(m) => m.name,
            Model::Fans(m) => m.name,
        }
    }

    pub fn vendor(&self) -> &'static str {
        match self {
            Model::Kraken(_) => "NZXT",
            Model::Hydro(_) => "Corsair",
            Model::Gpu(_) => "NVIDIA",
            Model::Fans(_) => "NZXT",
        }
    }

    pub fn channels(&self) -> Vec<ChannelSpec> {
        match self {
            Model::Kraken(m) => {
                let mut v = vec![ChannelSpec {
                    id: "pump",
                    name: "Pump",
                    min_duty: PUMP_MIN_DUTY,
                    kind: ChannelKind::Curve,
                }];
                if m.fan_id.is_some() {
                    v.push(ChannelSpec {
                        id: "fan",
                        name: "Radiator fans",
                        min_duty: 0,
                        kind: ChannelKind::Curve,
                    });
                }
                v
            }
            Model::Gpu(m) => vec![ChannelSpec {
                id: "fan",
                name: "Graphics card fans",
                min_duty: m.min_duty,
                kind: ChannelKind::Curve,
            }],
            Model::Fans(m) => (0..m.fans as usize)
                .map(|i| ChannelSpec {
                    id: nzxt_fans::CHANNELS[i],
                    name: nzxt_fans::CHANNEL_NAMES[i],
                    min_duty: nzxt_fans::MIN_DUTY,
                    kind: ChannelKind::Curve,
                })
                .collect(),
            Model::Hydro(_) => vec![
                ChannelSpec {
                    id: "pump",
                    name: "Pump",
                    min_duty: 0,
                    kind: ChannelKind::Modes,
                },
                ChannelSpec {
                    id: "fan",
                    name: "Radiator fans",
                    min_duty: 0,
                    kind: ChannelKind::Curve,
                },
            ],
        }
    }

    /// Screen size in pixels.
    pub fn screen(&self) -> Option<(u16, u16)> {
        match self {
            Model::Kraken(m) => m.lcd,
            Model::Hydro(_) | Model::Gpu(_) | Model::Fans(_) => None,
        }
    }

    /// The device stores no curves, so Lumora drives every speed live.
    pub fn software_only(&self) -> bool {
        matches!(self, Model::Gpu(_) | Model::Fans(_))
    }

    /// "aio", "gpu" or "fans", for the page.
    pub fn kind(&self) -> &'static str {
        match self {
            Model::Gpu(_) => "gpu",
            Model::Fans(_) => "fans",
            _ => "aio",
        }
    }

    /// The temperature every channel follows, when it can't be chosen.
    pub fn fixed_source(&self) -> Option<&'static str> {
        matches!(self, Model::Gpu(_)).then_some("gpu")
    }

    /// A graphics card rather than a liquid cooler.
    pub fn is_gpu(&self) -> bool {
        matches!(self, Model::Gpu(_))
    }

    /// Has lighting the cooler runs itself (set from the cooling page).
    pub fn light(&self) -> bool {
        matches!(self, Model::Hydro(_))
    }

    /// How many fans the cooler reports one by one.
    pub fn fan_count(&self) -> u8 {
        match self {
            Model::Kraken(_) => 0,
            Model::Hydro(m) => m.fans,
            Model::Gpu(_) | Model::Fans(_) => 0,
        }
    }

    pub fn driver(&self, link: Box<dyn Link>) -> Box<dyn Driver> {
        match self {
            Model::Kraken(m) => Box::new(Kraken::new(link, m)),
            Model::Hydro(m) => Box::new(HydroPro::new(link, m)),
            Model::Gpu(m) => Box::new(GpuFans::new(link, m)),
            Model::Fans(m) => Box::new(FanController::new(link, m)),
        }
    }
}

/// Talks to one opened cooler.
pub trait Driver: Send {
    /// Runs once after opening, before anything else.
    fn initialize(&mut self) -> io::Result<()>;
    fn firmware(&self) -> Option<String>;
    fn status(&mut self) -> io::Result<CoolerStatus>;
    fn set_speed(&mut self, channel: &str, setting: &SpeedSetting) -> io::Result<()>;
    /// `pixels` is set when the screen should show a picture, already in the
    /// screen's layout (see [`super::screen`]).
    fn set_screen(&mut self, _settings: &ScreenSettings, _pixels: Option<&[u8]>) -> io::Result<()> {
        Ok(())
    }
    /// Needs a bulk connection before it can show pictures.
    fn wants_bulk(&self) -> bool {
        false
    }
    fn attach_bulk(&mut self, _bulk: Box<dyn Bulk>) {}
    fn set_light(&mut self, _settings: &LightSettings) -> io::Result<()> {
        Ok(())
    }
}
