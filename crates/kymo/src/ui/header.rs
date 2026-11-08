//! The sidebar: the file's header and its channels.

use eframe::egui::{
    self, Align, Button, Checkbox, Color32, CursorIcon, FontId, Frame, Id, Label, Layout, Margin,
    Response, RichText, ScrollArea, Sense, TextStyle, Ui, UiBuilder, vec2,
};
use kymo_core::{ChannelId, ChannelKind};

use crate::annotations::Legend;
use crate::document::{Document, Summary, plural};
use crate::ui::palette::{annotation_color, bad_color, trace_color};
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

/// Rows of the annotation legend shown before it scrolls.
const LEGEND_ROWS: f32 = 8.0;

/// File header, channel types and channels, and the annotation legend at the bottom. The
/// channel list shows and hides channels and marks them bad in `view`; the legend shows and
/// hides annotations by description in `legend`. Returns whether Open was clicked.
pub fn header_panel(
    ui: &mut Ui,
    document: &Document,
    summary: &Summary,
    view: &mut ViewState,
    legend: &mut Legend,
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
    // The legend at the bottom, so the channel list fills the space below its types.
    if !legend.is_empty() {
        egui::Panel::bottom("legend")
            .resizable(false)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| legend_list(ui, legend));
    }
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

/// A row for all annotations: a checkbox (mixed while some descriptions are shown) and how
/// many there are; then a row per description: a checkbox, its colour, the description and
/// how many annotations have it. Counts are right-aligned. Scrolls beyond [`LEGEND_ROWS`]
/// description rows.
fn legend_list(ui: &mut Ui, legend: &mut Legend) {
    let (shown, all) = legend.shown_count();
    let row_height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.add(Label::new(RichText::new(legend.total().to_string()).weak()).extend());
    row.with_layout(Layout::left_to_right(Align::Center), |ui| {
        let mut checked = shown == all;
        let checkbox =
            Checkbox::new(&mut checked, "Annotations").indeterminate(0 < shown && shown < all);
        if ui.add(checkbox).changed() {
            legend.set_all_shown(checked);
        }
    });
    let entries: Vec<(String, usize, usize, bool)> = legend
        .entries()
        .map(|e| (e.description.clone(), e.color, e.count, e.shown))
        .collect();
    // Exactly as tall as its rows, up to LEGEND_ROWS: the panel holding it would otherwise
    // keep its first height.
    let height = (entries.len() as f32).min(LEGEND_ROWS)
        * (row_height + ui.spacing().item_spacing.y)
        - ui.spacing().item_spacing.y;
    ScrollArea::vertical()
        .id_salt("legend")
        .min_scrolled_height(height)
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (description, color, count, shown) in entries {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
                let mut row = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                // The count first, at the right, so a long description truncates.
                row.add(Label::new(RichText::new(count.to_string()).weak()).extend());
                row.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let mut checked = shown;
                    let response = ui.add(Checkbox::without_text(&mut checked));
                    accessible_label(ui, &response, format!("Show {description}"));
                    if response.changed() {
                        legend.set_shown(&description, checked);
                    }
                    let side = ui.text_style_height(&TextStyle::Body) * 0.7;
                    let (swatch, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                    let color = annotation_color(color, ui.visuals());
                    ui.painter().rect_filled(swatch, 2.0, color);
                    let text = if shown {
                        RichText::new(&description)
                    } else {
                        RichText::new(&description).weak()
                    };
                    ui.add(Label::new(text).truncate());
                });
            }
        });
}

/// Widths of the channel list's text columns, fitting every channel's entry, so they stay
/// put while the list scrolls (only the rows in view are drawn).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Columns {
    name: f32,
    kind: f32,
    unit: f32,
    rate: f32,
}

/// Column widths with the font and scale they were measured for.
#[derive(Clone, Debug, PartialEq)]
struct Measured {
    font: FontId,
    pixels_per_point: f32,
    columns: Columns,
}

impl Columns {
    /// The widths for `document`'s channels in `ui`'s body font, measured once per document,
    /// font and scale.
    fn of(ui: &Ui, document: &Document) -> Columns {
        let font = TextStyle::Body.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        let rec = &document.recording;
        let key = Id::unique(("kymo channel columns", &document.path, rec.channels().len()));
        if let Some(m) = ui.data(|d| d.get_temp::<Measured>(key))
            && m.font == font
            && m.pixels_per_point == pixels_per_point
        {
            return m.columns;
        }
        let widest = |texts: &mut dyn Iterator<Item = String>| {
            ui.fonts_mut(|f| {
                texts
                    .map(|t| {
                        f.layout_no_wrap(t, font.clone(), Color32::PLACEHOLDER)
                            .size()
                            .x
                    })
                    .fold(0.0, f32::max)
            })
        };
        let channels = rec.channels();
        let columns = Columns {
            name: widest(&mut channels.iter().map(|c| c.name.clone())),
            kind: widest(&mut channels.iter().map(|c| kind_name(c.kind).to_owned())),
            unit: widest(&mut channels.iter().map(|c| c.tile_unit().to_owned())),
            rate: widest(&mut channels.iter().map(|c| rate_text(rec.grid(c.grid).rate()))),
        };
        ui.data_mut(|d| {
            d.insert_temp(
                key,
                Measured {
                    font,
                    pixels_per_point,
                    columns,
                },
            )
        });
        columns
    }
}

fn rate_text(rate: f64) -> String {
    format!("{rate} Hz")
}

/// All channels, drawing only the rows in view, in columns as wide as the widest entry of
/// any channel. Per channel: whether it is shown, its name (a click scrolls the traces to
/// it), whether it is bad, its type, unit and rate (right-aligned).
fn channel_list(ui: &mut Ui, document: &Document, view: &mut ViewState) {
    let rec = &document.recording;
    let columns = Columns::of(ui, document);
    let row_height = ui.spacing().interact_size.y;
    let gap = 8.0;
    ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        row_height,
        rec.channels().len(),
        |ui, rows| {
            for i in rows {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
                if i % 2 == 1 {
                    let stripe = rect.expand2(vec2(0.0, ui.spacing().item_spacing.y / 2.0));
                    ui.painter()
                        .rect_filled(stripe, 0.0, ui.visuals().faint_bg_color);
                }
                // A narrow panel clips the columns on the right.
                let mut row = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                row.spacing_mut().item_spacing.x = gap;
                let ui = &mut row;
                // A cell of exactly `width`, the label at its left, or at its right with
                // `Layout::right_to_left`.
                let cell = |ui: &mut Ui, width: f32, layout: Layout, label: Label| {
                    let (_, rect) = ui.allocate_space(vec2(width, row_height));
                    // A child that leaves the row's cursor after the whole cell.
                    let builder = UiBuilder::new().max_rect(rect).layout(layout);
                    ui.new_child(builder).add(label)
                };
                let left = Layout::left_to_right(Align::Center);

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
                let label = Label::new(RichText::new(name).color(color))
                    .sense(Sense::click())
                    .extend();
                let response = cell(ui, columns.name, left, label);
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

                let weak = |text: String| Label::new(RichText::new(text).weak()).extend();
                cell(
                    ui,
                    columns.kind,
                    left,
                    weak(kind_name(channel.kind).to_owned()),
                );
                cell(ui, columns.unit, left, weak(channel.tile_unit().to_owned()));
                let rate = rec.grid(channel.grid).rate();
                let right = Layout::right_to_left(Align::Center);
                cell(ui, columns.rate, right, weak(rate_text(rate)));
            }
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
        ChannelKind::Stim => "Stim",
        ChannelKind::Misc => "Misc",
        _ => "Other",
    }
}
