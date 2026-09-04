pub use rodio;
pub use rodio::cpal;

use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use rodio::{Decoder, DeviceSinkBuilder, Player};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NyaaError {
    #[error("failed to open audio output: {0}")]
    Output(#[source] rodio::DeviceSinkError),
    #[error("failed to open audio file: {0}")]
    File(#[source] std::io::Error),
    #[error("failed to decode audio file: {0}")]
    Decode(#[source] rodio::decoder::DecoderError),
}

pub struct Nyaa {
    mixer_device_sink: rodio::MixerDeviceSink,
    player: Player,
}

impl Nyaa {
    pub fn new() -> Result<Self, NyaaError> {
        let mut mixer_device_sink =
            DeviceSinkBuilder::open_default_sink().map_err(NyaaError::Output)?;
        let player = Player::connect_new(mixer_device_sink.mixer());
        mixer_device_sink.log_on_drop(false);
        Ok(Self {
            mixer_device_sink,
            player,
        })
    }

    pub fn log_on_drop(&mut self, log: bool) {
        self.mixer_device_sink.log_on_drop(log);
    }

    pub fn play_file(&self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;
        self.player.append(source);
        self.player.play();

        Ok(())
    }

    pub fn play_bytes(&self, bytes: impl AsRef<[u8]>) -> Result<(), NyaaError> {
        let source =
            Decoder::try_from(Cursor::new(bytes.as_ref().to_vec())).map_err(NyaaError::Decode)?;
        self.player.append(source);
        self.player.play();

        Ok(())
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn resume(&self) {
        self.player.play();
    }

    pub fn stop(&self) {
        self.player.stop();
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }

    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn is_empty(&self) -> bool {
        self.player.empty()
    }

    pub fn wait_until_end(&self) {
        self.player.sleep_until_end();
    }
}
