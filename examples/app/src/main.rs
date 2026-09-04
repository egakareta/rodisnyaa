use std::time::Duration as StdDuration;

use web_time::Duration;

use eframe::egui;
use rodisnyaa::Nyaa;

const MY_AUDIO_FILE: &[u8] = include_bytes!("../../polar 240 yay.mp3");

struct App {
    nyaa: Option<Nyaa>,
    playing: bool,
    duration: StdDuration,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        let duration = match Nyaa::duration_from_bytes(MY_AUDIO_FILE) {
            Ok(Some(duration)) => duration,
            Ok(None) => StdDuration::ZERO,
            Err(error) => {
                log::error!("could not determine audio duration: {error}");
                StdDuration::ZERO
            }
        };

        Self {
            nyaa: None,
            playing: false,
            duration,
        }
    }

    fn play(&mut self) {
        if self.nyaa.is_none() {
            match Nyaa::new() {
                Ok(nyaa) => self.nyaa = Some(nyaa),
                Err(error) => {
                    log::error!("could not initialize audio: {error}");
                    return;
                }
            }
        }

        if let Some(nyaa) = self.nyaa.as_mut() {
            match nyaa.play_bytes(MY_AUDIO_FILE) {
                Ok(()) => self.playing = true,
                Err(error) => log::error!("could not play audio: {error}"),
            }
        }
    }
    
    fn stop(&mut self) {
        if let Some(nyaa) = self.nyaa.as_mut() {
            nyaa.stop();
        }
        self.playing = false;
    }
}

fn format_timestamp(duration: StdDuration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.playing {
            if self.nyaa.as_ref().is_some_and(Nyaa::is_empty) {
                self.playing = false;
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                if ui
                    .button(if self.playing { "Stop" } else { "Play" })
                    .clicked()
                {
                    if self.playing {
                        self.stop();
                    } else {
                        self.play();
                    }
                }

                let duration_secs = self.duration.as_secs_f32();
                let slider_max = duration_secs.max(1.0);
                let slider_width = ui.available_width().min(360.0);
                let mut position_secs = if self.playing {
                    self.nyaa
                        .as_ref()
                        .map(Nyaa::position)
                        .unwrap_or_default()
                        .as_secs_f32()
                } else {
                    0.0
                }
                .min(duration_secs);
                let response = ui
                    .add_enabled_ui(self.playing, |ui| {
                        ui.add_sized(
                            [slider_width, 20.0],
                            egui::Slider::new(&mut position_secs, 0.0..=slider_max)
                                .show_value(false),
                        )
                    })
                    .inner;

                if response.drag_started() {
                    if let Some(nyaa) = self.nyaa.as_ref() {
                        nyaa.pause();
                    }
                }

                if response.changed() {
                    if let Some(nyaa) = self.nyaa.as_mut() {
                        if let Err(error) = nyaa.try_seek(StdDuration::from_secs_f32(position_secs))
                        {
                            log::error!("could not seek audio: {error}");
                        }
                    }
                }

                if response.drag_stopped() {
                    if let Some(nyaa) = self.nyaa.as_mut() {
                        nyaa.resume();
                    }
                }

                ui.horizontal(|ui| {
                    ui.label(format_timestamp(StdDuration::from_secs_f32(position_secs)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format_timestamp(self.duration));
                    });
                });
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
