#![cfg(target_arch = "wasm32")]

use std::time::Duration;

use euphorium::{PlaybackState, SoundAsset, SoundSource, Soundscape};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const WAV_DATA_URL: &str = "data:audio/wav;base64,UklGRiwAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQgAAACAgICAgICAgA==";

async fn wait_for_browser_task() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move || {
            resolve
                .call0(&JsValue::UNDEFINED)
                .expect("timer promise should resolve");
        });

        web_sys::window()
            .expect("browser window is unavailable")
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0)
            .expect("browser timer should be scheduled");
    });

    JsFuture::from(promise)
        .await
        .expect("browser task timer should resolve");
}

#[wasm_bindgen_test]
async fn duration_from_asset_fetches_and_decodes_browser_url() {
    let asset = SoundAsset::new("missing/native/audio.wav", WAV_DATA_URL);

    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound("browser", SoundSource::asset(asset))
        .expect("the browser sound should be created");
    sound
        .load()
        .await
        .expect("browser audio asset should be fetched and decoded");
    let duration = sound
        .duration()
        .expect("the browser sound should remain valid")
        .expect("WAV duration should be available");

    assert!(duration > Duration::ZERO);

    sound
        .play()
        .expect("cached browser audio asset should be playable");

    assert!(!sound.is_loading().unwrap());
}

#[wasm_bindgen_test]
async fn soundscape_owns_pending_asset_playback_and_duration() {
    let asset = SoundAsset::new("missing/native/audio.wav", WAV_DATA_URL);
    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound("pending", SoundSource::asset(asset))
        .expect("the pending browser sound should be created");

    sound
        .play()
        .expect("browser audio playback should start loading");
    assert!(sound.is_loading().unwrap());

    loop {
        let failures = soundscape.update();
        assert!(
            failures.is_empty(),
            "browser audio asset should be playable"
        );
        if !sound.is_loading().unwrap() {
            break;
        }

        wait_for_browser_task().await;
    }

    assert!(!sound.is_loading().unwrap());
    assert!(
        sound
            .duration()
            .unwrap()
            .is_some_and(|duration| duration > Duration::ZERO)
    );
}

#[wasm_bindgen_test]
async fn group_loads_browser_assets_with_shared_configuration() {
    let first = SoundAsset::new("missing/native/first.wav", WAV_DATA_URL);
    let second = SoundAsset::new("missing/native/second.wav", WAV_DATA_URL);
    let soundscape = Soundscape::new();
    let group = soundscape.create_group("browser").unwrap();
    group.set_volume(0.4).unwrap();
    let first = group
        .create_sound("first", SoundSource::asset(first))
        .unwrap();
    let second = group
        .create_sound("second", SoundSource::asset(second))
        .unwrap();

    first.load().await.expect("the first asset should load");
    second.load().await.expect("the second asset should load");

    assert_eq!(group.sounds().unwrap().len(), 2);
    assert!(
        first
            .duration()
            .unwrap()
            .is_some_and(|duration| !duration.is_zero())
    );
    assert_eq!(first.playback_state().unwrap(), PlaybackState::Idle);
    assert_eq!(second.playback_state().unwrap(), PlaybackState::Idle);
}
