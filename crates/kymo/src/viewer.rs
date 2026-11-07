//! An open recording on screen: the sidebar, the trace area with channel names, time axis
//! and scrollbars, and the status line.

use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui::{
    self, Align, Align2, Color32, Event, FontId, Pos2, Rect, Sense, TextStyle, Ui, pos2,
};
use kymo_core::ChannelId;
use kymo_core::service::TileService;
use kymo_core::tile::query;

use crate::action::Action;
use crate::document::{Document, Summary};
use crate::keymap::Keymap;
use crate::render::{Frame, Scene};
use crate::ui::axis::{self, Clock};
use crate::ui::gpu::Gpu;
use crate::ui::header::{header_panel, within_width};
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
/// The share of the tile cache that tiles beyond the screen may take.
const PREFETCH_SHARE: usize = 2;

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
}

impl Viewer {
    pub fn new(document: Document, service: TileService) -> Viewer {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        Viewer {
            summary: document.summary(),
            view: ViewState::new(&document.recording),
            document,
            service: Some(service),
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            hover: Hover::default(),
            status: StatusLine::default(),
            width_px: 0,
            names_width: None,
            layout: None,
            missing: 0,
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

    pub fn status(&self) -> &StatusLine {
        &self.status
    }

    /// Whether the last frame had every tile it wanted, none drawn from a coarser one.
    pub fn complete(&self) -> bool {
        self.layout.is_some() && self.missing == 0
    }

    /// The layout of the last frame, once there was one with room for traces.
    pub fn layout(&self) -> Option<Layout> {
        self.layout
    }

    fn service(&self) -> &TileService {
        // Only taken in `drop`.
        self.service
            .as_ref()
            .expect("the tile service lives as long as the viewer")
    }

    /// Applies a view action; [`Action::ToggleBad`] acts on the channel under the pointer.
    pub fn apply(&mut self, action: Action) -> bool {
        self.view.apply(action, self.hover.channel)
    }

    /// Shows the recording in `ui`. Returns whether the sidebar's Open button was clicked.
    pub fn show(&mut self, ui: &mut Ui, gpu: &Gpu, keymap: &Keymap) -> bool {
        let mut open_clicked = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status.left);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(&self.status.right);
                });
            });
        });
        egui::Panel::left("header")
            .resizable(true)
            .default_size(300.0)
            .min_size(SIDEBAR_MIN_WIDTH)
            .max_size((ui.available_width() * SIDEBAR_MAX_FRACTION).max(SIDEBAR_MIN_WIDTH))
            .show(ui, |ui| {
                open_clicked =
                    within_width(ui, |ui| header_panel(ui, &self.document, &self.summary));
            });
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
                    let reading = readout::read(rec, service.cache(), &view, id, time)
                        .map(|r| status::reading_text(r, &info.unit.label));
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

        self.handle_input(ui, id, keymap, traces, names, axis_rect, area);
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

        // Tiles: ask for what's missing, draw what's cached.
        self.width_px = size_px[0];
        let rec = &self.document.recording;
        let service = self
            .service
            .as_ref()
            .expect("the tile service lives as long as the viewer");
        let budget = service.cache().budget() / PREFETCH_SHARE;
        if let Some((generation, keys)) = self.view.request(rec, size_px[0], budget) {
            service.submit(generation, keys);
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
            // Centred on the tick; at the ends, shifted inward to stay whole.
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

        let trace_color = rgba(visuals.text_color());
        let bad_color = rgba(visuals.weak_text_color().gamma_multiply(0.6));
        let mut scene = Scene::from_view(&self.view, size_px, trace_color);
        for row in &mut scene.rows {
            if self.view.is_bad(row.channel) {
                row.color = bad_color;
            }
        }
        let query = query(rec, &self.view.tile_view(size_px[0]), service.cache());
        self.missing = query.missing.len();
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

        // Channel names, centred on their rows.
        let names_painter = painter.with_clip_rect(names);
        let height = area.height;
        for (i, &channel) in self.view.visible_rows().zip(self.view.visible_channels()) {
            let y = traces.top() + self.view.y_of_row(i as f64 + 0.5, height) as f32;
            let color = if self.view.is_bad(channel) {
                visuals.weak_text_color().gamma_multiply(0.6)
            } else if self.hover.channel == Some(channel) {
                visuals.strong_text_color()
            } else {
                visuals.text_color()
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

    /// Dragging, scrolling and pinching over the trace region, and clicks on names.
    #[expect(clippy::too_many_arguments)]
    fn handle_input(
        &mut self,
        ui: &Ui,
        id: egui::Id,
        keymap: &Keymap,
        traces: Rect,
        names: Rect,
        axis_rect: Rect,
        area: Area,
    ) {
        let response = ui.interact(traces, id.with("drag"), Sense::click_and_drag());
        if response.dragged() {
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

/// An egui colour as the renderer's premultiplied RGBA.
fn rgba(color: Color32) -> [f32; 4] {
    color.to_normalized_gamma_f32()
}
