#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), rodisnyaa::NyaaError> {
    let examples = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut group = rodisnyaa::NyaaGroup::new();
    group.set_volume(0.8);
    group.try_set_speed(0.9)?;

    group.load_files_keyed([
        ("music", examples.join("ATLAS 270 [WHAT NO].wav")),
        ("ambience", examples.join("polar 240 yay.mp3")),
    ])?;

    // Tune and start one layer independently.
    group
        .member_mut("ambience")
        .expect("ambience was just loaded")
        .set_volume(0.2);
    group.play_member("ambience")?;

    std::thread::sleep(std::time::Duration::from_secs(1));
    group.play_member("music")?;

    group.wait_until_end();

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Native file playback isn't available on WASM. Use NyaaGroup::play_assets there.
}
