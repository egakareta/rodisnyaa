use web_time::Duration;

use eframe::egui;
use rodisnyaa::Nyaa;

const MY_AUDIO_FILE: &[u8] = include_bytes!("../../polar 240 yay.mp3");

struct App {
    nyaa: Option<Nyaa>,
    playing: bool,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        Self {
            nyaa: None,
            playing: false,
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

        if let Some(nyaa) = self.nyaa.as_ref() {
            match nyaa.play_bytes(MY_AUDIO_FILE) {
                Ok(()) => self.playing = true,
                Err(error) => log::error!("could not play audio: {error}"),
            }
        }
    }

    fn stop(&mut self) {
        if let Some(nyaa) = self.nyaa.as_ref() {
            nyaa.stop();
        }
        self.playing = false;
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
