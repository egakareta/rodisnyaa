static AUDIO: &[u8] = include_bytes!("polar 240 yay.mp3");

fn main() -> Result<(), rodisnyaa::NyaaError> {
    let mut nyaa = rodisnyaa::Nyaa::new();

    nyaa.set_volume(0.8);
    nyaa.set_speed(0.9);
    nyaa.set_preserve_pitch(true)?;
    nyaa.try_seek_secs(30.0)?;

    nyaa.play_static_bytes(AUDIO)?;
    nyaa.wait_until_end();

    Ok(())
}
