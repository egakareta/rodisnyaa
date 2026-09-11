use std::thread;

use euphorium::{Output, SoundSource, Soundscape, SoundscapeError};
use web_time::Duration;

static AUDIO: &[u8] = include_bytes!("polar 240 yay.mp3");

fn main() -> Result<(), SoundscapeError> {
    let backends = Output::available_backends();
    println!("Available audio backends ({}):", backends.len());
    for (index, backend) in backends.iter().enumerate() {
        println!("{index}: {}", Output::backend_label(*backend));
    }

    if backends.is_empty() {
        println!("No audio backends are available.");
        return Ok(());
    }

    let soundscape = Soundscape::new();
    let sound = soundscape.create_sound("music", SoundSource::static_bytes(AUDIO))?;
    sound.set_looping(true)?;
    sound.play()?;
    println!(
        "Playing through the default backend: {}",
        soundscape.backend_display_name()
    );

    for backend in backends {
        thread::sleep(Duration::from_secs(2));
        let label = Output::backend_label(backend);
        match soundscape.switch_backend(backend) {
            Ok(()) => println!("Switched playback to {label}"),
            Err(error) => eprintln!("Could not switch to {label}: {error}"),
        }
    }

    sound.stop()?;
    Ok(())
}
