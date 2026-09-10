#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;
use std::{
    fmt::{Debug, Formatter},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(target_arch = "wasm32")]
use web_sys::Response;

#[cfg(target_arch = "wasm32")]
use crate::NyaaError;

/// An audio file with locations for native and browser targets.
///
/// Native targets open [`native_path`](Self::native_path) as a file and stream it through the
/// decoder. WASM targets fetch [`wasm_url`](Self::wasm_url) from the browser and decode the
/// response in memory, because browser requests are not exposed as seekable Rust readers.
#[derive(Clone)]
pub struct SoundAsset {
    native_path: PathBuf,
    wasm_url: String,
    #[cfg(target_arch = "wasm32")]
    browser_bytes: Arc<Mutex<Option<Arc<[u8]>>>>,
}

impl Debug for SoundAsset {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SoundAsset")
            .field("native_path", &self.native_path)
            .field("wasm_url", &self.wasm_url)
            .finish()
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

impl SoundAsset {
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
    /// Clones of this asset share the same cache. Sounds and waveform builders can retain their
    /// own references independently, and WebAudio may release a stopped decoder asynchronously.
    pub fn clear_browser_cache(&self) {
        #[cfg(target_arch = "wasm32")]
        self.browser_bytes.lock().unwrap().take();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn cached_browser_bytes(&self) -> Option<Arc<[u8]>> {
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

/// An audio source assignment retained by a [`crate::Sound`].
#[derive(Clone, Debug)]
pub enum SoundSource {
    /// No audio resource has been assigned yet.
    Empty,
    /// Encoded bytes with a static lifetime, such as bytes produced by `include_bytes!`.
    StaticBytes(&'static [u8]),
    /// Encoded bytes in shared heap storage.
    SharedBytes(Arc<[u8]>),
    /// A native filesystem path.
    #[cfg(not(target_arch = "wasm32"))]
    File(PathBuf),
    /// A native-path/browser-URL pair.
    Asset(SoundAsset),
}

impl SoundSource {
    pub(crate) fn same_resource(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::StaticBytes(left), Self::StaticBytes(right)) => std::ptr::eq(*left, *right),
            (Self::SharedBytes(left), Self::SharedBytes(right)) => Arc::ptr_eq(left, right),
            #[cfg(not(target_arch = "wasm32"))]
            (Self::File(left), Self::File(right)) => left == right,
            (Self::Asset(left), Self::Asset(right)) => {
                left.native_path == right.native_path && left.wasm_url == right.wasm_url
            }
            _ => false,
        }
    }

    /// Creates an empty source that can be replaced later with [`crate::Sound::set_source`].
    pub fn empty() -> Self {
        Self::Empty
    }

    /// Creates a source from encoded bytes with a static lifetime.
    pub fn static_bytes(bytes: &'static [u8]) -> Self {
        Self::StaticBytes(bytes)
    }

    /// Copies encoded bytes into shared storage.
    pub fn shared_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::SharedBytes(Arc::from(bytes.as_ref()))
    }

    /// Creates a source from an existing shared byte allocation.
    pub fn shared_arc_bytes(bytes: Arc<[u8]>) -> Self {
        Self::SharedBytes(bytes)
    }

    /// Creates a source from a native filesystem path.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    /// Creates a source from a cross-platform audio asset.
    pub fn asset(asset: SoundAsset) -> Self {
        Self::Asset(asset)
    }
}

impl Default for SoundSource {
    fn default() -> Self {
        Self::Empty
    }
}

impl From<&'static [u8]> for SoundSource {
    fn from(bytes: &'static [u8]) -> Self {
        Self::static_bytes(bytes)
    }
}

impl From<Arc<[u8]>> for SoundSource {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self::shared_arc_bytes(bytes)
    }
}

impl From<Vec<u8>> for SoundSource {
    fn from(bytes: Vec<u8>) -> Self {
        Self::SharedBytes(bytes.into())
    }
}

impl From<SoundAsset> for SoundSource {
    fn from(asset: SoundAsset) -> Self {
        Self::asset(asset)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<PathBuf> for SoundSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}
