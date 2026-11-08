//! The Settings window.

use eframe::egui::{self, DragValue, Label, RichText, Ui};

use crate::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB};

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
