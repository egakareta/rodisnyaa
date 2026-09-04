pub use rodio;
pub use rodio::cpal;

use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
use rodio::decoder::DecoderError;
use rodio::source::SeekError;
use rodio::{Decoder, DeviceSinkBuilder, DeviceSinkError, MixerDeviceSink, Player, Source};
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
use std::io::{Cursor, Error};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(target_arch = "wasm32")]
use web_sys::Response;

/// The error type for [`Nyaa`].
#[derive(Debug, Error)]
pub enum NyaaError {
    /// Errors that might occur when interfacing with audio output.
    #[error("failed to open audio output: {0}")]
    Output(#[source] DeviceSinkError),

    /// The error type for I/O operations of the Read, Write, Seek, and associated traits.
    #[error("failed to open audio file: {0}")]
    File(#[source] Error),

    /// Errors that can occur when creating a decoder.
    #[error("failed to decode audio file: {0}")]
    Decode(#[source] DecoderError),

    /// Occurs when `try_seek` fails because the underlying decoder has an error or does not support seeking.
    #[error("failed to seek audio: {0}")]
    Seek(#[source] SeekError),

    /// Errors that might occur when loading an audio asset in a browser.
    #[cfg(target_arch = "wasm32")]
    #[error("failed to load browser audio asset: {0}")]
    BrowserAsset(String),
}

/// An audio file with locations for native and browser targets.
///
/// Native targets open [`native_path`](Self::native_path) as a file and stream it through the
/// decoder. WASM targets fetch [`wasm_url`](Self::wasm_url) from the browser and decode the
/// response in memory, because browser requests are not exposed as seekable Rust readers.
#[derive(Clone, Debug)]
pub struct AudioAsset {
    native_path: PathBuf,
    wasm_url: String,
}

impl AudioAsset {
    /// Creates an asset using a native filesystem path and a browser URL.
    pub fn new(native_path: impl Into<PathBuf>, wasm_url: impl Into<String>) -> Self {
        Self {
            native_path: native_path.into(),
            wasm_url: wasm_url.into(),
        }
    }

    /// Returns the native filesystem path.
    pub fn native_path(&self) -> &Path {
        &self.native_path
    }

    /// Returns the browser URL.
    pub fn wasm_url(&self) -> &str {
        &self.wasm_url
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_asset_error(error: impl std::fmt::Debug) -> NyaaError {
    NyaaError::BrowserAsset(format!("{error:?}"))
}

#[cfg(target_arch = "wasm32")]
async fn fetch_browser_asset(url: &str) -> Result<Arc<[u8]>, NyaaError> {
    let window = web_sys::window()
        .ok_or_else(|| NyaaError::BrowserAsset("browser window is unavailable".to_string()))?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(browser_asset_error)?
        .dyn_into::<Response>()
        .map_err(browser_asset_error)?;

    if !response.ok() {
        return Err(NyaaError::BrowserAsset(format!(
            "request returned HTTP status {} {}",
            response.status(),
            response.status_text()
        )));
    }

    let buffer = JsFuture::from(response.array_buffer().map_err(browser_asset_error)?)
        .await
        .map_err(browser_asset_error)?;
    let bytes = Uint8Array::new(&buffer).to_vec();

    Ok(Arc::from(bytes))
}

/// rodisnyaa
///
/// Painless audio playback for native and web platforms.
pub struct Nyaa {
    /// rodio [`MixerDeviceSink`]
    mixer_device_sink: MixerDeviceSink,

    /// rodio [`Player`]
    player: Player,

    /// The total duration of the current source, if known.
    ///
    /// You should probably use [`Nyaa::duration()`] instead.
    duration: Mutex<Option<Duration>>,

    /// The bytes of the current source, if it was played from a heap allocation.
    current_shared_bytes: Option<Arc<[u8]>>,

    /// The bytes of the current source, if it was played from a `'static` lifetime.
    current_static_bytes: Option<&'static [u8]>,

    /// The offset of the current position in the source.
    ///
    /// You should probably use [`Nyaa::position()`] instead.
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
            current_shared_bytes: None,
            current_static_bytes: None,
            position_offset: Duration::ZERO,
        })
    }

    /// When [`MixerDeviceSink`] is dropped a message is logged to stderr or emitted through tracing if the tracing feature is enabled.
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

    fn decoder_from_static_bytes(
        bytes: &'static [u8],
    ) -> Result<Decoder<Cursor<&'static [u8]>>, NyaaError> {
        Decoder::builder()
            .with_data(Cursor::new(bytes))
            .with_byte_len(bytes.len() as u64)
            .build()
            .map_err(NyaaError::Decode)
    }

    fn fresh_player(&self) -> Player {
        Player::connect_new(self.mixer_device_sink.mixer())
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
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
        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        self.position_offset = Duration::ZERO;
        *self.duration.lock().unwrap() = duration;
    }

    pub fn play_shared_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());

        let source = Self::decoder_from_shared_bytes(bytes.clone())?;
        self.play_source(source);
        self.current_shared_bytes = Some(bytes);

        Ok(())
    }

    /// Plays bytes with a `'static` lifetime, such as data from `include_bytes!`, without
    /// copying the encoded audio onto the heap.
    pub fn play_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let source = Self::decoder_from_static_bytes(bytes)?;
        self.play_source(source);
        self.current_static_bytes = Some(bytes);

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;

        self.play_source(source);

        Ok(())
    }

    /// Plays an audio asset using its native path or browser URL.
    pub async fn play_asset(&mut self, asset: &AudioAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = File::open(asset.native_path()).map_err(NyaaError::File)?;
            let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;

            self.play_source(source);

            Ok(())
        }

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = fetch_browser_asset(asset.wasm_url()).await?;
            let source = Self::decoder_from_shared_bytes(bytes.clone())?;

            self.play_source(source);
            self.current_shared_bytes = Some(bytes);

            Ok(())
        }
    }

    /// Attempts to seek to a given position in the current source.
    ///
    /// This blocks between 0 and ~5 milliseconds.
    ///
    /// As long as the duration of the source is known, seek is guaranteed to saturate
    /// at the end of the source. For example given a source that reports a total duration
    /// of 42 seconds calling `try_seek()` with 60 seconds as argument will seek to
    /// 42 seconds.
    ///
    /// # Errors
    /// This function will return [`SeekError::NotSupported`] if one of the underlying
    /// sources does not support seeking.
    ///
    /// It will return an error if an implementation ran
    /// into one during the seek.
    ///
    /// When seeking beyond the end of a source this
    /// function might return an error if the duration of the source is not known.
    pub fn try_seek(&mut self, position: Duration) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.player.try_seek(position).map_err(NyaaError::Seek)
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
        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };

        if let Some(bytes) = self.current_shared_bytes.as_ref() {
            let source = Self::decoder_from_shared_bytes(bytes.clone())?;
            return self.seek_with_decode_source(source, position);
        }

        if let Some(bytes) = self.current_static_bytes {
            let source = Self::decoder_from_static_bytes(bytes)?;
            return self.seek_with_decode_source(source, position);
        }

        Ok(())
    }

    fn seek_with_decode_source<S>(
        &mut self,
        mut source: S,
        position: Duration,
    ) -> Result<(), NyaaError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        // This decoder is NOT owned by the audio callback yet,
        // therefore this seek executes directly on this thread.

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

    /// Returns the duration of an audio asset using its native path or browser URL.
    pub async fn duration_from_asset(asset: &AudioAsset) -> Result<Option<Duration>, NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = File::open(asset.native_path()).map_err(NyaaError::File)?;
            Ok(Decoder::try_from(file)
                .map_err(NyaaError::Decode)?
                .total_duration())
        }

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = fetch_browser_asset(asset.wasm_url()).await?;
            Ok(Self::decoder_from_shared_bytes(bytes)?.total_duration())
        }
    }

    pub fn duration_from_shared_bytes(
        bytes: impl AsRef<[u8]>,
    ) -> Result<Option<Duration>, NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());
        Ok(Self::decoder_from_shared_bytes(bytes)?.total_duration())
    }

    /// Returns the duration of static bytes, such as data from `include_bytes!`, without copying
    /// the encoded audio onto the heap.
    pub fn duration_from_static_bytes(bytes: &'static [u8]) -> Result<Option<Duration>, NyaaError> {
        Ok(Self::decoder_from_static_bytes(bytes)?.total_duration())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn duration_from_file(path: impl AsRef<Path>) -> Result<Option<Duration>, NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        Ok(Decoder::try_from(file)
            .map_err(NyaaError::Decode)?
            .total_duration())
    }

    /// Returns the position of the sound that's being played.
    ///
    /// This takes into account any speedup or delay applied.
    ///
    /// Example: if you apply a speedup of *2* to an mp3 decoder source and
    /// [`get_pos()`](Player::get_pos) returns *5s* then the position in the mp3
    /// recording is *10s* from its start.
    pub fn position(&self) -> Duration {
        let position = self.position_offset.saturating_add(self.player.get_pos());

        match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Returns the total duration of the audio source.
    pub fn duration(&self) -> Option<Duration> {
        *self.duration.lock().unwrap()
    }

    /// Pauses playback of this player.
    ///
    /// No effect if already paused.
    ///
    /// A paused sink can be resumed with `play()`.
    pub fn pause(&self) {
        self.player.pause();
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    pub fn resume(&self) {
        self.player.play();
    }

    /// Stops the sink by emptying the queue.
    pub fn stop(&self) {
        self.player.stop();
    }

    /// Changes the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than `1.0` will
    /// multiply each sample by this value.
    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }

    /// Gets the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than 1.0 will
    /// multiply each sample by this value.
    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    /// Gets if a sink is playing
    ///
    /// Equivalent to the inverse of [`Nyaa::is_paused()`].
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is playing.
    pub fn is_playing(&self) -> bool {
        !self.player.is_paused()
    }

    /// Gets if a sink is paused
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is paused.
    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    /// Returns true if this sink has no more sounds to play.
    pub fn is_empty(&self) -> bool {
        self.player.empty()
    }

    /// Sleeps the current thread until the sound ends.
    pub fn wait_until_end(&self) {
        self.player.sleep_until_end();
    }
}
