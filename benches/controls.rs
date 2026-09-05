mod support;

use criterion::{criterion_group, criterion_main};

criterion_group! {
    name = controls;
    config = support::benchmark_criterion();
    targets =
        support::bench_play,
        support::bench_stop,
        support::bench_seek,
        support::bench_speed,
        support::bench_volume
}
criterion_main!(controls);
