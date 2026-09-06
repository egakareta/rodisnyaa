#[cfg(not(target_arch = "wasm32"))]
use criterion::Criterion;
use rodisnyaa::{Nyaa, Output, format_timestamp};
use std::hint::black_box;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::{Criterion, Instant};

const AUDIO_BYTES: &[u8] = include_bytes!("../../examples/THE UNFORGIVING.mp3");
const NEAR_END_MARGIN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum ByteStorage {
    Static,
    Shared,
}

impl ByteStorage {
    const ALL: [Self; 2] = [Self::Static, Self::Shared];

    fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Shared => "shared",
        }
    }

    fn play(self, nyaa: &mut Nyaa) {
        match self {
            Self::Static => nyaa.play_static_bytes(AUDIO_BYTES),
            Self::Shared => nyaa.play_shared_bytes(AUDIO_BYTES),
        }
        .expect("the benchmark MP3 should be playable");
    }
}

#[derive(Clone, Copy)]
struct BenchmarkPosition {
    label: &'static str,
    value: Duration,
}

pub fn benchmark_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
}

fn audio_duration() -> Duration {
    Nyaa::duration_from_static_bytes(AUDIO_BYTES)
        .expect("the benchmark MP3 duration should be readable")
        .expect("the benchmark MP3 should report its duration")
}

fn benchmark_positions() -> [BenchmarkPosition; 5] {
    let duration = audio_duration();

    [
        BenchmarkPosition {
            label: "start",
            value: Duration::ZERO,
        },
        BenchmarkPosition {
            label: "quarter",
            value: duration.mul_f64(0.25),
        },
        BenchmarkPosition {
            label: "middle",
            value: duration.mul_f64(0.5),
        },
        BenchmarkPosition {
            label: "three_quarters",
            value: duration.mul_f64(0.75),
        },
        BenchmarkPosition {
            label: "near_end",
            value: duration.saturating_sub(NEAR_END_MARGIN),
        },
    ]
}

fn benchmark_name(operation: &str, storage: ByteStorage, position: BenchmarkPosition) -> String {
    format!(
        "{operation}/{}/{label}@{timestamp}",
        storage.label(),
        label = position.label,
        timestamp = format_timestamp(position.value)
    )
}

fn prepare_player(output: &Output, storage: ByteStorage, position: Duration, paused: bool) -> Nyaa {
    let mut nyaa = Nyaa::new_with_output(output.clone());
    storage.play(&mut nyaa);
    nyaa.try_seek(position)
        .expect("the benchmark player should seek to its setup position");

    if paused {
        nyaa.pause();
        assert!(
            nyaa.is_paused(),
            "benchmarks require an available audio output"
        );
    } else {
        assert!(
            nyaa.is_playing(),
            "benchmarks require an available audio output"
        );
    }

    nyaa
}

pub fn bench_play(criterion: &mut Criterion) {
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("play", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let mut nyaa = Nyaa::new_with_output(output.clone());

                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;

                    for _ in 0..iterations {
                        nyaa.try_seek(position.value)
                            .expect("the benchmark player should accept a deferred seek");
                        let start = Instant::now();
                        storage.play(&mut nyaa);
                        elapsed += start.elapsed();

                        assert!(
                            nyaa.is_playing(),
                            "benchmarks require an available audio output"
                        );
                        black_box(nyaa.position());
                        nyaa.stop();
                    }

                    elapsed
                });
            });
        }
    }
}

pub fn bench_stop(criterion: &mut Criterion) {
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("stop", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let mut nyaa = prepare_player(&output, storage, position.value, false);

                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;

                    for _ in 0..iterations {
                        let start = Instant::now();
                        nyaa.stop();
                        elapsed += start.elapsed();
                        black_box(nyaa.position());

                        storage.play(&mut nyaa);
                        nyaa.try_seek(position.value)
                            .expect("the benchmark player should reset its position");
                        assert!(nyaa.is_playing());
                    }

                    elapsed
                });
            });
        }
    }
}

pub fn bench_seek(criterion: &mut Criterion) {
    let duration = audio_duration();
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("seek", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let neighboring_position = if position.value + Duration::from_secs(1) < duration {
                    position.value + Duration::from_secs(1)
                } else {
                    position.value.saturating_sub(Duration::from_secs(1))
                };
                let mut nyaa = prepare_player(&output, storage, neighboring_position, true);
                let mut seek_to_position = true;

                bencher.iter(|| {
                    let target = if seek_to_position {
                        position.value
                    } else {
                        neighboring_position
                    };
                    seek_to_position = !seek_to_position;

                    nyaa.try_seek(black_box(target))
                        .expect("the benchmark seek should succeed");
                    black_box(nyaa.position())
                });
            });
        }
    }
}

pub fn bench_speed(criterion: &mut Criterion) {
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("speed", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let mut nyaa = prepare_player(&output, storage, position.value, true);
                let mut fast = false;

                bencher.iter(|| {
                    fast = !fast;
                    nyaa.set_speed(black_box(if fast { 1.5 } else { 0.75 }));
                    black_box(nyaa.speed())
                });
            });
        }
    }
}

pub fn bench_volume(criterion: &mut Criterion) {
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("volume", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let nyaa = prepare_player(&output, storage, position.value, true);
                let mut quiet = false;

                bencher.iter(|| {
                    quiet = !quiet;
                    nyaa.set_volume(black_box(if quiet { 0.25 } else { 1.0 }));
                    black_box(nyaa.volume())
                });
            });
        }
    }
}
