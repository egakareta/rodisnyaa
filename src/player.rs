#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::io::BufReader;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use std::{
    collections::VecDeque,
    io::Cursor,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
use rodio::{Decoder, Player, Source};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(target_arch = "wasm32")]
use web_sys::Response;

use crate::{
    NyaaError, Output, OutputError, PlaybackRangeSource, SoundAsset, SoundEffects,
    format_timestamp, wsola::Wsola,
};

/// The current playback lifecycle state of a [`crate::Sound`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackState {
    /// No source is currently playing or loading.
    Idle,
    /// An audio asset is being loaded asynchronously.
    Loading,
    /// A source is actively playing.
    Playing,
    /// The current source is paused.
    Paused,
    /// The current source reached the end of playback.
    Ended,
    /// The most recent loading or playback operation failed.
    Failed,
}

/// A playback lifecycle event emitted by [`crate::Sound::poll_event`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackEvent {
    /// The player moved from one lifecycle state to another.
    StateChanged {
        /// The state before the transition.
        previous: PlaybackState,
        /// The state after the transition.
        current: PlaybackState,
    },
}

/// rodisnyaa: Painless audio playback for native and web platforms.
///
/// If you are only playing one audio track at a time, you can use [`SoundPlayer`] directly.
/// If you want to play multiple tracks simultaneously, create an [`Output`] and
/// pass clones to [`SoundPlayer::new_with_output`].
pub(crate) struct SoundPlayer {
    /// The audio output shared by this player.
    pub(crate) output: Output,

    /// An optional scene mixer that receives this player's source before the device output.
    target_mixer: Option<rodio::mixer::Mixer>,

    /// rodio [`Player`]
    player: Option<Player>,

    /// The total duration of the current source, if known.
    duration: Mutex<Option<Duration>>,

    /// The bytes of the current source, if it was played from a heap allocation.
    current_shared_bytes: Option<Arc<[u8]>>,

    /// The bytes of the current source, if it was played from a `'static` lifetime.
    current_static_bytes: Option<&'static [u8]>,

    /// The path of the current source, if it was played from a file.
    #[cfg(not(target_arch = "wasm32"))]
    current_file_path: Option<PathBuf>,

    /// The offset of the current position in the source.
    position_offset: Duration,

    /// The player position corresponding to [`Self::position_offset`].
    player_position_anchor: Duration,
    speed: f32,
    preserve_pitch: bool,
    volume: Mutex<f32>,
    effects: SoundEffects,

    /// Whether playback repeats at its configured boundary.
    looping: Arc<AtomicBool>,

    /// The configured playback boundary within a source.
    loop_range: Option<Range<Duration>>,

    /// Whether the offset is the complete position while no source is actively playing.
    position_is_held: bool,

    /// The last observed playback lifecycle state.
    playback_state: Mutex<PlaybackState>,

    /// Playback lifecycle transitions waiting to be observed.
    playback_events: Mutex<VecDeque<PlaybackEvent>>,

    /// Maximum number of playback lifecycle transitions retained for observation.
    event_capacity: usize,

    /// The result of an audio asset being loaded in the browser.
    #[cfg(target_arch = "wasm32")]
    pending_playback: Option<PendingPlayback>,
}

#[cfg(target_arch = "wasm32")]
type PendingPlayback = Rc<RefCell<Option<Result<Arc<[u8]>, NyaaError>>>>;

impl Default for SoundPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPlayer {
    /// Creates a player with its own default audio output.
    pub fn new() -> Self {
        let sound_player = Self::new_with_output(Output::new());
        sound_player
    }

    /// Creates a player connected to a reusable audio output.
    ///
    /// The preferred backend is initialized to the output's backend, so
    /// [`SoundPlayer::ensure_output`] keeps using it.
    pub fn new_with_output(output: Output) -> Self {
        let player = output.connect_player();

        Self {
            output,
            target_mixer: None,
            player,
            duration: Mutex::new(None),
            current_shared_bytes: None,
            current_static_bytes: None,
            #[cfg(not(target_arch = "wasm32"))]
            current_file_path: None,
            position_offset: Duration::ZERO,
            player_position_anchor: Duration::ZERO,
            speed: 1.0,
            preserve_pitch: false,
            volume: Mutex::new(1.0),
            effects: SoundEffects::default(),
            looping: Arc::new(AtomicBool::new(false)),
            loop_range: None,
            position_is_held: true,
            playback_state: Mutex::new(PlaybackState::Idle),
            playback_events: Mutex::new(VecDeque::new()),
            event_capacity: usize::MAX,
            #[cfg(target_arch = "wasm32")]
            pending_playback: None,
        }
    }

    pub(crate) fn new_with_mixer(output: Output, mixer: rodio::mixer::Mixer) -> Self {
        let mut sound_player = Self::new_with_output(output);
        sound_player.player = Some(Player::connect_new(&mixer));
        sound_player.target_mixer = Some(mixer);
        sound_player
    }

    fn connect_player(&self) -> Option<Player> {
        self.target_mixer
            .as_ref()
            .map(Player::connect_new)
            .or_else(|| self.output.connect_player())
    }

    /// Returns whether this player currently has an initialized audio output.
    ///
    /// A browser player can return `false` until playback initializes WebAudio in response to a
    /// user gesture. A player with a deferred preferred backend (see
    /// [`SoundPlayer::set_preferred_backend`]) also returns `false` until
    /// [`SoundPlayer::ensure_output`] opens it.
    pub fn has_output(&self) -> bool {
        self.target_mixer.is_some() || self.output.has_output()
    }

    /// Retries opening this player's selected audio output.
    ///
    /// This can recover a player created by [`SoundPlayer::new`] after its initial best-effort device
    /// initialization failed.
    pub fn retry_output(&mut self) -> Result<(), OutputError> {
        if let Err(error) = self.output.retry_sink() {
            self.set_playback_state(PlaybackState::Failed);
            return Err(error);
        }

        if self.player.is_none() {
            self.player = self.connect_player();
        }

        Ok(())
    }

    fn has_current_source(&self) -> bool {
        if self.current_shared_bytes.is_some() || self.current_static_bytes.is_some() {
            return true;
        }

        #[cfg(not(target_arch = "wasm32"))]
        if self.current_file_path.is_some() {
            return true;
        }

        #[cfg(target_arch = "wasm32")]
        let _ = ();

        false
    }

    pub(crate) fn replace_output_preserving_playback(&mut self, output: Output) {
        let position = self.position();
        let was_playing = self.is_playing();
        let was_paused = self.is_paused();
        let has_source = self.has_current_source();

        self.output = output;

        // A switch originates from a user gesture, so give a deferred output (for
        // example a browser output waiting on autoplay policy) one chance to open
        // now instead of dropping active playback back to `Idle`. Failures stay
        // lazy: the next `play_*` retries and surfaces them.
        if !self.output.has_output() {
            let _ = self.output.retry_sink();
        }

        if (was_playing || was_paused) && has_source && self.has_output() {
            // Rebuild the decoder on the new output at the same source position.
            // This preserves effects, speed, looping, volume, and the paused state.
            if self.try_seek_with_decode(position).is_err() {
                // The playback state is already `Failed`; just make sure the player
                // is tied to the new output and the position stays held.
                self.player = self.connect_player();

                if let Some(player) = self.player.as_ref() {
                    player.set_volume(self.volume());
                    player.set_speed(self.player_speed());
                }

                self.position_offset = position;
                self.player_position_anchor = Duration::ZERO;
                self.position_is_held = true;
            }

            return;
        }

        if (was_playing || was_paused) && has_source {
            // Deferred output without a sink (for example a browser output waiting for
            // a user gesture): keep the source so the next `play_*` resumes from the
            // held position instead of restarting.
            self.position_offset = position;
            self.player_position_anchor = Duration::ZERO;
            self.position_is_held = true;
            self.player = self.connect_player();
            self.set_playback_state(PlaybackState::Idle);
            return;
        }

        // Idle, ended, loading, failed, or no source: keep the held position,
        // duration, source handles, and lifecycle state; only reconnect an idle player.
        self.player = self.connect_player();

        if let Some(player) = self.player.as_ref() {
            player.set_volume(self.volume());
            player.set_speed(self.player_speed());

            if was_paused {
                player.pause();
            }
        }
    }

    pub(crate) fn replace_routing_preserving_playback(
        &mut self,
        output: Output,
        mixer: rodio::mixer::Mixer,
    ) {
        self.target_mixer = Some(mixer);
        self.replace_output_preserving_playback(output);
    }

    /// Creates a [`Decoder`] from bytes stored in an [`Arc`], which allows multiple players to share the same audio data without copying it onto the heap.
    pub fn decoder_from_shared_bytes(
        bytes: Arc<[u8]>,
    ) -> Result<Decoder<Cursor<Arc<[u8]>>>, NyaaError> {
        let len = bytes.len() as u64;

        Decoder::builder()
            .with_data(Cursor::new(bytes))
            .with_byte_len(len)
            .build()
            .map_err(NyaaError::Decode)
    }

    /// Creates a [`Decoder`] from bytes with a `'static` lifetime, such as data from `include_bytes!`,
    /// without copying the encoded audio onto the heap.
    pub fn decoder_from_static_bytes(
        bytes: &'static [u8],
    ) -> Result<Decoder<Cursor<&'static [u8]>>, NyaaError> {
        Decoder::builder()
            .with_data(Cursor::new(bytes))
            .with_byte_len(bytes.len() as u64)
            .build()
            .map_err(NyaaError::Decode)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn decoder_from_file(
        path: impl AsRef<Path>,
    ) -> Result<Decoder<BufReader<File>>, NyaaError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(NyaaError::File)?;
        let byte_len = file.metadata().map_err(NyaaError::File)?.len();
        let mut builder = Decoder::builder()
            .with_data(BufReader::new(file))
            .with_byte_len(byte_len)
            .with_coarse_seek(true);

        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            builder = builder.with_hint(extension);
        }

        builder.build().map_err(NyaaError::Decode)
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    fn play_source<S>(&mut self, source: S) -> Result<(), NyaaError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        self.play_source_at(source, None)
    }

    fn play_source_at<S>(
        &mut self,
        source: S,
        requested_position: Option<Duration>,
    ) -> Result<(), NyaaError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        self.queue_source_at(source, requested_position, true)
    }

    fn queue_source_at<S>(
        &mut self,
        mut source: S,
        requested_position: Option<Duration>,
        autoplay: bool,
    ) -> Result<(), NyaaError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let duration = source.total_duration();
        let (range_start, range_end) = self
            .playback_bounds(duration)
            .map_err(|error| self.record_failure(error))?;
        let position = if let Some(position) = requested_position {
            position
        } else if self.is_empty() {
            match source.total_duration() {
                Some(duration) => self.position_offset.min(duration),
                None => self.position_offset,
            }
        } else {
            Duration::ZERO
        };
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });

        if !position.is_zero() {
            source
                .try_seek(position)
                .map_err(NyaaError::Seek)
                .map_err(|error| self.record_failure(error))?;
        }

        let source = PlaybackRangeSource::new(
            source,
            range_start,
            range_end,
            position,
            self.looping.clone(),
        );
        let source = self.process_source(source);
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        *self.duration.lock().unwrap() = duration;

        if !self.has_output() {
            self.retry_output()?;
        }

        let new_player = self
            .connect_player()
            .expect("an initialized audio output must accept players");
        let volume = self.volume();

        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());
        if !autoplay {
            new_player.pause();
        }
        new_player.append(source);
        if autoplay {
            new_player.play();
        }

        self.player = Some(new_player);
        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.current_file_path = None;
        }
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = false;
        if autoplay {
            self.set_playback_state(PlaybackState::Playing);
        }

        Ok(())
    }

    fn play_current_source_at(&mut self, position: Duration) -> Result<(), NyaaError> {
        if let Some(bytes) = self.current_shared_bytes.clone() {
            let source = Self::decoder_from_shared_bytes(bytes.clone())
                .map_err(|error| self.record_failure(error))?;
            self.play_source_at(source, Some(position))?;
            self.current_shared_bytes = Some(bytes);
            return Ok(());
        }

        if let Some(bytes) = self.current_static_bytes {
            let source = Self::decoder_from_static_bytes(bytes)
                .map_err(|error| self.record_failure(error))?;
            self.play_source_at(source, Some(position))?;
            self.current_static_bytes = Some(bytes);
            return Ok(());
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.current_file_path.clone() {
            let source =
                Self::decoder_from_file(&path).map_err(|error| self.record_failure(error))?;
            self.play_source_at(source, Some(position))?;
            self.current_file_path = Some(path);
            return Ok(());
        }

        Err(self.record_failure(NyaaError::NoAudioSource))
    }

    fn playback_bounds(
        &self,
        duration: Option<Duration>,
    ) -> Result<(Duration, Option<Duration>), NyaaError> {
        let Some(range) = self.loop_range.as_ref() else {
            return Ok((Duration::ZERO, duration));
        };
        let end = duration.map_or(range.end, |duration| range.end.min(duration));

        if range.start >= end {
            return Err(NyaaError::InvalidPlaybackRange);
        }

        Ok((range.start, Some(end)))
    }

    fn process_source<S>(&self, source: S) -> Box<dyn Source<Item = f32> + Send>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let source = self.effects.apply(source);

        if self.preserve_pitch {
            Box::new(Wsola::new(source, self.speed))
        } else {
            source
        }
    }

    fn player_speed(&self) -> f32 {
        if self.preserve_pitch { 1.0 } else { self.speed }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn player_position_for_source_position(&self, position: Duration) -> Duration {
        if self.preserve_pitch {
            position.div_f32(self.speed)
        } else {
            position
        }
    }

    fn reset_loaded_source(&mut self, duration: Option<Duration>) {
        if let Some(player) = self.player.as_ref() {
            player.stop();
        }

        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.current_file_path = None;
        }
        self.position_offset = Duration::ZERO;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        *self.duration.lock().unwrap() = duration;
        self.set_playback_state(PlaybackState::Idle);
    }

    /// Loads bytes stored in an [`Arc`], which allows multiple players to share the same
    /// audio data without copying it onto the heap, without starting playback.
    pub fn load_shared_arc_bytes(&mut self, bytes: Arc<[u8]>) -> Result<(), NyaaError> {
        let duration = Self::decoder_from_shared_bytes(bytes.clone())
            .map_err(|error| self.record_failure(error))?
            .total_duration();

        self.reset_loaded_source(duration);
        self.current_shared_bytes = Some(bytes);

        Ok(())
    }

    /// Loads static bytes and their duration without starting playback.
    pub fn load_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let duration = Self::decoder_from_static_bytes(bytes)
            .map_err(|error| self.record_failure(error))?
            .total_duration();

        self.reset_loaded_source(duration);
        self.current_static_bytes = Some(bytes);

        Ok(())
    }

    /// Plays bytes with a `'static` lifetime, such as data from `include_bytes!`, without
    /// copying the encoded audio onto the heap.
    #[allow(dead_code)]
    pub fn play_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let source =
            Self::decoder_from_static_bytes(bytes).map_err(|error| self.record_failure(error))?;
        self.play_source(source)?;
        self.current_static_bytes = Some(bytes);

        Ok(())
    }

    /// Loads an audio file from its native path without starting playback.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let path = path.as_ref().to_path_buf();
        let duration = Self::decoder_from_file(&path)
            .map_err(|error| self.record_failure(error))?
            .total_duration();

        self.reset_loaded_source(duration);
        self.current_file_path = Some(path);

        Ok(())
    }

    /// Plays an audio asset using its native path.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let path = path.as_ref().to_path_buf();
        let source = Self::decoder_from_file(&path).map_err(|error| self.record_failure(error))?;

        self.play_source(source)?;
        self.current_file_path = Some(path);

        Ok(())
    }

    /// Starts playing an audio asset while keeping browser loading state in this instance.
    ///
    /// Native assets start synchronously. Browser assets are fetched in the background and
    /// completed by calling [`SoundPlayer::poll_pending_playback`].
    pub fn start_asset_playback(&mut self, asset: &SoundAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.play_file(asset.native_path())
        }

        #[cfg(target_arch = "wasm32")]
        {
            if self.pending_playback.is_some() {
                return Ok(());
            }

            self.retry_output()?;

            if let Some(bytes) = asset.cached_browser_bytes() {
                let source = Self::decoder_from_shared_bytes(bytes.clone())
                    .map_err(|error| self.record_failure(error))?;

                self.play_source(source)?;
                self.current_shared_bytes = Some(bytes);

                return Ok(());
            }

            let pending_playback = PendingPlayback::default();
            let pending_result = pending_playback.clone();
            let asset = asset.clone();

            wasm_bindgen_futures::spawn_local(async move {
                *pending_result.borrow_mut() = Some(asset.load_browser_bytes().await);
            });

            self.pending_playback = Some(pending_playback);
            self.set_playback_state(PlaybackState::Loading);

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

            let result = result.and_then(|bytes| {
                let source = Self::decoder_from_shared_bytes(bytes.clone())?;
                self.play_source(source)?;
                self.current_shared_bytes = Some(bytes);

                Ok(())
            });

            if result.is_err() {
                self.set_playback_state(PlaybackState::Failed);
            }

            Some(result)
        }
    }

    pub(crate) fn cancel_pending_playback(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_playback = None;
        }
    }

    /// Returns the current playback lifecycle state.
    pub fn state(&self) -> PlaybackState {
        self.refresh_playback_state();
        *self.playback_state.lock().unwrap()
    }

    /// Returns the next pending playback lifecycle event.
    ///
    /// Natural playback completion is detected when this method or [`SoundPlayer::state`] is called.
    pub fn poll_event(&self) -> Option<PlaybackEvent> {
        self.refresh_playback_state();
        self.playback_events.lock().unwrap().pop_front()
    }

    fn refresh_playback_state(&self) {
        let state = *self.playback_state.lock().unwrap();

        if matches!(state, PlaybackState::Playing | PlaybackState::Paused)
            && self.player.as_ref().is_none_or(Player::empty)
        {
            self.set_playback_state(PlaybackState::Ended);
        }
    }

    fn set_playback_state(&self, state: PlaybackState) {
        let mut current = self.playback_state.lock().unwrap();

        if *current == state {
            return;
        }

        let previous = *current;
        *current = state;
        drop(current);

        if self.event_capacity == 0 {
            return;
        }

        let mut events = self.playback_events.lock().unwrap();
        if events.len() >= self.event_capacity {
            events.pop_front();
        }
        events.push_back(PlaybackEvent::StateChanged {
            previous,
            current: state,
        });
    }

    fn record_failure(&self, error: NyaaError) -> NyaaError {
        self.set_playback_state(PlaybackState::Failed);
        error
    }

    /// Returns whether a browser audio asset is currently loading.
    pub fn is_loading(&self) -> bool {
        self.state() == PlaybackState::Loading
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
                    let player_position = self.player_position_for_source_position(position);
                    player
                        .try_seek(player_position)
                        .map_err(NyaaError::Seek)
                        .map_err(|error| self.record_failure(error))?;
                    self.position_offset = position;
                    self.player_position_anchor = player_position;
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

    /// Convenience version of [`SoundPlayer::try_seek()`] that accepts the position in seconds.
    pub fn try_seek_secs(&mut self, position_secs: f64) -> Result<(), NyaaError> {
        if position_secs.is_nan() || position_secs.is_infinite() || position_secs < 0.0 {
            return Err(self.record_failure(NyaaError::InvalidSeekPosition));
        }

        self.try_seek(Duration::from_secs_f64(position_secs))
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

        if let Some(bytes) = self.current_shared_bytes.clone() {
            let source = Self::decoder_from_shared_bytes(bytes)?;
            return self.seek_with_decode_source(source, position);
        }

        if let Some(bytes) = self.current_static_bytes {
            let source = Self::decoder_from_static_bytes(bytes)?;
            return self.seek_with_decode_source(source, position);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.current_file_path.clone() {
            let source = Self::decoder_from_file(path)?;
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

        if let Some(new_player) = self.connect_player() {
            new_player.set_volume(volume);
            new_player.set_speed(self.player_speed());

            if was_paused {
                new_player.pause();
            }

            self.player = Some(new_player);
        }

        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
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
        let Some(player) = self.player.as_ref() else {
            return Ok(());
        };

        // This decoder is NOT owned by the audio callback yet,
        // therefore this seek executes directly on this thread.

        let (range_start, range_end) = self
            .playback_bounds(source.total_duration())
            .map_err(|error| self.record_failure(error))?;
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });
        source
            .try_seek(position)
            .map_err(NyaaError::Seek)
            .map_err(|error| self.record_failure(error))?;
        let source = PlaybackRangeSource::new(
            source,
            range_start,
            range_end,
            position,
            self.looping.clone(),
        );
        let source = self.process_source(source);

        let was_paused = player.is_paused();
        let volume = self.volume();

        let new_player = self
            .connect_player()
            .expect("an initialized audio output must accept players");

        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());

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
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = false;

        Ok(())
    }

    /// Returns the current rodio post-processing settings.
    pub fn effects(&self) -> SoundEffects {
        self.effects
    }

    /// Replaces the rodio post-processing chain.
    ///
    /// If a source is playing, its decoder is rebuilt at the current position and keeps its
    /// paused state. Loaded or stopped sources use the settings the next time playback starts.
    pub fn set_effects(&mut self, effects: SoundEffects) -> Result<(), NyaaError> {
        effects.validate()?;

        if effects == self.effects {
            return Ok(());
        }

        if self.is_empty() {
            self.effects = effects;
            return Ok(());
        }

        let position = self.position();
        let previous_effects = std::mem::replace(&mut self.effects, effects);
        let result = self.try_seek_with_decode(position);

        if result.is_err() {
            self.effects = previous_effects;
        }

        result
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

        self.position_at_player_time(player_position)
    }

    fn position_at_player_time(&self, player_position: Duration) -> Duration {
        let elapsed = player_position.saturating_sub(self.player_position_anchor);
        let mut position = self
            .position_offset
            .saturating_add(elapsed.mul_f32(self.speed));

        if self.is_looping()
            && let Ok((start, Some(end))) = self.playback_bounds(self.duration())
            && position >= end
        {
            let range_duration = end - start;
            let range_nanos = range_duration.as_nanos();

            if range_nanos > 0 {
                let position_nanos = position.saturating_sub(start).as_nanos() % range_nanos;
                position = start
                    + Duration::new(
                        (position_nanos / 1_000_000_000) as u64,
                        (position_nanos % 1_000_000_000) as u32,
                    );
            }
        }

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

    /// [`SoundPlayer::position()`] clamped to the range `[0, duration]`.
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
    pub fn seek_range(&self) -> std::ops::RangeInclusive<f64> {
        0.0..=self.duration().unwrap_or_default().as_secs_f64().max(1.0)
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

        if self.state() == PlaybackState::Playing {
            self.set_playback_state(PlaybackState::Paused);
        }
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    pub fn resume(&self) {
        if let Some(player) = self.player.as_ref() {
            player.play();
        }

        if self.state() == PlaybackState::Paused {
            self.set_playback_state(PlaybackState::Playing);
        }
    }

    /// Stops the sink by emptying the queue.
    pub fn stop(&mut self) {
        let position = self.position();

        if let Some(player) = self.player.as_ref() {
            player.stop();
        }

        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.current_file_path = None;
        }
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        self.set_playback_state(PlaybackState::Idle);
    }

    /// Stops playback but retains the source, metadata, and position.
    ///
    /// Like [`SoundPlayer::stop`], this empties playback, holds the reported position, and
    /// moves to [`PlaybackState::Idle`]. Unlike `stop`, it keeps the loaded source
    /// handles and duration so [`SoundPlayer::play_at`] or [`SoundPlayer::play_range`] can rebuild
    /// playback without reloading.
    pub fn stop_preserving_source(&mut self) {
        let position = self.position();

        if let Some(player) = self.player.as_ref() {
            player.stop();
        }

        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        self.set_playback_state(PlaybackState::Idle);
    }

    /// Seeks to `position` and starts playback.
    ///
    /// If a source is actively playing or paused, this seeks in place and resumes
    /// playing. Otherwise, if a source was loaded, stopped with
    /// [`SoundPlayer::stop_preserving_source`], or ended, this rebuilds playback from the
    /// retained source at `position`.
    ///
    /// The requested position is clamped to the known duration and the configured
    /// playback bounds (see [`SoundPlayer::set_loop_range`]).
    ///
    /// Returns [`NyaaError::NoAudioSource`] when no source is available.
    pub fn try_play_at(&mut self, position: Duration) -> Result<(), NyaaError> {
        if !self.has_current_source() {
            return Err(self.record_failure(NyaaError::NoAudioSource));
        }

        let duration = self.duration();
        let (range_start, range_end) = self
            .playback_bounds(duration)
            .map_err(|error| self.record_failure(error))?;
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });

        if (self.is_playing() || self.is_paused()) && self.player.is_some() {
            self.try_seek(position)?;
            self.resume();
            return Ok(());
        }

        self.play_current_source_at(position)
    }

    /// Changes the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than `1.0` will
    /// multiply each sample by this value.
    pub fn set_volume(&self, volume: f32) {
        *self.volume.lock().unwrap() = volume;

        if let Some(player) = self.player.as_ref() {
            player.set_volume(volume);
        }
    }

    /// Gets the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than 1.0 will
    /// multiply each sample by this value.
    pub fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }

    /// Changes playback speed and reports errors from rebuilding a pitch-preserving source.
    pub fn try_set_speed(&mut self, speed: f32) -> Result<(), NyaaError> {
        if self.preserve_pitch && self.speed != speed && !self.is_empty() {
            let position = self.position();
            let previous_speed = std::mem::replace(&mut self.speed, speed);
            let result = self.try_seek_with_decode(position);

            if result.is_err() {
                self.speed = previous_speed;
            }

            return result;
        }

        if !self.position_is_held {
            let player_position = self.player.as_ref().map_or(Duration::ZERO, Player::get_pos);
            self.position_offset = self.position_at_player_time(player_position);
            self.player_position_anchor = player_position;
        }

        self.speed = speed;

        if let Some(player) = self.player.as_ref() {
            player.set_speed(self.player_speed());
        }

        Ok(())
    }

    /// Gets the playback speed of the sound.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Enables or disables pitch preservation for playback speed changes.
    ///
    /// Active playback is rebuilt at its current source position so the new mode takes effect
    /// immediately while preserving whether playback is paused.
    pub fn set_preserve_pitch(&mut self, preserve_pitch: bool) -> Result<(), NyaaError> {
        if preserve_pitch == self.preserve_pitch {
            return Ok(());
        }

        if self.is_empty() {
            self.preserve_pitch = preserve_pitch;

            if let Some(player) = self.player.as_ref() {
                player.set_speed(self.player_speed());
            }

            return Ok(());
        }

        let position = self.position();
        self.preserve_pitch = preserve_pitch;
        let result = self.try_seek_with_decode(position);

        if result.is_err() {
            self.preserve_pitch = !preserve_pitch;
        }

        result
    }

    /// Returns whether playback speed changes preserve the source pitch.
    pub fn preserves_pitch(&self) -> bool {
        self.preserve_pitch
    }

    /// Enables or disables repeating at the configured playback boundary.
    ///
    /// Without a loop range, the complete source repeats. Changes apply to active playback at
    /// its next boundary as well as to future sources.
    pub fn set_looping(&self, looping: bool) {
        self.looping.store(looping, Ordering::Relaxed);
    }

    /// Returns whether playback repeats at its configured boundary.
    pub fn is_looping(&self) -> bool {
        self.looping.load(Ordering::Relaxed)
    }

    /// Sets the source-time range used as the playback and looping boundary.
    ///
    /// The range applies to active and future playback. Its end is clamped to the source duration.
    pub fn set_loop_range(&mut self, range: Range<Duration>) -> Result<(), NyaaError> {
        if range.start >= range.end {
            return Err(NyaaError::InvalidPlaybackRange);
        }

        let state = self.state();
        let position = self.position().clamp(range.start, range.end);
        let previous_range = self.loop_range.replace(range);

        if matches!(state, PlaybackState::Playing | PlaybackState::Paused)
            && let Err(error) = self.try_seek_with_decode(position)
        {
            self.loop_range = previous_range;
            return Err(error);
        }

        Ok(())
    }

    /// Returns the configured playback and looping range.
    pub fn loop_range(&self) -> Option<Range<Duration>> {
        self.loop_range.clone()
    }

    /// Removes the configured playback and looping range.
    pub fn clear_loop_range(&mut self) {
        self.loop_range = None;
    }

    /// Gets if a sink is playing
    ///
    /// Equivalent to `!is_paused() && !is_empty()`.
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is playing.
    pub fn is_playing(&self) -> bool {
        self.state() == PlaybackState::Playing
    }

    /// Gets if a sink is paused
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is paused.
    pub fn is_paused(&self) -> bool {
        self.state() == PlaybackState::Paused
    }

    /// Returns true if this sink has no more sounds to play.
    pub fn is_empty(&self) -> bool {
        !matches!(self.state(), PlaybackState::Playing | PlaybackState::Paused)
    }

    /// Sleeps the current thread until the sound ends.
    pub fn wait_until_end(&self) {
        if let Some(player) = self.player.as_ref() {
            player.sleep_until_end();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::thread;

    use super::*;

    const TEST_AUDIO_BYTES: &[u8] = include_bytes!("../examples/THE UNFORGIVING.mp3");

    #[test]
    fn seeking_without_a_track_updates_position() {
        let mut sound_player = SoundPlayer::new();
        let position = Duration::from_secs(42);

        sound_player
            .try_seek(position)
            .expect("seeking without a track should succeed");

        assert_eq!(sound_player.position(), position);
    }

    #[test]
    fn playback_after_seeking_without_a_track_starts_from_that_position() {
        let mut sound_player = SoundPlayer::new();
        let requested_position = Duration::from_secs(42);

        sound_player
            .try_seek(requested_position)
            .expect("seeking without a track should succeed");
        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");

        let playback_position = sound_player.position();

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
        let mut sound_player = SoundPlayer::new();

        sound_player
            .try_seek(Duration::from_secs(42))
            .expect("seeking without a track should succeed");
        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        sound_player
            .try_seek(Duration::ZERO)
            .expect("active playback should seek back to the start");

        let playback_position = sound_player.position();

        assert!(
            playback_position < Duration::from_secs(1),
            "playback reported the stale deferred position {playback_position:?} after seeking to the start"
        );
    }

    #[test]
    fn playback_state_and_events_follow_transport_controls() {
        let mut sound_player = SoundPlayer::new();

        assert_eq!(sound_player.state(), PlaybackState::Idle);
        assert_eq!(sound_player.poll_event(), None);

        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        assert_eq!(sound_player.state(), PlaybackState::Playing);
        assert_eq!(
            sound_player.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Idle,
                current: PlaybackState::Playing,
            })
        );

        sound_player.pause();
        assert_eq!(sound_player.state(), PlaybackState::Paused);
        assert_eq!(
            sound_player.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Playing,
                current: PlaybackState::Paused,
            })
        );

        sound_player.stop();
        assert_eq!(sound_player.state(), PlaybackState::Idle);
        assert_eq!(
            sound_player.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Paused,
                current: PlaybackState::Idle,
            })
        );
    }

    #[test]
    fn configured_range_bounds_playback_and_can_loop() {
        let mut sound_player = SoundPlayer::new();
        let range = Duration::from_secs(42)..Duration::from_millis(42_050);

        sound_player
            .set_loop_range(range.clone())
            .expect("a non-empty range should be accepted");
        sound_player.set_looping(true);
        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("the configured range should be playable");
        thread::sleep(Duration::from_millis(125));

        assert_eq!(sound_player.state(), PlaybackState::Playing);
        assert!(range.contains(&sound_player.position()));
    }

    #[test]
    fn players_sharing_an_output_keep_independent_state() {
        let output = Output::new();
        let mut first = SoundPlayer::new_with_output(output.clone());
        let mut second = SoundPlayer::new_with_output(output);

        first
            .load_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be loadable by the first player");
        second
            .load_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be loadable by the second player");
        first
            .try_seek(Duration::from_secs(42))
            .expect("the first player should seek independently");

        assert_eq!(first.position(), Duration::from_secs(42));
        assert_eq!(second.position(), Duration::ZERO);
        assert_eq!(first.duration(), second.duration());
    }

    #[test]
    fn stopping_playback_holds_the_reported_position() {
        let mut sound_player = SoundPlayer::new();
        let start_position = Duration::from_secs(42);

        sound_player
            .try_seek(start_position)
            .expect("seeking without a track should succeed");
        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        thread::sleep(Duration::from_millis(25));
        sound_player.stop();

        let stopped_position = sound_player.position();

        assert!(
            stopped_position >= start_position,
            "stopped playback moved backward to {stopped_position:?}"
        );

        thread::sleep(Duration::from_millis(50));
        assert_eq!(sound_player.position(), stopped_position);
    }

    #[test]
    fn playback_preserves_volume_selected_before_a_track() {
        let mut sound_player = SoundPlayer::new();

        sound_player.set_volume(0.35);
        assert_eq!(sound_player.volume(), 0.35);

        sound_player
            .play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");

        assert_eq!(sound_player.volume(), 0.35);
    }

    #[test]
    fn output_device_enumeration_reports_usable_devices() {
        let backends = Output::available_backends();
        let devices = Output::available_devices();

        let mut seen_names = std::collections::HashSet::new();
        for device in &devices {
            assert!(
                !device.name().is_empty(),
                "enumerated device has an empty name"
            );
            assert!(
                seen_names.insert(device.name().to_string()),
                "combined device list contains duplicate name {:?}",
                device.name()
            );
            assert!(
                backends.contains(&device.backend()),
                "device {} reports unknown backend {:?}",
                device.name(),
                device.backend()
            );
            assert_eq!(device.description().name(), device.name());
            assert!(
                device.to_string().contains(device.name()),
                "device display should contain its name"
            );
        }

        let mut defaults_per_backend = std::collections::HashMap::new();
        for device in &devices {
            if device.is_default() {
                *defaults_per_backend.entry(device.backend()).or_insert(0) += 1;
            }
        }

        for (backend, defaults) in defaults_per_backend {
            assert!(
                defaults <= 1,
                "backend {backend} reports {defaults} default devices"
            );
        }

        for backend in backends {
            match Output::available_devices_for_backend(backend) {
                Ok(backend_devices) => {
                    assert!(
                        backend_devices
                            .iter()
                            .all(|device| device.backend() == backend),
                        "backend {backend} listed a device from another backend"
                    );
                }
                Err(error) => {
                    assert!(
                        matches!(error, OutputError::ListDevices { .. }),
                        "unexpected enumeration error for backend {backend}: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn refreshed_enumeration_populates_the_caches() {
        let backends = Output::refresh_available_backends();
        assert_eq!(Output::available_backends(), backends);

        let devices = Output::refresh_available_devices();
        assert_eq!(Output::available_devices(), devices);
    }
}
