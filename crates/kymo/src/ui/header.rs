//! The sidebar: the file's header and its channels.

use eframe::egui::{
    self, Align, Button, Checkbox, Color32, CursorIcon, FontId, Frame, Id, Label, Layout, Margin,
    Response, RichText, ScrollArea, Sense, TextStyle, Ui, UiBuilder, vec2,
};
use kymo_core::{ChannelId, ChannelKind};

use crate::annotations::{AnnotationColor, AnnotationColors, Legend};
use crate::document::{Document, Summary, format_rate, plural};
use crate::ui::color::{Choice, swatch};
use crate::ui::palette::{TypeColors, annotation_color, bad_color, type_color};
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

/// What the sidebar's annotation part edits: which descriptions are shown, their picked
/// colors, and in annotation mode the description of new annotations.
pub struct AnnotationPanel<'a> {
    pub legend: &'a mut Legend,
    pub colors: &'a mut AnnotationColors,
    /// The description field, in annotation mode.
    pub field: Option<DescriptionField<'a>>,
}

/// What the description field above the legend edits.
pub enum DescriptionField<'a> {
    /// The description new annotations get.
    New(&'a mut String),
    /// The selected annotation's, `current`, as typed into `draft`; the caller applies the
    /// draft once the field gives up the keyboard, unless by Esc.
    Selected {
        current: &'a str,
        draft: &'a mut String,
    },
}

/// What the header panel asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderResponse {
    /// The Open button was clicked.
    pub open: bool,
    /// A description picked from the menu for the selected annotation.
    pub relabel: Option<String>,
}

/// The description field's id, to know whether it has the keyboard.
pub fn description_field_id() -> egui::Id {
    egui::Id::unique("kymo description field")
}

/// Rows of the annotation legend shown before it scrolls.
const LEGEND_ROWS: f32 = 8.0;

/// File header, channel types and channels, and the annotation legend at the bottom. The
/// channel list shows and hides channels and marks them bad in `view`, in their types'
/// colors (`type_colors` where picked); the legend shows and hides annotations by
/// description in `annotations.legend`, and picks their colors in `annotations.colors`. In
/// annotation mode, `annotations.field` edits the description new annotations get, or the
/// selected annotation's, above the legend, which then shows even without annotations.
pub fn header_panel(
    ui: &mut Ui,
    document: &Document,
    summary: &Summary,
    view: &mut ViewState,
    type_colors: &TypeColors,
    annotations: AnnotationPanel<'_>,
) -> HeaderResponse {
    let AnnotationPanel {
        legend,
        colors: annotation_colors,
        field,
    } = annotations;
    let mut open_clicked = false;
    let mut relabel = None;
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
            for row in summary.sidebar_rows() {
                ui.add(Label::new(RichText::new(row.key).weak()).extend());
                let value = ui.add(Label::new(row.value).truncate());
                if let Some(hover) = row.hover {
                    value.on_hover_text(hover);
                }
                ui.end_row();
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
    if !legend.is_empty() || field.is_some() {
        egui::Panel::bottom("legend")
            .resizable(false)
            .frame(Frame::NONE.inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| {
                if let Some(field) = field {
                    relabel = description_row(ui, field, legend);
                }
                if !legend.is_empty() {
                    legend_list(ui, legend, annotation_colors);
                }
            });
    }
    kind_list(ui, view, type_colors);
    ui.separator();
    channel_list(ui, document, view, type_colors);
    HeaderResponse {
        open: open_clicked,
        relabel,
    }
}

/// A row per channel type in the recording: a checkbox, in the type's trace color, that
/// shows or hides all of its channels (mixed while some are shown), and how many are shown.
fn kind_list(ui: &mut Ui, view: &mut ViewState, type_colors: &TypeColors) {
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
                let text = RichText::new(kind_name(kind)).color(type_color(
                    kind,
                    type_colors,
                    ui.visuals(),
                ));
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

/// Rows of descriptions the description menu shows before it scrolls.
const DESCRIPTION_MENU_ROWS: f32 = 12.0;

/// The description field: the description of new annotations, or the selected one's, with
/// a menu of the legend's descriptions to pick one from. Returns a description picked for
/// the selected annotation.
fn description_row(ui: &mut Ui, field: DescriptionField<'_>, legend: &Legend) -> Option<String> {
    let row_height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let (label, accessible, text, current) = match field {
        DescriptionField::New(text) => ("New", "Description of new annotations", text, None),
        DescriptionField::Selected { current, draft } => (
            "Selected",
            "Description of the selected annotation",
            draft,
            Some(current),
        ),
    };
    let mut picked = None;
    // The menu first, at the right, so the field takes the rest.
    let descriptions: Vec<String> = legend.entries().map(|e| e.description.clone()).collect();
    let menu = row.add_enabled_ui(!descriptions.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("descriptions")
            .selected_text("")
            .width(0.0)
            .height(DESCRIPTION_MENU_ROWS * row_height)
            .show_ui(ui, |ui| {
                for d in descriptions {
                    let chosen = *text == d;
                    if ui.selectable_label(chosen, &d).clicked() {
                        picked = Some(d);
                    }
                }
            })
            .response
    });
    accessible_label(&row, &menu.inner, "Descriptions".to_owned());
    row.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(Label::new(RichText::new(label).weak()).extend());
        let id = description_field_id();
        let field = ui.add(
            egui::TextEdit::singleline(text)
                .id(id)
                .hint_text("Description")
                .desired_width(f32::INFINITY),
        );
        accessible_label(ui, &field, accessible.to_owned());
    });
    ui.add_space(4.0);
    match (picked, current) {
        (Some(d), None) => {
            *text = d;
            None
        }
        (Some(d), Some(_)) => {
            *text = d.clone();
            Some(d)
        }
        (None, _) => None,
    }
}

/// A row for all annotations: a checkbox (mixed while some descriptions are shown) and how
/// many there are; then a row per description: a checkbox, its color (a click picks
/// another, kept in `picked`), the description and how many annotations have it. Counts
/// are right-aligned. Scrolls beyond [`LEGEND_ROWS`] description rows.
fn legend_list(ui: &mut Ui, legend: &mut Legend, picked: &mut AnnotationColors) {
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
    let entries: Vec<(String, AnnotationColor, usize, bool)> = legend
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
                    let shown_color = annotation_color(color, ui.visuals());
                    let label = format!("Color of {description}");
                    let is_picked = picked.contains_key(&description);
                    match swatch(ui, &label, shown_color, is_picked, vec2(side, side)) {
                        Some(Choice::Picked(c)) => {
                            picked.insert(description.clone(), [c.r(), c.g(), c.b()]);
                            legend.apply_colors(picked);
                        }
                        Some(Choice::Reset) => {
                            picked.remove(&description);
                            legend.apply_colors(picked);
                        }
                        None => {}
                    }
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
            rate: widest(
                &mut channels
                    .iter()
                    .map(|c| format_rate(rec.grid(c.grid).rate())),
            ),
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

/// All channels, drawing only the rows in view, in columns as wide as the widest entry of
/// any channel. Per channel: whether it is shown, its name (a click scrolls the traces to
/// it), whether it is bad, its type, unit and rate (right-aligned).
fn channel_list(ui: &mut Ui, document: &Document, view: &mut ViewState, type_colors: &TypeColors) {
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
                    type_color(channel.kind, type_colors, ui.visuals())
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
                cell(ui, columns.rate, right, weak(format_rate(rate)));
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
        ChannelKind::Mag => "MEG mag",
        ChannelKind::Grad => "MEG grad",
        ChannelKind::RefMeg => "MEG ref",
        ChannelKind::Seeg => "sEEG",
        ChannelKind::Ecog => "ECoG",
        ChannelKind::Dbs => "DBS",
        ChannelKind::Resp => "Resp",
        ChannelKind::Bio => "Bio",
        _ => "Misc",
    }
}
