//! The window: no recording, one being opened, one on screen, or why it couldn't open.

use std::path::{Path, PathBuf};

use anyhow::Result;
use eframe::egui::{self, Align, Color32, Event, Layout, RichText, Ui};
use kymo_core::pyramid::Sidecar;
use kymo_core::service::{PyramidStore, ServiceOptions, TileService};

use crate::action::Action;
use crate::document::{self, Document, Loading, error_text, file_name};
use crate::keymap::{Keymap, Platform};
use crate::ui::gpu::Gpu;
use crate::viewer::Viewer;

const APP_NAME: &str = "Kymo";

/// How the window keeps recordings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where pyramid sidecars go; `None` keeps pyramids in memory.
    pub sidecar_dir: Option<PathBuf>,
}

impl Default for Options {
    /// Sidecars in the user's cache directory.
    fn default() -> Self {
        Options {
            sidecar_dir: Sidecar::default_dir(),
        }
    }
}

pub struct KymoApp {
    state: State,
    gpu: Gpu,
    keymap: Keymap,
    options: Options,
    /// Show the file dialog once the window is on screen.
    pick_file: bool,
    /// Whether a frame has been shown yet.
    shown: bool,
}

/// A recording ready to show, as the loading thread hands it over.
type Opened = (Document, TileService);

enum State {
    Empty,
    Loading(Loading<Opened>),
    Ready(Box<Viewer>),
    Failed { path: PathBuf, error: String },
}

impl KymoApp {
    /// A window opening `path`, or showing a file dialog without one.
    pub fn new(ctx: &egui::Context, gpu: Gpu, options: Options, path: Option<PathBuf>) -> Self {
        // ⌘+ and ⌘− zoom time, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut app = KymoApp {
            state: State::Empty,
            gpu,
            keymap: Keymap::new(Platform::current()),
            options,
            pick_file: path.is_none(),
            shown: false,
        };
        if let Some(path) = path {
            app.open(ctx, path);
        }
        app
    }

    /// The recording on screen, if there is one.
    pub fn viewer(&self) -> Option<&Viewer> {
        match &self.state {
            State::Ready(viewer) => Some(viewer),
            _ => None,
        }
    }

    /// Whether a recording is being opened.
    pub fn is_loading(&self) -> bool {
        matches!(self.state, State::Loading(_))
    }

    /// Why the last recording couldn't be opened.
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            State::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Opens `path` on a background thread, with its tile service.
    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        set_title(ctx, Some(&file_name(&path)));
        let store = match &self.options.sidecar_dir {
            Some(dir) => PyramidStore::Sidecar {
                dir: dir.clone(),
                source: path.clone(),
            },
            None => PyramidStore::Memory,
        };
        let repaint = ctx.clone();
        let tiles = ctx.clone();
        let open = move |path: &Path| -> Result<Opened> {
            let document = document::open(path)?;
            let service = TileService::new(
                document.recording.clone(),
                ServiceOptions::new(store),
                move || tiles.request_repaint(),
            )?;
            Ok((document, service))
        };
        self.state = State::Loading(Loading::start(path, open, move || {
            repaint.request_repaint();
        }));
    }

    /// One frame: picks up a finished open, handles keys, and draws the window.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll_loading(&ctx);
        // The startup dialog is modal: show it once the window has been drawn and has focus.
        // On macOS a panel opened before the app is active is cancelled at once; gaining
        // focus repaints, which brings us back here.
        if self.pick_file {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if self.shown && focused {
                self.pick_file = false;
                self.pick_and_open(&ctx);
            } else if !self.shown {
                ctx.request_repaint();
            }
        }
        self.shown = true;

        for action in self.key_actions(&ctx) {
            self.perform(&ctx, action);
        }

        let mut open_clicked = false;
        match &mut self.state {
            State::Ready(viewer) => {
                open_clicked = viewer.show(ui, &self.gpu, &self.keymap);
            }
            State::Empty => {
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked = centered_message(ui, "No recording open.", None);
                });
            }
            State::Loading(loading) => {
                let text = format!("Opening {}…", loading.path().display());
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spinner();
                            ui.label(text);
                        });
                    });
                });
            }
            State::Failed { error, .. } => {
                let color = ui.visuals().error_fg_color;
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked =
                        centered_message(ui, "Could not open the recording.", Some((error, color)));
                });
            }
        }
        if open_clicked {
            self.pick_and_open(&ctx);
        }
    }

    /// Carries out `action`.
    pub fn perform(&mut self, ctx: &egui::Context, action: Action) {
        if !action.enabled(matches!(self.state, State::Ready(_))) {
            return;
        }
        match action {
            Action::Open => self.pick_and_open(ctx),
            Action::Close => {
                self.state = State::Empty;
                set_title(ctx, None);
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            // The settings window, the shortcut overview and About come with the menus;
            // DC removal with the window means (steps 4b and 5).
            Action::Settings | Action::Shortcuts | Action::About | Action::ToggleDc => {}
            _ => {
                if let State::Ready(viewer) = &mut self.state {
                    viewer.apply(action);
                }
            }
        }
    }

    /// The actions of this frame's key presses, unless a text field has the keyboard.
    fn key_actions(&self, ctx: &egui::Context) -> Vec<Action> {
        if ctx.egui_wants_keyboard_input() {
            return Vec::new();
        }
        ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match *event {
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => self.keymap.action(key, modifiers),
                    _ => None,
                })
                .collect()
        })
    }

    fn pick_and_open(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open a recording")
            .add_filter("EDF and BDF", &["edf", "bdf", "EDF", "BDF"])
            .add_filter("All files", &["*"]);
        if let Some(dir) = self.current_path().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(ctx, path);
        }
    }

    fn current_path(&self) -> Option<&Path> {
        match &self.state {
            State::Empty => None,
            State::Loading(loading) => Some(loading.path()),
            State::Ready(viewer) => Some(&viewer.document().path),
            State::Failed { path, .. } => Some(path),
        }
    }

    fn poll_loading(&mut self, ctx: &egui::Context) {
        let State::Loading(loading) = &self.state else {
            return;
        };
        let Some(result) = loading.poll() else {
            return;
        };
        self.state = match result {
            Ok((document, service)) => {
                set_title(ctx, Some(&document.name()));
                State::Ready(Box::new(Viewer::new(document, service)))
            }
            Err(error) => State::Failed {
                path: loading.path().to_owned(),
                error: error_text(&error),
            },
        };
    }
}

impl eframe::App for KymoApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

fn set_title(ctx: &egui::Context, file: Option<&str>) {
    let title = match file {
        Some(file) => format!("{file} – {APP_NAME}"),
        None => APP_NAME.to_owned(),
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
}

/// A message with an optional detail and an Open button, centred. Returns whether the
/// button was clicked.
fn centered_message(ui: &mut Ui, message: &str, detail: Option<(&String, Color32)>) -> bool {
    let mut clicked = false;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading(message);
        if let Some((detail, color)) = detail {
            ui.add_space(8.0);
            ui.label(RichText::new(detail).color(color));
        }
        ui.add_space(12.0);
        clicked = ui.button("Open…").clicked();
    });
    clicked
}

/// Starts the window and runs until it closes.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    // winit's default app menu reads "About kymo", "Quit kymo" from the process name, which
    // outside an app bundle is the executable's. Step 4b replaces that menu.
    #[cfg(target_os = "macos")]
    objc2_foundation::NSProcessInfo::processInfo()
        .setProcessName(objc2_foundation::ns_string!(APP_NAME));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("kymo")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            Ok(Box::new(KymoApp::new(
                &cc.egui_ctx,
                gpu,
                Options::default(),
                path,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}
