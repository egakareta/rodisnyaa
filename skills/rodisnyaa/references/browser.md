# Browser and WASM Integration

Read this reference before implementing `wasm32-unknown-unknown` playback.

## Select the Browser Path

Use the normal WebAudio path unless the application specifically needs CPAL's AudioWorklet
support. A basic browser dependency can remain on stable Rust:

```toml
[dependencies]
rodisnyaa = { version = "0.1", default-features = false, features = ["mp3", "wav"] }
```

Enable rodisnyaa's `nightly` feature only for the AudioWorklet path. That path needs a nightly
toolchain, `rust-src`, a shared-memory WASM build, and cross-origin isolation headers; see
[AudioWorklet Setup](#audioworklet-setup).

## Asset Model

Use `AudioAsset` to keep target-specific locations out of playback code:

```rust
use rodisnyaa::AudioAsset;

let asset = AudioAsset::new("assets/music.mp3", "/assets/music.mp3");
```

On WASM, the URL is resolved and fetched by the browser. The response must be successful and its
complete body is held in memory for decoding. Ensure that:

- The application server publishes the URL used by `wasm_url()`.
- Cross-origin assets send a CORS policy that permits the application origin.
- The chosen decoder feature matches the actual encoded format.
- Large assets fit the browser's memory budget because they are not streamed.

Clones of one `AudioAsset` share its browser-byte cache. Reuse the asset for replay and waveform
generation. Call `clear_browser_cache()` only when a later operation should fetch the URL again;
players, waveform builders, and WebAudio can retain their own references after that call.

## Start from a User Gesture

Browsers generally require audio output to open during transient user activation. In the click,
pointer, or keyboard handler, call `start_asset_playback()` synchronously:

```rust
fn on_play_gesture(
    player: &mut rodisnyaa::Nyaa,
    asset: &rodisnyaa::AudioAsset,
) -> Result<(), rodisnyaa::NyaaError> {
    player.start_asset_playback(asset)
}
```

This opens deferred WebAudio before spawning the asset fetch. Do not fetch or await first: the
browser can clear transient activation while the future is pending.

`play_asset().await` remains supported. Use it on native, after audio output is already initialized,
or in browser code whose activation behavior is otherwise guaranteed. Do not use it as the default
first-play pattern in an interactive browser UI.

## Complete Pending Playback

`start_asset_playback()` starts a background fetch on WASM and sets the state to `Loading`. It does
not autonomously append the decoded source when the fetch completes. Call this from the
application's update loop:

```rust
fn update_audio(player: &mut rodisnyaa::Nyaa) -> Result<(), rodisnyaa::NyaaError> {
    if let Some(result) = player.poll_pending_playback() {
        result?;
    }

    Ok(())
}
```

Request another UI update while `player.is_loading()` is true. On native,
`start_asset_playback()` starts synchronously and `poll_pending_playback()` returns `None`, so this
pattern can be shared across targets.

Calling `start_asset_playback()` while another asset fetch is pending is a no-op. Disable or
otherwise serialize source-changing controls during `Loading` if the product expects a different
selection to replace the pending request.

## AudioWorklet Setup

For the `nightly` feature, add a pinned nightly toolchain with `rust-src` and configure the WASM
target for atomics and shared memory. The rodisnyaa example uses:

```toml
[target.wasm32-unknown-unknown]
rustflags = [
    "-C", "target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C", "link-arg=--shared-memory",
    "-C", "link-arg=--max-memory=1073741824",
    "-C", "link-arg=--import-memory",
    "-C", "link-arg=--export=__wasm_init_tls",
    "-C", "link-arg=--export=__tls_size",
    "-C", "link-arg=--export=__tls_align",
    "-C", "link-arg=--export=__tls_base",
]

[unstable]
build-std = ["std", "panic_abort"]
```

Serve the application with these response headers:

```text
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Resource-Policy: same-site
```

Adapt memory size and resource policy to the application's deployment. All subresources must also
satisfy cross-origin isolation requirements.

With `wasm32` plus `nightly`, rodisnyaa invokes its AudioWorklet text-encoding polyfill while
opening output. `rodisnyaa::patch::ensure_audioworklet_text_polyfill()` is public for the specific
case where custom initialization reaches the same missing `TextDecoder`/`TextEncoder` environment;
do not call it routinely on stable/native paths.

## Verify Browser Behavior

1. Add the target with `rustup target add wasm32-unknown-unknown` for the selected toolchain.
2. Run `cargo check --target wasm32-unknown-unknown` in the consumer project.
3. Run the project's browser test command if it has one.
4. Serve over HTTP with the production-equivalent asset paths and headers.
5. Confirm first playback from a real user gesture, loading/error state, replay from cache, seek,
   and completion.
6. Confirm failed HTTP responses and CORS failures become visible application errors.
