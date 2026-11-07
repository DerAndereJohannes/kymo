//! The sidebar: the file's header and its channels.

use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, TextStyle, Ui};
use kymo_core::ChannelKind;

use crate::document::{Document, Summary, format_duration, plural};

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

/// File header and channel list. Returns whether Open was clicked.
pub fn header_panel(ui: &mut Ui, document: &Document, summary: &Summary) -> bool {
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
            ui.add(Label::new(RichText::new(warning).color(color)).wrap());
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
                    // Every column at its full width; a narrow panel clips the right ones.
                    let cell = |ui: &mut Ui, text: RichText| {
                        ui.add(Label::new(text).extend());
                    };
                    for channel in &rec.channels()[rows] {
                        let rate = rec.grid(channel.grid).rate();
                        cell(ui, RichText::new(&channel.name));
                        cell(ui, RichText::new(kind_name(channel.kind)).weak());
                        cell(ui, RichText::new(&channel.unit.label).weak());
                        cell(ui, RichText::new(format!("{rate} Hz")).weak());
                        ui.end_row();
                    }
                });
        },
    );
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
