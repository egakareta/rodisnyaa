use rodisnyaa::{SoundSource, Soundscape};

static AUDIO: &[u8] = include_bytes!("polar 240 yay.mp3");

fn main() -> Result<(), rodisnyaa::SoundscapeError> {
    let soundscape = Soundscape::new();
    let sound = soundscape.create_sound("music", SoundSource::static_bytes(AUDIO))?;

    sound.set_volume(0.8)?;
    sound.set_speed(0.9)?;
    sound.set_preserve_pitch(true)?;
    sound.try_seek_secs(30.0)?;
    sound.play()?;
    sound.wait_until_end()?;

    Ok(())
}
