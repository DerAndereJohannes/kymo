//! The window: a header panel beside an (as yet empty) trace area.

use std::path::PathBuf;

use eframe::egui::{self, Align, Color32, Layout, RichText, ScrollArea, TextStyle, Ui};
use kymo_core::ChannelKind;

use crate::document::{Document, Loading, Summary, error_text, file_name, format_duration, plural};

const APP_NAME: &str = "Kymo";

pub struct KymoApp {
    state: State,
    /// Show the file dialog once the window is on screen.
    pick_file: bool,
    /// Whether a frame has been shown yet.
    shown: bool,
}

enum State {
    Empty,
    Loading(Loading),
    Ready {
        document: Box<Document>,
        summary: Summary,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
}

impl KymoApp {
    /// A window opening `path`, or showing a file dialog without one.
    pub fn new(ctx: &egui::Context, path: Option<PathBuf>) -> Self {
        let mut app = KymoApp {
            state: State::Empty,
            pick_file: path.is_none(),
            shown: false,
        };
        if let Some(path) = path {
            app.open(ctx, path);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        set_title(ctx, Some(&file_name(&path)));
        let repaint = ctx.clone();
        self.state = State::Loading(Loading::start(path, move || repaint.request_repaint()));
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

    fn current_path(&self) -> Option<&std::path::Path> {
        match &self.state {
            State::Empty => None,
            State::Loading(loading) => Some(loading.path()),
            State::Ready { document, .. } => Some(&document.path),
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
            Ok(document) => {
                set_title(ctx, Some(&document.name()));
                State::Ready {
                    summary: document.summary(),
                    document: Box::new(document),
                }
            }
            Err(error) => State::Failed {
                path: loading.path().to_owned(),
                error: error_text(&error),
            },
        };
    }
}

impl eframe::App for KymoApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_loading(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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

        let mut open_clicked = false;
        match &self.state {
            State::Ready { document, summary } => {
                egui::Panel::left("header")
                    .resizable(true)
                    .default_size(300.0)
                    .show(ui, |ui| {
                        open_clicked = header_panel(ui, document, summary);
                    });
                egui::CentralPanel::default().show(ui, |_ui| {
                    // The trace area; traces arrive with the renderer.
                });
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

/// File header and channel list. Returns whether Open was clicked.
fn header_panel(ui: &mut Ui, document: &Document, summary: &Summary) -> bool {
    let mut open_clicked = false;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading(document.name());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            open_clicked = ui.button("Open…").clicked();
        });
    });
    ui.label(
        RichText::new(document.path.display().to_string())
            .small()
            .weak(),
    );
    ui.add_space(6.0);

    egui::Grid::new("summary")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            let row = |ui: &mut Ui, key: &str, value: String| {
                ui.label(RichText::new(key).weak());
                ui.label(value);
                ui.end_row();
            };
            row(ui, "Format", summary.format.to_owned());
            row(
                ui,
                "Start",
                summary.start.clone().unwrap_or_else(|| "unknown".into()),
            );
            row(ui, "Duration", format_duration(summary.duration));
            row(ui, "Channels", summary.channels.to_string());
            row(ui, "Annotations", summary.annotations.to_string());
            for (i, grid) in summary.grids.iter().enumerate() {
                let key = if summary.grids.len() == 1 {
                    "Sampling".to_owned()
                } else {
                    format!("Grid {}", i + 1)
                };
                row(ui, &key, grid.describe());
            }
        });

    if !summary.warnings.is_empty() {
        ui.add_space(6.0);
        let color = ui.visuals().warn_fg_color;
        ui.label(
            RichText::new(format!(
                "{} warning{}",
                summary.warnings.len(),
                plural(summary.warnings.len())
            ))
            .color(color)
            .strong(),
        );
        for warning in &summary.warnings {
            ui.label(RichText::new(warning).color(color));
        }
    }

    ui.add_space(6.0);
    ui.separator();
    channel_list(ui, document);
    open_clicked
}

/// All channels, drawing only the rows in view.
fn channel_list(ui: &mut Ui, document: &Document) {
    let rec = &document.recording;
    let row_height = ui.text_style_height(&TextStyle::Body);
    ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        row_height,
        rec.channels().len(),
        |ui, rows| {
            egui::Grid::new("channels")
                .num_columns(4)
                .striped(true)
                .spacing([12.0, ui.spacing().item_spacing.y])
                .show(ui, |ui| {
                    for channel in &rec.channels()[rows] {
                        ui.label(&channel.name);
                        ui.label(RichText::new(kind_name(channel.kind)).weak());
                        ui.label(RichText::new(&channel.unit.label).weak());
                        ui.label(
                            RichText::new(format!("{} Hz", rec.grid(channel.grid).rate())).weak(),
                        );
                        ui.end_row();
                    }
                });
        },
    );
}

fn kind_name(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Eeg => "EEG",
        ChannelKind::Eog => "EOG",
        ChannelKind::Ecg => "ECG",
        ChannelKind::Emg => "EMG",
        ChannelKind::Stim => "stim",
        ChannelKind::Misc => "misc",
        _ => "other",
    }
}

/// Starts the window and runs until it closes.
pub fn run(path: Option<PathBuf>) -> anyhow::Result<()> {
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
        Box::new(|cc| Ok(Box::new(KymoApp::new(&cc.egui_ctx, path)))),
    )
    .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}
