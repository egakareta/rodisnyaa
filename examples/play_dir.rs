use include_dir::{Dir, include_dir};
use rodisnyaa::{Nyaa, NyaaError, SoundSource};

static SFX_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/examples/sfx");

fn main() -> Result<(), NyaaError> {
    let candidates: Vec<_> = SFX_DIR
        .files()
        .filter(|file| {
            file.path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some()
        })
        .collect();

    assert!(
        !candidates.is_empty(),
        "no playable files found, enable a decoder feature"
    );

    let nyaa = Nyaa::new();

    for file in &candidates {
        let name = file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        nyaa.create_sound(name, SoundSource::static_bytes(file.contents()))?;
    }

    let sounds = nyaa.sounds();
    println!("Available sounds ({}):", sounds.len());
    for sound in &sounds {
        let name = sound.name().unwrap_or_else(|_| "?".to_owned());
        let duration = sound
            .duration()
            .ok()
            .flatten()
            .map(|d| format!("{:.2}s", d.as_secs_f32()))
            .unwrap_or_else(|| "unknown duration".to_owned());
        let size = sound
            .len()
            .ok()
            .flatten()
            .map(|len| format!("{len} B"))
            .unwrap_or_else(|| "unknown size".to_owned());
        println!("- {name} ({duration}, {size})");
    }

    let sound = fastrand::choice(&sounds).expect("sounds is not empty");
    let name = sound.name().unwrap_or_else(|_| "?".to_owned());
    let duration = sound
        .duration()
        .ok()
        .flatten()
        .map(|d| format!("{:.2}s", d.as_secs_f32()))
        .unwrap_or_else(|| "unknown duration".to_owned());
    println!("Playing {name} ({duration})");
    sound.play()?;
    sound.wait_until_end()?;

    Ok(())
}
