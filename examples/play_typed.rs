use std::thread;

use rodisnyaa::{Nyaa, NyaaError, SoundSource};
use web_time::Duration;

static MUSIC: &[u8] = include_bytes!("polar 240 yay.mp3");
static CLICK: &[u8] = include_bytes!("sfx/soft-hitclap.ogg");
static FINISH: &[u8] = include_bytes!("sfx/soft-hitfinish.ogg");

// Nested paths create the `effects` and `effects/ui` groups automatically.
rodisnyaa::sound_key! {
    enum GameSound {
        Music => "music/theme",
        Click => "effects/ui/click",
        Finish => "effects/ui/finish",
    }
}

fn main() -> Result<(), NyaaError> {
    let nyaa = Nyaa::<GameSound>::builder()
        .sound(GameSound::Music, SoundSource::static_bytes(MUSIC))
        .sound(GameSound::Click, SoundSource::static_bytes(CLICK))
        .placeholder(GameSound::Finish) // Placeholder sound can be assigned later.
        .build()?;

    // `build()` validates every key, so this returns `Sound` directly instead of `Option<Sound>`.
    let music = nyaa.sound(GameSound::Music);
    let click = nyaa.sound(GameSound::Click);
    let finish = nyaa.sound(GameSound::Finish);

    // The same sounds remain reachable by path for dynamic content.
    assert_eq!(
        nyaa.find_sound("music/theme")
            .expect("typed sound exists")
            .id(),
        music.id()
    );
    println!("music path: {}", music.path()?);
    println!("click path: {}", click.path()?);

    // Paths double as mixer buses: one volume scales the whole subtree!
    if let Some(effects) = nyaa.group("effects") {
        effects.set_volume(0.8)?;
    }
    music.set_volume(0.5)?;
    music.set_looping(true)?;
    music.play()?;

    thread::sleep(Duration::from_secs(1));
    println!("click!");
    click.replay()?;

    thread::sleep(Duration::from_millis(500));
    println!("click!");
    click.replay()?;

    // Placeholders will error if you try to play them.
    assert!(finish.play().is_err());

    // Assign the audio resource to the placeholder and play it.
    println!("finish!");
    finish.set_source(SoundSource::static_bytes(FINISH))?;
    finish.play()?;

    finish.wait_until_end()?;

    music.stop()?;
    Ok(())
}
