//! An open recording on screen: the sidebar, the trace area with channel names, time axis
//! and scrollbars, and the status line.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Event, FontId, Pos2, Rangef, Rect, Sense, Stroke,
    StrokeKind, TextStyle, Ui, pos2, vec2,
};
use kymo_core::service::TileService;
use kymo_core::stim::EventOptions;
use kymo_core::tile::query;
use kymo_core::{AnnotationData, AnnotationId, ChannelId, ChannelKind, GridId, Origin};

use crate::action::Action;
use crate::annotations::AnnotationColors;
use crate::annotations::{self, Legend, Marks};
use crate::document::{Document, Summary};
use crate::history::{self, Change, History};
use crate::keymap::Keymap;
use crate::render::{Frame, Scene};
use crate::scale;
use crate::stim_annotations::{StimAnnotations, Update};
use crate::ui::axis::{self, Clock};
use crate::ui::gpu::Gpu;
use crate::ui::header::{AnnotationPanel, header_panel, within_width};
use crate::ui::palette::{TypeColors, annotation_color, bad_color, type_color};
use crate::ui::{readout, scrollbar, status};
use crate::view::{Area, ViewState};

/// Narrowest the sidebar can be dragged.
const SIDEBAR_MIN_WIDTH: f32 = 200.0;
/// Widest the sidebar can be dragged, as a fraction of the window.
const SIDEBAR_MAX_FRACTION: f32 = 0.5;
/// Widest the channel names column gets, as a fraction of the trace region.
const NAMES_MAX_FRACTION: f32 = 0.3;
/// Padding around channel names, in points.
const NAME_PADDING: f32 = 6.0;
/// Length of the axis' tick marks, in points.
const TICK: f32 = 4.0;
/// Room between axis labels beyond the widest label, in points.
const LABEL_GAP: f32 = 24.0;
/// Opacity of annotation bands.
const BAND_ALPHA: f32 = 0.2;
/// Room left of an annotation label, and between labels, in points.
const ANNOTATION_LABEL_PAD: f32 = 3.0;
const ANNOTATION_LABEL_GAP: f32 = 8.0;
/// How near an annotation's mark the pointer must be, in points, for the status line to
/// name it; 1-pixel lines are hard to hit exactly.
const POINTER_SLACK: f32 = 2.0;
/// The share of the tile cache that tiles beyond the screen may take.
const PREFETCH_SHARE: usize = 2;
/// How far a drag must reach sideways, in points, to create an annotation.
const MIN_DRAG: f32 = 3.0;
/// Width of the selected annotation's outline, in points.
const OUTLINE_WIDTH: f32 = 2.0;
/// The description new annotations start with: MNE treats descriptions starting with
/// "BAD" as bad segments.
pub const DEFAULT_DESCRIPTION: &str = "BAD";

/// An annotation being created by a drag.
#[derive(Clone, Copy, Debug)]
struct Creating {
    /// The time the drag started at.
    anchor: f64,
    /// Its id, once the drag reached far enough to make it.
    id: Option<AnnotationId>,
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hover {
    /// The channel row under the pointer, over the traces or the names.
    pub channel: Option<ChannelId>,
    /// The time under the pointer, over the traces.
    pub time: Option<f64>,
}

/// Where the trace area and the channel names were at the last frame, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub traces: Rect,
    pub names: Rect,
    /// The scrollbars for time (below the axis) and rows (right of the traces).
    pub time_bar: Rect,
    pub row_bar: Rect,
}

/// The status line's texts, on the left and the right.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusLine {
    pub left: String,
    pub right: String,
}

pub struct Viewer {
    document: Document,
    summary: Summary,
    /// Always `Some` until dropped; see [`Drop`].
    service: Option<TileService>,
    view: ViewState,
    /// Names this recording for the renderer.
    serial: u64,
    hover: Hover,
    status: StatusLine,
    /// Width of the trace area in physical pixels at the last frame.
    width_px: u32,
    /// Widest channel name, measured once per font size.
    names_width: Option<(f32, f32)>,
    layout: Option<Layout>,
    /// Tiles the last frame wanted that weren't cached.
    missing: usize,
    legend: Legend,
    /// The annotation marks of the last frame.
    marks: Marks,
    /// The annotations under the pointer, as the status line shows them.
    pointed: String,
    /// The stim channels' annotations, for recordings that have stim channels.
    stim: Option<StimAnnotations>,
    /// Whether traces are anti-aliased.
    antialias: bool,
    /// Whether [`Action::AutoScale`] waits for a complete frame.
    auto_scale: bool,
    /// Colors picked per channel type.
    type_colors: TypeColors,
    /// Whether annotation mode is on: dragging creates annotations, clicking selects.
    editing: bool,
    /// The selected annotation, only in annotation mode; never a stim annotation or a
    /// hidden one.
    selected: Option<AnnotationId>,
    /// Where the selection's outline was drawn at the last frame, in points.
    outline: Option<Rect>,
    /// The annotation a drag is creating.
    creating: Option<Creating>,
    /// The description of annotations created from now on.
    description: String,
    history: History,
}

impl Viewer {
    /// A recording on screen with its tile service, showing `default_span` seconds at
    /// first, with stim annotations by `stim`.
    pub fn new(
        document: Document,
        service: TileService,
        default_span: f64,
        stim: EventOptions,
    ) -> Viewer {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let has_stim = document
            .recording
            .channels()
            .iter()
            .any(|c| c.kind == ChannelKind::Stim);
        Viewer {
            stim: has_stim.then(|| StimAnnotations::new(stim)),
            summary: document.summary(),
            legend: Legend::new(&document.annotations),
            view: ViewState::with_default_span(&document.recording, default_span),
            document,
            service: Some(service),
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            hover: Hover::default(),
            status: StatusLine::default(),
            width_px: 0,
            names_width: None,
            layout: None,
            missing: 0,
            marks: Marks::default(),
            pointed: String::new(),
            antialias: false,
            auto_scale: false,
            type_colors: TypeColors::default(),
            editing: false,
            selected: None,
            outline: None,
            creating: None,
            description: DEFAULT_DESCRIPTION.to_owned(),
            history: History::default(),
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    pub fn hover(&self) -> Hover {
        self.hover
    }

    /// The annotations' descriptions, colors and whether they are shown.
    pub fn legend(&self) -> &Legend {
        &self.legend
    }

    /// The annotation marks of the last frame, in physical pixels of the trace area.
    pub fn marks(&self) -> &Marks {
        &self.marks
    }

    pub fn status(&self) -> &StatusLine {
        &self.status
    }

    /// Whether annotation mode is on.
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// The selected annotation.
    pub fn selected(&self) -> Option<AnnotationId> {
        self.selected
    }

    /// Where the selected annotation's outline was drawn at the last frame, in points.
    pub fn outline(&self) -> Option<Rect> {
        self.outline
    }

    /// The description new annotations get.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The annotation edits that can be undone and redone.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether the last frame had every tile it wanted, none drawn from a coarser one.
    pub fn complete(&self) -> bool {
        self.layout.is_some() && self.missing == 0
    }

    /// The layout of the last frame, once there was one with room for traces.
    pub fn layout(&self) -> Option<Layout> {
        self.layout
    }

    /// The tile service's cache, for its budget and size.
    pub fn cache(&self) -> &kymo_core::tile::TileCache {
        self.service().cache()
    }

    /// Whether the recording has stim channels, whose codes the rule annotates.
    pub fn has_stim(&self) -> bool {
        self.stim.is_some()
    }

    /// Annotates the stim codes by `options` from the next frame on.
    pub fn set_stim_options(&mut self, options: EventOptions) {
        if let Some(stim) = &mut self.stim {
            stim.set_options(options);
        }
    }

    /// Whether traces are anti-aliased from the next frame on.
    pub fn set_antialias(&mut self, on: bool) {
        self.antialias = on;
    }

    /// Draws channel types in the colors picked for them, the others in their defaults.
    pub fn set_type_colors(&mut self, colors: TypeColors) {
        self.type_colors = colors;
    }

    /// Seconds [`Action::DefaultWindow`] shows from now on.
    pub fn set_default_span(&mut self, seconds: f64) {
        self.view.set_default_span(seconds);
    }

    fn service(&self) -> &TileService {
        live(&self.service)
    }

    /// The physical value drawn at the middle of `channel`'s row: its mean over the window
    /// with DC removal on (the last one computed, 0 until there is one), otherwise 0. Always
    /// 0 for stim channels: DC removal asks for no means of them.
    pub fn baseline(&self, channel: ChannelId) -> f64 {
        if !self.view.dc_removed() {
            return 0.0;
        }
        self.service().mean(channel).map_or(0.0, |m| m.value)
    }

    /// Applies a view action; [`Action::ToggleBad`] acts on the channel under the pointer.
    /// [`Action::AutoScale`] applies at the first frame that has all its tiles, as coarser
    /// stand-ins would draw too wide an envelope; any other scale action cancels it.
    pub fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::AutoScale => {
                self.auto_scale = true;
                return true;
            }
            Action::ToggleEditing => {
                self.finish_creating();
                self.editing = !self.editing;
                self.selected = None;
                return true;
            }
            Action::Undo => {
                let changed = self.history.undo(&mut self.document.annotations);
                self.after_history(changed);
                return true;
            }
            Action::Redo => {
                let changed = self.history.redo(&mut self.document.annotations);
                self.after_history(changed);
                return true;
            }
            Action::DeleteAnnotation => {
                if let Some(id) = self.selected.take() {
                    // Fails only for an annotation that is gone, leaving nothing to do.
                    let _ = self
                        .history
                        .edit(&mut self.document.annotations, vec![(id, None)]);
                    self.legend.sync(&self.document.annotations);
                }
                return true;
            }
            Action::Deselect => {
                self.selected = None;
                return true;
            }
            Action::ScaleUp | Action::ScaleDown | Action::DefaultScale => self.auto_scale = false,
            _ => {}
        }
        self.view.apply(action, self.hover.channel)
    }

    /// After an undo or redo that changed `changed` (`None` if there was nothing to do):
    /// the legend follows, and in annotation mode the first of them still there is
    /// selected.
    fn after_history(&mut self, changed: Option<Vec<AnnotationId>>) {
        let Some(changed) = changed else {
            return;
        };
        self.legend.sync(&self.document.annotations);
        self.selected = None;
        if self.editing {
            self.selected = changed.into_iter().find(|&id| self.selectable(id));
        }
    }

    /// Whether the selected annotation `id` can stay selected: there, and shown. Stim
    /// annotations never get selected (see [`annotations::pick`]), and the history never
    /// holds them.
    fn selectable(&self, id: AnnotationId) -> bool {
        self.document.annotations.get(id).is_some_and(|a| {
            self.legend
                .entry(&a.data.description)
                .is_some_and(|e| e.shown)
        })
    }

    /// Ends the drag creating an annotation: records it as one edit, and selects it.
    fn finish_creating(&mut self) {
        let Some(Creating { id: Some(id), .. }) = self.creating.take() else {
            return;
        };
        if let Some(a) = self.document.annotations.get(id) {
            self.history.push(vec![Change {
                id,
                before: None,
                after: Some(a.data.clone()),
            }]);
            self.selected = Some(id);
        }
    }

    /// Shows the recording in `ui`, with the sidebar if `sidebar`, its annotations in the
    /// colors picked in `annotation_colors` (which the legend changes). Returns whether the
    /// sidebar's Open button was clicked.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        gpu: &Gpu,
        keymap: &Keymap,
        sidebar: bool,
        annotation_colors: &mut AnnotationColors,
    ) -> bool {
        self.sync_stim(ui.ctx());
        self.legend.apply_colors(annotation_colors);
        if let Some(id) = self.selected
            && !self.selectable(id)
        {
            self.selected = None;
        }
        let mut open_clicked = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status.left);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(&self.status.right);
                });
            });
        });
        if sidebar {
            egui::Panel::left("header")
                .resizable(true)
                .default_size(300.0)
                .min_size(SIDEBAR_MIN_WIDTH)
                .max_size((ui.available_width() * SIDEBAR_MAX_FRACTION).max(SIDEBAR_MIN_WIDTH))
                .show(ui, |ui| {
                    open_clicked = within_width(ui, |ui| {
                        header_panel(
                            ui,
                            &self.document,
                            &self.summary,
                            &mut self.view,
                            &self.type_colors,
                            AnnotationPanel {
                                legend: &mut self.legend,
                                colors: annotation_colors,
                                description: self.editing.then_some(&mut self.description),
                            },
                        )
                    });
                });
        }
        egui::CentralPanel::default().show(ui, |ui| self.trace_region(ui, gpu, keymap));

        // The status line was laid out before the traces moved; show the new texts in the
        // next frame, and only ask for one if they changed.
        let status = self.status_line(gpu);
        if status != self.status {
            self.status = status;
            ui.ctx().request_repaint();
        }
        open_clicked
    }

    /// Brings the stim annotations up to the build and the rule, and the legend with them.
    fn sync_stim(&mut self, ctx: &egui::Context) {
        let Some(stim) = &mut self.stim else {
            return;
        };
        let service = live(&self.service);
        let rec = &self.document.recording;
        let changes = || -> Vec<kymo_core::stim::StimChanges> {
            (0..rec.grids().len() as u32)
                .filter_map(|grid| service.pyramid(GridId(grid)))
                .flat_map(|pyramid| pyramid.stim_changes(rec))
                .collect()
        };
        let done = service.progress().done;
        let now = ctx.input(|i| i.time);
        match stim.update(&mut self.document.annotations, rec, done, now, changes) {
            Update::Changed => self.legend.sync(&self.document.annotations),
            Update::Wait(seconds) => ctx.request_repaint_after(Duration::from_secs_f64(seconds)),
            Update::Unchanged => {}
        }
    }

    fn status_line(&self, gpu: &Gpu) -> StatusLine {
        let service = self.service();
        let rec = &self.document.recording;
        let left = self
            .hover
            .time
            .map(|time| {
                let rate = rec.sfreq();
                let channel = self.hover.channel.map(|id| {
                    let info = rec.channel(id);
                    let view = self.view.tile_view(self.width_px.max(1));
                    let reading = readout::read(rec, service.cache(), &view, id, time).map(|r| {
                        status::reading_text(r.relative_to(self.baseline(id)), info.tile_unit())
                    });
                    (info.name.as_str(), reading)
                });
                status::pointer_text(
                    time,
                    status::time_decimals(rate),
                    Clock::for_end(self.view.duration()),
                    channel,
                )
            })
            .unwrap_or_default();
        let mode = self.editing.then(|| {
            match self
                .selected
                .and_then(|id| self.document.annotations.get(id))
            {
                Some(a) => status::selection_text(
                    &a.data.description,
                    a.data.onset,
                    a.data.duration,
                    status::time_decimals(rec.sfreq()),
                    Clock::for_end(self.view.duration()),
                ),
                None => "Editing annotations".to_owned(),
            }
        });
        let left = [mode.unwrap_or_default(), left, self.pointed.clone()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let mut right = status::memory_text(service.cache().bytes(), gpu.bytes());
        if let Some(build) = status::build_text(&service.progress()) {
            right = format!("{build} · {right}");
        }
        StatusLine { left, right }
    }

    /// Channel names, traces, the time axis and both scrollbars.
    fn trace_region(&mut self, ui: &mut Ui, gpu: &Gpu, keymap: &Keymap) {
        let full = ui.available_rect_before_wrap();
        let id = egui::Id::unique("kymo traces");
        let small = TextStyle::Small.resolve(ui.style());
        let body = TextStyle::Body.resolve(ui.style());
        let bar = scrollbar::THICKNESS;
        let names_width = (self.names_width(ui, &body) + 2.0 * NAME_PADDING)
            .min(full.width() * NAMES_MAX_FRACTION);
        let axis_height = TICK + ui.fonts_mut(|f| f.row_height(&small)) + 4.0;

        // Trace area on whole physical pixels, so its columns are the renderer's.
        let ppp = ui.pixels_per_point();
        let px = |v: f32| (v * ppp).round();
        let left = px(full.left() + names_width);
        let right = px(full.right() - bar - 2.0).max(left);
        let top = px(full.top());
        let bottom = px(full.bottom() - bar - axis_height).max(top);
        let traces = Rect::from_min_max(pos2(left, top) / ppp, pos2(right, bottom) / ppp);
        let size_px = [(right - left) as u32, (bottom - top) as u32];
        let names = Rect::from_min_max(pos2(full.left(), traces.top()), traces.left_bottom());
        let axis_rect = Rect::from_min_max(
            traces.left_bottom(),
            pos2(traces.right(), traces.bottom() + axis_height),
        );
        let vbar = Rect::from_min_max(
            pos2(full.right() - bar, traces.top()),
            pos2(full.right(), traces.bottom()),
        );
        let hbar = Rect::from_min_max(
            pos2(traces.left(), full.bottom() - bar),
            pos2(traces.right(), full.bottom()),
        );
        ui.advance_cursor_after_rect(full);
        if size_px[0] == 0 || size_px[1] == 0 {
            self.hover = Hover::default();
            self.pointed.clear();
            return;
        }
        self.layout = Some(Layout {
            traces,
            names,
            time_bar: hbar,
            row_bar: vbar,
        });
        let area = Area {
            width: f64::from(traces.width()),
            height: f64::from(traces.height()),
        };

        self.handle_input(ui, id, keymap, traces, names, axis_rect, size_px[0]);
        if let Some(start) = scrollbar::show(
            ui,
            id.with("time"),
            hbar,
            false,
            self.view.duration(),
            self.view.time(),
        ) {
            self.view.pan(start - self.view.start());
        }
        let rows = self.view.rows() as f64;
        if let Some(top) = scrollbar::show(
            ui,
            id.with("rows"),
            vbar,
            true,
            self.view.row_count() as f64,
            self.view.top()..self.view.top() + rows,
        ) {
            self.view.set_top(top);
        }
        self.hover = self.hover_at(ui.input(|i| i.pointer.hover_pos()), traces, names);
        self.pointed = match self.hover.time {
            Some(time) => {
                let x = self.view.x_at(time, f64::from(size_px[0]));
                let slack = (POINTER_SLACK * ppp).round() as u32;
                let under = annotations::under(
                    &self.view,
                    &self.document.annotations,
                    &self.legend,
                    size_px[0],
                    x,
                    slack,
                    self.hover.channel,
                )
                .into_iter()
                .map(|a| a.data.description.as_str());
                status::annotations_text(under)
            }
            None => String::new(),
        };

        // Tiles: ask for what's missing, draw what's cached.
        self.width_px = size_px[0];
        let rec = &self.document.recording;
        let service = live(&self.service);
        let budget = service.cache().budget() / PREFETCH_SHARE;
        if let Some((generation, keys)) = self.view.request(rec, size_px[0], budget) {
            service.submit(generation, keys);
        }
        if let Some(windows) = self.view.dc_request(rec) {
            service.request_means(windows);
        }
        let visuals = ui.visuals();
        let painter = ui.painter_at(traces.union(names).union(axis_rect));
        painter.rect_filled(traces, 0.0, visuals.extreme_bg_color);
        let labels = ui.painter_at(Rect::from_x_y_ranges(full.x_range(), axis_rect.y_range()));
        let grid_color = visuals.widgets.noninteractive.bg_stroke.color;
        let label_color = visuals.weak_text_color();
        let widest = ui.fonts_mut(|f| {
            f.layout_no_wrap("00:00:00.00".into(), small.clone(), label_color)
                .size()
                .x
        });
        for tick in axis::ticks(self.view.time(), area.width, f64::from(widest + LABEL_GAP)) {
            let x = traces.left() + self.view.x_at(tick.time, area.width) as f32;
            let x = (x * ppp).round() / ppp + 0.5 / ppp;
            painter.vline(x, traces.y_range(), (1.0 / ppp, grid_color));
            painter.vline(
                x,
                axis_rect.top()..=axis_rect.top() + TICK,
                (1.0, label_color),
            );
            // Centered on the tick; at the ends, shifted inward to stay whole.
            let galley = painter.layout_no_wrap(tick.label, small.clone(), label_color);
            let width = galley.size().x;
            let left =
                (x - width / 2.0).clamp(full.left(), (full.right() - width).max(full.left()));
            labels.galley(
                pos2(left, axis_rect.top() + TICK + 1.0),
                galley,
                label_color,
            );
        }

        // Annotations behind the traces; their labels go on top, below.
        self.marks = annotations::marks(
            &self.view,
            &self.document.annotations,
            &self.legend,
            size_px[0],
            |text| {
                let width = ui.fonts_mut(|f| {
                    f.layout_no_wrap(text.to_owned(), small.clone(), Color32::PLACEHOLDER)
                        .size()
                        .x
                });
                f64::from((width + ANNOTATION_LABEL_PAD + ANNOTATION_LABEL_GAP) * ppp)
            },
        );
        let overlay = painter.with_clip_rect(traces);
        let x_of = |column: u32| traces.left() + column as f32 / ppp;
        let rect_of = |mark: &annotations::Mark| {
            let y = annotations::span_y(&self.view, mark.span, area.height);
            let top = traces.top();
            let y = Rangef::new(top + y.start as f32, top + y.end as f32);
            let x = Rangef::new(x_of(mark.columns.start), x_of(mark.columns.end));
            Rect::from_x_y_ranges(x, y)
        };
        for band in &self.marks.bands {
            let color = annotation_color(band.color, visuals).gamma_multiply(BAND_ALPHA);
            overlay.rect_filled(rect_of(band), 0.0, color);
        }
        for line in &self.marks.lines {
            overlay.rect_filled(rect_of(line), 0.0, annotation_color(line.color, visuals));
        }

        // The selection's outline, in its annotation's color.
        self.outline = self
            .selected
            .and_then(|id| self.document.annotations.get(id))
            .and_then(|a| {
                let columns = annotations::extent(&self.view, &a.data, size_px[0])?;
                let color = self.legend.entry(&a.data.description)?.color;
                let x = Rangef::new(x_of(columns.start), x_of(columns.end));
                let mut rect = Rect::from_x_y_ranges(x, traces.y_range());
                if columns.len() == 1 {
                    // A line: the outline goes around it.
                    rect = rect.expand2(vec2(OUTLINE_WIDTH, 0.0));
                }
                overlay.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(OUTLINE_WIDTH, annotation_color(color, visuals)),
                    StrokeKind::Inside,
                );
                Some(rect)
            });

        let query = query(rec, &self.view.tile_view(size_px[0]), service.cache());
        self.missing = query.missing.len();
        if self.auto_scale && query.missing.is_empty() {
            self.view
                .set_amplitudes(scale::fit(rec, &self.view, &query, size_px[0]));
            self.auto_scale = false;
        }
        let mut scene = Scene::from_view(&self.view, size_px, rgba(visuals.text_color()));
        scene.antialias = self.antialias;
        for row in &mut scene.rows {
            row.color = rgba(self.channel_color(row.channel, visuals));
            row.baseline = self.baseline(row.channel);
        }
        let frame = Frame::new(rec, &scene, &query);
        gpu.paint(&painter, traces, frame, self.serial);
        if let Some(error) = gpu.error() {
            let galley = painter.layout(
                error.to_owned(),
                body.clone(),
                visuals.error_fg_color,
                (traces.width() - 40.0).max(100.0),
            );
            let pos = traces.center() - galley.size() / 2.0;
            painter.galley(pos, galley, visuals.error_fg_color);
        }

        for label in &self.marks.labels {
            overlay.text(
                pos2(
                    traces.left() + label.x as f32 / ppp + ANNOTATION_LABEL_PAD,
                    traces.top() + 1.0,
                ),
                Align2::LEFT_TOP,
                &label.text,
                small.clone(),
                annotation_color(label.color, visuals),
            );
        }

        // Channel names, centered on their rows.
        let names_painter = painter.with_clip_rect(names);
        let height = area.height;
        for (i, &channel) in self.view.visible_rows().zip(self.view.visible_channels()) {
            let y = traces.top() + self.view.y_of_row(i as f64 + 0.5, height) as f32;
            let color = if self.hover.channel == Some(channel) && !self.view.is_bad(channel) {
                visuals.strong_text_color()
            } else {
                self.channel_color(channel, visuals)
            };
            names_painter.text(
                pos2(names.left() + NAME_PADDING, y),
                Align2::LEFT_CENTER,
                &rec.channel(channel).name,
                body.clone(),
                color,
            );
        }
    }

    /// The color of a channel's trace and name: its type's, or grayed out if it is bad.
    fn channel_color(&self, channel: ChannelId, visuals: &egui::Visuals) -> Color32 {
        if self.view.is_bad(channel) {
            bad_color(visuals)
        } else {
            type_color(
                self.document.recording.channel(channel).kind,
                &self.type_colors,
                visuals,
            )
        }
    }

    /// Dragging, scrolling and pinching over the trace region, and clicks on names. The
    /// trace area is `width_px` physical pixels wide.
    #[expect(clippy::too_many_arguments)]
    fn handle_input(
        &mut self,
        ui: &Ui,
        id: egui::Id,
        keymap: &Keymap,
        traces: Rect,
        names: Rect,
        axis_rect: Rect,
        width_px: u32,
    ) {
        let area = Area {
            width: f64::from(traces.width()),
            height: f64::from(traces.height()),
        };
        let response = ui.interact(traces, id.with("drag"), Sense::click_and_drag());
        // A press in the traces takes the keyboard from a text field (the description),
        // so keys act on the traces again: ⌫ deletes the annotation just made.
        if response.is_pointer_button_down_on() {
            ui.memory_mut(|m| m.stop_text_input());
        }
        if self.editing {
            self.edit_input(ui, &response, traces, width_px);
        } else if response.dragged() {
            self.view
                .pan_points(f64::from(response.drag_delta().x), area.width);
        }
        let names_response = ui.interact(names, id.with("names"), Sense::click());
        if names_response.clicked()
            && let Some(p) = names_response.interact_pointer_pos()
            && let Some(channel) = self
                .view
                .channel_at(f64::from(p.y - traces.top()), area.height)
        {
            self.view.toggle_bad(channel);
        }

        let region = traces.union(names).union(axis_rect);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if !pointer.is_some_and(|p| region.contains(p)) || !ui.rect_contains_pointer(region) {
            return;
        }
        let pointer_x = pointer
            .filter(|p| traces.x_range().contains(p.x))
            .map(|p| f64::from(p.x - traces.left()));
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            let gestures = match event {
                Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } => keymap.wheel(unit, delta, modifiers, traces.size()),
                Event::Zoom(factor) => vec![keymap.pinch(factor, modifiers)],
                _ => continue,
            };
            for gesture in gestures {
                self.view.gesture(gesture, area, pointer_x);
            }
        }
    }

    /// Annotation mode's gestures on the traces: a drag creates an annotation of all
    /// channels with the current description, a click selects the annotation under the
    /// pointer, the next one there on the selected one (see [`annotations::pick`]), or,
    /// away from them, deselects.
    fn edit_input(&mut self, ui: &Ui, response: &egui::Response, traces: Rect, width_px: u32) {
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        let width = f64::from(traces.width());
        let duration = self.view.duration();
        let x_of = |p: Pos2| f64::from(p.x - traces.left());
        if response.drag_started()
            && !self.description.is_empty()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.finish_creating();
            self.creating = Some(Creating {
                // The window lies within the recording.
                anchor: self.view.time_at(x_of(origin), width),
                id: None,
            });
        }
        if response.dragged()
            && let Some(creating) = &mut self.creating
            && let Some(p) = response.interact_pointer_pos()
        {
            let anchor = creating.anchor;
            let time = self.view.time_at(x_of(p), width).clamp(0.0, duration);
            let reach = self.view.x_at(time, width) - self.view.x_at(anchor, width);
            // Too short a drag makes nothing, or takes back what it made.
            let data = (reach.abs() >= f64::from(MIN_DRAG)).then(|| AnnotationData {
                onset: anchor.min(time),
                duration: (time - anchor).abs(),
                description: self.description.clone(),
                channels: Vec::new(),
                origin: Origin::Viewer,
            });
            let store = &mut self.document.annotations;
            if data.is_some() || creating.id.is_some() {
                let id = *creating.id.get_or_insert_with(|| store.new_id());
                history::set(store, id, data).expect("times within the recording are valid");
                self.legend.sync(store);
                self.legend.set_shown(&self.description, true);
            }
        }
        if response.drag_stopped() {
            self.finish_creating();
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let ppp = ui.pixels_per_point();
            let channel = self
                .view
                .channel_at(f64::from(p.y - traces.top()), f64::from(traces.height()));
            self.selected = annotations::pick(
                &self.view,
                &self.document.annotations,
                &self.legend,
                width_px,
                f64::from((p.x - traces.left()) * ppp),
                (POINTER_SLACK * ppp).round() as u32,
                channel,
                self.selected,
            );
        }
    }

    fn hover_at(&self, pointer: Option<Pos2>, traces: Rect, names: Rect) -> Hover {
        let Some(p) = pointer else {
            return Hover::default();
        };
        let height = f64::from(traces.height());
        let channel = || self.view.channel_at(f64::from(p.y - traces.top()), height);
        if traces.contains(p) {
            Hover {
                channel: channel(),
                time: Some(
                    self.view
                        .time_at(f64::from(p.x - traces.left()), f64::from(traces.width())),
                ),
            }
        } else if names.contains(p) {
            Hover {
                channel: channel(),
                time: None,
            }
        } else {
            Hover::default()
        }
    }

    /// Width of the widest channel name in `font`.
    fn names_width(&mut self, ui: &Ui, font: &FontId) -> f32 {
        if let Some((size, width)) = self.names_width
            && size == font.size
        {
            return width;
        }
        let width = ui.fonts_mut(|f| {
            self.document
                .recording
                .channels()
                .iter()
                .map(|c| {
                    f.layout_no_wrap(c.name.clone(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                })
                .fold(0.0, f32::max)
        });
        self.names_width = Some((font.size, width));
        width
    }
}

impl Drop for Viewer {
    /// Stopping the tile service joins its threads, which may wait for a build step; that
    /// happens on a thread of its own so the window never waits.
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            // If no thread starts, the closure and the service are dropped right here.
            let _ = std::thread::Builder::new()
                .name("kymo-close".into())
                .spawn(move || drop(service));
        }
    }
}

/// The viewer's tile service, borrowed on its own so other fields can be borrowed mutably
/// beside it. Only `drop` takes it.
fn live(service: &Option<TileService>) -> &TileService {
    service
        .as_ref()
        .expect("the tile service lives as long as the viewer")
}

/// An egui color as the renderer's premultiplied RGBA.
fn rgba(color: Color32) -> [f32; 4] {
    color.to_normalized_gamma_f32()
}
