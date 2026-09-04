pub use rodio;
pub use rodio::cpal;

use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::{fs::File, io::Cursor, path::Path};

use rodio::{Decoder, DeviceSinkBuilder, Player, Source};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NyaaError {
    #[error("failed to open audio output: {0}")]
    Output(#[source] rodio::DeviceSinkError),

    #[error("failed to open audio file: {0}")]
    File(#[source] std::io::Error),

    #[error("failed to decode audio file: {0}")]
    Decode(#[source] rodio::decoder::DecoderError),

    #[error("failed to seek audio: {0}")]
    Seek(#[source] rodio::source::SeekError),
}

pub struct Nyaa {
    mixer_device_sink: rodio::MixerDeviceSink,
    player: Player,
    duration: Mutex<Option<Duration>>,
    current_bytes: Option<Arc<[u8]>>,
    position_offset: Duration,
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
            duration: Mutex::new(None),
            current_bytes: None,
            position_offset: Duration::ZERO,
        })
    }

    pub fn log_on_drop(&mut self, log: bool) {
        self.mixer_device_sink.log_on_drop(log);
    }

    fn decoder_from_shared_bytes(
        bytes: Arc<[u8]>,
    ) -> Result<Decoder<Cursor<Arc<[u8]>>>, NyaaError> {
        let len = bytes.len() as u64;

        Decoder::builder()
            .with_data(Cursor::new(bytes))
            .with_byte_len(len)
            .build()
            .map_err(NyaaError::Decode)
    }

    fn fresh_player(&self) -> Player {
        Player::connect_new(self.mixer_device_sink.mixer())
    }

    fn play_source<S>(&mut self, source: S)
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let duration = source.total_duration();
        let new_player = self.fresh_player();

        new_player.set_volume(self.player.volume());
        new_player.append(source);
        new_player.play();

        self.player = new_player;
        self.current_bytes = None;
        self.position_offset = Duration::ZERO;
        *self.duration.lock().unwrap() = duration;
    }

    pub fn play_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());

        let source = Self::decoder_from_shared_bytes(bytes.clone())?;
        self.play_source(source);
        self.current_bytes = Some(bytes);

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;

        self.play_source(source);

        Ok(())
    }

    pub fn try_seek(&mut self, position: Duration) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return self.player.try_seek(position).map_err(NyaaError::Seek);
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.try_seek_with_decode(position)
        }
    }

    /// Functionally equivalent to `try_seek`, but avoids the deadlock that occurs when calling
    /// [`Player::try_seek`] on the same thread as the audio callback.
    ///
    /// You *can* use this on non-wasm targets, but it will be slower than calling `try_seek`
    /// directly.
    pub fn try_seek_with_decode(&mut self, position: Duration) -> Result<(), NyaaError> {
        let Some(bytes) = self.current_bytes.as_ref() else {
            return Ok(());
        };

        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };

        // This decoder is NOT owned by the audio callback yet,
        // therefore this seek executes directly on this thread.
        let mut source = Self::decoder_from_shared_bytes(bytes.clone())?;

        source.try_seek(position).map_err(NyaaError::Seek)?;

        let was_paused = self.player.is_paused();
        let volume = self.player.volume();

        let new_player = self.fresh_player();

        new_player.set_volume(volume);

        if was_paused {
            new_player.pause();
        }

        new_player.append(source);

        if !was_paused {
            new_player.play();
        }

        // Dropping the old Player only marks its source as stopped.
        // It does not synchronously wait for WebAudio.
        self.player = new_player;

        self.position_offset = position;

        Ok(())
    }

    pub fn duration_from_bytes(bytes: impl AsRef<[u8]>) -> Result<Option<Duration>, NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());
        Ok(Self::decoder_from_shared_bytes(bytes)?.total_duration())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn duration_from_file(path: impl AsRef<Path>) -> Result<Option<Duration>, NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        Ok(Decoder::try_from(file)
            .map_err(NyaaError::Decode)?
            .total_duration())
    }

    pub fn position(&self) -> Duration {
        let position = self.position_offset.saturating_add(self.player.get_pos());

        match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    pub fn duration(&self) -> Option<Duration> {
        *self.duration.lock().unwrap()
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
