//! The Recording Info window: everything `kymo --info` prints, with a button that copies it.

use std::path::Path;

use eframe::egui::{self, Align, Label, Layout, RichText, TextStyle};

use crate::action::Action;
use crate::document::{Summary, info_text};
use crate::ui::Overlay;

/// What the Copy button is named, also for tests.
pub const COPY: &str = "Copy";

/// The Recording Info window, while `open`: the file's name and path, then the rows of
/// [`Summary::info_rows`], keys at the left (capitalized, grayed) and values wrapping in the
/// width left, format fields after their names. Copy puts `--info`'s text on the clipboard.
pub fn info_window(ctx: &egui::Context, open: &mut bool, path: &Path, summary: &Summary) {
    let rows = summary.info_rows();
    let style = ctx.global_style();
    let font = TextStyle::Body.resolve(&style);
    let color = style.visuals.text_color();
    let key_width = ctx.fonts_mut(|f| {
        rows.iter()
            .map(|row| {
                f.layout_no_wrap(capitalized(&row.key), font.clone(), color)
                    .size()
                    .x
            })
            .fold(0.0, f32::max)
    });
    egui::Window::new(Action::RecordingInfo.label().trim_end_matches('…'))
        .id(Overlay::RecordingInfo.id())
        .open(open)
        .collapsible(false)
        // As wide as dragged, and as tall as its rows up to most of the screen, where they
        // scroll: a window's own scroll area (`vscroll`) keeps it at a fixed height.
        .resizable([true, false])
        .default_width(520.0)
        .show(ctx, |ui| {
            let max_height = ui.ctx().content_rect().height() * 0.7;
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| rows_ui(ui, path, summary, key_width));
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.button(COPY).clicked() {
                    ui.ctx().copy_text(info_text(path, summary));
                }
            });
        });
}

/// The window's rows: the file's name and path, then [`Summary::info_rows`], with the keys
/// in a column `key_width` wide.
fn rows_ui(ui: &mut egui::Ui, path: &Path, summary: &Summary, key_width: f32) {
    let name = path.file_name().unwrap_or(path.as_os_str());
    ui.label(RichText::new(name.to_string_lossy()).strong());
    ui.add(Label::new(RichText::new(path.display().to_string()).small().weak()).wrap());
    ui.add_space(6.0);
    let gap = 12.0;
    for row in summary.info_rows() {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(key_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_min_width(key_width);
                    ui.add(Label::new(RichText::new(capitalized(&row.key)).weak()).extend());
                },
            );
            ui.add_space(gap - ui.spacing().item_spacing.x);
            if let Some(field) = &row.field {
                ui.add(Label::new(RichText::new(format!("{field}:")).weak()).extend());
            }
            ui.add(Label::new(&row.value).wrap());
        });
    }
}

/// `key` with its first letter in upper case ("grid 1" → "Grid 1").
fn capitalized(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
