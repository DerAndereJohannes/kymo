//! The Settings window.

use eframe::egui::{self, DragValue, Label, RichText, Ui};

use crate::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB};
use crate::ui::color::{Choice, swatch};
use crate::ui::header::kind_name;
use crate::ui::palette::type_color;
use crate::view::KINDS;

/// The Settings window, while `open`. Returns whether a setting changed; the caller applies
/// it at once.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> bool {
    let before = *settings;
    egui::Window::new("Settings")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    row(
                        ui,
                        "Tile cache",
                        "Memory for tiles in RAM. A smaller cache frees memory at once.",
                        DragValue::new(&mut settings.tile_cache_mb)
                            .range(TILE_CACHE_MB)
                            .speed(8.0)
                            .suffix(" MB"),
                    );
                    row(
                        ui,
                        "GPU memory",
                        "Memory for tiles on the graphics card.",
                        DragValue::new(&mut settings.gpu_memory_mb)
                            .range(GPU_MEMORY_MB)
                            .speed(8.0)
                            .suffix(" MB"),
                    );
                    ui.label("Traces");
                    ui.checkbox(&mut settings.antialias, "Anti-aliased");
                    ui.end_row();
                    hint(ui, "Smooth trace edges. Off draws traces in whole pixels.");
                    ui.label("Scale");
                    ui.checkbox(&mut settings.auto_scale_on_open, "Automatic on opening");
                    ui.end_row();
                    hint(
                        ui,
                        "Fit each channel type to the first window, as Auto Scale does.",
                    );
                    ui.label("Colors");
                    type_colors(ui, settings);
                    ui.end_row();
                    hint(
                        ui,
                        "Click a channel type's color to change it. Bad channels stay gray.",
                    );
                    row(
                        ui,
                        "Default window",
                        "Seconds shown when a recording opens and after Default Window.",
                        DragValue::new(&mut settings.default_window_s)
                            .range(DEFAULT_WINDOW_S)
                            .speed(0.5)
                            .max_decimals(3)
                            .suffix(" s"),
                    );
                });
            ui.add_space(6.0);
            if ui.button("Restore Defaults").clicked() {
                *settings = Settings::default();
            }
        });
    *settings = settings.clamped();
    *settings != before
}

/// A swatch and name per channel type, three to a row; a click on a swatch picks its color
/// or resets it.
fn type_colors(ui: &mut Ui, settings: &mut Settings) {
    let side = ui.text_style_height(&egui::TextStyle::Body);
    egui::Grid::new("type colors")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (i, kind) in KINDS.into_iter().enumerate() {
                ui.horizontal(|ui| {
                    let color = type_color(kind, &settings.type_colors, ui.visuals());
                    let picked = settings.type_colors[i].is_some();
                    let label = format!("Color of {}", kind_name(kind));
                    match swatch(ui, &label, color, picked, egui::vec2(side, side)) {
                        Some(Choice::Picked(c)) => settings.type_colors[i] = Some(c),
                        Some(Choice::Reset) => settings.type_colors[i] = None,
                        None => {}
                    }
                    ui.label(kind_name(kind));
                });
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
}

/// A label, its value, and a hint below in the value's column.
fn row(ui: &mut Ui, label: &str, hint: &str, value: DragValue<'_>) {
    let label = ui.label(label);
    ui.add(value).labelled_by(label.id);
    ui.end_row();
    self::hint(ui, hint);
}

/// A hint below a row, in the value's column.
fn hint(ui: &mut Ui, text: &str) {
    ui.label("");
    ui.add(Label::new(RichText::new(text).small().weak()).wrap());
    ui.end_row();
}
