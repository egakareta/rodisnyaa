# rodisnyaa 🐈

Painless audio playback for native and web platforms.

A small Rust audio player built on top of [rodio](https://github.com/RustAudio/rodio) and [CPAL](https://github.com/RustAudio/cpal).

## Installation

```sh
cargo add rodisnyaa
```

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

    // Make it 25% faster
    nyaa.set_speed(1.25);

    // Seek to 30 seconds
    nyaa.try_seek_secs(30.0)?;
    Ok(())
}

fn play_embedded_audio(audio: &'static [u8]) -> Result<(), NyaaError> {
    let mut nyaa = Nyaa::new();
    nyaa.play_static_bytes(audio)?;
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
    let mut nyaa = Nyaa::new();
    nyaa.start_asset_playback(&asset)?;
    Ok(())
}
```

> Browsers require audio output to be opened in response to a user gesture.
> Start playback from a click, pointer, or keyboard event when possible.

Please find these [examples](https://github.com/egakareta/rodisnyaa/tree/master/examples) for more guidance.

## Feature flags

| Feature         | Enables              |
| --------------- | -------------------- |
| `mp3` (default) | MP3 decoding         |
| `flac`          | FLAC decoding        |
| `mp4`           | MP4 decoding         |
| `vorbis`        | Vorbis decoding      |
| `wav` (default) | WAV decoding         |
| `asio`          | ASIO backend         |
| `jack`          | JACK backend         |
| `nightly`       | AudioWorklet backend |

## Motivation

[rodio](https://github.com/rustaudio/rodio) and [CPAL](https://github.com/RustAudio/cpal) are both great libraries, but they are not very ergonomic to use. Here are some issues that I have encountered while using them:

1. There is no Null backend, so you must forever live with `Option<MixerDeviceSink>`.
2. `rodio::Player::try_seek` deadlocks on WASM.
3. rodio seems to prioritize correctness, which is fine, but you end up needing to wrap a lot of code yourself to get predictable behavior e.g. setting time position or playback speed when no audio is playing.
4. rodio does not officially support AudioWorklet while CPAL does.
5. No convenience code in rodio for native and web audio resource handling, causing a ton of `#[cfg(target_arch = "wasm32")]`.
6. No waveform visualization.

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
