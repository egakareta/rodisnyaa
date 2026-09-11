//! Audio handles must be usable from worker threads.
//!
//! [`Sound`] and [`SoundGroup`] are the handles applications share across
//! threads; the error types must also cross thread boundaries so fallible
//! operations can return through thread-pool joins. The [`Soundscape`] root
//! itself stays on its creating thread on WASM, where it owns the
//! thread-affine audio output.

#[cfg(not(target_arch = "wasm32"))]
use euphorium::{Output, Soundscape};
use euphorium::{OutputError, Sound, SoundGroup, SoundSource, SoundscapeError};

euphorium::sound_key! {
    enum TestSound {
        Music => "music",
    }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn handles_and_errors_are_send_sync() {
    assert_send_sync::<Sound>();
    assert_send_sync::<SoundGroup>();
    assert_send_sync::<SoundSource>();
    assert_send_sync::<SoundscapeError>();
    assert_send_sync::<OutputError>();
}

/// The scene root (and its output) remain shareable on native targets.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scene_root_is_send_sync_on_native() {
    assert_send_sync::<Soundscape<()>>();
    assert_send_sync::<Soundscape<TestSound>>();
    assert_send_sync::<Output>();
}

/// Concurrent control from many threads must not deadlock, panic, or corrupt
/// the scene. Needs no audio device: only local control state is touched.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn concurrent_sound_control_is_sound() {
    use std::thread;

    let soundscape = Soundscape::new_with_output(Output::new_deferred(None));
    let music = soundscape
        .create_sound("music", SoundSource::Empty)
        .expect("the sound should be created");

    thread::scope(|scope| {
        for worker in 0..8u32 {
            let sound = music.clone();
            scope.spawn(move || {
                for step in 0..50u32 {
                    let volume = 0.1 * ((worker + step) % 10) as f32;
                    sound.set_volume(volume).expect("volume should apply");
                    sound
                        .set_speed(0.5 + 0.1 * ((worker + step) % 10) as f32)
                        .expect("speed should apply");
                    if step % 2 == 0 {
                        sound.pause().expect("pause should apply");
                    } else {
                        sound.resume().expect("resume should apply");
                    }
                }
            });
        }

        scope.spawn(|| {
            for _ in 0..50 {
                let failures = soundscape.update();
                assert!(
                    failures.is_empty(),
                    "no background failure is expected: {failures:?}"
                );
            }
        });
    });

    // The scene must still be coherent after the concurrent storm: a final
    // main-thread update succeeds and a final write reads back exactly.
    let failures = soundscape.update();
    assert!(
        failures.is_empty(),
        "no background failure is expected: {failures:?}"
    );
    music.set_volume(0.25).expect("volume should apply");
    assert!((music.local_volume().expect("volume should read") - 0.25).abs() < f32::EPSILON);
}
