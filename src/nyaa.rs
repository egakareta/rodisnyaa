use crate::wsola::Wsola;
use dasp_sample::FromSample;
#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::decoder::DecoderError;
use rodio::source::{AutomaticGainControlSettings, LimitSettings, SeekError};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::io::BufReader;
use std::io::{Cursor, Error};
use std::ops::Range;
use std::path::{Path, PathBuf};
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

    /// The audio output could not be initialized.
    #[error(transparent)]
    Output(#[from] AudioOutputError),

    /// The playback range is empty or starts beyond the end of the source.
    #[error("the playback range must contain audio")]
    InvalidPlaybackRange,

    /// A playback operation required a previously loaded or played source.
    #[error("no audio source is available")]
    NoAudioSource,

    /// Errors that might occur when loading an audio asset in a browser.
    #[cfg(target_arch = "wasm32")]
    #[error("failed to load browser audio asset: {0}")]
    BrowserAsset(String),
}

/// A CPAL audio host that can provide an output device.
pub type Backend = rodio::cpal::HostId;

/// An error that can occur while opening an audio backend or device.
#[derive(Debug, Error)]
pub enum AudioOutputError {
    /// The requested backend is not available on this system.
    #[error("audio backend {backend} is unavailable: {source}")]
    BackendUnavailable {
        /// The backend that was requested.
        backend: Backend,
        /// The error returned by CPAL while initializing the backend.
        #[source]
        source: rodio::cpal::HostUnavailable,
    },

    /// The requested backend has no output device.
    #[error("audio backend {0} has no output device")]
    NoOutputDevice(Backend),

    /// The output devices of the requested backend could not be listed.
    #[error("failed to list output devices of audio backend {backend}: {source}")]
    ListDevices {
        /// The backend whose devices could not be listed.
        backend: Backend,
        /// The error returned by CPAL while listing devices.
        #[source]
        source: rodio::cpal::DevicesError,
    },

    /// The requested output device could not be found.
    ///
    /// The device may have been unplugged or disabled after enumeration.
    #[error("audio output device \"{device}\" was not found")]
    DeviceNotFound {
        /// The device that could not be found.
        device: Box<AudioDevice>,
    },

    /// The requested backend's output stream could not be opened.
    #[error("failed to open audio backend {backend}: {source}")]
    OpenStream {
        /// The backend that was requested.
        backend: Backend,
        /// The error returned by rodio while opening the output stream.
        #[source]
        source: rodio::stream::DeviceSinkError,
    },

    /// The requested output device's stream could not be opened.
    #[error("failed to open audio output device \"{device}\": {source}")]
    OpenDeviceStream {
        /// The device that was requested.
        device: Box<AudioDevice>,
        /// The error returned by rodio while opening the output stream.
        #[source]
        source: rodio::stream::DeviceSinkError,
    },
}

/// An individual output device such as speakers, headphones, or a virtual device.
///
/// Values are obtained from [`Output::available_devices`] or
/// [`Output::available_devices_for_backend`] and can be passed to
/// [`Output::try_new_with_device`] or [`Nyaa::switch_device`].
///
/// Devices are identified by their CPAL device id when the platform provides one and fall back
/// to name matching otherwise. A device obtained from enumeration may no longer exist when it is
/// opened, in which case opening returns [`AudioOutputError::DeviceNotFound`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    backend: Backend,
    id: Option<rodio::cpal::DeviceId>,
    description: rodio::cpal::DeviceDescription,
    is_default: bool,
}

impl std::fmt::Display for AudioDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.description)?;
        Ok(())
    }
}

impl AudioDevice {
    /// Returns the backend that provides this device.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Returns the stable device id when the platform provides one.
    pub fn id(&self) -> Option<&rodio::cpal::DeviceId> {
        self.id.as_ref()
    }

    /// Returns the structured CPAL description of this device.
    ///
    /// The description exposes the human-readable name plus manufacturer, device type
    /// (speaker, headphones, headset, virtual, ...), and interface type where the platform
    /// reports them.
    pub fn description(&self) -> &rodio::cpal::DeviceDescription {
        &self.description
    }

    /// Returns the human-readable device name.
    pub fn name(&self) -> &str {
        self.description.name()
    }

    /// Returns whether this device is the default output device of its backend.
    pub fn is_default(&self) -> bool {
        self.is_default
    }

    /// Returns the device type categorization (speaker, headphones, virtual, ...).
    pub fn device_type(&self) -> rodio::cpal::DeviceType {
        self.description.device_type()
    }

    /// Returns the interface/connection type (built-in, USB, Bluetooth, virtual, ...).
    pub fn interface_type(&self) -> rodio::cpal::InterfaceType {
        self.description.interface_type()
    }

    fn from_cpal_device(
        backend: Backend,
        device: &rodio::cpal::Device,
        is_default: bool,
    ) -> Option<Self> {
        let description = device.description().ok()?;
        let id = device.id().ok();

        Some(Self {
            backend,
            id,
            description,
            is_default,
        })
    }

    fn matches_cpal_device(&self, device: &rodio::cpal::Device) -> bool {
        if let (Some(wanted), Ok(actual)) = (self.id.as_ref(), device.id())
            && wanted == &actual
        {
            return true;
        }

        device
            .description()
            .is_ok_and(|description| description.name() == self.name())
    }
}

/// The current playback lifecycle state of a [`Nyaa`] player.
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

/// A playback lifecycle event emitted by [`Nyaa::poll_event`].
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
/// use rodisnyaa::{Output, Nyaa};
///
/// let output = Output::new();
/// let first_player = Nyaa::new_with_output(output.clone());
/// let second_player = Nyaa::new_with_output(output);
/// ```
#[derive(Clone)]
pub struct Output {
    mixer_device_sink: Arc<Mutex<Option<MixerDeviceSink>>>,
    backend: Option<Backend>,
    device: Option<AudioDevice>,
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

/// Cached audio backends from the most recent enumeration.
///
/// Populated on the first [`Output::available_backends`] call and refreshed by
/// [`Output::refresh_available_backends`]. Caching keeps per-frame UI polling cheap:
/// backend enumeration probes every compiled host via `is_available()`.
static CACHED_AUDIO_BACKENDS: OnceLock<Mutex<Option<Vec<Backend>>>> = OnceLock::new();

/// Cached output devices from the most recent enumeration.
///
/// Populated on the first [`Output::available_devices`] call and refreshed by
/// [`Output::refresh_available_devices`]. Caching keeps per-frame UI polling
/// cheap: device enumeration queries every backend for its device list.
static CACHED_OUTPUT_DEVICES: OnceLock<Mutex<Option<Vec<AudioDevice>>>> = OnceLock::new();

impl Output {
    /// Opens the default audio output when the platform permits it.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn new() -> Self {
        let backend = Some(rodio::cpal::default_host().id());
        let device = backend.and_then(Self::default_device_for_backend);
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink: Option<MixerDeviceSink> = Self::open_default_sink();
        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = None;

        Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend,
            device,
        }
    }

    /// Produces a list of hosts that are currently available on the system.
    ///
    /// The first call enumerates the system backends and caches the result.
    /// [`Output::refresh_available_backends`] to re-enumerate after the
    /// platform reports new hardware.
    pub fn available_backends() -> Vec<Backend> {
        let cache = CACHED_AUDIO_BACKENDS.get_or_init(|| Mutex::new(None));

        if let Some(backends) = cache.lock().unwrap().clone() {
            return backends;
        }

        Self::refresh_available_backends()
    }

    /// Re-enumerates the audio backends, updates the cache, and returns the fresh list.
    /// Also invalidates cached output devices.
    pub fn refresh_available_backends() -> Vec<Backend> {
        let backends = rodio::cpal::available_hosts();

        *CACHED_AUDIO_BACKENDS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(backends.clone());
        *CACHED_OUTPUT_DEVICES
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = None;

        backends
    }

    /// Produces a list of hosts that are currently available on the system.
    ///
    /// The first call enumerates the system backends and caches the result.
    /// [`Output::refresh_available_devices`] to re-enumerate after the
    /// platform reports new hardware.
    pub fn available_devices() -> Vec<AudioDevice> {
        let cache = CACHED_OUTPUT_DEVICES.get_or_init(|| Mutex::new(None));

        if let Some(devices) = cache.lock().unwrap().clone() {
            return devices;
        }

        Self::refresh_available_devices()
    }

    /// Re-enumerates the output devices of every current backend, updates the caches, and
    /// returns the fresh list.
    pub fn refresh_available_devices() -> Vec<AudioDevice> {
        let backends = rodio::cpal::available_hosts();
        let default_backend = rodio::cpal::default_host().id();
        let mut devices: Vec<AudioDevice> = backends
            .iter()
            .flat_map(|backend| Self::available_devices_for_backend(*backend).unwrap_or_default())
            .collect();

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| {
                    (second.backend == default_backend).cmp(&(first.backend == default_backend))
                })
                .then_with(|| first.name().cmp(second.name()))
                .then_with(|| first.backend.name().cmp(second.backend.name()))
        });

        let mut seen = HashSet::new();
        devices.retain(|device| seen.insert(device.name().to_string()));

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| first.name().cmp(second.name()))
        });

        *CACHED_AUDIO_BACKENDS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(backends);
        *CACHED_OUTPUT_DEVICES
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(devices.clone());

        devices
    }

    /// Returns the canonical UI label for an audio backend.
    pub fn backend_label(backend: Backend) -> String {
        format!("{backend:?}")
    }

    /// Parses a backend label produced by [`Output::backend_label`].
    ///
    /// Matching is case-insensitive and ignores surrounding whitespace. Only backends
    /// reported by [`Output::available_backends`] are recognized.
    pub fn parse_backend_label(label: &str) -> Option<Backend> {
        Self::available_backends()
            .into_iter()
            .find(|backend| Self::backend_label(*backend).eq_ignore_ascii_case(label.trim()))
    }

    /// Returns the output devices currently available from a specific audio backend.
    ///
    /// Unlike [`Output::available_devices`], this always performs a live query
    /// and does not consult the cache.
    pub fn available_devices_for_backend(
        backend: Backend,
    ) -> Result<Vec<AudioDevice>, AudioOutputError> {
        let host = rodio::cpal::host_from_id(backend)
            .map_err(|source| AudioOutputError::BackendUnavailable { backend, source })?;
        let default_device = host.default_output_device();
        let default_id = default_device.as_ref().and_then(|device| device.id().ok());
        let default_name = default_device
            .as_ref()
            .and_then(|device| device.description().ok())
            .map(|description| description.name().to_string());
        let mut devices: Vec<AudioDevice> = host
            .output_devices()
            .map_err(|source| AudioOutputError::ListDevices { backend, source })?
            .filter_map(|device| {
                let is_default = match (&default_id, device.id().ok()) {
                    (Some(default_id), Some(id)) => &id == default_id,
                    _ => device.description().is_ok_and(|description| {
                        default_name.as_deref() == Some(description.name())
                    }),
                };

                AudioDevice::from_cpal_device(backend, &device, is_default)
            })
            .collect();

        if let Some(default_device) = default_device
            && !devices.iter().any(|device| device.is_default)
            && let Some(default) = AudioDevice::from_cpal_device(backend, &default_device, true)
        {
            devices.push(default);
        }

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| first.name().cmp(second.name()))
        });

        Ok(devices)
    }

    /// Opens a specific output device such as a speaker, headphone, or virtual device.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn try_new_with_device(device: &AudioDevice) -> Result<Self, AudioOutputError> {
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink = Some(Self::open_device_sink(device)?);

        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = {
            rodio::cpal::host_from_id(device.backend).map_err(|source| {
                AudioOutputError::BackendUnavailable {
                    backend: device.backend,
                    source,
                }
            })?;
            None
        };

        Ok(Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend: Some(device.backend),
            device: Some(device.clone()),
        })
    }

    /// Opens the default output device provided by a specific audio backend.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn try_new_with_backend(backend: Backend) -> Result<Self, AudioOutputError> {
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
            device: Self::default_device_for_backend(backend),
        })
    }

    /// Creates an output for a backend preference without failing.
    ///
    /// `None` follows the system default ([`Output::new`]). `Some(backend)` tries to
    /// open that backend and falls back to a deferred output that remembers the backend
    /// when opening fails. Deferred outputs report [`Output::has_output`] as
    /// `false` until [`Output::retry_output`] (or [`Nyaa::ensure_output`])
    /// succeeds, so callers do not need their own `Option<Output>` plus preference
    /// state.
    pub(crate) fn new_with_preferred_backend(preferred: Option<Backend>) -> Self {
        let Some(backend) = preferred else {
            return Self::new();
        };

        match Self::try_new_with_backend(backend) {
            Ok(output) => output,
            Err(_) => Self::new_deferred(Some(backend)),
        }
    }

    /// Creates a deferred output for a backend preference without opening any device.
    ///
    /// `None` records the system default backend; `Some(backend)` records that backend.
    /// Playback opens the device lazily via [`Output::retry_output`].
    pub fn new_deferred(preferred: Option<Backend>) -> Self {
        let Some(backend) = preferred else {
            let backend = rodio::cpal::default_host().id();

            return Self {
                mixer_device_sink: Arc::new(Mutex::new(None)),
                backend: Some(backend),
                device: Self::default_device_for_backend(backend),
            };
        };

        Self {
            mixer_device_sink: Arc::new(Mutex::new(None)),
            backend: Some(backend),
            device: Self::default_device_for_backend(backend),
        }
    }

    /// Returns the selected audio backend, or `None` for an output created from a sink.
    pub fn backend(&self) -> Option<Backend> {
        self.backend
    }

    /// Returns the selected output device, if one was chosen or resolved.
    ///
    /// Returns `None` for an output created with [`Output::from_sink`] or when the
    /// platform did not report a device description.
    pub fn device(&self) -> Option<AudioDevice> {
        self.device.clone()
    }

    /// Returns a UI-friendly name for this output.
    ///
    /// Returns the backend label with the device name in parentheses when both are known
    /// (for example `"Alsa (Built-in Audio)"`), the backend label alone when no device was
    /// resolved, and `"Custom"` for an output created with [`Output::from_sink`].
    pub fn display_name(&self) -> String {
        let Some(backend) = self.backend else {
            return "Custom".to_string();
        };
        let label = Self::backend_label(backend);

        self.device.as_ref().map_or_else(
            || label.clone(),
            |device| {
                if device.name().is_empty() {
                    label.clone()
                } else {
                    format!("{label} ({})", device.name())
                }
            },
        )
    }

    /// Creates a shared output from an existing rodio device sink.
    pub fn from_sink(mixer_device_sink: MixerDeviceSink) -> Self {
        Self {
            mixer_device_sink: Arc::new(Mutex::new(Some(mixer_device_sink))),
            backend: None,
            device: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_default_sink() -> Option<MixerDeviceSink> {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                Some(sink)
            }
            Err(_) => None,
        }
    }

    fn open_backend_sink(backend: Backend) -> Result<MixerDeviceSink, AudioOutputError> {
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

    fn default_device_for_backend(backend: Backend) -> Option<AudioDevice> {
        let host = rodio::cpal::host_from_id(backend).ok()?;
        let device = host.default_output_device()?;

        AudioDevice::from_cpal_device(backend, &device, true)
    }

    fn find_backend_device(device: &AudioDevice) -> Result<rodio::cpal::Device, AudioOutputError> {
        let host = rodio::cpal::host_from_id(device.backend).map_err(|source| {
            AudioOutputError::BackendUnavailable {
                backend: device.backend,
                source,
            }
        })?;

        if let Some(id) = device.id.as_ref()
            && let Some(device) = host.device_by_id(id)
        {
            return Ok(device);
        }

        host.output_devices()
            .map_err(|source| AudioOutputError::ListDevices {
                backend: device.backend,
                source,
            })?
            .find(|candidate| device.matches_cpal_device(candidate))
            .ok_or_else(|| AudioOutputError::DeviceNotFound {
                device: Box::new(device.clone()),
            })
    }

    fn open_device_sink(device: &AudioDevice) -> Result<MixerDeviceSink, AudioOutputError> {
        let backend_device = Self::find_backend_device(device)?;
        let mut sink = DeviceSinkBuilder::from_device(backend_device)
            .and_then(|builder| builder.open_sink_or_fallback())
            .map_err(|source| AudioOutputError::OpenDeviceStream {
                device: Box::new(device.clone()),
                source,
            })?;

        sink.log_on_drop(false);
        Ok(sink)
    }

    /// Retries opening this output's selected backend or device.
    ///
    /// Does nothing when the sink already exists or when the output was created with
    /// [`Output::from_sink`] without a backend or device.
    pub fn retry_sink(&self) -> Result<(), AudioOutputError> {
        #[cfg(all(target_arch = "wasm32", feature = "nightly"))]
        crate::patch::ensure_audioworklet_text_polyfill();

        let mut mixer_device_sink = self.mixer_device_sink.lock().unwrap();

        if mixer_device_sink.is_none() {
            let sink = match (self.device.clone(), self.backend) {
                (Some(device), _) => Self::open_device_sink(&device)?,
                (None, Some(backend)) => Self::open_backend_sink(backend)?,
                (None, None) => return Ok(()),
            };

            *mixer_device_sink = Some(sink);
        }

        Ok(())
    }

    /// Returns whether this output currently has an initialized device sink.
    ///
    /// Browser outputs return `false` until playback initializes WebAudio in response to a
    /// user gesture. Deferred outputs created with
    /// [`Output::new_deferred_with_backend`] also return `false` until
    /// [`Output::retry_output`] succeeds.
    fn has_sink(&self) -> bool {
        self.mixer_device_sink.lock().unwrap().is_some()
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

struct PlaybackRangeSource<S> {
    input: S,
    start: Duration,
    end: Option<Duration>,
    remaining: Option<Duration>,
    duration_per_sample: Duration,
    looping: Arc<AtomicBool>,
}

impl<S> PlaybackRangeSource<S>
where
    S: Source,
{
    fn new(
        input: S,
        start: Duration,
        end: Option<Duration>,
        position: Duration,
        looping: Arc<AtomicBool>,
    ) -> Self {
        let duration_per_sample = Self::duration_per_sample(&input);

        Self {
            input,
            start,
            end,
            remaining: end.map(|end| end.saturating_sub(position)),
            duration_per_sample,
            looping,
        }
    }

    fn duration_per_sample(input: &S) -> Duration {
        Duration::from_secs_f64(
            1.0 / (f64::from(input.sample_rate().get()) * f64::from(input.channels().get())),
        )
    }

    fn restart(&mut self) -> bool {
        if !self.looping.load(Ordering::Relaxed) || self.input.try_seek(self.start).is_err() {
            return false;
        }

        self.remaining = self.end.map(|end| end.saturating_sub(self.start));
        self.duration_per_sample = Self::duration_per_sample(&self.input);
        true
    }
}

impl<S> Iterator for PlaybackRangeSource<S>
where
    S: Source,
{
    type Item = S::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if self
            .remaining
            .is_some_and(|remaining| remaining <= self.duration_per_sample)
            && !self.restart()
        {
            return None;
        }

        if let Some(sample) = self.input.next() {
            if let Some(remaining) = self.remaining.as_mut() {
                *remaining = remaining.saturating_sub(self.duration_per_sample);
            }

            return Some(sample);
        }

        if self.restart() {
            let sample = self.input.next()?;

            if let Some(remaining) = self.remaining.as_mut() {
                *remaining = remaining.saturating_sub(self.duration_per_sample);
            }

            Some(sample)
        } else {
            None
        }
    }
}

impl<S> Source for PlaybackRangeSource<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        let remaining_samples = self.remaining.map(|remaining| {
            let samples = remaining.as_nanos() / self.duration_per_sample.as_nanos();
            let channels = u128::from(self.input.channels().get());
            (samples - samples % channels) as usize
        });

        let span = match (self.input.current_span_len(), remaining_samples) {
            (Some(input), Some(remaining)) => Some(input.min(remaining)),
            (Some(input), None) => Some(input),
            (None, remaining) => remaining,
        };

        // Don't report an infinite span. Downstream sources only re-read
        // sample rate at span boundaries.
        if span.is_none() {
            let channels = usize::from(self.input.channels().get());
            const FALLBACK_SAMPLES: usize = 512;
            return Some(FALLBACK_SAMPLES.div_ceil(channels) * channels);
        }

        span
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.input.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.looping.load(Ordering::Relaxed) {
            None
        } else {
            self.end.map(|end| end.saturating_sub(self.start))
        }
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        let position = self.end.map_or(position.max(self.start), |end| {
            position.clamp(self.start, end)
        });
        self.input.try_seek(position)?;
        self.remaining = self.end.map(|end| end.saturating_sub(position));
        self.duration_per_sample = Self::duration_per_sample(&self.input);
        Ok(())
    }
}

/// rodisnyaa: Painless audio playback for native and web platforms.
///
/// If you are only playing one audio track at a time, you can use [`Nyaa`] directly.
/// If you want to play multiple tracks simultaneously, create an [`Output`] and
/// pass clones to [`Nyaa::new_with_output`].
///
/// Avoid naming a Nyaa instance `player` to prevent confusion with the rodio
/// [`Player`], instead name it `nyaa` or prefix with `nyaa_`.
pub struct Nyaa {
    /// The audio output shared by this player.
    output: Output,

    /// The backend this player prefers when opening its output.
    preferred_backend: Option<Backend>,

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
    effects: AudioEffects,

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
        let mut nyaa = Self::new_with_output(Output::new());
        nyaa.preferred_backend = None;
        nyaa
    }

    /// Creates a player and returns an error if its default audio output cannot be opened.
    ///
    /// Browser targets defer opening the output until playback or
    /// [`Nyaa::retry_output`] so initialization can occur in response to a user gesture.
    pub fn try_new() -> Result<Self, AudioOutputError> {
        let backend = rodio::cpal::default_host().id();
        let output = Output::try_new_with_backend(backend)?;
        let mut nyaa = Self::new_with_output(output);
        nyaa.preferred_backend = None;
        Ok(nyaa)
    }
    /// Creates a player connected to a reusable audio output.
    ///
    /// The preferred backend is initialized to the output's backend, so
    /// [`Nyaa::ensure_output`] keeps using it.
    pub fn new_with_output(output: Output) -> Self {
        let player = output.connect_player();
        let preferred_backend = output.backend();

        Self {
            output,
            preferred_backend,
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
            effects: AudioEffects::default(),
            looping: Arc::new(AtomicBool::new(false)),
            loop_range: None,
            position_is_held: true,
            playback_state: Mutex::new(PlaybackState::Idle),
            playback_events: Mutex::new(VecDeque::new()),
            #[cfg(target_arch = "wasm32")]
            pending_playback: None,
        }
    }

    /// Returns the audio backend selected for this player.
    ///
    /// Returns `None` when the player uses an [`Output`] created with
    /// [`Output::from_sink`].
    pub fn backend(&self) -> Option<Backend> {
        self.output.backend()
    }

    /// Returns the output device selected for this player, if one was chosen or resolved.
    ///
    /// Returns `None` when the player uses an [`Output`] created with
    /// [`Output::from_sink`] or when the platform did not report a device description.
    pub fn device(&self) -> Option<AudioDevice> {
        self.output.device()
    }

    /// Returns whether this player currently has an initialized audio output.
    ///
    /// A browser player can return `false` until playback initializes WebAudio in response to a
    /// user gesture. A player with a deferred preferred backend (see
    /// [`Nyaa::set_preferred_backend`]) also returns `false` until
    /// [`Nyaa::ensure_output`] opens it.
    pub fn has_output(&self) -> bool {
        self.output.has_sink()
    }

    /// Retries opening this player's selected audio output.
    ///
    /// This can recover a player created by [`Nyaa::new`] after its initial best-effort device
    /// initialization failed.
    pub fn retry_output(&mut self) -> Result<(), AudioOutputError> {
        if let Err(error) = self.output.retry_sink() {
            self.set_playback_state(PlaybackState::Failed);
            return Err(error);
        }

        if self.player.is_none() {
            self.player = self.output.connect_player();
        }

        Ok(())
    }

    /// Returns the backend this player prefers when opening its output.
    ///
    /// `None` means the system default. This is the sticky choice set with
    /// [`Nyaa::set_preferred_backend`]; it is retained even when opening fails so a
    /// later [`Nyaa::ensure_output`] can retry without extra caller state.
    pub fn preferred_backend(&self) -> Option<Backend> {
        self.preferred_backend
    }

    /// Returns the UI label for this player's backend preference.
    pub fn preferred_backend_name(&self) -> String {
        Output::backend_label(self.preferred_backend.unwrap_or_else(|| {
            self.output
                .backend()
                .unwrap_or_else(|| rodio::cpal::default_host().id())
        }))
    }

    /// Returns a UI-friendly name for this player's output.
    ///
    /// Returns the active backend and device once the output is initialized or
    /// [`Nyaa::preferred_backend_name`] when the output is not yet open.
    pub fn backend_display_name(&self) -> String {
        if self.has_output() {
            self.output.display_name()
        } else {
            self.preferred_backend_name()
        }
    }

    /// Remembers a backend preference, replacing the current output. `None` follows
    /// the system default.
    ///
    /// The new output is opened immediately when possible.
    ///
    /// Does nothing when the preference is unchanged and an output backend is already
    /// selected.
    pub fn set_preferred_backend(&mut self, backend: Option<Backend>) {
        if self.preferred_backend == backend && self.backend().is_some() {
            return;
        }

        self.stop();
        self.preferred_backend = backend;
        self.output = Output::new_with_preferred_backend(backend);
        self.player = self.output.connect_player();
    }

    /// Remembers a backend preference using [`Output::parse_backend_label`].
    ///
    /// Returns `false` without changing anything when the label is unknown.
    pub fn set_preferred_backend_by_name(&mut self, name: &str) -> bool {
        let Some(backend) = Output::parse_backend_label(name) else {
            return false;
        };

        self.set_preferred_backend(Some(backend));
        true
    }

    /// Ensures this player's output is initialized for its preferred backend.
    ///
    /// Aligns a custom [`Output::from_sink`] output to an explicitly preferred
    /// backend, then retries opening the output when its sink is missing and reconnects
    /// the player. Returns `Ok` once [`Nyaa::has_output`] is true.
    ///
    /// Call this before playback when the backend may have been chosen while no device
    /// was open (for example after [`Nyaa::set_preferred_backend`] deferred the
    /// open, or after the device was unplugged). Playback methods already retry
    /// automatically, so this is primarily for warming the output up front or for
    /// surfacing [`AudioOutputError`] outside of playback.
    pub fn ensure_output(&mut self) -> Result<(), AudioOutputError> {
        if self.backend().is_none()
            && let Some(backend) = self.preferred_backend
        {
            self.stop();
            self.output = Output::new_with_preferred_backend(Some(backend));
            self.player = self.output.connect_player();
        }

        self.retry_output()
    }

    /// Switches this player to a specific audio backend.
    ///
    /// A successful switch stops the current source, holds its last reported position, and
    /// remembers the backend as the new preference (see
    /// [`Nyaa::preferred_backend`]). A failed switch leaves the current output and
    /// preference untouched. Other players that shared the previous [`Output`] are
    /// not affected. To remember a backend even when opening fails, use
    /// [`Nyaa::set_preferred_backend`] instead.
    pub fn switch_backend(&mut self, backend: Backend) -> Result<(), AudioOutputError> {
        if self.backend() == Some(backend) {
            self.preferred_backend = Some(backend);
            return Ok(());
        }

        let output = Output::try_new_with_backend(backend)?;

        self.stop();
        self.preferred_backend = Some(backend);
        self.output = output;
        self.player = self.output.connect_player();

        Ok(())
    }

    /// Switches this player to a specific output device such as a speaker, headphone, or
    /// virtual device.
    ///
    /// A successful switch stops the current source, holds its last reported position, and
    /// remembers the device's backend as the new preference. Other players that shared the
    /// previous [`Output`] are not affected. Switching devices also switches the
    /// player's backend to the device's backend.
    pub fn switch_device(&mut self, device: &AudioDevice) -> Result<(), AudioOutputError> {
        if self.device().as_ref() == Some(device) {
            self.preferred_backend = Some(device.backend);
            return Ok(());
        }

        let output = Output::try_new_with_device(device)?;

        self.stop();
        self.preferred_backend = Some(device.backend);
        self.output = output;
        self.player = self.output.connect_player();

        Ok(())
    }

    /// When [`MixerDeviceSink`] is dropped a message is logged to stderr or emitted through tracing if the tracing feature is enabled.
    pub fn log_on_drop(&mut self, log: bool) {
        self.output.log_on_drop(log);
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
        mut source: S,
        requested_position: Option<Duration>,
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
            .output
            .connect_player()
            .expect("an initialized audio output must accept players");
        let volume = self.volume();

        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());
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
        self.set_playback_state(PlaybackState::Playing);

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

    /// Plays bytes stored in an [`Arc`], which allows multiple players to share the same
    /// audio data without copying it onto the heap.
    pub fn play_shared_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), NyaaError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());

        let source = Self::decoder_from_shared_bytes(bytes.clone())
            .map_err(|error| self.record_failure(error))?;
        self.play_source(source)?;
        self.current_shared_bytes = Some(bytes);

        Ok(())
    }

    /// Loads static bytes and their duration without starting playback.
    pub fn load_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let duration = Self::decoder_from_static_bytes(bytes)
            .map_err(|error| self.record_failure(error))?
            .total_duration();

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
        self.set_playback_state(PlaybackState::Idle);

        Ok(())
    }

    /// Plays bytes with a `'static` lifetime, such as data from `include_bytes!`, without
    /// copying the encoded audio onto the heap.
    pub fn play_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), NyaaError> {
        let source =
            Self::decoder_from_static_bytes(bytes).map_err(|error| self.record_failure(error))?;
        self.play_source(source)?;
        self.current_static_bytes = Some(bytes);

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

    /// Plays an audio asset using its native path or browser URL.
    pub async fn play_asset(&mut self, asset: &AudioAsset) -> Result<(), NyaaError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.play_file(asset.native_path())
        }

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = asset
                .load_browser_bytes()
                .await
                .map_err(|error| self.record_failure(error))?;
            let source = Self::decoder_from_shared_bytes(bytes.clone())
                .map_err(|error| self.record_failure(error))?;

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

    /// Returns the current playback lifecycle state.
    pub fn state(&self) -> PlaybackState {
        self.refresh_playback_state();
        *self.playback_state.lock().unwrap()
    }

    /// Returns the next pending playback lifecycle event.
    ///
    /// Natural playback completion is detected when this method or [`Nyaa::state`] is called.
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
        self.playback_events
            .lock()
            .unwrap()
            .push_back(PlaybackEvent::StateChanged {
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

        if let Some(new_player) = self.output.connect_player() {
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
            .output
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

    /// Changes the playback speed of the sound.
    ///
    /// A value of `1.0` uses the original speed. For example, `0.5` plays at half speed and
    /// `2.0` plays at double speed. Pitch changes by the same factor unless
    /// [`Nyaa::set_preserve_pitch`] is enabled.
    pub fn set_speed(&mut self, speed: f32) {
        if let Err(error) = self.try_set_speed(speed) {
            self.record_failure(error);
        }
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

    /// Plays a source-time range from the current loaded or previously played source.
    ///
    /// Playback stops at the range end unless looping is enabled with [`Nyaa::set_looping`].
    pub fn play_range(&mut self, range: Range<Duration>) -> Result<(), NyaaError> {
        if range.start >= range.end {
            return Err(self.record_failure(NyaaError::InvalidPlaybackRange));
        }

        let position = range.start;
        let previous_range = self.loop_range.replace(range);
        let result = self.play_current_source_at(position);

        if result.is_err() {
            self.loop_range = previous_range;
        }

        result
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
    fn playback_state_and_events_follow_transport_controls() {
        let mut nyaa = Nyaa::new();

        assert_eq!(nyaa.state(), PlaybackState::Idle);
        assert_eq!(nyaa.poll_event(), None);

        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        assert_eq!(nyaa.state(), PlaybackState::Playing);
        assert_eq!(
            nyaa.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Idle,
                current: PlaybackState::Playing,
            })
        );

        nyaa.pause();
        assert_eq!(nyaa.state(), PlaybackState::Paused);
        assert_eq!(
            nyaa.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Playing,
                current: PlaybackState::Paused,
            })
        );

        nyaa.stop();
        assert_eq!(nyaa.state(), PlaybackState::Idle);
        assert_eq!(
            nyaa.poll_event(),
            Some(PlaybackEvent::StateChanged {
                previous: PlaybackState::Paused,
                current: PlaybackState::Idle,
            })
        );
    }

    #[test]
    fn configured_range_bounds_playback_and_can_loop() {
        let mut nyaa = Nyaa::new();
        let range = Duration::from_secs(42)..Duration::from_millis(42_050);

        nyaa.set_loop_range(range.clone())
            .expect("a non-empty range should be accepted");
        nyaa.set_looping(true);
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("the configured range should be playable");
        thread::sleep(Duration::from_millis(125));

        assert_eq!(nyaa.state(), PlaybackState::Playing);
        assert!(range.contains(&nyaa.position()));
    }

    #[test]
    fn play_range_stops_at_its_end() {
        let mut nyaa = Nyaa::new();

        nyaa.load_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be loadable");
        nyaa.play_range(Duration::from_secs(42)..Duration::from_millis(42_050))
            .expect("a range from the loaded source should be playable");
        thread::sleep(Duration::from_millis(125));

        assert_eq!(nyaa.state(), PlaybackState::Ended);
    }

    #[test]
    fn players_sharing_an_output_keep_independent_state() {
        let output = Output::new();
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

    #[test]
    fn position_tracks_source_time_when_speed_changes_while_looping() {
        let mut nyaa = Nyaa::new();

        nyaa.set_looping(true);
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
            "position advanced only {advancement:?} during 250ms of looped playback at 2x speed"
        );
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
                        matches!(error, AudioOutputError::ListDevices { .. }),
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

    #[test]
    fn switching_to_a_listed_device_selects_it() {
        let devices = Output::available_devices();

        for device in &devices {
            let Ok(output) = Output::try_new_with_device(device) else {
                continue;
            };

            assert_eq!(output.backend(), Some(device.backend()));
            assert_eq!(output.device().as_ref(), Some(device));

            let mut nyaa = Nyaa::new_with_output(output);
            assert_eq!(nyaa.device().as_ref(), Some(device));
            assert_eq!(nyaa.backend(), Some(device.backend()));

            nyaa.switch_device(device)
                .expect("switching to the current device should be a no-op");

            nyaa.load_static_bytes(TEST_AUDIO_BYTES)
                .expect("device output should accept players");
            return;
        }
    }

    #[test]
    fn setting_preferred_backend_by_name_rejects_unknown_labels() {
        let mut nyaa = Nyaa::new();

        if let Some(backend) = Output::available_backends().into_iter().next() {
            let label = Output::backend_label(backend);
            assert!(nyaa.set_preferred_backend_by_name(&label.to_lowercase()));
            assert_eq!(nyaa.preferred_backend(), Some(backend));
        }

        let before = nyaa.preferred_backend();
        assert!(!nyaa.set_preferred_backend_by_name("not-a-backend"));
        assert_eq!(nyaa.preferred_backend(), before);
    }

    #[test]
    fn reselecting_the_current_preference_does_not_interrupt_playback() {
        let mut nyaa = Nyaa::new();
        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        assert!(nyaa.is_playing());

        let preferred = nyaa.preferred_backend();
        nyaa.set_preferred_backend(preferred);
        assert_eq!(nyaa.preferred_backend(), preferred);
        assert!(
            nyaa.is_playing(),
            "re-selecting the current preference stopped playback"
        );
    }

    #[test]
    fn switching_backend_remembers_the_new_preference() {
        let mut nyaa = Nyaa::new();
        let Some(backend) = nyaa.backend() else {
            eprintln!("Skipping preference assertions: no audio backend");
            return;
        };

        nyaa.switch_backend(backend)
            .expect("switching to the current backend should be a no-op");
        assert_eq!(nyaa.preferred_backend(), Some(backend));
        assert_eq!(nyaa.backend(), Some(backend));
    }

    #[test]
    fn deferred_output_opens_lazily_through_ensure() {
        let backends = Output::available_backends();
        let Some(backend) = backends.first().copied() else {
            eprintln!("Skipping deferred assertions: no audio backend");
            return;
        };

        let output = Output::new_deferred(Some(backend));
        assert_eq!(output.backend(), Some(backend));
        assert!(!output.has_sink());

        let mut nyaa = Nyaa::new_with_output(output);
        assert_eq!(nyaa.preferred_backend(), Some(backend));
        assert!(!nyaa.has_output());
        assert_eq!(nyaa.backend_display_name(), Output::backend_label(backend));

        if nyaa.ensure_output().is_err() {
            eprintln!("Skipping deferred open assertions: backend {backend:?} has no device");
            return;
        }

        assert!(nyaa.has_output());
        assert_eq!(nyaa.backend(), Some(backend));
        assert!(
            nyaa.backend_display_name()
                .contains(&Output::backend_label(backend))
        );

        nyaa.play_static_bytes(TEST_AUDIO_BYTES)
            .expect("deferred output should play after ensure");
        assert!(nyaa.is_playing());
    }
}
