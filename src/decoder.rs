//! Create a [`Decoder`] from various sources.

#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::io::BufReader;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::{io::Cursor, sync::Arc};

use rodio::Decoder;

use crate::SoundscapeError;

/// Creates a new [`Decoder`] from a shared byte slice.
///
/// Use this function when you don't want to copy data while creating a decoder.
pub fn from_shared_bytes(bytes: Arc<[u8]>) -> Result<Decoder<Cursor<Arc<[u8]>>>, SoundscapeError> {
    let len = bytes.len() as u64;

    Decoder::builder()
        .with_data(Cursor::new(bytes))
        .with_byte_len(len)
        .build()
        .map_err(SoundscapeError::Decode)
}

/// Creates a new [`Decoder`] from a static byte slice.
///
/// Use this function when you are loading from embedded data.
pub fn from_static_bytes(
    bytes: &'static [u8],
) -> Result<Decoder<Cursor<&'static [u8]>>, SoundscapeError> {
    Decoder::builder()
        .with_data(Cursor::new(bytes))
        .with_byte_len(bytes.len() as u64)
        .build()
        .map_err(SoundscapeError::Decode)
}

/// Creates a new [`Decoder`] from a native file path.
///
/// Use this function when you want to stream audio from a file.
#[cfg(not(target_arch = "wasm32"))]
pub fn from_file(path: impl AsRef<Path>) -> Result<Decoder<BufReader<File>>, SoundscapeError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(SoundscapeError::File)?;
    let byte_len = file.metadata().map_err(SoundscapeError::File)?.len();
    let mut builder = Decoder::builder()
        .with_data(BufReader::new(file))
        .with_byte_len(byte_len)
        .with_coarse_seek(true);

    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        builder = builder.with_hint(extension);
    }

    builder.build().map_err(SoundscapeError::Decode)
}
