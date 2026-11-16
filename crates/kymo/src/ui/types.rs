//! What sets channel types' settings: a type's swatch in the sidebar, whose popup sets its
//! color, DC removal and scaling, and the Channel Types window, which lists every type kymo
//! knows, those set and the open recording's.

use eframe::egui::{
    self, Align, Button, Checkbox, Layout, Popup, PopupCloseBehavior, RichText, TextEdit, Ui, Vec2,
};
use kymo_core::{ChannelKind, KNOWN_KINDS};

use crate::types::TypeSettings;
use crate::ui::Overlay;
use crate::ui::color::{picker, swatch_button};
use crate::ui::palette::type_color;

/// The window's title, and the button that opens it.
pub const CHANNEL_TYPES: &str = "Channel Types";

/// What opens the window from a type's popup.
pub const ALL_TYPES: &str = "All Types\u{2026}";

/// What a type's DC removal checkbox is named, for screen readers.
pub fn remove_dc_label(kind: &ChannelKind) -> String {
    format!("Remove DC of {}", kind.name())
}

/// What a type's scaling checkbox is named, for screen readers.
pub fn own_scale_label(kind: &ChannelKind) -> String {
    format!("Scale Each {} Channel", kind.name())
}

/// A swatch of `size` in `kind`'s color, named "Options for {label}" for screen readers
/// (`label` names the row, as "EEG channels in Amp"). A click opens a popup with the type's
/// name, a color picker, Remove DC and Scale Each Channel (neither for stim channels),
/// Reset, enabled while something differs from the type's start, and All Types…, which asks
/// for the Channel Types window. Returns whether it was asked for.
pub fn type_swatch(
    ui: &mut Ui,
    kind: &ChannelKind,
    label: &str,
    types: &mut TypeSettings,
    size: Vec2,
) -> bool {
    let color = type_color(kind, types, ui.visuals());
    let name = kind.name();
    let response = swatch_button(ui, &format!("Options for {label}"), color, size)
        .on_hover_text(format!("{name}: Color, Remove DC and Scaling"));
    let mut all_types = false;
    Popup::menu(&response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.label(RichText::new(name).strong());
            if let Some(c) = picker(ui, color) {
                types.set_color(kind, Some(c));
            }
            if !kind.is_stim() {
                ui.add_space(4.0);
                flags(ui, kind, types);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let reset = ui.add_enabled(types.changed(kind), Button::new("Reset"));
                label_for(ui, &reset, format!("Reset {name}"));
                if reset.clicked() {
                    types.reset(kind);
                }
                if ui.button(ALL_TYPES).clicked() {
                    all_types = true;
                    ui.close();
                }
            });
        });
    all_types
}

/// `kind`'s Remove DC and Scale Each Channel checkboxes, applied to `types` as they change.
fn flags(ui: &mut Ui, kind: &ChannelKind, types: &mut TypeSettings) {
    let mut remove_dc = types.remove_dc(kind);
    let mut own_scale = types.own_scale(kind);
    for (flag, shown, label) in [
        (&mut remove_dc, REMOVE_DC, remove_dc_label(kind)),
        (&mut own_scale, OWN_SCALE, own_scale_label(kind)),
    ] {
        let response = ui.add(Checkbox::new(flag, shown));
        label_for(ui, &response, label);
    }
    if remove_dc != types.remove_dc(kind) {
        types.set_remove_dc(kind, remove_dc);
    }
    if own_scale != types.own_scale(kind) {
        types.set_own_scale(kind, own_scale);
    }
}

/// The checkboxes' text, also the window's column headers.
const REMOVE_DC: &str = "Remove DC";
const OWN_SCALE: &str = "Scale Each Channel";

/// What the window marks types kymo knows and the others with.
const BUILT_IN: &str = "Built in";
const CUSTOM: &str = "Custom";

/// Names a widget for screen readers (and tests), whatever its text says.
fn label_for(ui: &Ui, response: &egui::Response, label: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}

/// The types the Channel Types window lists: every type kymo knows, every other type
/// something was set for, and the other types of the open recording (`recording`), each
/// once, by name in alphabetical order, ignoring case.
pub fn listed_types(types: &TypeSettings, recording: &[ChannelKind]) -> Vec<ChannelKind> {
    let mut listed: Vec<ChannelKind> = KNOWN_KINDS
        .iter()
        .map(|k| ChannelKind::named(k.name))
        .collect();
    let others = types
        .entries()
        .map(|t| ChannelKind::named(&t.name))
        .chain(recording.iter().cloned());
    for kind in others {
        if !listed.contains(&kind) {
            listed.push(kind);
        }
    }
    listed.sort_by_cached_key(|k| (k.name().to_lowercase(), k.name().to_owned()));
    listed
}

/// Whether `kind` matches the window's filter: its name or another of its spellings
/// contains `filter`, or "Built in" or "Custom" does, as the window marks it; ignoring case
/// and surrounding spaces. Everything matches an empty filter.
pub fn matches(kind: &ChannelKind, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let origin = if kind.known().is_some() {
        BUILT_IN
    } else {
        CUSTOM
    };
    let aliases = kind.known().map_or(&[][..], |k| k.aliases);
    [kind.name(), kind.key(), origin]
        .into_iter()
        .chain(aliases.iter().copied())
        .any(|text| text.to_lowercase().contains(&filter))
}

/// The Channel Types window, while `open`: a filter field (`filter`), then a row per type
/// of [`listed_types`] that matches it ([`matches`]): its swatch (a click picks its color),
/// its name, Remove DC and Scale Each Channel (neither for stim channels), "Built in" or
/// "Custom" grayed, and Reset for a type kymo knows while something differs from its start,
/// Forget for another type something was set for (which removes it from the list unless
/// the recording has it). The rows scroll beyond most of the screen. Returns whether
/// a setting changed; the caller applies it at once.
pub fn channel_types_window(
    ctx: &egui::Context,
    open: &mut bool,
    types: &mut TypeSettings,
    recording: &[ChannelKind],
    filter: &mut String,
) -> bool {
    let before = types.clone();
    let listed = listed_types(types, recording);
    let columns = Columns::of(ctx, &listed);
    egui::Window::new(CHANNEL_TYPES)
        .id(Overlay::ChannelTypes.id())
        .open(open)
        .collapsible(false)
        .resizable(false)
        // The window's width counts its frame too; set from the first frame, which egui
        // would lay out narrower, widening the window a frame later.
        .min_width(
            columns.total()
                + egui::Frame::window(&ctx.global_style())
                    .total_margin()
                    .sum()
                    .x,
        )
        .show(ctx, |ui| {
            let field = ui.add(
                TextEdit::singleline(filter)
                    .hint_text("Filter")
                    .desired_width(columns.total()),
            );
            label_for(ui, &field, "Filter Channel Types".to_owned());
            ui.add_space(6.0);
            let shown: Vec<&ChannelKind> = listed.iter().filter(|k| matches(k, filter)).collect();
            let max_height = ui.ctx().content_rect().height() * 0.6;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.label(RichText::new("No channel types match.").weak());
                        return;
                    }
                    type_grid(ui, &shown, types, &columns);
                });
            ui.add_space(6.0);
            ui.add(super::hint(
                "Remove DC centers a type's traces while Remove DC is on. Scale Each Channel \
                 scales each of its channels on its own, rather than all alike. A type's color \
                 in the sidebar sets them too.",
            ));
        });
    *types != before
}

/// Space between the window's columns.
const GAP: f32 = 12.0;

/// Space between a swatch and its type's name.
const SWATCH_GAP: f32 = 6.0;

/// The widths of the window's columns, fitting every type listed, so they stay put as the
/// filter changes what shows.
struct Columns {
    /// A row's height, also the swatch's side.
    row: f32,
    name: f32,
    remove_dc: f32,
    own_scale: f32,
    origin: f32,
    button: f32,
}

impl Columns {
    fn of(ctx: &egui::Context, listed: &[ChannelKind]) -> Columns {
        let style = ctx.global_style();
        let font = egui::TextStyle::Body.resolve(&style);
        let width = |text: &str| {
            ctx.fonts_mut(|f| {
                f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                    .size()
                    .x
            })
        };
        let row = ctx.fonts_mut(|f| f.row_height(&font));
        let names = listed.iter().map(|k| width(k.name())).fold(0.0, f32::max);
        let padding = 2.0 * style.spacing.button_padding.x;
        Columns {
            row,
            name: row + SWATCH_GAP + names,
            remove_dc: width(REMOVE_DC),
            own_scale: width(OWN_SCALE),
            origin: width(BUILT_IN).max(width(CUSTOM)),
            button: width("Reset").max(width("Forget")) + padding,
        }
    }

    fn total(&self) -> f32 {
        self.name + self.remove_dc + self.own_scale + self.origin + self.button + 4.0 * GAP
    }
}

/// The window's rows of `shown` under a row of column headers (see
/// [`channel_types_window`]).
fn type_grid(ui: &mut Ui, shown: &[&ChannelKind], types: &mut TypeSettings, columns: &Columns) {
    let fixed = |ui: &mut Ui, width: f32, align: Align, add: &mut dyn FnMut(&mut Ui)| {
        let layout = Layout::left_to_right(Align::Center).with_main_align(align);
        ui.allocate_ui_with_layout(egui::vec2(width, columns.row), layout, |ui| {
            ui.set_min_width(width);
            add(ui);
        });
    };
    egui::Grid::new("channel types")
        .num_columns(5)
        .spacing([GAP, 4.0])
        .min_col_width(0.0)
        .striped(true)
        .show(ui, |ui| {
            let header = |text: &str| RichText::new(text).weak();
            fixed(ui, columns.name, Align::Min, &mut |ui| {
                ui.label(header("Type"));
            });
            fixed(ui, columns.remove_dc, Align::Center, &mut |ui| {
                ui.label(header(REMOVE_DC));
            });
            fixed(ui, columns.own_scale, Align::Center, &mut |ui| {
                ui.label(header(OWN_SCALE));
            });
            ui.label("");
            ui.label("");
            ui.end_row();
            for &kind in shown {
                type_row(ui, kind, types, columns, &fixed);
                ui.end_row();
            }
        });
}

/// A layout helper of [`type_grid`]: a cell of a width, its contents aligned so.
type Fixed<'a> = dyn Fn(&mut Ui, f32, Align, &mut dyn FnMut(&mut Ui)) + 'a;

/// One type's row in the window.
fn type_row(
    ui: &mut Ui,
    kind: &ChannelKind,
    types: &mut TypeSettings,
    columns: &Columns,
    fixed: &Fixed<'_>,
) {
    let name = kind.name();
    fixed(ui, columns.name, Align::Min, &mut |ui| {
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        let color = type_color(kind, types, ui.visuals());
        let size = egui::vec2(columns.row, columns.row) * 0.8;
        let response = swatch_button(ui, &format!("Color of {name}"), color, size);
        Popup::menu(&response)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                if let Some(c) = picker(ui, color) {
                    types.set_color(kind, Some(c));
                }
            });
        ui.label(name);
    });
    if kind.is_stim() {
        ui.label("");
        ui.label("");
    } else {
        // Each checkbox centered under its header.
        let mut remove_dc = types.remove_dc(kind);
        let mut own_scale = types.own_scale(kind);
        for (flag, width, label) in [
            (&mut remove_dc, columns.remove_dc, remove_dc_label(kind)),
            (&mut own_scale, columns.own_scale, own_scale_label(kind)),
        ] {
            fixed(ui, width, Align::Center, &mut |ui| {
                let response = ui.add(Checkbox::without_text(flag));
                label_for(ui, &response, label.clone());
            });
        }
        if remove_dc != types.remove_dc(kind) {
            types.set_remove_dc(kind, remove_dc);
        }
        if own_scale != types.own_scale(kind) {
            types.set_own_scale(kind, own_scale);
        }
    }
    let known = kind.known().is_some();
    ui.label(RichText::new(if known { BUILT_IN } else { CUSTOM }).weak());
    fixed(ui, columns.button, Align::Min, &mut |ui| {
        if known && types.changed(kind) {
            let reset = ui.button("Reset");
            label_for(ui, &reset, format!("Reset {name}"));
            if reset.clicked() {
                types.reset(kind);
            }
        } else if !known && types.stored(kind) {
            let forget = ui.button("Forget");
            label_for(ui, &forget, format!("Forget {name}"));
            if forget.clicked() {
                types.forget(kind);
            }
        }
    });
}
