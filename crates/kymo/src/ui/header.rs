//! The sidebar: the file's header and its channels.

use eframe::egui::{
    self, Align, Checkbox, Color32, CursorIcon, FontId, Frame, Id, Label, Layout, Margin, Response,
    RichText, ScrollArea, Sense, TextStyle, Ui, UiBuilder, vec2,
};
use kymo_core::{ChannelId, ChannelKind, Stream, StreamContent};

use crate::annotations::{AnnotationColor, AnnotationColors, Legend};
use crate::document::{Document, Summary, format_rate, grid_rate, plural};
use crate::ui::color::{Choice, swatch};
use crate::ui::palette::{TypeColors, annotation_color, bad_color, type_color};
use crate::ui::readout::display_unit;
use crate::ui::{elide_middle, text_button};
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
/// Returns a description picked from the field's menu for the selected annotation.
/// `recording` names this opening of the recording (the viewer's serial): the lists start
/// at their top for each.
pub fn header_panel(
    ui: &mut Ui,
    recording: u64,
    document: &Document,
    summary: &Summary,
    view: &mut ViewState,
    type_colors: &TypeColors,
    annotations: AnnotationPanel<'_>,
) -> Option<String> {
    let AnnotationPanel {
        legend,
        colors: annotation_colors,
        field,
    } = annotations;
    let mut relabel = None;
    ui.add_space(6.0);
    ui.add(Label::new(RichText::new(document.name()).heading()).truncate());
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
                let key = Label::new(RichText::new(row.key).weak()).extend();
                let value = if row.value.contains('\n') {
                    // A line each (the rates), each shortened on its own, the key at the top.
                    ui.vertical(|ui| ui.add(key));
                    ui.vertical(|ui| {
                        for line in row.value.lines() {
                            ui.add(Label::new(line).truncate());
                        }
                    })
                    .response
                } else {
                    ui.add(key);
                    ui.add(Label::new(row.value).truncate())
                };
                if let Some(hover) = row.hover {
                    value.on_hover_text(hover);
                }
                ui.end_row();
            }
        });

    if !summary.warnings.is_empty() {
        ui.add_space(6.0);
        warning_line(ui, &summary.warnings);
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
                    legend_list(ui, recording, legend, annotation_colors);
                }
            });
    }
    kind_list(ui, view, type_colors);
    ui.separator();
    channel_list(ui, recording, document, view, type_colors);
    relabel
}

/// Widest a warning tooltip gets before its text wraps, in points.
const WARNING_TOOLTIP_WIDTH: f32 = 360.0;

/// The reader's warnings as one line, a warning sign and how many ("1 Warning"); hovering
/// it shows them in full.
fn warning_line(ui: &mut Ui, warnings: &[String]) {
    let count = format!("{} Warning{}", warnings.len(), plural(warnings.len()));
    let response = ui
        .horizontal(|ui| {
            let size = ui.text_style_height(&TextStyle::Body);
            let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
            warning_sign(ui.painter(), rect);
            ui.add(Label::new(count).selectable(false));
        })
        .response
        .interact(Sense::hover());
    response
        .on_hover_cursor(CursorIcon::Help)
        .on_hover_ui(|ui| {
            ui.set_max_width(WARNING_TOOLTIP_WIDTH);
            for warning in warnings {
                ui.add(Label::new(sentence(warning)).wrap());
            }
        });
}

/// A warning sign filling `rect`: a yellow triangle with a dark exclamation mark, the same
/// in light and dark themes.
fn warning_sign(painter: &egui::Painter, rect: egui::Rect) {
    let at = |x: f32, y: f32| rect.lerp_inside(vec2(x, y));
    let yellow = Color32::from_rgb(0xFF, 0xCC, 0x00);
    let dark = Color32::from_gray(0x20);
    let corners = vec![at(0.5, 0.06), at(0.98, 0.92), at(0.02, 0.92)];
    // A stroke in the fill's color rounds the corners a little.
    let stroke = egui::Stroke::new(rect.width() * 0.1, yellow);
    painter.add(egui::Shape::convex_polygon(corners, yellow, stroke));
    let width = rect.width() * 0.13;
    painter.line_segment([at(0.5, 0.36), at(0.5, 0.62)], (width, dark));
    painter.circle_filled(at(0.5, 0.78), width * 0.6, dark);
}

/// `text` with its first letter upper case, as a sentence starts; readers write warnings
/// in lower case, for `kymo --info`'s "warning" lines.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
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
fn legend_list(ui: &mut Ui, recording: u64, legend: &mut Legend, picked: &mut AnnotationColors) {
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
    from_top(ui, "legend", recording, ScrollArea::vertical())
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

/// Narrowest the list's names get, in ems (the body font's size), before columns drop out;
/// narrower names are shortened in the middle.
const LIST_MIN_NAME_EMS: f32 = 6.0;

/// Narrowest the list's units get, in ems, before the names narrow: units kymo doesn't know
/// can be long ("normalized_signed").
const LIST_MIN_UNIT_EMS: f32 = 4.0;

/// Gap between the channel list's columns, in points.
const LIST_GAP: f32 = 8.0;

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
            unit: widest(&mut channels.iter().map(|c| display_unit(c).0)),
            rate: widest(&mut channels.iter().map(|c| list_rate(rec.grid(c.grid)))),
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

    /// What a row of `width` shows: the type, unit and rate columns, and the name's and
    /// unit's widths. Everything in full where it fits; with too little room the unit narrows
    /// first, down to `min_unit`, then the name, down to `min_name`; then the rate drops out,
    /// then the unit, then the type, the name and then the unit widening again into the room
    /// they leave. `fixed` is what every row has besides (the checkbox, the Bad toggle and
    /// the gaps between them and the name). Without `rated` rows have no rate column.
    fn fit(
        &self,
        width: f32,
        fixed: f32,
        gap: f32,
        min_name: f32,
        min_unit: f32,
        rated: bool,
    ) -> RowColumns {
        let options = [
            (true, true, rated),
            (true, true, false),
            (true, false, false),
        ];
        let min_name = min_name.min(self.name);
        let min_unit = min_unit.min(self.unit);
        for (kind, unit, rate) in options {
            let others = [(kind, self.kind), (unit, 0.0), (rate, self.rate)]
                .iter()
                .filter(|(shown, _)| *shown)
                .map(|(_, w)| gap + w)
                .sum::<f32>();
            // The room for the name and the unit.
            let room = width - fixed - others;
            let unit_least = if unit { min_unit } else { 0.0 };
            if room >= min_name + unit_least {
                let name = (room - unit_least).min(self.name);
                return RowColumns {
                    name,
                    kind,
                    unit: unit.then(|| (room - name).min(self.unit)),
                    rate,
                };
            }
        }
        RowColumns {
            name: (width - fixed).clamp(0.0, self.name),
            kind: false,
            unit: None,
            rate: false,
        }
    }
}

/// What a row of the channel list shows (see [`Columns::fit`]): the name's width, the type,
/// the unit's width and the rate.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowColumns {
    name: f32,
    kind: bool,
    unit: Option<f32>,
    rate: bool,
}

impl RowColumns {
    /// Whether the row shows every column it has; `rated` if it has a rate column.
    fn all(&self, rated: bool) -> bool {
        self.kind && self.unit.is_some() && (self.rate || !rated)
    }
}

/// A grid's rate in the channel list: an irregular grid's average rate as "≈ 300 Hz".
fn list_rate(grid: &kymo_core::Grid) -> String {
    if grid.times().is_some() {
        format!("\u{2248} {}", format_rate(grid.rate()))
    } else {
        format_rate(grid.rate())
    }
}

/// A grid's rate in full: an irregular grid's as "irregular, 300 Hz on average".
fn full_rate(grid: &kymo_core::Grid) -> String {
    if grid.times().is_some() {
        format!("{} on average", grid_rate(grid))
    } else {
        grid_rate(grid)
    }
}

/// One row of the channel list.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Item {
    /// A stream's row (by its index in the recording), above its channels.
    Stream(usize),
    Channel(ChannelId),
    /// A stream of the file that isn't shown, at the end.
    NotShown(usize),
}

/// The channel list's rows: the channels in the view's order, grouped by stream (a closed
/// stream's channels left out) if `grouped`, then the streams that aren't shown.
fn list_items(document: &Document, view: &ViewState, grouped: bool, closed: &[usize]) -> Vec<Item> {
    let streams = document.recording.streams();
    let stream_of = |c: ChannelId| {
        streams
            .iter()
            .position(|s| matches!(&s.content, StreamContent::Channels(ids) if ids.contains(&c)))
    };
    let mut items = Vec::with_capacity(view.order().len() + streams.len());
    let mut current = None;
    for &c in view.order() {
        if grouped {
            let stream = stream_of(c);
            if stream != current {
                current = stream;
                items.extend(stream.map(Item::Stream));
            }
            if stream.is_some_and(|s| closed.contains(&s)) {
                continue;
            }
        }
        items.push(Item::Channel(c));
    }
    items.extend(
        streams
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.content, StreamContent::NotShown(_)))
            .map(|(i, _)| Item::NotShown(i)),
    );
    items
}

/// A stream's name as the list shows it: "Stream 3" for one without a name.
fn stream_name(stream: &Stream) -> String {
    if stream.name.trim().is_empty() {
        format!("Stream {}", stream.id)
    } else {
        stream.name.clone()
    }
}

/// All channels in the view's order, drawing only the rows in view, in columns as wide as
/// the widest entry of any channel. Per channel: whether it is shown, its name (a click
/// scrolls the traces to it), whether it is bad, its type, its unit as the readout shows
/// values (µV for EEG whatever the file stores) and its rate (right-aligned; "≈ 300 Hz" for
/// an irregular grid's average). A list too narrow for all of it narrows the units and
/// names (shortened in the middle) and then leaves out the rate, unit and type (see
/// [`Columns::fit`]); hovering a row whose text is shortened or left out shows it all.
///
/// A recording with several streams of channels (XDF) groups them by stream: a row per
/// stream (see [`stream_row`]) above its channels, which leave out the rate, the stream's.
/// Streams the file has but doesn't show are listed at the end, grayed, with the reason on
/// hover. Nothing of streams shows for other recordings.
fn channel_list(
    ui: &mut Ui,
    recording: u64,
    document: &Document,
    view: &mut ViewState,
    type_colors: &TypeColors,
) {
    let rec = &document.recording;
    let streams = rec.streams();
    let grouped = streams
        .iter()
        .filter(|s| matches!(s.content, StreamContent::Channels(_)))
        .count()
        > 1;
    let closed_key = ui.scope_id().with(("closed streams", recording));
    let mut closed: Vec<usize> = ui.data(|d| d.get_temp(closed_key)).unwrap_or_default();
    let items = list_items(document, view, grouped, &closed);
    let mut toggled = None;
    let columns = Columns::of(ui, document);
    let row_height = ui.spacing().interact_size.y;
    let gap = LIST_GAP;
    let body = TextStyle::Body.resolve(ui.style());
    // Grouped, a channel's checkbox lines up with its stream's, past the disclosure arrow.
    let indent = if grouped {
        ui.spacing().icon_width + gap
    } else {
        0.0
    };
    // The width of a row's checkbox and Bad toggle with the gaps beside them (and the
    // indent), as the last frame laid them out (a guess before).
    let fixed_key = Id::unique("kymo channel list fixed width");
    let fixed = ui
        .data(|d| d.get_temp::<f32>(fixed_key))
        .unwrap_or(indent + ui.spacing().interact_size.x + ui.spacing().icon_width + 3.0 * gap);
    let mut measured = None;
    from_top(ui, "channels", recording, ScrollArea::vertical())
        .auto_shrink(false)
        .show_rows(ui, row_height, items.len(), |ui, rows| {
            let fit = columns.fit(
                ui.available_width(),
                fixed,
                gap,
                LIST_MIN_NAME_EMS * body.size,
                LIST_MIN_UNIT_EMS * body.size,
                !grouped,
            );
            for i in rows {
                let (rect, row_response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
                if i % 2 == 1 {
                    let stripe = rect.expand2(vec2(0.0, ui.spacing().item_spacing.y / 2.0));
                    ui.painter()
                        .rect_filled(stripe, 0.0, ui.visuals().faint_bg_color);
                }
                // What is left past the row's right edge is clipped.
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

                let id = match items[i] {
                    Item::Stream(s) => {
                        let open = !closed.contains(&s);
                        if stream_row(ui, document, s, open, view, row_response) {
                            toggled = Some(s);
                        }
                        continue;
                    }
                    Item::NotShown(s) => {
                        not_shown_row(ui, &streams[s], row_response);
                        continue;
                    }
                    Item::Channel(id) => id,
                };
                ui.add_space(indent);
                let channel = rec.channel(id);
                let name = &channel.name;
                let grid = rec.grid(channel.grid);
                let (unit, _) = display_unit(channel);
                let short_name = elide_middle(ui.ctx(), name, &body, fit.name);
                let short_unit = fit
                    .unit
                    .and_then(|width| elide_middle(ui.ctx(), &unit, &body, width));
                // The row's whole text where some of it is shortened or left out.
                let full = (short_name.is_some() || short_unit.is_some() || !fit.all(!grouped))
                    .then(|| {
                        let unit = if unit.is_empty() {
                            String::new()
                        } else {
                            format!(" \u{b7} {unit}")
                        };
                        format!(
                            "{name}\n{}{unit} \u{b7} {}",
                            kind_name(channel.kind),
                            full_rate(grid)
                        )
                    });

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
                let text = short_name.as_deref().unwrap_or(name);
                let label = Label::new(RichText::new(text).color(color))
                    .sense(Sense::click())
                    .extend();
                let mut response = cell(ui, fit.name, left, label);
                if short_name.is_some() {
                    accessible_text(ui, &response, name.clone());
                }
                let hover = match (&full, shown) {
                    (Some(full), true) => Some(format!("{full}\n\nShow in Traces")),
                    (Some(full), false) => Some(full.clone()),
                    (None, true) => Some("Show in Traces".to_owned()),
                    (None, false) => None,
                };
                if let Some(hover) = hover {
                    response = response.on_hover_text(hover);
                }
                if shown {
                    let response = response.on_hover_cursor(CursorIcon::PointingHand);
                    if response.clicked() {
                        view.scroll_to(id);
                    }
                }

                let response = ui.add(text_button(
                    {
                        let text = RichText::new("Bad").small();
                        if bad { text } else { text.weak() }
                    },
                    Some(bad),
                ));
                accessible_label(ui, &response, format!("{name} bad"));
                if response.clicked() {
                    view.toggle_bad(id);
                }
                // Everything but the name, without the gap before the next column; in whole
                // points, so positions rounded to pixels don't change it from frame to frame.
                measured = Some((ui.cursor().left() - rect.left() - fit.name - gap).ceil());

                let weak = |text: &str| Label::new(RichText::new(text).weak()).extend();
                // Labels are selectable, so they hide the row from the pointer: each cell
                // shows the row's whole text too.
                let mut cells = vec![row_response];
                if fit.kind {
                    cells.push(cell(ui, columns.kind, left, weak(kind_name(channel.kind))));
                }
                if let Some(width) = fit.unit {
                    let response = cell(
                        ui,
                        width,
                        left,
                        weak(short_unit.as_deref().unwrap_or(&unit)),
                    );
                    if short_unit.is_some() {
                        accessible_text(ui, &response, unit.clone());
                    }
                    cells.push(response);
                }
                if fit.rate {
                    let right = Layout::right_to_left(Align::Center);
                    let response = cell(ui, columns.rate, right, weak(&list_rate(grid)));
                    if grid.times().is_some() && full.is_none() {
                        response.on_hover_text(full_rate(grid));
                    } else {
                        cells.push(response);
                    }
                }
                if let Some(full) = full {
                    for response in cells {
                        response.on_hover_text(&full);
                    }
                }
            }
        });
    if let Some(width) = measured
        && width != fixed
    {
        ui.data_mut(|d| d.insert_temp(fixed_key, width));
        ui.ctx().request_repaint();
    }
    if let Some(s) = toggled {
        match closed.iter().position(|&c| c == s) {
            Some(at) => {
                closed.remove(at);
            }
            None => closed.push(s),
        }
        ui.data_mut(|d| d.insert_temp(closed_key, closed));
    }
}

/// A stream's row in the channel list: a disclosure arrow that shows or hides its channels'
/// rows, a checkbox that shows or hides its channels in the traces (mixed while some are
/// shown), its name (a click scrolls the traces to its first shown channel), its type, and
/// at the right its number of channels and its rate. A narrow row leaves out the type, then
/// the number and rate, then shortens the name; hovering shows it all. Returns whether the
/// arrow was clicked.
fn stream_row(
    ui: &mut Ui,
    document: &Document,
    s: usize,
    open: bool,
    view: &mut ViewState,
    row: Response,
) -> bool {
    let rec = &document.recording;
    let stream = &rec.streams()[s];
    let StreamContent::Channels(ids) = &stream.content else {
        return false;
    };
    let name = stream_name(stream);
    let grid = rec.grid(rec.channel(ids[0]).grid);
    let (shown, all) = view.stream_shown(s);
    let row_height = row.rect.height();
    let icon = ui.spacing().icon_width;

    let (_, arrow) = ui.allocate_exact_size(vec2(icon, row_height), Sense::click());
    egui::collapsing_header::paint_default_icon(ui, if open { 1.0 } else { 0.0 }, &arrow);
    let verb = if open { "Collapse" } else { "Expand" };
    accessible_label(ui, &arrow, format!("{verb} {name}"));
    let arrow = arrow.on_hover_cursor(CursorIcon::PointingHand);
    let clicked = arrow.clicked();

    let mut checked = shown == all;
    let checkbox = Checkbox::without_text(&mut checked).indeterminate(0 < shown && shown < all);
    let response = ui.add(checkbox);
    accessible_label(ui, &response, format!("Show {name}"));
    if response.changed() {
        view.set_stream_shown(s, checked);
    }

    let body = TextStyle::Body.resolve(ui.style());
    let width = |text: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), body.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
    };
    let kind = stream.kind.trim();
    let count = format!("{all} \u{b7} {}", list_rate(grid));
    let gap = ui.spacing().item_spacing.x;
    let room = ui.available_width();
    let (name_width, kind_width, count_width) = (width(&name), width(kind), width(&count));
    let least = name_width.min(LIST_MIN_NAME_EMS * body.size);
    let beside = |kind: bool, count: bool| {
        f32::from(u8::from(kind)) * (gap + kind_width)
            + f32::from(u8::from(count)) * (gap + count_width)
    };
    let (show_kind, show_count) = [(!kind.is_empty(), true), (false, true), (false, false)]
        .into_iter()
        .find(|&(k, c)| least + beside(k, c) <= room)
        .unwrap_or((false, false));
    let name_room = (room - beside(show_kind, show_count)).clamp(0.0, name_width);
    let short = elide_middle(ui.ctx(), &name, &body, name_room);

    let channels = format!("{all} channel{}", plural(all));
    let full = [kind, &channels, &full_rate(grid)]
        .iter()
        .filter(|t| !t.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let full = format!("{name}\n{full}");

    let (_, rect) = ui.allocate_space(vec2(name_room, row_height));
    let label = Label::new(RichText::new(short.as_deref().unwrap_or(&name)).strong())
        .sense(Sense::click())
        .extend();
    let mut response = ui
        .new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        )
        .add(label);
    if short.is_some() {
        accessible_text(ui, &response, name.clone());
    }
    let first_shown = view
        .order()
        .iter()
        .copied()
        .find(|c| ids.contains(c) && view.is_shown(*c));
    if let Some(first) = first_shown {
        response = response
            .on_hover_text(format!("{full}\n\nShow in Traces"))
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            view.scroll_to(first);
        }
    } else {
        response.on_hover_text(&full);
    }
    let weak = |text: &str| Label::new(RichText::new(text).weak()).extend();
    // Labels hide the row from the pointer: each shows the row's whole text too.
    let mut cells = vec![row];
    if show_kind {
        cells.push(ui.add(weak(kind)));
    }
    if show_count {
        let response = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(weak(&count))
            })
            .inner;
        cells.push(response);
    }
    for cell in cells {
        cell.on_hover_text(&full);
    }
    clicked
}

/// A stream the file has but kymo doesn't show, grayed, in line with the streams' names;
/// hovering it says why.
fn not_shown_row(ui: &mut Ui, stream: &Stream, row: Response) {
    let StreamContent::NotShown(reason) = &stream.content else {
        return;
    };
    let name = stream_name(stream);
    let icon = ui.spacing().icon_width;
    let height = row.rect.height();
    // Where a stream's arrow and checkbox are.
    ui.allocate_exact_size(vec2(icon, height), Sense::hover());
    ui.add_visible(false, Checkbox::without_text(&mut false));
    let hover = format!("{name} isn't shown: {reason}");
    let weak = |text: &str| Label::new(RichText::new(text).weak()).truncate();
    let mut cells = vec![row, ui.add(weak(&name))];
    let kind = stream.kind.trim();
    if !kind.is_empty() {
        cells.push(ui.add(weak(kind)));
    }
    for cell in cells {
        cell.on_hover_text(&hover);
    }
}

/// The whole text of a label that shows it shortened, for screen readers (and tests), which
/// read a label's text as its value.
fn accessible_text(ui: &Ui, response: &Response, text: String) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_value(text));
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
        ChannelKind::Mag => "MEG Mag",
        ChannelKind::Grad => "MEG Grad",
        ChannelKind::RefMeg => "MEG Ref",
        ChannelKind::Seeg => "sEEG",
        ChannelKind::Ecog => "ECoG",
        ChannelKind::Dbs => "DBS",
        ChannelKind::Resp => "Resp",
        ChannelKind::Bio => "Bio",
        _ => "Misc",
    }
}

/// `area` scrolled to its top the first time it shows for `recording`. egui keeps scroll
/// offsets by id, so another recording would start where the last one was, and, as egui's
/// memory is saved, so would the first one after a restart.
fn from_top(ui: &Ui, salt: &str, recording: u64, area: ScrollArea) -> ScrollArea {
    let id = ui.scope_id().with(("scrolled for", salt));
    let first = ui.data_mut(|d| {
        let first = d.get_temp::<u64>(id) != Some(recording);
        d.insert_temp(id, recording);
        first
    });
    if first {
        area.vertical_scroll_offset(0.0)
    } else {
        area
    }
}
