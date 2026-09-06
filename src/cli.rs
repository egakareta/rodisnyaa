#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::process::ExitCode;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Parser)]
#[command(about = "Play an audio file", version)]
struct Cli {
    /// Audio file to play.
    path: PathBuf,

    /// Playback volume, where 1 is the original volume.
    #[arg(short, long, default_value_t = 1.0, value_parser = non_negative_f32)]
    volume: f32,

    /// Playback speed, where 1 is the original speed.
    #[arg(short, long, default_value_t = 1.0, value_parser = positive_f32)]
    speed: f32,

    /// Position in seconds at which playback starts.
    #[arg(long, default_value_t = 0.0, value_parser = non_negative_f64)]
    start: f64,
}

#[cfg(not(target_arch = "wasm32"))]
fn non_negative_f32(value: &str) -> Result<f32, String> {
    let value = value
        .parse::<f32>()
        .map_err(|error| format!("invalid number: {error}"))?;

    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err("value must be a finite, non-negative number".to_owned())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn positive_f32(value: &str) -> Result<f32, String> {
    let value = non_negative_f32(value)?;

    if value > 0.0 {
        Ok(value)
    } else {
        Err("value must be greater than zero".to_owned())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn non_negative_f64(value: &str) -> Result<f64, String> {
    let value = value
        .parse::<f64>()
        .map_err(|error| format!("invalid number: {error}"))?;

    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err("value must be a finite, non-negative number".to_owned())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run(cli: Cli) -> Result<(), rodisnyaa::NyaaError> {
    let mut nyaa = rodisnyaa::Nyaa::try_new()?;
    nyaa.set_volume(cli.volume);
    nyaa.try_set_speed(cli.speed)?;
    nyaa.try_seek_secs(cli.start)?;
    nyaa.play_file(cli.path)?;
    nyaa.wait_until_end();

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("the rodisnyaa CLI is only available on native targets");
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_path_with_default_playback_settings() {
        let cli = Cli::try_parse_from(["rodisnyaa", "song.mp3"]).unwrap();

        assert_eq!(cli.path, PathBuf::from("song.mp3"));
        assert_eq!(cli.volume, 1.0);
        assert_eq!(cli.speed, 1.0);
        assert_eq!(cli.start, 0.0);
    }

    #[test]
    fn accepts_custom_playback_settings() {
        let cli = Cli::try_parse_from([
            "rodisnyaa",
            "song.wav",
            "--volume",
            "0.5",
            "--speed",
            "1.25",
            "--start",
            "30",
        ])
        .unwrap();

        assert_eq!(cli.volume, 0.5);
        assert_eq!(cli.speed, 1.25);
        assert_eq!(cli.start, 30.0);
    }

    #[test]
    fn rejects_invalid_playback_settings() {
        for arguments in [
            ["rodisnyaa", "song.mp3", "--volume", "-1"],
            ["rodisnyaa", "song.mp3", "--speed", "0"],
            ["rodisnyaa", "song.mp3", "--start", "NaN"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err());
        }
    }
}
