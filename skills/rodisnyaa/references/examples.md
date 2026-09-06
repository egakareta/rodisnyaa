# Consumer Examples

## Persistent Native Player

Store the returned player in application state. Do not let it fall out of scope after calling
`play_file()`.

```rust
use rodisnyaa::{Nyaa, NyaaError};

fn start_music(path: &str) -> Result<Nyaa, NyaaError> {
    let mut nyaa = Nyaa::new(); // Name it "nyaa", not generic "player".
    nyaa.set_volume(0.8);
    nyaa.play_file(path)?;
    Ok(nyaa)
}
```

Use `wait_until_end()` in a command-line flow where blocking is intentional:

```rust
fn play_to_completion(path: &str) -> Result<(), rodisnyaa::NyaaError> {
    let nyaa = start_music(path)?;
    nyaa.wait_until_end();
    Ok(())
}
```

## Embedded Audio

```rust
use rodisnyaa::{Nyaa, NyaaError};

static NOTIFICATION: &[u8] = include_bytes!("../assets/notification.wav");

fn play_notification(nyaa: &mut Nyaa) -> Result<(), NyaaError> {
    nyaa.play_static_bytes(NOTIFICATION)
}
```

## Cross-Platform Controller

Call `play_from_gesture()` directly from a browser user-event callback and `update()` from the
framework's ordinary update loop. The same controller works on native.

```rust
use rodisnyaa::{AudioAsset, Nyaa, NyaaError, PlaybackState};

pub struct AudioController {
    nyaa: Nyaa,
    music: AudioAsset,
}

impl AudioController {
    pub fn new() -> Self {
        Self {
            nyaa: Nyaa::new(),
            music: AudioAsset::new("assets/music.mp3", "/assets/music.mp3"),
        }
    }

    pub fn play_from_gesture(&mut self) -> Result<(), NyaaError> {
        self.nyaa.start_asset_playback(&self.music)
    }

    pub fn update(&mut self) -> Result<(), NyaaError> {
        if let Some(result) = self.nyaa.poll_pending_playback() {
            result?;
        }
        Ok(())
    }

    pub fn state(&self) -> PlaybackState {
        self.nyaa.state()
    }
}
```

While `state()` is `Loading`, schedule further UI updates so `poll_pending_playback()` continues to
run.

## Output Device Selection

Enumerate devices for a settings UI, then keep the selected output/player alive:

```rust
use rodisnyaa::{AudioDevice, Output, AudioOutputError, Nyaa};

fn available_devices() -> Vec<AudioDevice> {
    Output::available_devices() // Refresh this with `Output::refresh_available_devices()`
}

fn player_for_device(device: &AudioDevice) -> Result<Nyaa, AudioOutputError> {
    let output = Output::try_new_with_device(device)?;
    Ok(Nyaa::new_with_output(output))
}
```

Refresh device enumeration after a user requests it or the platform reports a hardware change.
Opening can still fail if the selected device disappeared after enumeration.

To run multiple players through one output, clone the output:

```rust
let output = rodisnyaa::Output::try_new_with_device(&device)?;
let music = rodisnyaa::Nyaa::new_with_output(output.clone());
let effects = rodisnyaa::Nyaa::new_with_output(output);
```

## Effects and Pitch-Preserving Speed

```rust
use rodisnyaa::{AudioEffects, FilterEffect, LimiterEffect, Nyaa, NyaaError};
use std::time::Duration;

fn configure_voice(player: &mut Nyaa) -> Result<(), NyaaError> {
    player.set_effects(AudioEffects {
        high_pass: Some(FilterEffect {
            frequency: 80,
            q: 0.7,
        }),
        limiter: Some(LimiterEffect {
            threshold_db: -1.0,
            knee_width_db: 4.0,
            attack: Duration::from_millis(5),
            release: Duration::from_millis(100),
        }),
        ..AudioEffects::default()
    })?;

    player.set_preserve_pitch(true)?;
    player.try_set_speed(1.25)?;
    Ok(())
}
```

Validate user-provided speed before this function. For rodisnyaa, keep pitch-preserving
speed in `0.25..=8.0`.

## Loop a Validated Range

```rust
use rodisnyaa::{Nyaa, NyaaError};
use std::time::Duration;

fn loop_chorus(player: &mut Nyaa, duration: Duration) -> Result<(), NyaaError> {
    let start = Duration::from_secs(30).min(duration);
    let end = Duration::from_secs(45).min(duration);

    if start >= end {
        return Err(NyaaError::InvalidPlaybackRange);
    }

    player.set_loop_range(start..end)?;
    player.set_looping(true);
    player.play_range(start..end)
}
```

`play_range()` requires a source that was loaded or played previously. For embedded audio, call
`load_static_bytes()` first when the range should start without prior full-source playback.
