#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), rodisnyaa::SoundscapeError> {
    use std::path::PathBuf;

    use rodisnyaa::{SoundSource, Soundscape};

    let soundscape = Soundscape::new();

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("polar 240 yay.mp3");

    let sound = soundscape.create_sound("music", SoundSource::file(path))?;
    sound.set_volume(0.8)?;
    sound.set_speed(0.9)?;
    sound.try_seek_secs(30.0)?;
    sound.play()?;
    sound.wait_until_end()?;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Native file playback example isn't available on WASM.
}
