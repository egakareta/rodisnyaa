#![cfg(target_arch = "wasm32")]

use rodisnyaa::{AudioAsset, Nyaa};
use std::time::Duration;
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
    let asset = AudioAsset::new("missing/native/audio.wav", WAV_DATA_URL);

    let duration = Nyaa::duration_from_asset(&asset)
        .await
        .expect("browser audio asset should be fetched and decoded")
        .expect("WAV duration should be available");

    assert!(duration > Duration::ZERO);
}

#[wasm_bindgen_test]
async fn nyaa_owns_pending_asset_playback_and_duration() {
    let asset = AudioAsset::new("missing/native/audio.wav", WAV_DATA_URL);
    let mut nyaa = Nyaa::new();

    nyaa.start_asset_playback(&asset)
        .expect("browser audio playback should start loading");
    assert!(nyaa.is_loading());

    loop {
        if let Some(result) = nyaa.poll_pending_playback() {
            result.expect("browser audio asset should be playable");
            break;
        }

        wait_for_browser_task().await;
    }

    assert!(!nyaa.is_loading());
    assert!(
        nyaa.duration()
            .is_some_and(|duration| duration > Duration::ZERO)
    );
}
