use std::thread;

use rodisnyaa::{Nyaa, NyaaError, SoundSource};
use web_time::Duration;

static AUDIO_0: &[u8] = include_bytes!("ATLAS 270 [WHAT NO].wav");
static AUDIO_1: &[u8] = include_bytes!("polar 240 yay.mp3");

fn main() -> Result<(), NyaaError> {
    let nyaa = Nyaa::new();
    let group = nyaa.create_group("mix")?;
    group.set_volume(0.8)?;
    let music = group.create_sound("music", SoundSource::static_bytes(AUDIO_0))?;
    let ambience = group.create_sound("ambience", SoundSource::static_bytes(AUDIO_1))?;

    // Tune and start one layer independently.
    ambience.set_volume(0.2)?;
    ambience.set_speed(0.9)?;
    music.set_speed(0.9)?;
    ambience.play()?;

    thread::sleep(Duration::from_secs(1));
    music.play()?;

    ambience.wait_until_end()?;
    music.wait_until_end()?;

    Ok(())
}
