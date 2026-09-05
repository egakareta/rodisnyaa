#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), rodisnyaa::NyaaError> {
    let mut nyaa = rodisnyaa::Nyaa::new();

    nyaa.set_volume(0.8);
    nyaa.set_speed(0.9);
    nyaa.try_seek_secs(30.0)?;

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("polar 240 yay.mp3");

    nyaa.play_file(&path)?;
    nyaa.wait_until_end();

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Native file playback example isn't available on WASM.
}
