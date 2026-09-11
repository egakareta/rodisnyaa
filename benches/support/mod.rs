#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use std::{hint::black_box, time::Duration};

#[cfg(not(target_arch = "wasm32"))]
use criterion::Criterion;
use euphorium::{Output, Sound, SoundSource, Soundscape, format_timestamp};
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

    fn source(self) -> SoundSource {
        match self {
            Self::Static => SoundSource::static_bytes(AUDIO_BYTES),
            Self::Shared => SoundSource::shared_bytes(AUDIO_BYTES),
        }
    }
}

struct PreparedPlayer {
    _soundscape: Soundscape,
    sound: Sound,
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
    let soundscape = Soundscape::new_with_output(Output::new_deferred(None));
    soundscape
        .create_sound("duration", SoundSource::static_bytes(AUDIO_BYTES))
        .expect("the benchmark MP3 duration should be readable")
        .duration()
        .expect("the benchmark sound should remain valid")
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

fn prepare_player(
    output: &Output,
    storage: ByteStorage,
    position: Duration,
    paused: bool,
) -> PreparedPlayer {
    let soundscape = Soundscape::new_with_output(output.clone());
    let sound = soundscape
        .create_sound("benchmark", storage.source())
        .expect("the benchmark sound should be created");
    sound.play().expect("the benchmark MP3 should be playable");
    sound
        .try_seek(position)
        .expect("the benchmark player should seek to its setup position");

    if paused {
        sound.pause().expect("the benchmark player should pause");
        assert!(
            sound.is_paused().unwrap(),
            "benchmarks require an available audio output"
        );
    } else {
        assert!(
            sound.is_playing().unwrap(),
            "benchmarks require an available audio output"
        );
    }

    PreparedPlayer {
        _soundscape: soundscape,
        sound,
    }
}

pub fn bench_play(criterion: &mut Criterion) {
    let output = Output::new();

    for storage in ByteStorage::ALL {
        for position in benchmark_positions() {
            let output = output.clone();
            let name = benchmark_name("play", storage, position);

            criterion.bench_function(&name, move |bencher| {
                let soundscape = Soundscape::new_with_output(output.clone());
                let sound = soundscape
                    .create_sound("benchmark", storage.source())
                    .expect("the benchmark sound should be created");

                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;

                    for _ in 0..iterations {
                        sound
                            .try_seek(position.value)
                            .expect("the benchmark player should accept a deferred seek");
                        let start = Instant::now();
                        sound
                            .replay()
                            .expect("the benchmark MP3 should be playable");
                        elapsed += start.elapsed();

                        assert!(
                            sound.is_playing().unwrap(),
                            "benchmarks require an available audio output"
                        );
                        black_box(sound.position().unwrap());
                        sound.stop().unwrap();
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
                let prepared = prepare_player(&output, storage, position.value, false);

                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;

                    for _ in 0..iterations {
                        let start = Instant::now();
                        prepared.sound.stop().unwrap();
                        elapsed += start.elapsed();
                        black_box(prepared.sound.position().unwrap());

                        prepared.sound.replay().unwrap();
                        prepared
                            .sound
                            .try_seek(position.value)
                            .expect("the benchmark player should reset its position");
                        assert!(prepared.sound.is_playing().unwrap());
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
                let prepared = prepare_player(&output, storage, neighboring_position, true);
                let mut seek_to_position = true;

                bencher.iter(|| {
                    let target = if seek_to_position {
                        position.value
                    } else {
                        neighboring_position
                    };
                    seek_to_position = !seek_to_position;

                    prepared
                        .sound
                        .try_seek(black_box(target))
                        .expect("the benchmark seek should succeed");
                    black_box(prepared.sound.position().unwrap())
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
                let prepared = prepare_player(&output, storage, position.value, true);
                let mut fast = false;

                bencher.iter(|| {
                    fast = !fast;
                    prepared
                        .sound
                        .set_speed(black_box(if fast { 1.5 } else { 0.75 }))
                        .unwrap();
                    black_box(prepared.sound.speed().unwrap())
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
                let prepared = prepare_player(&output, storage, position.value, true);
                let mut quiet = false;

                bencher.iter(|| {
                    quiet = !quiet;
                    prepared
                        .sound
                        .set_volume(black_box(if quiet { 0.25 } else { 1.0 }))
                        .unwrap();
                    black_box(prepared.sound.local_volume().unwrap())
                });
            });
        }
    }
}
