//! The Settings window.

use eframe::egui::{self, DragValue, Ui};

use crate::disk_cache::DiskCache;
use crate::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB, Theme};
use crate::ui::Overlay;
use crate::ui::fonts::SYSTEM_FONT_NAME;
use crate::ui::types::CHANNEL_TYPES;

/// What the Channel Types row's button is labeled.
pub const EDIT_TYPES: &str = "Edit\u{2026}";

/// What the Disk Cache row's button is labeled.
pub const CLEAR_CACHE: &str = "Clear";

/// The Settings window, while `open`. Its Channel Types row's button opens the Channel
/// Types window (`channel_types`); its Disk Cache row shows `cache`, where there is one, and
/// sets `clear_cache` when its button is clicked. Returns whether a setting changed; the
/// caller applies it at once.
pub fn settings_window(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut Settings,
    channel_types: &mut bool,
    cache: &DiskCache,
    clear_cache: &mut bool,
) -> bool {
    let before = settings.clone();
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
            // Within most of the screen, scrolling beyond.
            let max_height = ui.ctx().content_rect().height() * 0.8;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
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
                    if let Some(name) = SYSTEM_FONT_NAME {
                        ui.label("Font");
                        ui.checkbox(&mut settings.system_font, "System Font");
                        ui.end_row();
                        hint(ui, &format!("Use the system font ({name}) for all text."));
                    }
                    let label = ui.label(CHANNEL_TYPES);
                    if ui.button(EDIT_TYPES).labelled_by(label.id).clicked() {
                        *channel_types = true;
                    }
                    ui.end_row();
                    hint(ui, "Color, DC removal, and scaling of each channel type.");
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
                    hint(ui, "Fit the amplitudes to the first window of a recording.");
                    row(
                        ui,
                        "Tile Cache",
                        "Memory for recently viewed data.",
                        DragValue::new(&mut settings.tile_cache_mb)
                            .range(TILE_CACHE_MB)
                            .speed(8.0)
                            .suffix(" MB"),
                    );
                    row(
                        ui,
                        "GPU Memory",
                        "Graphics memory for recently viewed data.",
                        DragValue::new(&mut settings.gpu_memory_mb)
                            .range(GPU_MEMORY_MB)
                            .speed(8.0)
                            .suffix(" MB"),
                    );
                    if cache.exists() {
                        let label = ui.label("Disk Cache");
                        ui.horizontal(|ui| {
                            let button = ui.add_enabled(
                                cache.clearable(),
                                egui::Button::new(CLEAR_CACHE),
                            );
                            if button.labelled_by(label.id).clicked() {
                                *clear_cache = true;
                            }
                            ui.label(cache.text());
                        });
                        ui.end_row();
                        hint(
                            ui,
                            "Overviews that make recordings open faster (settings are kept).",
                        );
                    }
                    ui.label("Updates");
                    ui.checkbox(&mut settings.check_updates, "Check at Startup");
                    ui.end_row();
                    hint(ui, "Look for a newer version on GitHub once a day.");
                });
                });
            ui.add_space(6.0);
            if ui.button("Restore Defaults").clicked() {
                // What the View menu sets stays as it is.
                *settings = Settings {
                    antialias: settings.antialias,
                    scale_bars: settings.scale_bars,
                    remove_dc: settings.remove_dc,
                    ..Settings::default()
                };
            }
        });
    *settings = settings.clone().clamped();
    *settings != before
}

/// Space between the label and value columns.
const GAP: f32 = 12.0;

/// The settings' row labels, in the first column.
const LABELS: [&str; 9] = [
    "Theme",
    "Font",
    "Channel Types",
    "Default Window",
    "Scale",
    "Tile Cache",
    "GPU Memory",
    "Disk Cache",
    "Updates",
];

fn text_width(ctx: &egui::Context, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(&ctx.global_style());
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    })
}

/// The width of the window's content: the widest label, then [`VALUES_WIDTH`]. Set from
/// the first frame, as egui sizes a window from a first pass that gives the values column
/// less room than its widest row takes, and would widen the window a frame later (moving its
/// close button).
fn content_width(ctx: &egui::Context) -> f32 {
    let labels = LABELS
        .iter()
        .map(|label| text_width(ctx, label))
        .fold(0.0, f32::max);
    labels + GAP + VALUES_WIDTH
}

/// The values column's width: wider than the widest value, so the hints wrap less.
const VALUES_WIDTH: f32 = 400.0;

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
