---
name: rodisnyaa
description: "Integrate rodisnyaa audio playback into downstream Rust applications on native or WebAssembly. Use when installing or configuring rodisnyaa, playing files, bytes, or assets, adding transport controls, selecting output devices, applying effects, preserving pitch, drawing waveforms, or fixing browser audio startup."
argument-hint: "Describe the target, audio source, and playback UI"
---

# Using rodisnyaa

Use rodisnyaa's public API to add audio playback to an application that consumes the crate. Do
not use this skill to modify rodisnyaa itself.

## Version Scope

This skill targets rodisnyaa `0.1.x`.

1. Inspect the consumer's `Cargo.toml` and resolved version with
   `cargo tree -p rodisnyaa` before changing code.
2. If the resolved version is not `0.1.x`, inspect that version's crate documentation and source
   before applying these patterns.
3. Preserve the consumer's existing Rust edition, runtime, framework, feature policy, error type,
   and test conventions.

## Inspect First

Determine these facts before implementing:

- Native, `wasm32-unknown-unknown`, or both.
- Audio formats that must decode.
- Filesystem paths, embedded bytes, runtime bytes, or native-path/browser-URL assets.
- Whether playback begins in a browser gesture handler.
- Whether the application has a persistent update loop in which to poll loading and state.
- Whether output selection, effects, pitch preservation, or waveform rendering is required.
- Whether an existing `Nyaa` or shared `Output` already exists. Reuse it when its lifetime
  matches the playback UI; do not create a player for every update or render.

## Choose Features

Prefer `default-features = false` and select only the features you need:

```toml
[dependencies]
rodisnyaa = { version = "0.1", default-features = false, features = ["mp3", "wav"] }
```

Use `flac`, `mp3`, `mp4`, `vorbis`, and `wav` for decoding those formats. Use `asio` or `jack`
when the native deployment requires that CPAL backend. Use `nightly` only for the AudioWorklet
path; it requires additional WASM build and server configuration.

## Implement Playback

1. Read [API Guide](./references/api.md) when choosing output devices, controls, effects, errors,
   or direct rodio integration.
2. Read [Browser and WASM](./references/browser.md) before changing a browser build or enabling
   `nightly`.
3. Read [Examples](./references/examples.md) before generating a full
   integration.
4. Read [Waveforms](./references/waveforms.md) before implementing waveform decoding or drawing.

## Verify

Verify behavior on a system with an audio output:

- Playback starts and errors are visible.
- Pause, resume, seek, speed, volume, stop, and end-state transitions behave as the UI reports.
- Browser playback starts from a user gesture and pending playback is polled.
- Required asset URLs are served successfully with appropriate CORS policy.
