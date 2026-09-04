use web_time::Duration;

use eframe::egui;
use rodisnyaa::{AudioAsset, Nyaa};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn add_allocated(bytes: usize) {
    let current = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };

        if !ptr.is_null() {
            add_allocated(layout.size());
        }

        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };

        if !ptr.is_null() {
            add_allocated(layout.size());
        }

        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(ptr, layout);
        }

        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };

        if !new_ptr.is_null() {
            let old_size = layout.size();

            if new_size > old_size {
                add_allocated(new_size - old_size);
            } else {
                LIVE_BYTES.fetch_sub(old_size - new_size, Ordering::Relaxed);
            }
        }

        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const MY_AUDIO_BYTES: &[u8] = include_bytes!("../../THE UNFORGIVING.mp3");
const MY_AUDIO_NATIVE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../THE UNFORGIVING.mp3");
const MY_AUDIO_WASM_URL: &str = "THE UNFORGIVING.mp3";

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioMode {
    StaticBytes,
    File,
}

struct App {
    nyaa: Nyaa,
    audio_mode: AudioMode,
    audio_asset: AudioAsset,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut nyaa = Nyaa::new();

        if let Err(error) = nyaa.load_static_bytes(MY_AUDIO_BYTES) {
            log::error!("could not load audio duration: {error}");
        }

        Self {
            nyaa,
            audio_mode: AudioMode::StaticBytes,
            audio_asset: AudioAsset::new(MY_AUDIO_NATIVE_PATH, MY_AUDIO_WASM_URL),
        }
    }

    fn play(&mut self) {
        let result = match self.audio_mode {
            AudioMode::StaticBytes => self.nyaa.play_static_bytes(MY_AUDIO_BYTES),
            AudioMode::File => self.nyaa.start_asset_playback(&self.audio_asset),
        };

        if let Err(error) = result {
            log::error!("could not play audio: {error}");
        }
    }

    fn stop(&mut self) {
        self.nyaa.stop();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(Err(error)) = self.nyaa.poll_pending_playback() {
            log::error!("could not play audio: {error}");
        }

        if self.nyaa.is_loading() {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }

        if self.nyaa.is_playing() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let live = LIVE_BYTES.load(Ordering::Relaxed);
                let peak = PEAK_BYTES.load(Ordering::Relaxed);

                ui.separator();

                ui.label(format!("Memory: {:.2} MiB", live as f64 / 1024.0 / 1024.0));

                ui.label(format!("Peak: {:.2} MiB", peak as f64 / 1024.0 / 1024.0));

                let tab_width = 90.0;
                let tab_height = ui.spacing().interact_size.y;
                let gap = ui.spacing().item_spacing.x;
                let tabs_width = tab_width * 2.0 + gap;

                ui.horizontal(|ui| {
                    let offset = ((ui.available_width() - tabs_width) / 2.0).max(0.0);
                    ui.add_space(offset);

                    if ui
                        .add_sized(
                            [tab_width, tab_height],
                            egui::Button::selectable(
                                self.audio_mode == AudioMode::StaticBytes,
                                "Static bytes",
                            ),
                        )
                        .clicked()
                    {
                        self.audio_mode = AudioMode::StaticBytes;
                    }

                    if ui
                        .add_sized(
                            [tab_width, tab_height],
                            egui::Button::selectable(self.audio_mode == AudioMode::File, "File"),
                        )
                        .clicked()
                    {
                        self.audio_mode = AudioMode::File;
                    }
                });

                let is_loading = self.nyaa.is_loading();
                let button = ui.add_enabled(
                    !is_loading,
                    egui::Button::new(if is_loading {
                        "Loading..."
                    } else if self.nyaa.is_playing() {
                        "Stop"
                    } else {
                        "Play"
                    }),
                );

                if button.clicked() {
                    if self.nyaa.is_playing() {
                        self.stop();
                    } else {
                        self.play();
                    }
                }

                let control_width = ui.available_width().min(360.0);

                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 0.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.set_width(control_width);

                        ui.spacing_mut().slider_width = control_width;

                        let mut position_secs: f32 = self.nyaa.clamped_position().as_secs_f32();
                        let response = ui.add(
                            egui::Slider::new(&mut position_secs, self.nyaa.seek_range())
                                .show_value(false),
                        );

                        if response.drag_started() {
                            self.nyaa.pause();
                        }

                        if response.changed() {
                            if let Err(error) = self.nyaa.try_seek_secs(position_secs) {
                                log::error!("could not seek audio: {error}");
                            }
                        }

                        if response.drag_stopped() {
                            self.nyaa.resume();
                        }

                        ui.allocate_ui_with_layout(
                            egui::vec2(control_width, 20.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.label(self.nyaa.position_formatted());

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(self.nyaa.duration_formatted());
                                    },
                                );
                            },
                        );

                        let mut speed = self.nyaa.speed();
                        let response = ui.add(
                            egui::Slider::new(&mut speed, 0.5..=2.0)
                                .text("Speed")
                                .suffix("x")
                                .logarithmic(true),
                        );

                        if response.changed() {
                            self.nyaa.set_speed(speed);
                        }
                    },
                );
            });
        });
    }
}

// entrypoint

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    eframe::run_native(
        "this is an app",
        eframe::NativeOptions::default(),
        Box::new(|creation_context| Ok(Box::new(App::new(creation_context)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;
    wasm_bindgen_futures::spawn_local(async {
        eframe::WebLogger::init(log::LevelFilter::Debug).ok();

        let document = web_sys::window()
            .expect("browser window is unavailable")
            .document()
            .expect("browser document is unavailable");
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("the_canvas_id canvas is missing")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id is not a canvas");

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|creation_context| Ok(Box::new(App::new(creation_context)))),
            )
            .await
            .expect("failed to start eframe");
    });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::thread;

    #[derive(Clone, Copy)]
    struct MemorySnapshot {
        live_bytes: usize,
        peak_bytes: usize,
    }

    fn memory_snapshot() -> MemorySnapshot {
        MemorySnapshot {
            live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
            peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        }
    }

    fn format_mib(bytes: usize) -> String {
        format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
    }

    fn print_memory_snapshot(label: &str, snapshot: MemorySnapshot, idle: MemorySnapshot) {
        let live_delta = snapshot.live_bytes as isize - idle.live_bytes as isize;
        let peak_delta = snapshot.peak_bytes as isize - idle.peak_bytes as isize;

        println!(
            "{label:>8}: live={} MiB ({:+.2} MiB), peak={} MiB ({:+.2} MiB)",
            format_mib(snapshot.live_bytes),
            live_delta as f64 / 1024.0 / 1024.0,
            format_mib(snapshot.peak_bytes),
            peak_delta as f64 / 1024.0 / 1024.0,
        );
    }

    #[test]
    fn reports_memory_for_idle_playing_and_stopped_audio() {
        let idle = memory_snapshot();
        let mut nyaa = Nyaa::new();

        nyaa.play_static_bytes(MY_AUDIO_BYTES)
            .expect("embedded audio should be playable");
        thread::sleep(Duration::from_millis(250));
        let playing = memory_snapshot();

        nyaa.stop();
        thread::sleep(Duration::from_millis(250));
        let stopped = memory_snapshot();

        assert!(
            playing.live_bytes.saturating_sub(idle.live_bytes) < MY_AUDIO_BYTES.len() / 2,
            "playing embedded audio should not copy the whole asset onto the heap"
        );
        assert!(
            playing.peak_bytes.saturating_sub(idle.peak_bytes) < MY_AUDIO_BYTES.len() / 2,
            "playing embedded audio should not allocate the whole asset"
        );

        println!("audio memory report (process allocations):");
        print_memory_snapshot("idle", idle, idle);
        print_memory_snapshot("playing", playing, idle);
        print_memory_snapshot("stopped", stopped, idle);
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    wasm_bindgen_test_configure!(run_in_browser);

    const AUDIO_BYTES_LEN: usize = MY_AUDIO_BYTES.len();

    #[derive(Clone, Copy)]
    struct MemorySnapshot {
        live_bytes: usize,
        peak_bytes: usize,
    }

    #[derive(Clone, Copy)]
    struct MemoryReport {
        idle: MemorySnapshot,
        playing: MemorySnapshot,
        stopped: MemorySnapshot,
    }

    fn memory_snapshot() -> MemorySnapshot {
        MemorySnapshot {
            live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
            peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        }
    }

    fn format_mib(bytes: usize) -> String {
        format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
    }

    fn print_memory_snapshot(label: &str, snapshot: MemorySnapshot, idle: MemorySnapshot) {
        let live_delta = snapshot.live_bytes as isize - idle.live_bytes as isize;
        let peak_delta = snapshot.peak_bytes as isize - idle.peak_bytes as isize;
        let message = format!(
            "{label:>16}: live={} MiB ({:+.2} MiB), peak={} MiB ({:+.2} MiB)",
            format_mib(snapshot.live_bytes),
            live_delta as f64 / 1024.0 / 1024.0,
            format_mib(snapshot.peak_bytes),
            peak_delta as f64 / 1024.0 / 1024.0,
        );

        web_sys::console::log_1(&JsValue::from_str(&message));
    }

    async fn wait_for_audio_settle() {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let callback = Closure::once_into_js(move || {
                resolve
                    .call0(&JsValue::UNDEFINED)
                    .expect("timer promise should resolve");
            });

            web_sys::window()
                .expect("browser window is unavailable")
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    250,
                )
                .expect("browser timer should be scheduled");
        });

        JsFuture::from(promise)
            .await
            .expect("audio settle timer should resolve");
    }

    fn object_url_for_audio(bytes: &[u8]) -> String {
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::new();
        parts.push(&array);
        let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
            .expect("audio blob should be created");

        web_sys::Url::create_object_url_with_blob(&blob)
            .expect("audio object URL should be created")
    }

    async fn measure_playback(use_asset: bool) -> MemoryReport {
        let asset_url = use_asset.then(|| object_url_for_audio(MY_AUDIO_BYTES));
        let asset = asset_url
            .as_deref()
            .map(|url| AudioAsset::new("missing/native/audio.wav", url));
        let mut nyaa = Nyaa::new();
        let idle = memory_snapshot();

        if let Some(asset) = asset.as_ref() {
            nyaa.play_asset(asset)
                .await
                .expect("browser audio asset should be playable");
        } else {
            nyaa.play_static_bytes(MY_AUDIO_BYTES)
                .expect("static audio should be playable");
        }
        wait_for_audio_settle().await;
        let playing = memory_snapshot();

        nyaa.stop();
        wait_for_audio_settle().await;
        let stopped = memory_snapshot();

        if let Some(url) = asset_url {
            web_sys::Url::revoke_object_url(&url).expect("audio object URL should be revoked");
        }

        MemoryReport {
            idle,
            playing,
            stopped,
        }
    }

    fn assert_static_memory_report(report: MemoryReport) {
        let playing_live_delta = report
            .playing
            .live_bytes
            .saturating_sub(report.idle.live_bytes);
        let playing_peak_delta = report
            .playing
            .peak_bytes
            .saturating_sub(report.idle.peak_bytes);

        assert!(
            playing_live_delta < AUDIO_BYTES_LEN / 2,
            "static bytes playback allocated {playing_live_delta} live bytes (peak delta {playing_peak_delta}); it should not copy the whole encoded asset onto the heap"
        );
        assert!(
            playing_peak_delta < AUDIO_BYTES_LEN / 2,
            "static bytes playback allocated a {playing_peak_delta}-byte peak delta; it should not allocate the whole encoded asset"
        );
    }

    fn assert_file_memory_report(report: MemoryReport) {
        let playing_live_delta = report
            .playing
            .live_bytes
            .saturating_sub(report.idle.live_bytes);
        let playing_peak_delta = report
            .playing
            .peak_bytes
            .saturating_sub(report.idle.peak_bytes);

        assert!(
            playing_live_delta >= AUDIO_BYTES_LEN / 2,
            "file playback retained only {playing_live_delta} live bytes; it should retain the fetched encoded asset"
        );
        assert!(
            playing_peak_delta >= AUDIO_BYTES_LEN / 2,
            "file playback allocated only a {playing_peak_delta}-byte peak delta; it should allocate the fetched encoded asset"
        );
    }

    fn print_memory_report(label: &str, report: MemoryReport) {
        web_sys::console::log_1(&JsValue::from_str(&format!(
            "{label} audio memory report (WASM allocations):"
        )));
        print_memory_snapshot("idle", report.idle, report.idle);
        print_memory_snapshot("playing", report.playing, report.idle);
        print_memory_snapshot("stopped", report.stopped, report.idle);
    }

    #[wasm_bindgen_test]
    async fn reports_memory_for_static_and_file_audio() {
        let static_bytes = measure_playback(false).await;
        let file = measure_playback(true).await;

        assert_static_memory_report(static_bytes);
        assert_file_memory_report(file);

        print_memory_report("static bytes", static_bytes);
        print_memory_report("file", file);
    }
}
