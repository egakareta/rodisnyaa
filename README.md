# rodisnyaa

Painless audio playback for native and web platforms.

A small Rust audio player built on top of [rodio](https://github.com/RustAudio/rodio) and [CPAL](https://github.com/RustAudio/cpal).

## Usage

```rust,no_run
use rodisnyaa::{AudioAsset, Nyaa, NyaaError};

fn main() -> Result<(), NyaaError> {
    let mut nyaa = Nyaa::new();

    // Set the volume to 80%
    nyaa.set_volume(0.8);

    // Play an audio file from path
    nyaa.play_file("audio.mp3")?;
    nyaa.wait_until_end();

    // Play some embedded bytes
    let audio: &'static [u8] = include_bytes!("../examples/polar 240 yay.mp3");
    nyaa.play_static_bytes(audio)?;

    // Make it 25% faster
    nyaa.set_speed(1.25);

    // Seek to 30 seconds
    nyaa.try_seek_secs(30.0)?;
    Ok(())
}

fn where_is_my_audio_at(nyaa: &mut Nyaa) -> Result<(), NyaaError> {
    nyaa.pause();
    nyaa.resume();
    println!("{} / {}", nyaa.position_formatted(), nyaa.duration_formatted());
    Ok(())
}

fn play_my_audio_cross_platform() -> Result<(), NyaaError> {
    // Use file path "assets/music.mp3" on native, resource "audio.mp3" on web
    let asset = AudioAsset::new("assets/music.mp3", "audio.mp3");
    let mut player = Nyaa::new();
    player.start_asset_playback(&asset)?;
    Ok(())
}
```

> Browsers require audio output to be opened in response to a user gesture.
> Start playback from a click, pointer, or keyboard event when possible.

Please find these [examples](https://github.com/egakareta/rodisnyaa/tree/master/examples) for more guidance.

## Feature flags

| Feature             | Enables              |
| ------------------- | -------------------- |
| `mp3` (default)     | MP3 decoding         |
| `flac`              | FLAC decoding        |
| `mp4`               | MP4 decoding         |
| `vorbis`            | Vorbis decoding      |
| `wav`               | WAV decoding         |
| `asio`              | ASIO backend         |
| `jack`              | JACK backend         |
| `nightly` (default) | AudioWorklet backend |

## Development

This repository uses [mise](https://mise.jdx.dev/).

```sh
mise bootstrap
mise install
mise run check
```

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
