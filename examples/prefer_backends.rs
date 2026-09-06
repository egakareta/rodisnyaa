//! Prefer a CPAL audio backend by name, then play a file through it.

static AUDIO: &[u8] = include_bytes!("polar 240 yay.mp3");

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), rodisnyaa::NyaaError> {
    use rodisnyaa::{AudioOutput, Nyaa};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut list = false;
    let mut backend_name = AudioOutput::DEFAULT_BACKEND_LABEL.to_string();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--list" => list = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            "--backend" => {
                index += 1;
                backend_name = args.get(index).cloned().unwrap_or_default();
            }
            flag if flag.starts_with("--backend=") => {
                backend_name = flag["--backend=".len()..].to_string();
            }
            _positional => {}
        }
        index += 1;
    }

    if list {
        for label in AudioOutput::available_backend_labels() {
            println!("{label}");
        }
        return Ok(());
    }

    // Start deferred so choosing a backend never grabs the audio device early.
    // The preference lives on `Nyaa`; no caller-side `Option` state needed.
    let mut nyaa = Nyaa::new_deferred_with_preferred_backend(None);
    println!("deferred output: {}", nyaa.backend_display_name());

    if !nyaa.set_preferred_audio_backend_by_name(&backend_name) {
        eprintln!("unknown backend {backend_name:?}; expected one of:");
        for label in AudioOutput::available_backend_labels() {
            eprintln!("  {label}");
        }
        std::process::exit(2);
    }
    println!("preferred backend: {}", nyaa.preferred_backend_name());

    // Open the preferred backend up front so failures surface before playback.
    // (Playback would retry automatically, so this step is optional.)
    nyaa.ensure_audio_output()?;
    println!("active output: {}", nyaa.backend_display_name());

    nyaa.play_static_bytes(AUDIO)?;
    nyaa.wait_until_end();

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn print_usage() {
    println!(
        "Usage: prefer_backends [--backend <name>] [--list] [path]\n\
         \n\
         --backend <name>  Backend label (case-insensitive) or \"Default\".\n\
         --list            Print available backend labels and exit.\n\
         path              Audio file to play (default: bundled sample)."
    );
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // Native file playback example isn't available on WASM.
}
