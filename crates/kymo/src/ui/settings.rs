//! The Settings window.

use eframe::egui::{self, DragValue, Ui};

use crate::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB, Theme};
use crate::ui::Overlay;
use crate::ui::color::{Choice, swatch};
use crate::ui::header::kind_name;
use crate::ui::palette::type_color;
use crate::view::KINDS;

/// The Settings window, while `open`. Returns whether a setting changed; the caller applies
/// it at once.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> bool {
    let before = *settings;
    egui::Window::new("Settings")
        .id(Overlay::Settings.id())
        .open(open)
        .collapsible(false)
        .resizable(false)
        // The window's width counts its frame too.
        .min_width(
            content_width(ctx)
                + egui::Frame::window(&ctx.global_style())
                    .total_margin()
                    .sum()
                    .x,
        )
        .show(ctx, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([GAP, 6.0])
                .show(ui, |ui| {
                    // How it looks, then how a recording opens, then memory.
                    ui.label("Theme");
                    ui.horizontal(|ui| {
                        for theme in Theme::ALL {
                            ui.radio_value(&mut settings.theme, theme, theme.label());
                        }
                    });
                    ui.end_row();
                    hint(ui, "System follows the system's light or dark mode.");
                    // At the first row of swatches, as the other labels sit at their
                    // control, rather than centered on all of them.
                    // In a row like the swatches' own, centered as they are.
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.horizontal(|ui| ui.label("Colors"));
                    });
                    type_colors(ui, settings);
                    ui.end_row();
                    hint(
                        ui,
                        "Click a channel type's color to change it. Bad channels stay gray.",
                    );
                    ui.label("Traces");
                    ui.checkbox(&mut settings.antialias, "Anti-Aliased");
                    ui.end_row();
                    hint(ui, "Smooth trace edges. Off draws traces in whole pixels.");
                    ui.label("");
                    ui.checkbox(&mut settings.remove_dc, "Remove DC");
                    ui.end_row();
                    hint(ui, "Center each trace on its mean over the window.");
                    ui.label("");
                    ui.checkbox(&mut settings.scale_bars, "Scale Bars");
                    ui.end_row();
                    hint(ui, "A bar per channel type shows how much a row spans.");
                    row(
                        ui,
                        "Default Window",
                        "Seconds shown when a recording opens.",
                        DragValue::new(&mut settings.default_window_s)
                            .range(DEFAULT_WINDOW_S)
                            .speed(0.5)
                            .max_decimals(3)
                            .suffix(" s"),
                    );
                    ui.label("Scale");
                    ui.checkbox(&mut settings.auto_scale_on_open, "Automatic on Opening");
                    ui.end_row();
                    hint(
                        ui,
                        "Fit each channel type to the first window, as Auto Scale does.",
                    );
                    row(
                        ui,
                        "Tile Cache",
                        "Memory for tiles in RAM. A smaller cache frees memory at once.",
                        DragValue::new(&mut settings.tile_cache_mb)
                            .range(TILE_CACHE_MB)
                            .speed(8.0)
                            .suffix(" MB"),
                    );
                    row(
                        ui,
                        "GPU Memory",
                        "Memory for tiles on the graphics card.",
                        DragValue::new(&mut settings.gpu_memory_mb)
                            .range(GPU_MEMORY_MB)
                            .speed(8.0)
                            .suffix(" MB"),
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

/// Space between the label and value columns, and between color cells.
const GAP: f32 = 12.0;

/// The settings' row labels, in the first column.
const LABELS: [&str; 7] = [
    "Theme",
    "Colors",
    "Traces",
    "Default Window",
    "Scale",
    "Tile Cache",
    "GPU Memory",
];

fn text_width(ctx: &egui::Context, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(&ctx.global_style());
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    })
}

/// The size of one color cell: a swatch and the widest type name.
fn color_cell(ctx: &egui::Context) -> egui::Vec2 {
    // A square swatch as tall as a line of text, as `Ui::text_style_height` gives.
    let font = egui::TextStyle::Body.resolve(&ctx.global_style());
    let side = ctx.fonts_mut(|f| f.row_height(&font));
    let widest = KINDS
        .iter()
        .map(|&kind| text_width(ctx, kind_name(kind)))
        .fold(0.0, f32::max);
    egui::vec2(side + SWATCH_GAP + widest, side)
}

/// Space between a swatch and its type name.
const SWATCH_GAP: f32 = 4.0;

/// The width of the window's content: the widest label, then three color cells, which are
/// wider than the other values, and [`HINT_ROOM`]. Set from the first frame, as egui sizes a window from a
/// first pass that gives the values column less room than the color rows take, and would
/// widen the window a frame later (moving its close button).
fn content_width(ctx: &egui::Context) -> f32 {
    let labels = LABELS
        .iter()
        .map(|label| text_width(ctx, label))
        .fold(0.0, f32::max);
    labels + GAP + 3.0 * color_cell(ctx).x + 2.0 * GAP + HINT_ROOM
}

/// Room beyond the color cells, so the hints wrap less.
const HINT_ROOM: f32 = 140.0;

/// A swatch and name per channel type, three to a row; a click on a swatch picks its color
/// or resets it.
fn type_colors(ui: &mut Ui, settings: &mut Settings) {
    let cell = color_cell(ui.ctx());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(GAP, 4.0);
        for (r, row) in KINDS.chunks(3).enumerate() {
            ui.horizontal(|ui| {
                for (k, &kind) in row.iter().enumerate() {
                    let i = 3 * r + k;
                    let layout = egui::Layout::left_to_right(egui::Align::Center);
                    ui.allocate_ui_with_layout(cell, layout, |ui| {
                        ui.set_min_size(cell);
                        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
                        let color = type_color(kind, &settings.type_colors, ui.visuals());
                        let picked = settings.type_colors[i].is_some();
                        let label = format!("Color of {}", kind_name(kind));
                        let size = egui::vec2(cell.y, cell.y);
                        match swatch(ui, &label, color, picked, size) {
                            Some(Choice::Picked(c)) => settings.type_colors[i] = Some(c),
                            Some(Choice::Reset) => settings.type_colors[i] = None,
                            None => {}
                        }
                        ui.label(kind_name(kind));
                    });
                }
            });
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
    ui.add(super::hint(text));
    ui.end_row();
}
