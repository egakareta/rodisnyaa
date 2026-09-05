#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use rodisnyaa::{Nyaa, NyaaError};

fn main() -> Result<(), NyaaError> {
    let mut nyaa = Nyaa::new();

    nyaa.set_volume(0.8);
    nyaa.set_speed(0.9);
    nyaa.try_seek_secs(30.0)?;

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("polar 240 yay.mp3");
    nyaa.play_file(&path)?;
    nyaa.wait_until_end();

    Ok(())
}
