//! rodisnyaa: Painless audio playback for native and web platforms.

#![allow(rust_analyzer::inactive_code)]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

mod nyaa;
mod waveform;

pub use nyaa::*;
pub use waveform::*;

// re-export our beloved
pub use rodio;
pub use rodio::cpal;
