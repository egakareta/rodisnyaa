#![cfg(target_arch = "wasm32")]

#[path = "../benches/support/mod.rs"]
mod support;

use wasm_bindgen_test::{Criterion, wasm_bindgen_bench, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

fn configure(criterion: &mut Criterion) {
    *criterion = support::benchmark_criterion();
}

#[wasm_bindgen_bench]
fn play(criterion: &mut Criterion) {
    configure(criterion);
    support::bench_play(criterion);
}

#[wasm_bindgen_bench]
fn stop(criterion: &mut Criterion) {
    configure(criterion);
    support::bench_stop(criterion);
}

#[wasm_bindgen_bench]
fn seek(criterion: &mut Criterion) {
    configure(criterion);
    support::bench_seek(criterion);
}

#[wasm_bindgen_bench]
fn speed(criterion: &mut Criterion) {
    configure(criterion);
    support::bench_speed(criterion);
}

#[wasm_bindgen_bench]
fn volume(criterion: &mut Criterion) {
    configure(criterion);
    support::bench_volume(criterion);
}
