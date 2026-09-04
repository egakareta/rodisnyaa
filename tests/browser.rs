#![cfg(target_arch = "wasm32")]

use rodisnyaa::{AudioAsset, Nyaa};
use std::time::Duration;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const WAV_DATA_URL: &str = "data:audio/wav;base64,UklGRiwAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQgAAACAgICAgICAgA==";

#[wasm_bindgen_test]
async fn duration_from_asset_fetches_and_decodes_browser_url() {
    let asset = AudioAsset::new("missing/native/audio.wav", WAV_DATA_URL);

    let duration = Nyaa::duration_from_asset(&asset)
        .await
        .expect("browser audio asset should be fetched and decoded")
        .expect("WAV duration should be available");

    assert!(duration > Duration::ZERO);
}
