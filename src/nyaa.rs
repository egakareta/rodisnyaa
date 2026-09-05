use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
use rodio::cpal::traits::HostTrait;
use rodio::decoder::DecoderError;
use rodio::source::{AutomaticGainControlSettings, LimitSettings, SeekError};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::io::BufReader;
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

    /// An audio effect setting is outside its supported range.
    #[error("invalid audio effect setting: {0}")]
    InvalidEffect(&'static str),

    /// Errors that might occur when loading an audio asset in a browser.
    #[cfg(target_arch = "wasm32")]
    #[error("failed to load browser audio asset: {0}")]
    BrowserAsset(String),
}

/// A CPAL audio host that can provide an output device.
pub type AudioBackend = rodio::cpal::HostId;

/// An error that can occur while opening an audio backend.
#[derive(Debug, Error)]
pub enum AudioOutputError {
    /// The requested backend is not available on this system.
    #[error("audio backend {backend} is unavailable: {source}")]
    BackendUnavailable {
        /// The backend that was requested.
        backend: AudioBackend,
        /// The error returned by CPAL while initializing the backend.
        #[source]
        source: rodio::cpal::HostUnavailable,
    },

    /// The requested backend has no output device.
    #[error("audio backend {0} has no output device")]
    NoOutputDevice(AudioBackend),

    /// The requested backend's output stream could not be opened.
    #[error("failed to open audio backend {backend}: {source}")]
    OpenStream {
        /// The backend that was requested.
        backend: AudioBackend,
        /// The error returned by rodio while opening the output stream.
        #[source]
        source: rodio::stream::DeviceSinkError,
    },
}

/// An audio file with locations for native and browser targets.
///
/// Native targets open [`native_path`](Self::native_path) as a file and stream it through the
/// decoder. WASM targets fetch [`wasm_url`](Self::wasm_url) from the browser and decode the
/// response in memory, because browser requests are not exposed as seekable Rust readers.
#[derive(Clone)]
pub struct AudioAsset {
    native_path: PathBuf,
    wasm_url: String,
    #[cfg(target_arch = "wasm32")]
    browser_bytes: Arc<Mutex<Option<Arc<[u8]>>>>,
}

impl std::fmt::Debug for AudioAsset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioAsset")
            .field("native_path", &self.native_path)
            .field("wasm_url", &self.wasm_url)
            .finish()
    }
}

impl AudioAsset {
    /// Creates an asset using a native filesystem path and a browser URL.
    pub fn new(native_path: impl Into<PathBuf>, wasm_url: impl Into<String>) -> Self {
        Self {
            native_path: native_path.into(),
            wasm_url: wasm_url.into(),
            #[cfg(target_arch = "wasm32")]
            browser_bytes: Arc::new(Mutex::new(None)),
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

    /// Clears bytes cached after loading this asset in a browser.
    ///
    /// Clones of this asset share the same cache. Players and waveform builders can retain their
    /// own references independently, and WebAudio may release a stopped decoder asynchronously.
    pub fn clear_browser_cache(&self) {
        #[cfg(target_arch = "wasm32")]
        self.browser_bytes.lock().unwrap().take();
    }

    #[cfg(target_arch = "wasm32")]
    fn cached_browser_bytes(&self) -> Option<Arc<[u8]>> {
        self.browser_bytes.lock().unwrap().clone()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn load_browser_bytes(&self) -> Result<Arc<[u8]>, NyaaError> {
        if let Some(bytes) = self.cached_browser_bytes() {
            return Ok(bytes);
        }

        let bytes = fetch_browser_asset(self.wasm_url()).await?;
        let mut browser_bytes = self.browser_bytes.lock().unwrap();

        if let Some(cached_bytes) = browser_bytes.as_ref() {
            return Ok(cached_bytes.clone());
        }

        *browser_bytes = Some(bytes.clone());
        Ok(bytes)
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_asset_error(error: impl std::fmt::Debug) -> NyaaError {
    NyaaError::BrowserAsset(format!("{error:?}"))
}

/// Fetches a web resource from the browser and returns its bytes.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_browser_asset(url: &str) -> Result<Arc<[u8]>, NyaaError> {
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
    let array = Uint8Array::new(&buffer);
    let mut bytes = Arc::<[u8]>::new_uninit_slice(array.length() as usize);
    array.copy_to_uninit(Arc::get_mut(&mut bytes).unwrap());

    Ok(unsafe { bytes.assume_init() })
}

/// A cloneable handle to an audio output sink shared by one or more [`Nyaa`] players.
///
/// Create one output and pass clones to [`Nyaa::new_with_output`] to avoid opening a separate
/// device sink for every player.
///
/// ```no_run
/// use rodisnyaa::{AudioOutput, Nyaa};
///
/// let output = AudioOutput::new();
/// let first_player = Nyaa::new_with_output(output.clone());
/// let second_player = Nyaa::new_with_output(output);
/// ```
#[derive(Clone)]
pub struct AudioOutput {
    mixer_device_sink: Arc<Mutex<Option<MixerDeviceSink>>>,
    backend: Option<AudioBackend>,
}

impl Default for AudioOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput {
    /// Opens the default audio output when the platform permits it.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn new() -> Self {
        let backend = Some(rodio::cpal::default_host().id());
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink = Self::open_default_sink();
        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = None;

        Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend,
        }
    }

    /// Returns the audio backends currently available on this system.
    pub fn available_backends() -> Vec<AudioBackend> {
        rodio::cpal::available_hosts()
    }

    /// Opens the default output device provided by a specific audio backend.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn try_new_with_backend(backend: AudioBackend) -> Result<Self, AudioOutputError> {
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink = Some(Self::open_backend_sink(backend)?);

        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = {
            rodio::cpal::host_from_id(backend)
                .map_err(|source| AudioOutputError::BackendUnavailable { backend, source })?;
            None
        };

        Ok(Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend: Some(backend),
        })
    }

    /// Returns the selected audio backend, or `None` for an output created from a sink.
    pub fn backend(&self) -> Option<AudioBackend> {
        self.backend
    }

    /// Creates a shared output from an existing rodio device sink.
    pub fn from_sink(mixer_device_sink: MixerDeviceSink) -> Self {
        Self {
            mixer_device_sink: Arc::new(Mutex::new(Some(mixer_device_sink))),
            backend: None,
        }
    }

    fn open_default_sink() -> Option<MixerDeviceSink> {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                Some(sink)
            }
            Err(_) => None,
        }
    }

    fn open_backend_sink(backend: AudioBackend) -> Result<MixerDeviceSink, AudioOutputError> {
        let host = rodio::cpal::host_from_id(backend)
            .map_err(|source| AudioOutputError::BackendUnavailable { backend, source })?;
        let device = host
            .default_output_device()
            .or_else(|| host.output_devices().ok()?.next())
            .ok_or(AudioOutputError::NoOutputDevice(backend))?;
        let mut sink = DeviceSinkBuilder::from_device(device)
            .and_then(|builder| builder.open_sink_or_fallback())
            .map_err(|source| AudioOutputError::OpenStream { backend, source })?;

        sink.log_on_drop(false);
        Ok(sink)
    }

    #[cfg(target_arch = "wasm32")]
    fn ensure_sink(&self) {
        let mut mixer_device_sink = self.mixer_device_sink.lock().unwrap();

        if mixer_device_sink.is_none() {
            *mixer_device_sink = match self.backend {
                Some(backend) => Self::open_backend_sink(backend).ok(),
                None => Self::open_default_sink(),
            };
        }
    }

    fn connect_player(&self) -> Option<Player> {
        self.mixer_device_sink
            .lock()
            .unwrap()
            .as_ref()
            .map(|mixer_device_sink| Player::connect_new(mixer_device_sink.mixer()))
    }

    /// Controls whether dropping the underlying device sink logs a message.
    pub fn log_on_drop(&self, log: bool) {
        if let Some(mixer_device_sink) = self.mixer_device_sink.lock().unwrap().as_mut() {
            mixer_device_sink.log_on_drop(log);
        }
    }
}

/// Settings for a rodio low-pass or high-pass filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterEffect {
    /// The filter cutoff frequency in hertz.
    pub frequency: u32,
    /// The filter resonance or bandwidth.
    pub q: f32,
}

impl Default for FilterEffect {
    fn default() -> Self {
        Self {
            frequency: 1_000,
            q: 0.5,
        }
    }
}

/// Settings for rodio's reverb effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbEffect {
    /// The delay before the reflected signal.
    pub delay: Duration,
    /// The amplitude of the reflected signal.
    pub amplitude: f32,
}

impl Default for ReverbEffect {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(120),
            amplitude: 0.35,
        }
    }
}

/// Settings for rodio's distortion effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistortionEffect {
    /// The gain applied before clipping.
    pub gain: f32,
    /// The absolute clipping threshold.
    pub threshold: f32,
}

impl Default for DistortionEffect {
    fn default() -> Self {
        Self {
            gain: 2.0,
            threshold: 0.8,
        }
    }
}

/// Settings for rodio's automatic gain control effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomaticGainEffect {
    /// The output level the effect tries to maintain.
    pub target_level: f32,
    /// How quickly the effect increases gain.
    pub attack: Duration,
    /// How quickly the effect reduces gain.
    pub release: Duration,
    /// The maximum gain the effect may apply.
    pub maximum_gain: f32,
}

impl Default for AutomaticGainEffect {
    fn default() -> Self {
        Self {
            target_level: 1.0,
            attack: Duration::from_secs(4),
            release: Duration::ZERO,
            maximum_gain: 7.0,
        }
    }
}

/// Settings for rodio's limiter effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterEffect {
    /// The negative dBFS level where limiting begins.
    pub threshold_db: f32,
    /// The width of the transition into limiting, in decibels.
    pub knee_width_db: f32,
    /// How quickly the limiter responds to peaks.
    pub attack: Duration,
    /// How quickly the limiter recovers after a peak.
    pub release: Duration,
}

impl Default for LimiterEffect {
    fn default() -> Self {
        Self {
            threshold_db: -1.0,
            knee_width_db: 4.0,
            attack: Duration::from_millis(5),
            release: Duration::from_millis(100),
        }
    }
}

/// Post-processing effects applied to newly played audio.
///
/// Optional effects are disabled by default. Calling [`Nyaa::set_effects`] while audio is
/// active rebuilds its decoder at the current position so the new chain takes effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioEffects {
    /// Linear gain applied before the other effects.
    pub input_gain: f32,
    /// Duration of the fade from silence when a source starts.
    pub fade_in: Duration,
    /// Optional high-pass filter settings.
    pub high_pass: Option<FilterEffect>,
    /// Optional low-pass filter settings.
    pub low_pass: Option<FilterEffect>,
    /// Optional distortion settings.
    pub distortion: Option<DistortionEffect>,
    /// Optional automatic gain control settings.
    pub automatic_gain: Option<AutomaticGainEffect>,
    /// Optional reverb settings.
    pub reverb: Option<ReverbEffect>,
    /// Optional limiter settings, applied last.
    pub limiter: Option<LimiterEffect>,
}

impl Default for AudioEffects {
    fn default() -> Self {
        Self {
            input_gain: 1.0,
            fade_in: Duration::ZERO,
            high_pass: None,
            low_pass: None,
            distortion: None,
            automatic_gain: None,
            reverb: None,
            limiter: None,
        }
    }
}

impl AudioEffects {
    fn validate(&self) -> Result<(), NyaaError> {
        if !self.input_gain.is_finite() || self.input_gain < 0.0 {
            return Err(NyaaError::InvalidEffect(
                "input gain must be finite and non-negative",
            ));
        }

        for filter in [self.high_pass, self.low_pass].into_iter().flatten() {
            if filter.frequency == 0 {
                return Err(NyaaError::InvalidEffect(
                    "filter frequency must be greater than zero",
                ));
            }
            if !filter.q.is_finite() || filter.q <= 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "filter Q must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.distortion {
            if !effect.gain.is_finite() || effect.gain < 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "distortion gain must be finite and non-negative",
                ));
            }
            if !effect.threshold.is_finite() || effect.threshold <= 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "distortion threshold must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.automatic_gain {
            if !effect.target_level.is_finite() || effect.target_level <= 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "automatic gain target must be finite and greater than zero",
                ));
            }
            if !effect.maximum_gain.is_finite() || effect.maximum_gain <= 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "automatic maximum gain must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.reverb
            && (!effect.amplitude.is_finite() || effect.amplitude < 0.0)
        {
            return Err(NyaaError::InvalidEffect(
                "reverb amplitude must be finite and non-negative",
            ));
        }

        if let Some(effect) = self.limiter {
            if !effect.threshold_db.is_finite() || effect.threshold_db >= 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "limiter threshold must be finite and below zero dBFS",
                ));
            }
            if !effect.knee_width_db.is_finite() || effect.knee_width_db < 0.0 {
                return Err(NyaaError::InvalidEffect(
                    "limiter knee width must be finite and non-negative",
                ));
            }
        }

        Ok(())
    }

    fn apply<S>(&self, source: S) -> Box<dyn Source<Item = f32> + Send>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let mut source: Box<dyn Source<Item = f32> + Send> = Box::new(source);

        if self.input_gain != 1.0 {
            source = Box::new(source.amplify(self.input_gain));
        }
        if let Some(effect) = self.high_pass {
            source = Box::new(source.high_pass_with_q(effect.frequency, effect.q));
        }
        if let Some(effect) = self.low_pass {
            source = Box::new(source.low_pass_with_q(effect.frequency, effect.q));
        }
        if let Some(effect) = self.distortion {
            source = Box::new(source.distortion(effect.gain, effect.threshold));
        }
        if let Some(effect) = self.automatic_gain {
            source = Box::new(source.automatic_gain_control(AutomaticGainControlSettings {
                target_level: effect.target_level,
                attack_time: effect.attack,
                release_time: effect.release,
                absolute_max_gain: effect.maximum_gain,
            }));
        }
        if let Some(effect) = self.reverb {
            source = Box::new(source.buffered().reverb(effect.delay, effect.amplitude));
        }
        if let Some(effect) = self.limiter {
            source = Box::new(source.limit(LimitSettings {
                threshold: effect.threshold_db,
                knee_width: effect.knee_width_db,
                attack: effect.attack,
                release: effect.release,
            }));
        }
        if !self.fade_in.is_zero() {
            source = Box::new(source.fade_in(self.fade_in));
        }

        source
    }
}

/// rodisnyaa: Painless audio playback for native and web platforms.
///
/// If you are only playing one audio track at a time, you can use [`Nyaa`] directly.
/// If you want to play multiple tracks simultaneously, create an [`AudioOutput`] and
/// pass clones to [`Nyaa::new_with_output`].
///
/// Avoid naming a Nyaa instance `player` to prevent confusion with the rodio
/// [`Player`], instead name it `nyaa` or prefix with `nyaa_`.
pub struct Nyaa {
    /// The audio output shared by this player.
    audio_output: AudioOutput,

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

    /// The path of the current source, if it was played from a file.
    #[cfg(not(target_arch = "wasm32"))]
    current_file_path: Option<PathBuf>,

    /// The offset of the current position in the source.
    ///
    /// You should probably use [`Nyaa::position()`] instead.
    position_offset: Duration,

    /// The player position corresponding to [`Self::position_offset`].
    player_position_anchor: Duration,

    /// The playback speed applied to the player.
    speed: f32,

    /// The volume applied to the player.
    volume: Mutex<f32>,

    /// The post-processing effects applied to playback sources.
    effects: AudioEffects,

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
    /// Creates a player with its own default audio output.
    pub fn new() -> Self {
        Self::new_with_output(AudioOutput::new())
    }

    /// Creates a player connected to a reusable audio output.
    pub fn new_with_output(audio_output: AudioOutput) -> Self {
        let player = audio_output.connect_player();

        Self {
            audio_output,
            player,
            duration: Mutex::new(None),
            current_shared_bytes: None,
            current_static_bytes: None,
            #[cfg(not(target_arch = "wasm32"))]
            current_file_path: None,
            position_offset: Duration::ZERO,
            player_position_anchor: Duration::ZERO,
            speed: 1.0,
            volume: Mutex::new(1.0),
            effects: AudioEffects::default(),
            position_is_held: true,
            #[cfg(target_arch = "wasm32")]
            pending_playback: None,
        }
    }

    /// Returns the audio backend selected for this player.
    ///
    /// Returns `None` when the player uses an [`AudioOutput`] created with
    /// [`AudioOutput::from_sink`].
    pub fn audio_backend(&self) -> Option<AudioBackend> {
        self.audio_output.backend()
    }

    /// Switches this player to a specific audio backend.
    ///
    /// A successful switch stops the current source and holds its last reported position.
    /// Other players that shared the previous [`AudioOutput`] are not affected.
    pub fn switch_audio_backend(&mut self, backend: AudioBackend) -> Result<(), AudioOutputError> {
        if self.audio_backend() == Some(backend) {
            return Ok(());
        }

        let audio_output = AudioOutput::try_new_with_backend(backend)?;

        self.stop();
        self.audio_output = audio_output;
        self.player = self.audio_output.connect_player();

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    fn ensure_audio_output(&mut self) {
        self.audio_output.ensure_sink();

        if self.player.is_none() {
            self.player = self.audio_output.connect_player();
        }
    }

    /// When [`MixerDeviceSink`] is dropped a message is logged to stderr or emitted through tracing if the tracing feature is enabled.
    pub fn log_on_drop(&mut self, log: bool) {
        self.audio_output.log_on_drop(log);
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
        let source = self.effects.apply(source);
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        *self.duration.lock().unwrap() = duration;

        #[cfg(target_arch = "wasm32")]
        self.ensure_audio_output();

        let Some(new_player) = self.audio_output.connect_player() else {
            return Ok(());
        };
        let volume = self.volume();

        new_player.set_volume(volume);
        new_player.set_speed(self.speed);
        new_player.append(source);
        new_player.play();

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

        Ok(())
    }

    /// Plays bytes stored in an [`Arc`], which allows multiple players to share the same
    /// audio data without copying it onto the heap.
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
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.current_file_path = None;
        }
        self.position_offset = Duration::ZERO;
        self.player_position_anchor = Duration::ZERO;
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

    /// Plays an audio asset using its native path.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), NyaaError> {
        let path = path.as_ref().to_path_buf();
        let source = Self::decoder_from_file(&path)?;

        self.play_source(source)?;
        self.current_file_path = Some(path);

        Ok(())
    }

    /// Plays an audio asset using its native path or browser URL.
    pub async fn play_asset(&mut self, asset: &AudioAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.play_file(asset.native_path())
        }

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = asset.load_browser_bytes().await?;
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

            if let Some(bytes) = asset.cached_browser_bytes() {
                let source = Self::decoder_from_shared_bytes(bytes.clone())?;

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
                    self.position_offset = position;
                    self.player_position_anchor = position;
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
    pub fn try_seek_secs(&mut self, position_secs: f64) -> Result<(), NyaaError> {
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

        if let Some(new_player) = self.audio_output.connect_player() {
            new_player.set_volume(volume);
            new_player.set_speed(self.speed);

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

        source.try_seek(position).map_err(NyaaError::Seek)?;
        let source = self.effects.apply(source);

        let was_paused = player.is_paused();
        let volume = self.volume();

        let Some(new_player) = self.audio_output.connect_player() else {
            return Ok(());
        };

        new_player.set_volume(volume);
        new_player.set_speed(self.speed);

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
    pub fn effects(&self) -> AudioEffects {
        self.effects
    }

    /// Replaces the rodio post-processing chain.
    ///
    /// If a source is playing, its decoder is rebuilt at the current position and keeps its
    /// paused state. Loaded or stopped sources use the settings the next time playback starts.
    pub fn set_effects(&mut self, effects: AudioEffects) -> Result<(), NyaaError> {
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
            let bytes = asset.load_browser_bytes().await?;
            Ok(Self::decoder_from_shared_bytes(bytes)?.total_duration())
        }
    }

    /// Returns the duration of shared bytes, such as data from a `Vec<u8>`, without copying the
    /// encoded audio onto the heap.
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

    /// Returns the duration of an audio file using its native path.
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

        self.position_at_player_time(player_position)
    }

    fn position_at_player_time(&self, player_position: Duration) -> Duration {
        let elapsed = player_position.saturating_sub(self.player_position_anchor);
        let position = self
            .position_offset
            .saturating_add(elapsed.mul_f32(self.speed));

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

        self.current_shared_bytes = None;
        self.current_static_bytes = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.current_file_path = None;
        }
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
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

    /// Changes the playback speed and pitch of the sound.
    ///
    /// A value of `1.0` uses the original speed. For example, `0.5` plays at half speed and
    /// `2.0` plays at double speed. Pitch changes by the same factor.
    pub fn set_speed(&mut self, speed: f32) {
        if !self.position_is_held {
            let player_position = self.player.as_ref().map_or(Duration::ZERO, Player::get_pos);
            self.position_offset = self.position_at_player_time(player_position);
            self.player_position_anchor = player_position;
        }

        self.speed = speed;

        if let Some(player) = self.player.as_ref() {
            player.set_speed(speed);
        }
    }

    /// Gets the playback speed of the sound.
    pub fn speed(&self) -> f32 {
        self.speed
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

/// Formats f64 seconds as a string in the format `H:MM:SS` or `M:SS`.
pub fn format_timestamp_secs(seconds: f64) -> String {
    format_timestamp(Duration::from_secs_f64(seconds))
}

/// Parses a timestamp string in the format `H:MM:SS`, `M:SS`, or `SS` into a number of seconds.
pub fn parse_timestamp(input: &str) -> Option<f64> {
    let components = input.trim().split(':').collect::<Vec<_>>();
    let parse_seconds = |value: &str| {
        value
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
    };

    match components.as_slice() {
        [seconds] => parse_seconds(seconds),
        [minutes, seconds] => {
            let minutes = minutes.parse::<u64>().ok()?;
            let seconds = parse_seconds(seconds)?;

            (seconds < 60.0).then_some(minutes as f64 * 60.0 + seconds)
        }
        [hours, minutes, seconds] => {
            let hours = hours.parse::<u64>().ok()?;
            let minutes = minutes.parse::<u64>().ok()?;
            let seconds = parse_seconds(seconds)?;

            (minutes < 60 && seconds < 60.0)
                .then_some(hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds)
        }
        _ => None,
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
    fn players_sharing_an_output_keep_independent_state() {
        let output = AudioOutput::new();
        let mut first = Nyaa::new_with_output(output.clone());
        let mut second = Nyaa::new_with_output(output);

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

    #[test]
    fn playback_preserves_speed_selected_before_a_track() {
        let mut nyaa = Nyaa::new();

        nyaa.set_speed(1.5);
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");

        assert_eq!(nyaa.speed(), 1.5);
    }

    #[test]
    fn playback_preserves_volume_selected_before_a_track() {
        let mut nyaa = Nyaa::new();

        nyaa.set_volume(0.35);
        assert_eq!(nyaa.volume(), 0.35);

        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");

        assert_eq!(nyaa.volume(), 0.35);
    }

    #[test]
    fn position_tracks_source_time_when_speed_changes() {
        let mut nyaa = Nyaa::new();

        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        thread::sleep(Duration::from_millis(200));

        let before_speed_change = nyaa.position();
        nyaa.set_speed(2.0);
        let after_speed_change = nyaa.position();

        assert!(
            after_speed_change.saturating_sub(before_speed_change) < Duration::from_millis(100),
            "changing speed jumped the position from {before_speed_change:?} to {after_speed_change:?}"
        );

        thread::sleep(Duration::from_millis(250));
        let advancement = nyaa.position().saturating_sub(after_speed_change);

        assert!(
            advancement >= Duration::from_millis(350),
            "position advanced only {advancement:?} during 250ms of playback at 2x speed"
        );
    }
}
