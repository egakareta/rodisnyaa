# rodisnyaa API Guide

## Supported Imports

Most public types are re-exported at the crate root:

```rust
use rodisnyaa::{
    AudioAsset, AudioEffects, AudioOutput, Nyaa, NyaaError, PlaybackEvent, PlaybackState,
    Waveform,
};
```

The following public modules are also supported:

- `rodisnyaa::wsola` for direct rodio source time stretching.
- `rodisnyaa::rodio` and `rodisnyaa::cpal` as re-exports matching the versions used by the crate.

## Player Construction

- `Nyaa::new()` (Best-effort default output, recommended): Native output failure is deferred; WASM output opens when playback begins.
- `Nyaa::new_with_output(output)` (Reuse a selected/shared output): Connects the player to that `AudioOutput`.
- `nyaa.retry_audio_output()` (Recover deferred output): Opens the selected output and marks failures in player state.
- `nyaa.has_audio_output()` (Inspect output readiness): May be false in a browser before the first gesture-backed playback.

Keep a player alive in application state. Dropping it ends its ownership of playback resources.

`Nyaa::try_new()` also exists and returns default-output failures with `AudioOutputError`; WASM still defers actual WebAudio opening.

## Loading and Playback

Use one of these five source-loading APIs to start playback, get the source duration, or build a waveform:

1. Native path (non-wasm targets only)

- `play_file(path)`
- `duration_from_file(path)`
- `Waveform::from_file(path)`/`Waveform::builder_from_file(path)`

2. `'static` bytes

- `play_static_bytes(bytes)`
- `duration_from_static_bytes(bytes)`
- `Waveform::from_static_bytes(bytes)`/`Waveform::builder_from_static_bytes(bytes)`

These accept `impl AsRef<[u8]>`, but rodisnyaa copies that slice into a new `Arc<[u8]>`. Use static-byte
APIs with `include_bytes!` when avoiding that copy matters.

`load_static_bytes()` records a source and duration without playing it. This is useful before
`play_range()`. Starting a new source preserves current volume, speed, pitch, effects, looping, and
configured range.

3. Runtime bytes

- `play_shared_bytes(bytes)`
- `duration_from_shared_bytes(bytes)`
- `Waveform::from_shared_bytes(bytes)`/`Waveform::builder_from_shared_bytes(bytes)`

4. Native path plus browser URL

- `play_asset(asset)`/`start_asset_playback(asset)`
- `duration_from_asset(asset)`
- `Waveform::from_asset(asset)`/`Waveform::builder_from_asset(asset)`

`AudioAsset::new(native_path, wasm_url)` stores both locations. On native, asset methods open the
path. On WASM, they fetch and decode the complete response in memory. Cloned assets share fetched
browser bytes; `clear_browser_cache()` removes the asset-level cached reference.

5. Public rodio source

- Construct and use the source directly.
- `Source::total_duration(source)`
- `Waveform::from_source(source)`/`Waveform::builder_from_source(source)`

## Transport and State

- `pause()` and `resume()` are no-ops when the current state does not support the action.
- `stop()` empties playback and changes state to `Idle`, but retains the reported position and
  removes the current source reference. Seek to zero or start/load another source explicitly.
- `wait_until_end()` blocks the calling thread. Restrict it to synchronous command-line or worker
  contexts.
- `try_seek(Duration)` uses the direct native seek path and the decode-based path on WASM.
- `try_seek_with_decode(Duration)` rebuilds the decoder; it is WASM-safe and slower on native.
- `try_seek_secs(f64)` constructs a `Duration` directly. Validate finite, non-negative values
  before calling it.
- `set_loop_range(start..end)` defines playback bounds for active and future sources.
- `play_range(start..end)` starts a previously loaded or played source at `start` and stops at
  `end` unless looping is enabled.
- `set_looping(true)` loops the complete source or configured loop range.
- `clear_loop_range()` restores complete-source bounds.

Use `state()` for `Idle`, `Loading`, `Playing`, `Paused`, `Ended`, or `Failed`. Calling `state()` or
`poll_event()` also detects natural completion. Drain `poll_event()` when every state transition
matters; its current event is `PlaybackEvent::StateChanged { previous, current }`.

For UI values:

- `position()` returns source time and accounts for playback speed.
- `clamped_position()` bounds position to known duration and is suitable for controls.
- `duration()` returns `Option<Duration>` because some sources do not report a duration.
- `seek_range()` returns `0.0..=duration_seconds`, or `0.0..=1.0` when duration is unknown.
- `position_formatted()` and `duration_formatted()` produce `H:MM:SS` or `M:SS`.
- `format_timestamp`, `format_timestamp_secs`, and `parse_timestamp` are public helpers.

Seeking an ended/empty player updates its held position but does not restart playback. Replay by
starting the source again.

## Volume, Speed, and Pitch

- `set_volume(1.0)` preserves input amplitude. Validate finite, non-negative values; values above
  `1.0` amplify and can clip.
- `set_speed(1.0)` is original speed. Prefer `try_set_speed()` when pitch preservation is enabled
  because changing speed may rebuild and seek the decoder.
- `set_preserve_pitch(true)` enables WSOLA and may rebuild active playback. It returns
  `Result<(), NyaaError>`.
- Keep pitch-preserving speed in `0.25..=8.0`. The underlying WSOLA source clamps to that default
  range, while `Nyaa` retains the requested value for position accounting.

For a custom rodio source pipeline, import `rodisnyaa::wsola::WsolaSourceExt` and call
`source.wsola(speed)`, or construct `Wsola::new(source, speed)`. A WSOLA input must retain a fixed
channel count and sample rate for its entire lifetime. Prefer `Nyaa::set_preserve_pitch` for normal
player use.

## Output Selection

Use these cached enumeration APIs for settings UIs:

- `AudioOutput::available_backends()`
- `AudioOutput::available_output_devices()`

Use `refresh_available_backends()` or `refresh_available_output_devices()` after an explicit
refresh/device-change action. Use `available_output_devices_for_backend(backend)` for a live,
backend-specific query.

Open an output with `AudioOutput::try_new_with_backend(backend)` or
`AudioOutput::try_new_with_device(&device)`. `AudioDevice` exposes `backend()`, `id()`,
`description()`, `name()`, `is_default()`, `device_type()`, and `interface_type()`.

`Nyaa::switch_audio_backend()` and `switch_audio_device()` stop that player's current source and
hold its last position. Other players that shared the previous output are unaffected.

## Effects

Build effects from `AudioEffects::default()` and replace only required fields. `set_effects()`
validates the chain and, during active playback, rebuilds the decoder at the current position while
preserving paused state.

Effects run in this order: input gain, high-pass filter, low-pass filter, distortion, automatic
gain, reverb, limiter, fade-in.

Validation requirements:

- `input_gain`: finite and non-negative.
- `FilterEffect`: nonzero frequency and finite positive `q`.
- `DistortionEffect`: finite non-negative gain and finite positive threshold.
- `AutomaticGainEffect`: finite positive target level and maximum gain.
- `ReverbEffect`: finite non-negative amplitude.
- `LimiterEffect`: finite negative `threshold_db` and finite non-negative `knee_width_db`.

## Errors

Return or surface `NyaaError` for file, decode, seek, effect, playback-range, source, output, and
WASM fetch failures. Handle `AudioOutputError` separately when the UI can recover by refreshing
devices or choosing another backend. A previously enumerated `AudioDevice` can become stale before
it is opened.
