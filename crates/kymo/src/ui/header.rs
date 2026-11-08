//! The sidebar: the file's header and its channels.

use eframe::egui::{
    self, Align, Button, Checkbox, CursorIcon, Label, Layout, Response, RichText, ScrollArea,
    Sense, Ui,
};
use kymo_core::{ChannelId, ChannelKind};

use crate::document::{Document, Summary, plural};
use crate::ui::palette::{bad_color, trace_color};
use crate::view::{KINDS, ViewState};

/// Runs `add_contents` in exactly the width `ui` offers, clipping whatever is wider. A panel
/// grows to fit wider contents, and while its edge is dragged the difference stays unpainted
/// (black), so panel contents must never be wider than the panel. Text that should shorten
/// with "…" instead opts in with `Label::truncate`.
pub fn within_width<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.shrink_clip_rect(rect);
    let inner = add_contents(&mut child);
    ui.advance_cursor_after_rect(rect);
    inner
}

/// File header and channel list, which shows and hides channels and marks them bad in
/// `view`. Returns whether Open was clicked.
pub fn header_panel(
    ui: &mut Ui,
    document: &Document,
    summary: &Summary,
    view: &mut ViewState,
) -> bool {
    let mut open_clicked = false;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        // The button first, so a long file name truncates instead of pushing it out.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            open_clicked = ui.button("Open…").clicked();
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(Label::new(RichText::new(document.name()).heading()).truncate());
            });
        });
    });
    ui.add(
        Label::new(
            RichText::new(document.path.display().to_string())
                .small()
                .weak(),
        )
        .truncate(),
    );
    ui.add_space(6.0);

    egui::Grid::new("summary")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            // Keys at their full width; values, in the last column, shorten to what is left.
            let row = |ui: &mut Ui, key: &str, value: String| {
                ui.add(Label::new(RichText::new(key).weak()).extend());
                ui.add(Label::new(value).truncate());
                ui.end_row();
            };
            for (key, value) in summary.sidebar_rows() {
                row(ui, &key, value);
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
            ui.add(Label::new(RichText::new(warning).color(color)).wrap());
        }
    }

    ui.add_space(6.0);
    ui.separator();
    kind_list(ui, view);
    ui.separator();
    channel_list(ui, document, view);
    open_clicked
}

/// A row per channel type in the recording: a checkbox, in the type's trace colour, that
/// shows or hides all of its channels (mixed while some are shown), and how many are shown.
fn kind_list(ui: &mut Ui, view: &mut ViewState) {
    egui::Grid::new("kinds")
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for kind in KINDS {
                let (shown, all) = view.kind_shown(kind);
                if all == 0 {
                    continue;
                }
                // A click on a mixed box shows them all.
                let mut checked = shown == all;
                let text = RichText::new(kind_name(kind)).color(trace_color(kind, ui.visuals()));
                let checkbox =
                    Checkbox::new(&mut checked, text).indeterminate(0 < shown && shown < all);
                if ui.add(checkbox).changed() {
                    view.set_kind_shown(kind, checked);
                }
                let count = format!("{shown} of {all} shown");
                ui.add(Label::new(RichText::new(count).weak()).extend());
                ui.end_row();
            }
        });
}

/// All channels, drawing only the rows in view. Per channel: whether it is shown, its name
/// (a click scrolls the traces to it), whether it is bad, its type, unit and rate.
fn channel_list(ui: &mut Ui, document: &Document, view: &mut ViewState) {
    let rec = &document.recording;
    let row_height = ui.spacing().interact_size.y;
    ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        row_height,
        rec.channels().len(),
        |ui, rows| {
            egui::Grid::new("channels")
                .num_columns(6)
                .striped(true)
                .min_row_height(row_height)
                // The checkbox and bad columns only as wide as their widgets.
                .min_col_width(0.0)
                .spacing([8.0, ui.spacing().item_spacing.y])
                .show(ui, |ui| {
                    // Every column at its full width; a narrow panel clips the right ones.
                    let cell = |ui: &mut Ui, text: RichText| {
                        ui.add(Label::new(text).extend());
                    };
                    for i in rows {
                        let id = ChannelId(i as u32);
                        let channel = rec.channel(id);
                        let name = &channel.name;
                        let mut shown = view.is_shown(id);
                        let response = ui.add(Checkbox::without_text(&mut shown));
                        accessible_label(ui, &response, format!("Show {name}"));
                        if response.changed() {
                            view.set_shown(id, shown);
                        }

                        let bad = view.is_bad(id);
                        let color = if bad {
                            bad_color(ui.visuals())
                        } else if !shown {
                            ui.visuals().weak_text_color()
                        } else {
                            trace_color(channel.kind, ui.visuals())
                        };
                        let response = ui.add(
                            Label::new(RichText::new(name).color(color))
                                .sense(Sense::click())
                                .extend(),
                        );
                        if shown {
                            let response = response.on_hover_cursor(CursorIcon::PointingHand);
                            if response.clicked() {
                                view.scroll_to(id);
                            }
                        }

                        let response = ui.add(Button::selectable(bad, {
                            let text = RichText::new("bad").small();
                            if bad { text } else { text.weak() }
                        }));
                        accessible_label(ui, &response, format!("{name} bad"));
                        if response.clicked() {
                            view.toggle_bad(id);
                        }

                        let rate = rec.grid(channel.grid).rate();
                        cell(ui, RichText::new(kind_name(channel.kind)).weak());
                        cell(ui, RichText::new(&channel.unit.label).weak());
                        cell(ui, RichText::new(format!("{rate} Hz")).weak());
                        ui.end_row();
                    }
                });
        },
    );
}

/// Names a widget whose text doesn't say what it is for, for screen readers (and tests).
fn accessible_label(ui: &Ui, response: &Response, label: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}

pub fn kind_name(kind: ChannelKind) -> &'static str {
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
