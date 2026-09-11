use std::thread;

use euphorium::{Output, SoundSource, Soundscape, SoundscapeError};
use web_time::Duration;

static AUDIO: &[u8] = include_bytes!("polar 240 yay.mp3");

fn main() -> Result<(), SoundscapeError> {
    let devices = Output::available_devices();
    println!("Available output devices ({}):", devices.len());
    for (index, device) in devices.iter().enumerate() {
        let default_marker = if device.is_default() {
            " (default)"
        } else {
            ""
        };
        println!(
            "{index}: {} [{}]{default_marker} ({:?}/{:?})",
            device.name(),
            Output::backend_label(device.backend()),
            device.device_type(),
            device.interface_type(),
        );
    }

    if devices.is_empty() {
        println!("No output devices are available.");
        return Ok(());
    }

    let soundscape = Soundscape::new();
    let sound = soundscape.create_sound("music", SoundSource::static_bytes(AUDIO))?;
    sound.set_looping(true)?;
    sound.play()?;
    println!(
        "Playing through the default output: {}",
        soundscape.backend_display_name()
    );

    for device in &devices {
        thread::sleep(Duration::from_secs(2));
        match soundscape.switch_device(device) {
            Ok(()) => println!(
                "Switched playback to {} [{}]",
                device.name(),
                Output::backend_label(device.backend())
            ),
            Err(error) => eprintln!("Could not switch to {}: {error}", device.name(),),
        }
    }

    sound.stop()?;
    Ok(())
}
