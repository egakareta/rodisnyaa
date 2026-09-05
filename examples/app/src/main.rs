use web_time::Duration;

use eframe::egui;
use rodisnyaa::{
    format_timestamp_secs, parse_timestamp, AudioAsset, Nyaa, Waveform, WaveformBuilder,
};
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

#[derive(Clone, Copy)]
struct Song {
    title: &'static str,
    bytes: &'static [u8],
    native_path: &'static str,
    wasm_url: &'static str,
}

const SONGS: [Song; 3] = [
    Song {
        title: "ATLAS 270 [WHAT NO]",
        bytes: include_bytes!("../../ATLAS 270 [WHAT NO].wav"),
        native_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../ATLAS 270 [WHAT NO].wav"),
        wasm_url: "ATLAS 270 [WHAT NO].wav",
    },
    Song {
        title: "polar 240 yay",
        bytes: include_bytes!("../../polar 240 yay.mp3"),
        native_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../polar 240 yay.mp3"),
        wasm_url: "polar 240 yay.mp3",
    },
    Song {
        title: "THE UNFORGIVING",
        bytes: include_bytes!("../../THE UNFORGIVING.mp3"),
        native_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../THE UNFORGIVING.mp3"),
        wasm_url: "THE UNFORGIVING.mp3",
    },
];

const DEFAULT_SONG_INDEX: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioMode {
    StaticBytes,
    File,
}

struct WaveformWindow {
    visible_secs: f64,
    view_start_secs: f64,
    resume_after_scrub: bool,
}

impl Default for WaveformWindow {
    fn default() -> Self {
        Self {
            visible_secs: 20.0,
            view_start_secs: 0.0,
            resume_after_scrub: false,
        }
    }
}

impl WaveformWindow {
    fn visible_range(&mut self, duration_secs: f64, playhead_secs: f64) -> std::ops::Range<f64> {
        if duration_secs <= 0.0 {
            return 0.0..0.0;
        }

        let minimum_visible_secs = duration_secs.min(0.25);
        self.visible_secs = self.visible_secs.clamp(minimum_visible_secs, duration_secs);
        let visible_secs = self.visible_secs;
        let max_start = (duration_secs - visible_secs).max(0.0);

        self.view_start_secs = (playhead_secs - visible_secs * 0.5).clamp(0.0, max_start);

        self.view_start_secs..self.view_start_secs + visible_secs
    }
}

struct App {
    nyaa: Nyaa,
    waveform: Option<WaveformBuilder>,
    waveform_window: WaveformWindow,
    audio_mode: AudioMode,
    selected_song: usize,
    audio_assets: Vec<AudioAsset>,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut nyaa = Nyaa::new();
        let song = SONGS[DEFAULT_SONG_INDEX];

        if let Err(error) = nyaa.load_static_bytes(song.bytes) {
            log::error!("could not load audio duration: {error}");
        }

        let waveform = Waveform::builder_from_static_bytes(song.bytes)
            .inspect_err(|error| log::error!("could not start waveform decoding: {error}"))
            .ok();
        let audio_assets = SONGS
            .iter()
            .map(|song| AudioAsset::new(song.native_path, song.wasm_url))
            .collect();

        Self {
            nyaa,
            waveform,
            waveform_window: WaveformWindow::default(),
            audio_mode: AudioMode::StaticBytes,
            selected_song: DEFAULT_SONG_INDEX,
            audio_assets,
        }
    }

    fn play(&mut self) {
        let song = SONGS[self.selected_song];
        let result = match self.audio_mode {
            AudioMode::StaticBytes => self.nyaa.play_static_bytes(song.bytes),
            AudioMode::File => self
                .nyaa
                .start_asset_playback(&self.audio_assets[self.selected_song]),
        };

        if let Err(error) = result {
            log::error!("could not play audio: {error}");
        }
    }

    fn select_song(&mut self, selected_song: usize) {
        let song = SONGS[selected_song];

        self.nyaa.stop();
        self.selected_song = selected_song;
        self.waveform_window = WaveformWindow::default();

        if let Err(error) = self.nyaa.load_static_bytes(song.bytes) {
            log::error!("could not load audio duration: {error}");
        }

        self.waveform = Waveform::builder_from_static_bytes(song.bytes)
            .inspect_err(|error| log::error!("could not start waveform decoding: {error}"))
            .ok();
    }

    fn stop(&mut self) {
        self.nyaa.stop();
    }

    fn show_waveform(&mut self, ui: &mut egui::Ui) {
        let Some(waveform) = self.waveform.as_mut() else {
            ui.label("Waveform unavailable");
            return;
        };
        let duration_secs = waveform
            .duration()
            .unwrap_or_else(|| waveform.decoded_duration())
            .as_secs_f64();
        let minimum_visible_secs = duration_secs.min(0.25);

        let playhead_secs = self.nyaa.position().as_secs_f64();
        let desired_size = egui::vec2(ui.available_width().min(760.0), 156.0);

        ui.horizontal(|ui| {
            let offset = ((ui.available_width() - desired_size.x) / 2.0).max(0.0);
            ui.add_space(offset);

            if ui.button("-").on_hover_text("Zoom out").clicked() {
                self.waveform_window.visible_secs *= 2.0;
            }

            ui.add(
                egui::Slider::new(
                    &mut self.waveform_window.visible_secs,
                    minimum_visible_secs..=duration_secs,
                )
                .suffix("s")
                .logarithmic(true),
            );

            if ui.button("+").on_hover_text("Zoom in").clicked() {
                self.waveform_window.visible_secs /= 2.0;
            }
        });

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());

        if response.hovered() {
            let (zoom_delta, scroll_delta) = ui.input(|input| {
                (
                    input.zoom_delta() as f64,
                    input.smooth_scroll_delta().y as f64,
                )
            });
            let zoom_delta = if zoom_delta != 1.0 {
                zoom_delta
            } else {
                (scroll_delta * 0.01).exp()
            };

            self.waveform_window.visible_secs = (self.waveform_window.visible_secs / zoom_delta)
                .clamp(minimum_visible_secs, duration_secs);
        }

        let visible_range = self
            .waveform_window
            .visible_range(duration_secs, playhead_secs);
        let visible_duration = visible_range.end - visible_range.start;

        let waveform_range = Duration::from_secs_f64(visible_range.start)
            ..Duration::from_secs_f64(visible_range.end);

        match waveform.prioritize(waveform_range.clone()) {
            Ok(()) if !waveform.is_finished() => {
                waveform.advance_frames(waveform.sample_rate() as usize);
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
            Ok(()) => {}
            Err(error) => log::error!("could not prioritize waveform decoding: {error}"),
        }

        if response.clicked() || response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let pointer_fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let seek_secs =
                    visible_range.start + visible_duration * f64::from(pointer_fraction);

                if let Err(error) = self.nyaa.try_seek_secs(seek_secs) {
                    log::error!("could not seek audio: {error}");
                }
            }
        }

        if response.drag_started() {
            self.waveform_window.resume_after_scrub = self.nyaa.is_playing();
            self.nyaa.pause();
        }

        if response.drag_stopped() && self.waveform_window.resume_after_scrub {
            self.nyaa.resume();
            self.waveform_window.resume_after_scrub = false;
        }

        let painter = ui.painter_at(rect);
        let background = egui::Color32::from_rgb(20, 24, 29);
        let grid = egui::Color32::from_rgb(53, 61, 68);
        let future = egui::Color32::from_rgb(81, 133, 141);
        let played = egui::Color32::from_rgb(238, 151, 73);
        let playhead = egui::Color32::from_rgb(250, 232, 191);
        let text = egui::Color32::from_rgb(184, 193, 198);
        let wave_rect = rect.shrink2(egui::vec2(0.0, 20.0));
        let center_y = wave_rect.center().y;

        painter.rect_filled(rect, 6.0, background);
        painter.line_segment(
            [
                egui::pos2(rect.left(), center_y),
                egui::pos2(rect.right(), center_y),
            ],
            egui::Stroke::new(1.0, grid),
        );

        for tick in 0..=4 {
            let fraction = tick as f32 / 4.0;
            let x = egui::lerp(rect.x_range(), fraction);
            let tick_secs = visible_range.start + visible_duration * f64::from(fraction);

            painter.line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, grid),
            );
            painter.text(
                egui::pos2(x, rect.bottom() - 4.0),
                egui::Align2::CENTER_BOTTOM,
                format_timestamp_secs(tick_secs),
                egui::FontId::monospace(10.0),
                text,
            );
        }

        if visible_duration > 0.0 {
            let waveform_slice =
                waveform.slice(waveform_range, (rect.width() / 2.0).max(1.0) as usize);
            let amplitude_height = wave_rect.height() * 0.46;

            for (index, peak) in waveform_slice.peaks().iter().enumerate() {
                if !peak.is_available() {
                    continue;
                }

                let Some(peak_range) = waveform_slice.peak_range(index) else {
                    continue;
                };
                let peak_secs =
                    (peak_range.start.as_secs_f64() + peak_range.end.as_secs_f64()) * 0.5;
                let fraction = ((peak_secs - visible_range.start) / visible_duration) as f32;
                let x = egui::lerp(rect.x_range(), fraction);
                let top = center_y - peak.max.clamp(-1.0, 1.0) * amplitude_height;
                let bottom = center_y - peak.min.clamp(-1.0, 1.0) * amplitude_height;
                let color = if peak_secs <= playhead_secs {
                    played
                } else {
                    future
                };

                if rect.x_range().contains(x) {
                    painter.line_segment(
                        [egui::pos2(x, top), egui::pos2(x, bottom.max(top + 1.0))],
                        egui::Stroke::new(1.5, color),
                    );
                }
            }

            let playhead_fraction =
                ((playhead_secs - visible_range.start) / visible_duration).clamp(0.0, 1.0) as f32;
            let playhead_x = egui::lerp(rect.x_range(), playhead_fraction);
            painter.line_segment(
                [
                    egui::pos2(playhead_x, rect.top()),
                    egui::pos2(playhead_x, rect.bottom()),
                ],
                egui::Stroke::new(2.0, playhead),
            );
        }
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

        ui.ctx().request_repaint_after(Duration::from_millis(250));

        egui::Panel::top("top_panel").show(ui, |ui| {
            let fps = ui.ctx().input(|input| 1.0 / f64::from(input.unstable_dt));

            let live = LIVE_BYTES.load(Ordering::Relaxed);
            let peak = PEAK_BYTES.load(Ordering::Relaxed);

            ui.horizontal(|ui| {
                ui.label(format!("i love rodisnyaa | fps: {fps:.0}"));

                // Consume all remaining space except what the right label needs.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "memory: {:.2}/{:.2} MiB",
                        live as f64 / 1024.0 / 1024.0,
                        peak as f64 / 1024.0 / 1024.0
                    ));
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let mut selected_song = self.selected_song;

                ui.add_enabled_ui(!self.nyaa.is_loading(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Song:");
                        egui::ComboBox::from_id_salt("song_selector")
                            .selected_text(SONGS[selected_song].title)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for (index, song) in SONGS.iter().enumerate() {
                                    ui.selectable_value(&mut selected_song, index, song.title);
                                }
                            });
                    });
                });

                if selected_song != self.selected_song {
                    self.select_song(selected_song);
                }

                self.show_waveform(ui);

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

                let control_width = ui.available_width().min(360.0);

                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 0.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.set_width(control_width);

                        ui.spacing_mut().slider_width = control_width;

                        let mut position_secs: f64 = self.nyaa.clamped_position().as_secs_f64();
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

                        let is_loading = self.nyaa.is_loading();
                        let button_size = ui.spacing().interact_size;

                        let (row_rect, _) = ui.allocate_exact_size(
                            egui::vec2(control_width, button_size.y),
                            egui::Sense::hover(),
                        );

                        let button_rect =
                            egui::Rect::from_center_size(row_rect.center(), button_size);

                        let left_rect = egui::Rect::from_min_max(
                            row_rect.min,
                            egui::pos2(button_rect.left(), row_rect.bottom()),
                        );

                        let mut left_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt("position_control")
                                .max_rect(left_rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );

                        let response = left_ui.add(
                            egui::DragValue::new(&mut position_secs)
                                .range(self.nyaa.seek_range())
                                .custom_formatter(|position, _| format_timestamp_secs(position))
                                .custom_parser(|input| parse_timestamp(input).map(f64::from)),
                        );

                        if response.changed() {
                            if let Err(error) = self.nyaa.try_seek_secs(position_secs) {
                                log::error!("could not seek audio: {error}");
                            }
                        }

                        let mut button_builder = egui::UiBuilder::new()
                            .id_salt("play_button")
                            .max_rect(button_rect)
                            .layout(egui::Layout::centered_and_justified(
                                egui::Direction::TopDown,
                            ));

                        if is_loading {
                            button_builder = button_builder.disabled();
                        }

                        let mut button_ui = ui.new_child(button_builder);

                        let button =
                            button_ui
                                .add(egui::Button::new(""))
                                .on_hover_text(if is_loading {
                                    "Loading"
                                } else if self.nyaa.is_playing() {
                                    "Stop"
                                } else {
                                    "Play"
                                });

                        let center = button.rect.center();
                        let icon_color = button_ui.style().interact(&button).fg_stroke.color;

                        if is_loading {
                            button_ui.painter().text(
                                center,
                                egui::Align2::CENTER_CENTER,
                                "...",
                                egui::TextStyle::Button.resolve(button_ui.style()),
                                icon_color,
                            );
                        } else if self.nyaa.is_playing() {
                            let size = 8.0;
                            button_ui.painter().rect_filled(
                                egui::Rect::from_center_size(center, egui::vec2(size, size)),
                                0.0,
                                icon_color,
                            );
                        } else {
                            let half_h = 6.0;
                            let half_w = 5.0;

                            button_ui.painter().add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(center.x - half_w, center.y - half_h),
                                    egui::pos2(center.x - half_w, center.y + half_h),
                                    egui::pos2(center.x + half_w, center.y),
                                ],
                                icon_color,
                                egui::Stroke::NONE,
                            ));
                        }

                        if button.clicked() {
                            if self.nyaa.is_playing() {
                                self.stop();
                            } else {
                                self.play();
                            }
                        }

                        ui.painter().text(
                            egui::pos2(row_rect.right(), row_rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            self.nyaa.duration_formatted(),
                            egui::TextStyle::Body.resolve(ui.style()),
                            ui.visuals().text_color(),
                        );

                        ui.add_space(30.0);

                        ui.columns(2, |columns| {
                            let mut speed = self.nyaa.speed();
                            let speed_width = columns[0].available_width();
                            columns[0].label(format!("Speed: {speed:.2}x"));
                            columns[0].spacing_mut().slider_width = speed_width;

                            let response = columns[0].add(
                                egui::Slider::new(&mut speed, 0.5..=2.0)
                                    .show_value(false)
                                    .logarithmic(true),
                            );

                            if response.changed() {
                                self.nyaa.set_speed(speed);
                            }

                            let mut volume = self.nyaa.volume() * 100.0;
                            let volume_width = columns[1].available_width();
                            columns[1].label(format!("Volume: {volume:.0}%"));
                            columns[1].spacing_mut().slider_width = volume_width;

                            let response = columns[1]
                                .add(egui::Slider::new(&mut volume, 0.0..=100.0).show_value(false));

                            if response.changed() {
                                self.nyaa.set_volume(volume / 100.0);
                            }
                        });
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
        const MY_AUDIO_BYTES: &[u8] = SONGS[DEFAULT_SONG_INDEX].bytes;

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
        replayed: MemorySnapshot,
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

        if let Some(asset) = asset.as_ref() {
            nyaa.play_asset(asset)
                .await
                .expect("cached browser audio asset should be replayable");
        } else {
            nyaa.play_static_bytes(MY_AUDIO_BYTES)
                .expect("static audio should be replayable");
        }
        wait_for_audio_settle().await;
        let replayed = memory_snapshot();

        nyaa.stop();
        if let Some(asset) = asset.as_ref() {
            asset.clear_browser_cache();
        }

        if let Some(url) = asset_url {
            web_sys::Url::revoke_object_url(&url).expect("audio object URL should be revoked");
        }

        MemoryReport {
            idle,
            playing,
            stopped,
            replayed,
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
        let replay_peak_growth = report
            .replayed
            .peak_bytes
            .saturating_sub(report.playing.peak_bytes);

        assert!(
            playing_live_delta >= AUDIO_BYTES_LEN / 2,
            "file playback retained only {playing_live_delta} live bytes; it should retain the fetched encoded asset"
        );
        assert!(
            playing_peak_delta >= AUDIO_BYTES_LEN / 2,
            "file playback allocated only a {playing_peak_delta}-byte peak delta; it should allocate the fetched encoded asset"
        );
        assert!(
            playing_peak_delta < AUDIO_BYTES_LEN * 3 / 2,
            "file playback allocated a {playing_peak_delta}-byte peak delta; fetching should allocate the encoded asset only once"
        );
        assert!(
            replay_peak_growth < AUDIO_BYTES_LEN / 2,
            "replaying the cached asset grew peak memory by {replay_peak_growth} bytes; it should reuse the fetched bytes"
        );
    }

    fn print_memory_report(label: &str, report: MemoryReport) {
        web_sys::console::log_1(&JsValue::from_str(&format!(
            "{label} audio memory report (WASM allocations):"
        )));
        print_memory_snapshot("idle", report.idle, report.idle);
        print_memory_snapshot("playing", report.playing, report.idle);
        print_memory_snapshot("stopped", report.stopped, report.idle);
        print_memory_snapshot("replayed", report.replayed, report.idle);
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
