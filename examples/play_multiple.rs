#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), rodisnyaa::NyaaError> {
    let examples = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let nyaa = rodisnyaa::Nyaa::new();
    let group = nyaa.create_group("mix")?;
    group.set_volume(0.8)?;
    let music = group.create_sound(
        "music",
        rodisnyaa::SoundSource::file(examples.join("ATLAS 270 [WHAT NO].wav")),
    )?;
    let ambience = group.create_sound(
        "ambience",
        rodisnyaa::SoundSource::file(examples.join("polar 240 yay.mp3")),
    )?;

    // Tune and start one layer independently.
    ambience.set_volume(0.2)?;
    ambience.set_speed(0.9)?;
    music.set_speed(0.9)?;
    ambience.play()?;

    std::thread::sleep(std::time::Duration::from_secs(1));
    music.play()?;

    ambience.wait_until_end()?;
    music.wait_until_end()?;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Native file playback isn't available on WASM. Use SoundSource::asset there.
}
