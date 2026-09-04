pub use rodio;
pub use rodio::cpal;

use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
use rodio::decoder::DecoderError;
use rodio::source::SeekError;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
use std::io::{Cursor, Error};
use std::path::{Path, PathBuf};
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
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
    mixer_device_sink: Option<MixerDeviceSink>,

    /// rodio [`Player`]
    player: Option<Player>,

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

    /// Whether the offset is the complete position while no source is actively playing.
    position_is_held: bool,

    /// The result of an audio asset being loaded in the browser.
    #[cfg(target_arch = "wasm32")]
    pending_playback: Option<PendingPlayback>,
}

#[cfg(target_arch = "wasm32")]
type PendingPlayback = Rc<RefCell<Option<Result<Arc<[u8]>, NyaaError>>>>;

impl Default for Nyaa {
    fn default() -> Self {
        Self::new()
    }
}

impl Nyaa {
    fn open_default_output() -> (Option<MixerDeviceSink>, Option<Player>) {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                let player = Player::connect_new(sink.mixer());
                sink.log_on_drop(false);

                (Some(sink), Some(player))
            }

            Err(_) => (None, None),
        }
    }

    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let (mixer_device_sink, player) = Self::open_default_output();
        #[cfg(target_arch = "wasm32")]
        let (mixer_device_sink, player) = (None, None);

        Self {
            mixer_device_sink,
            player,
            duration: Mutex::new(None),
            current_shared_bytes: None,
            current_static_bytes: None,
            position_offset: Duration::ZERO,
            position_is_held: true,
            #[cfg(target_arch = "wasm32")]
            pending_playback: None,
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn ensure_audio_output(&mut self) {
        if self.mixer_device_sink.is_some() {
            return;
        }

        (self.mixer_device_sink, self.player) = Self::open_default_output();
    }

    /// When [`MixerDeviceSink`] is dropped a message is logged to stderr or emitted through tracing if the tracing feature is enabled.
    pub fn log_on_drop(&mut self, log: bool) {
        if let Some(mixer_device_sink) = self.mixer_device_sink.as_mut() {
            mixer_device_sink.log_on_drop(log);
        }
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

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    fn play_source<S>(&mut self, mut source: S) -> Result<(), NyaaError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let position = if self.is_empty() {
            match source.total_duration() {
                Some(duration) => self.position_offset.min(duration),
                None => self.position_offset,
            }
        } else {
            Duration::ZERO
        };

        if !position.is_zero() {
            source.try_seek(position).map_err(NyaaError::Seek)?;
        }

        let duration = source.total_duration();
        self.position_offset = position;
        self.position_is_held = true;
        *self.duration.lock().unwrap() = duration;

        #[cfg(target_arch = "wasm32")]
        self.ensure_audio_output();

        let Some(mixer_device_sink) = self.mixer_device_sink.as_ref() else {
            return Ok(());
        };
        let volume = self.player.as_ref().map_or(1.0, Player::volume);
        let new_player = Player::connect_new(mixer_device_sink.mixer());

        new_player.set_volume(volume);
        new_player.append(source);
        new_player.play();

        self.player = Some(new_player);
        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        self.position_offset = position;
        self.position_is_held = false;

        Ok(())
    }

    pub fn play_shared_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());

        let source = Self::decoder_from_shared_bytes(bytes.clone())?;
        self.play_source(source)?;
        self.current_shared_bytes = Some(bytes);

        Ok(())
    }

    /// Loads static bytes and their duration without starting playback.
    pub fn load_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let duration = Self::decoder_from_static_bytes(bytes)?.total_duration();

        if let Some(player) = self.player.as_ref() {
            player.stop();
        }

        self.current_shared_bytes = None;
        self.current_static_bytes = Some(bytes);
        self.position_offset = Duration::ZERO;
        self.position_is_held = true;
        *self.duration.lock().unwrap() = duration;

        Ok(())
    }

    /// Plays bytes with a `'static` lifetime, such as data from `include_bytes!`, without
    /// copying the encoded audio onto the heap.
    pub fn play_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let source = Self::decoder_from_static_bytes(bytes)?;
        self.play_source(source)?;
        self.current_static_bytes = Some(bytes);

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let file = File::open(path).map_err(NyaaError::File)?;
        let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;

        self.play_source(source)
    }

    /// Plays an audio asset using its native path or browser URL.
    pub async fn play_asset(&mut self, asset: &AudioAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = File::open(asset.native_path()).map_err(NyaaError::File)?;
            let source = Decoder::try_from(file).map_err(NyaaError::Decode)?;

            self.play_source(source)
        }

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = fetch_browser_asset(asset.wasm_url()).await?;
            let source = Self::decoder_from_shared_bytes(bytes.clone())?;

            self.play_source(source)?;
            self.current_shared_bytes = Some(bytes);

            Ok(())
        }
    }

    /// Starts playing an audio asset while keeping browser loading state in this instance.
    ///
    /// Native assets start synchronously. Browser assets are fetched in the background and
    /// completed by calling [`Nyaa::poll_pending_playback`].
    pub fn start_asset_playback(&mut self, asset: &AudioAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.play_file(asset.native_path())
        }

        #[cfg(target_arch = "wasm32")]
        {
            if self.pending_playback.is_some() {
                return Ok(());
            }

            self.ensure_audio_output();

            let pending_playback = PendingPlayback::default();
            let pending_result = pending_playback.clone();
            let url = asset.wasm_url().to_owned();

            wasm_bindgen_futures::spawn_local(async move {
                *pending_result.borrow_mut() = Some(fetch_browser_asset(&url).await);
            });

            self.pending_playback = Some(pending_playback);

            Ok(())
        }
    }

    /// Completes a browser asset playback once its fetch has finished.
    ///
    /// Returns `None` while no playback is pending or the current fetch is still in progress.
    pub fn poll_pending_playback(&mut self) -> Option<Result<(), NyaaError>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            None
        }

        #[cfg(target_arch = "wasm32")]
        {
            let result = self.pending_playback.as_ref()?.borrow_mut().take()?;
            self.pending_playback = None;

            Some(result.and_then(|bytes| {
                let source = Self::decoder_from_shared_bytes(bytes.clone())?;
                self.play_source(source)?;
                self.current_shared_bytes = Some(bytes);

                Ok(())
            }))
        }
    }

    /// Returns whether a browser audio asset is currently loading.
    pub fn is_loading(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.pending_playback.is_some()
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
    /// If there is no current source, this updates the reported position without starting
    /// playback.
    pub fn try_seek(&mut self, position: Duration) -> Result<(), NyaaError> {
        if self.set_position_if_empty(position) {
            return Ok(());
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.player.as_ref() {
                Some(player) => {
                    player.try_seek(position).map_err(NyaaError::Seek)?;
                    self.position_offset = Duration::ZERO;
                    self.position_is_held = false;

                    Ok(())
                }
                None => Ok(()),
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.try_seek_with_decode(position)
        }
    }

    /// Convenience version of [`Nyaa::try_seek()`] that accepts the position in seconds.
    pub fn try_seek_secs(&mut self, position_secs: f32) -> Result<(), NyaaError> {
        self.try_seek(Duration::from_secs_f32(position_secs))
    }

    /// Functionally equivalent to `try_seek`, but avoids the deadlock that occurs when calling
    /// [`Player::try_seek`] on the same thread as the audio callback.
    ///
    /// You *can* use this on non-wasm targets, but it will be slower than calling `try_seek`
    /// directly.
    pub fn try_seek_with_decode(&mut self, position: Duration) -> Result<(), NyaaError> {
        if self.set_position_if_empty(position) {
            return Ok(());
        }

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

    fn set_position_if_empty(&mut self, position: Duration) -> bool {
        if !self.is_empty() {
            return false;
        }

        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };
        let volume = self.volume();
        let was_paused = self.is_paused();

        if let Some(mixer_device_sink) = self.mixer_device_sink.as_ref() {
            let new_player = Player::connect_new(mixer_device_sink.mixer());
            new_player.set_volume(volume);

            if was_paused {
                new_player.pause();
            }

            self.player = Some(new_player);
        }

        self.position_offset = position;
        self.position_is_held = true;

        true
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
        let (Some(mixer_device_sink), Some(player)) =
            (self.mixer_device_sink.as_ref(), self.player.as_ref())
        else {
            return Ok(());
        };

        // This decoder is NOT owned by the audio callback yet,
        // therefore this seek executes directly on this thread.

        source.try_seek(position).map_err(NyaaError::Seek)?;

        let was_paused = player.is_paused();
        let volume = player.volume();

        let new_player = Player::connect_new(mixer_device_sink.mixer());

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
        self.player = Some(new_player);

        self.position_offset = position;
        self.position_is_held = false;

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
        let player_position = if self.position_is_held {
            Duration::ZERO
        } else {
            self.player.as_ref().map_or(Duration::ZERO, Player::get_pos)
        };
        let position = self.position_offset.saturating_add(player_position);

        match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Returns the position of the sound that's being played in the format `H:MM:SS` or `M:SS`.
    pub fn position_formatted(&self) -> String {
        format_timestamp(self.position())
    }

    /// Returns the total duration of the audio source.
    pub fn duration(&self) -> Option<Duration> {
        *self.duration.lock().unwrap()
    }

    /// Returns the total duration of the audio source in the format `H:MM:SS` or `M:SS`.
    ///
    /// Falls back to `0:00` if the duration is unknown.
    pub fn duration_formatted(&self) -> String {
        self.duration()
            .map(format_timestamp)
            .unwrap_or_else(|| "0:00".into())
    }

    /// [`Nyaa::position()`] clamped to the range `[0, duration]`.
    ///
    /// This is useful for user input.
    pub fn clamped_position(&self) -> Duration {
        self.position()
            .min(self.duration().unwrap_or_default())
            .max(Duration::ZERO)
    }

    /// Attempts to produce a valid range for a seek slider, even if the duration is unknown.
    ///
    /// Will return `0.0..=1.0` if the duration is unknown, otherwise returns `0.0..=duration`.
    pub fn seek_range(&self) -> std::ops::RangeInclusive<f32> {
        0.0..=self.duration().unwrap_or_default().as_secs_f32().max(1.0)
    }

    /// Pauses playback of this player.
    ///
    /// No effect if already paused.
    ///
    /// A paused sink can be resumed with `play()`.
    pub fn pause(&self) {
        if let Some(player) = self.player.as_ref() {
            player.pause();
        }
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    pub fn resume(&self) {
        if let Some(player) = self.player.as_ref() {
            player.play();
        }
    }

    /// Stops the sink by emptying the queue.
    pub fn stop(&mut self) {
        let position = self.position();

        if let Some(player) = self.player.as_ref() {
            player.stop();
        }

        self.position_offset = position;
        self.position_is_held = true;
    }

    /// Changes the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than `1.0` will
    /// multiply each sample by this value.
    pub fn set_volume(&self, volume: f32) {
        if let Some(player) = self.player.as_ref() {
            player.set_volume(volume);
        }
    }

    /// Gets the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than 1.0 will
    /// multiply each sample by this value.
    pub fn volume(&self) -> f32 {
        self.player.as_ref().map_or(1.0, Player::volume)
    }

    /// Gets if a sink is playing
    ///
    /// Equivalent to `!is_paused() && !is_empty()`.
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is playing.
    pub fn is_playing(&self) -> bool {
        !self.position_is_held
            && self
                .player
                .as_ref()
                .is_some_and(|player| !player.is_paused() && !player.empty())
    }

    /// Gets if a sink is paused
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is paused.
    pub fn is_paused(&self) -> bool {
        self.player.as_ref().is_some_and(Player::is_paused)
    }

    /// Returns true if this sink has no more sounds to play.
    pub fn is_empty(&self) -> bool {
        self.position_is_held || self.player.as_ref().is_none_or(Player::empty)
    }

    /// Sleeps the current thread until the sound ends.
    pub fn wait_until_end(&self) {
        if let Some(player) = self.player.as_ref() {
            player.sleep_until_end();
        }
    }
}

/// Formats a [`Duration`] as a string in the format `H:MM:SS` or `M:SS`.
pub fn format_timestamp(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::thread;

    const TEST_AUDIO_BYTES: &[u8] = include_bytes!("../examples/THE UNFORGIVING.mp3");

    #[test]
    fn seeking_without_a_track_updates_position() {
        let mut nyaa = Nyaa::new();
        let position = Duration::from_secs(42);

        nyaa.try_seek(position)
            .expect("seeking without a track should succeed");

        assert_eq!(nyaa.position(), position);
    }

    #[test]
    fn playback_after_seeking_without_a_track_starts_from_that_position() {
        let mut nyaa = Nyaa::new();
        let requested_position = Duration::from_secs(42);

        nyaa.try_seek(requested_position)
            .expect("seeking without a track should succeed");
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");

        let playback_position = nyaa.position();

        assert!(
            playback_position >= requested_position,
            "playback started at {playback_position:?} instead of {requested_position:?}"
        );
        assert!(
            playback_position < requested_position + Duration::from_secs(1),
            "playback advanced unexpectedly far to {playback_position:?}"
        );
    }

    #[test]
    fn seeking_playback_replaces_the_deferred_position() {
        let mut nyaa = Nyaa::new();

        nyaa.try_seek(Duration::from_secs(42))
            .expect("seeking without a track should succeed");
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        nyaa.try_seek(Duration::ZERO)
            .expect("active playback should seek back to the start");

        let playback_position = nyaa.position();

        assert!(
            playback_position < Duration::from_secs(1),
            "playback reported the stale deferred position {playback_position:?} after seeking to the start"
        );
    }

    #[test]
    fn loading_static_bytes_exposes_duration_without_playing() {
        let mut nyaa = Nyaa::new();
        let expected_duration = Nyaa::duration_from_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio duration should be readable");

        nyaa.load_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be loadable");

        assert_eq!(nyaa.duration(), expected_duration);
        assert!(!nyaa.is_playing());
    }

    #[test]
    fn stopping_playback_holds_the_reported_position() {
        let mut nyaa = Nyaa::new();
        let start_position = Duration::from_secs(42);

        nyaa.try_seek(start_position)
            .expect("seeking without a track should succeed");
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        thread::sleep(Duration::from_millis(25));
        nyaa.stop();

        let stopped_position = nyaa.position();

        assert!(
            stopped_position >= start_position,
            "stopped playback moved backward to {stopped_position:?}"
        );
        assert!(
            stopped_position < start_position + Duration::from_secs(1),
            "stopped playback reported {stopped_position:?} instead of holding near {start_position:?}"
        );

        thread::sleep(Duration::from_millis(50));
        assert_eq!(nyaa.position(), stopped_position);
    }
}
