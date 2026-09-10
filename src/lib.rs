//! rodisnyaa: Painless audio playback for native and web platforms.

#![allow(rust_analyzer::inactive_code)]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

pub mod decoder;
mod effect;
mod error;
pub mod patch;
mod range;
mod scene;
mod sink;
mod sound;
mod sound_source;
mod timestamp;
mod waveform;
pub mod wsola;

pub use effect::*;
pub use error::*;
pub(crate) use range::*;
pub use rodio::{self, cpal};
pub use scene::*;
pub use sink::*;
pub use sound::{PlaybackEvent, PlaybackState};
pub use sound_source::*;
pub use timestamp::*;
pub use waveform::*;

/// A CPAL audio host that can provide an output device.
pub type Backend = rodio::cpal::HostId;
