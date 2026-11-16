//! The Settings window.

use eframe::egui::{self, Checkbox, DragValue, RichText, Ui};
use kymo_core::ChannelKind;

use crate::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB, Theme};
use crate::ui::Overlay;
use crate::ui::color::{Choice, swatch};
use crate::ui::fonts::SYSTEM_FONT_NAME;
use crate::ui::palette::type_color;

/// The types the channel types' table lists.
const KINDS: [ChannelKind; 14] = [
    ChannelKind::EEG,
    ChannelKind::MAG,
    ChannelKind::GRAD,
    ChannelKind::REF_MEG,
    ChannelKind::EOG,
    ChannelKind::ECG,
    ChannelKind::EMG,
    ChannelKind::SEEG,
    ChannelKind::ECOG,
    ChannelKind::DBS,
    ChannelKind::RESP,
    ChannelKind::BIO,
    ChannelKind::MISC,
    ChannelKind::STIM,
];

/// The Settings window, while `open`. Returns whether a setting changed; the caller applies
/// it at once.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> bool {
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
            // Within most of the screen, scrolling beyond (the channel types take many rows).
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
                        hint(ui, &format!("Show all text in {name}, the system's font."));
                    }
                    // At the first row of swatches, as the other labels sit at their
                    // control, rather than centered on all of them.
                    // In a row like the swatches' own, centered as they are.
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.horizontal(|ui| ui.label("Channel Types"));
                    });
                    type_table(ui, settings);
                    ui.end_row();
                    hint(
                        ui,
                        "Click a type's color to change it; bad channels stay gray. Remove DC \
                         centers the type's traces while Remove DC is on below. Per Channel \
                         scales each of its channels on its own, rather than all alike.",
                    );
                    ui.label("Traces");
                    ui.checkbox(&mut settings.antialias, "Anti-Aliased");
                    ui.end_row();
                    hint(ui, "Smooth trace edges. Off draws traces in whole pixels.");
                    ui.label("");
                    ui.checkbox(&mut settings.remove_dc, "Remove DC");
                    ui.end_row();
                    hint(
                        ui,
                        "Center each trace on its mean over the window, for the channel types \
                         ticked above.",
                    );
                    ui.label("");
                    ui.checkbox(&mut settings.scale_bars, "Scale Bars");
                    ui.end_row();
                    hint(
                        ui,
                        "A bar per channel type shows how much a row spans, one per channel \
                         where each is scaled on its own.",
                    );
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
                        "Fit each channel type (or channel) to the first window, as Auto Scale \
                         does.",
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
                });
            ui.add_space(6.0);
            if ui.button("Restore Defaults").clicked() {
                *settings = Settings::default();
            }
        });
    *settings = settings.clone().clamped();
    *settings != before
}

/// Space between the label and value columns, and between color cells.
const GAP: f32 = 12.0;

/// The settings' row labels, in the first column.
const LABELS: [&str; 8] = [
    "Theme",
    "Font",
    "Channel Types",
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
        .map(|kind| text_width(ctx, kind.name()))
        .fold(0.0, f32::max);
    egui::vec2(side + SWATCH_GAP + widest, side)
}

/// Space between a swatch and its type name.
const SWATCH_GAP: f32 = 4.0;

/// The width of the window's content: the widest label, then the channel types' table, or
/// three color cells' width and [`HINT_ROOM`] if wider, which is wider than the other
/// values. Set from the first frame, as egui sizes a window from a first pass that gives the
/// values column less room than the table takes, and would widen the window a frame later
/// (moving its close button).
fn content_width(ctx: &egui::Context) -> f32 {
    let labels = LABELS
        .iter()
        .map(|label| text_width(ctx, label))
        .fold(0.0, f32::max);
    let half = color_cell(ctx).x + text_width(ctx, REMOVE_DC) + text_width(ctx, OWN_SCALE);
    // Two halves, and the six gaps between their columns and the empty one between them.
    let table = 2.0 * half + 6.0 * GAP;
    let colors = 3.0 * color_cell(ctx).x + 2.0 * GAP;
    labels + GAP + table.max(colors + HINT_ROOM)
}

/// Room beyond the color cells, so the hints wrap less.
const HINT_ROOM: f32 = 140.0;

/// The channel types' table's column headers.
const REMOVE_DC: &str = "Remove DC";
const OWN_SCALE: &str = "Per Channel";

/// Rows of the channel types' table: its two halves side by side, the types in
/// [`KINDS`] order down the left half, then down the right.
const TYPE_ROWS: usize = KINDS.len().div_ceil(2);

/// The channel types' table: a header row naming the columns, then per channel type its
/// swatch (a click picks its color or resets it) and name, and checkboxes for DC removal and
/// scaling each channel on its own, centered under the headers; none for stim channels,
/// which neither applies to.
fn type_table(ui: &mut Ui, settings: &mut Settings) {
    let cell = color_cell(ui.ctx());
    let columns = [REMOVE_DC, OWN_SCALE].map(|header| text_width(ui.ctx(), header));
    egui::Grid::new("channel types")
        .num_columns(7)
        .spacing([GAP, 4.0])
        // Not egui's 40 points for the empty column between the halves, which would widen
        // the window beyond `content_width` a frame after it opened.
        .min_col_width(0.0)
        .show(ui, |ui| {
            for half in 0..2 {
                if half == 1 {
                    // Between the halves: twice the gap.
                    ui.label("");
                }
                ui.label("");
                for header in [REMOVE_DC, OWN_SCALE] {
                    ui.label(RichText::new(header).weak());
                }
            }
            ui.end_row();
            for row in 0..TYPE_ROWS {
                for half in 0..2 {
                    if half == 1 {
                        ui.label("");
                    }
                    if let Some(kind) = KINDS.get(half * TYPE_ROWS + row) {
                        type_row(ui, settings, kind, cell, columns);
                    }
                }
                ui.end_row();
            }
        });
}

/// A channel type's cells in the channel types' table: swatch and name, `cell` in size,
/// then its checkboxes in columns `columns` wide.
fn type_row(
    ui: &mut Ui,
    settings: &mut Settings,
    kind: &ChannelKind,
    cell: egui::Vec2,
    columns: [f32; 2],
) {
    let name = kind.name();
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(cell, layout, |ui| {
        ui.set_min_size(cell);
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        let color = type_color(kind, &settings.types, ui.visuals());
        let picked = settings.types.color(kind).is_some();
        let size = egui::vec2(cell.y, cell.y);
        match swatch(ui, &format!("Color of {name}"), color, picked, size) {
            Some(Choice::Picked(c)) => settings.types.set_color(kind, Some(c)),
            Some(Choice::Reset) => settings.types.set_color(kind, None),
            None => {}
        }
        ui.label(name);
    });
    if kind.is_stim() {
        ui.label("");
        ui.label("");
        return;
    }
    let mut flags = [
        settings.types.remove_dc(kind),
        settings.types.own_scale(kind),
    ];
    let labels = [
        format!("Remove DC of {name}"),
        format!("Scale Each {name} Channel"),
    ];
    for ((flag, label), width) in flags.iter_mut().zip(labels).zip(columns) {
        let layout = egui::Layout::top_down(egui::Align::Center);
        ui.allocate_ui_with_layout(egui::vec2(width, cell.y), layout, |ui| {
            ui.set_min_width(width);
            let response = ui.add(Checkbox::without_text(flag));
            ui.ctx()
                .accesskit_node_builder(response.id, |node| node.set_label(label));
        });
    }
    let [remove_dc, own_scale] = flags;
    if remove_dc != settings.types.remove_dc(kind) {
        settings.types.set_remove_dc(kind, remove_dc);
    }
    if own_scale != settings.types.own_scale(kind) {
        settings.types.set_own_scale(kind, own_scale);
    }
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
