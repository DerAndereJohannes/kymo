//! What the trace area shows and how it moves, without any GUI.
//!
//! Times are seconds from the recording start and positions are points (logical pixels)
//! within the trace area, all in f64. Every change is clamped to the recording.

use std::collections::BTreeSet;
use std::ops::Range;

use kymo_core::tile::{Direction, SignalVersion, TileKey, View, prefetch};
use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::action::{Action, Gesture};

/// Seconds shown at first and after [`Action::DefaultWindow`].
pub const DEFAULT_SPAN: f64 = 20.0;
/// Channel rows shown at first, as MNE's `n_channels`.
pub const DEFAULT_ROWS: usize = 20;
/// The shortest window, in samples of the fastest grid.
pub const MIN_SPAN_SAMPLES: f64 = 10.0;
/// Time magnification per [`Action::ZoomIn`].
pub const ZOOM_STEP: f64 = 2.0;
/// Trace height factor per [`Action::ScaleUp`]: 2^(1/4), so four steps double it.
pub const SCALE_STEP: f64 = 1.189_207_115_002_721;
/// Factor on the number of rows per [`Action::MoreChannels`].
pub const ROWS_STEP: f64 = 1.25;

/// Size of the trace area in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub width: f64,
    pub height: f64,
}

/// Default amplitude per channel spacing in SI units, as MNE's `scalings`.
fn default_amplitude(kind: ChannelKind) -> f64 {
    match kind {
        ChannelKind::Eeg => 20e-6,
        ChannelKind::Eog => 150e-6,
        ChannelKind::Ecg => 5e-3,
        ChannelKind::Emg => 1e-3,
        _ => 1.0,
    }
}

const KINDS: [ChannelKind; 6] = [
    ChannelKind::Eeg,
    ChannelKind::Eog,
    ChannelKind::Ecg,
    ChannelKind::Emg,
    ChannelKind::Stim,
    ChannelKind::Misc,
];

fn kind_index(kind: ChannelKind) -> usize {
    KINDS
        .iter()
        .position(|&k| k == kind)
        .unwrap_or(KINDS.len() - 1)
}

/// The viewer's display state for one recording.
#[derive(Clone, Debug)]
pub struct ViewState {
    duration: f64,
    min_span: f64,
    /// The rows, top to bottom.
    rows_order: Vec<ChannelId>,
    /// Per channel (by id): its type and its unit's factor to SI.
    kinds: Vec<ChannelKind>,
    to_si: Vec<Option<f64>>,
    start: f64,
    span: f64,
    /// The first row on screen; fractional while scrolling smoothly.
    top: f64,
    rows: usize,
    /// Amplitude per channel spacing, SI units, per [`KINDS`].
    amplitudes: [f64; KINDS.len()],
    dc: bool,
    bad: BTreeSet<ChannelId>,
    direction: Direction,
    generation: u64,
    requested: Option<View>,
}

impl ViewState {
    /// [`DEFAULT_SPAN`] from the start (or the whole recording if shorter), the first
    /// [`DEFAULT_ROWS`] channels, default amplitudes, DC removal off, no bad channels.
    pub fn new(recording: &Recording) -> Self {
        let channels = recording.channels();
        let rate = recording.sfreq();
        let mut state = ViewState {
            duration: recording.duration().max(0.0),
            min_span: if rate > 0.0 {
                MIN_SPAN_SAMPLES / rate
            } else {
                0.0
            },
            rows_order: (0..channels.len() as u32).map(ChannelId).collect(),
            kinds: channels.iter().map(|c| c.kind).collect(),
            to_si: channels.iter().map(|c| c.unit.to_si).collect(),
            start: 0.0,
            span: DEFAULT_SPAN,
            top: 0.0,
            rows: DEFAULT_ROWS,
            amplitudes: KINDS.map(default_amplitude),
            dc: false,
            bad: BTreeSet::new(),
            direction: Direction::Forward,
            generation: 0,
            requested: None,
        };
        state.clamp();
        state
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Seconds shown.
    pub fn time(&self) -> Range<f64> {
        self.start..self.start + self.span
    }

    pub fn start(&self) -> f64 {
        self.start
    }

    pub fn span(&self) -> f64 {
        self.span
    }

    /// The first row on screen, fractional while scrolling.
    pub fn top(&self) -> f64 {
        self.top
    }

    /// All channel rows, on screen or not.
    pub fn row_count(&self) -> usize {
        self.rows_order.len()
    }

    /// Rows that fit on screen.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Indices of the rows on screen, including partly visible ones.
    pub fn visible_rows(&self) -> Range<usize> {
        let n = self.rows_order.len();
        let first = (self.top.floor() as usize).min(n);
        let last = ((self.top + self.rows as f64).ceil() as usize).min(n);
        first..last.max(first)
    }

    /// The channels on screen, top to bottom.
    pub fn visible_channels(&self) -> &[ChannelId] {
        &self.rows_order[self.visible_rows()]
    }

    /// Amplitude per channel spacing for a channel type, in SI units (V for EEG).
    pub fn amplitude(&self, kind: ChannelKind) -> f64 {
        self.amplitudes[kind_index(kind)]
    }

    /// Amplitude per channel spacing for one channel, in its own physical unit. A unit
    /// without a known SI factor counts as SI.
    pub fn channel_amplitude(&self, channel: ChannelId) -> f64 {
        let i = channel.0 as usize;
        let kind = self.kinds.get(i).copied().unwrap_or(ChannelKind::Misc);
        let to_si = self.to_si.get(i).copied().flatten().unwrap_or(1.0);
        self.amplitude(kind) / to_si
    }

    pub fn dc_removed(&self) -> bool {
        self.dc
    }

    pub fn is_bad(&self, channel: ChannelId) -> bool {
        self.bad.contains(&channel)
    }

    pub fn bad_channels(&self) -> impl Iterator<Item = ChannelId> + '_ {
        self.bad.iter().copied()
    }

    /// Applies a view action; `under_pointer` is the channel [`Action::ToggleBad`] acts
    /// on. Returns `false` for actions outside the view (open, quit, ...).
    pub fn apply(&mut self, action: Action, under_pointer: Option<ChannelId>) -> bool {
        let rows = self.rows as f64;
        match action {
            Action::PanLeft => self.pan(-self.span / 4.0),
            Action::PanRight => self.pan(self.span / 4.0),
            Action::PageLeft => self.pan(-self.span),
            Action::PageRight => self.pan(self.span),
            Action::ToStart => self.pan(-self.duration),
            Action::ToEnd => self.pan(self.duration),
            Action::ZoomIn => self.zoom(ZOOM_STEP, 0.5),
            Action::ZoomOut => self.zoom(1.0 / ZOOM_STEP, 0.5),
            Action::DefaultWindow => {
                self.span = DEFAULT_SPAN;
                self.clamp();
            }
            Action::Fit => {
                self.start = 0.0;
                self.span = self.duration;
                self.clamp();
            }
            // Row steps land on whole rows, also after smooth scrolling.
            Action::ChannelUp => self.set_top((self.top - 1.0).ceil()),
            Action::ChannelDown => self.set_top((self.top + 1.0).floor()),
            Action::ChannelPageUp => self.set_top((self.top - rows).ceil()),
            Action::ChannelPageDown => self.set_top((self.top + rows).floor()),
            Action::FirstChannels => self.set_top(0.0),
            Action::LastChannels => self.set_top(f64::INFINITY),
            Action::MoreChannels => {
                let more = (rows * ROWS_STEP).round() as usize;
                self.set_rows(more.max(self.rows + 1));
            }
            Action::FewerChannels => {
                let fewer = (rows / ROWS_STEP).round() as usize;
                self.set_rows(fewer.min(self.rows.saturating_sub(1)));
            }
            Action::ScaleUp => self.scale(SCALE_STEP),
            Action::ScaleDown => self.scale(1.0 / SCALE_STEP),
            Action::ToggleDc => self.dc = !self.dc,
            Action::ToggleBad => {
                if let Some(channel) = under_pointer {
                    self.toggle_bad(channel);
                }
            }
            Action::Open
            | Action::Close
            | Action::Quit
            | Action::Settings
            | Action::Shortcuts
            | Action::About
            | Action::ToggleSidebar => return false,
        }
        true
    }

    /// Applies a gesture made over a trace area of size `area`, with the pointer at
    /// `pointer_x` points from its left edge (zooming centres on the area without one).
    pub fn gesture(&mut self, gesture: Gesture, area: Area, pointer_x: Option<f64>) {
        match gesture {
            Gesture::Pan { points } => self.pan_points(points, area.width),
            Gesture::ScrollRows { points } => {
                let row = self.row_height(area.height);
                if row > 0.0 {
                    self.set_top(self.top - points / row);
                }
            }
            Gesture::Zoom { factor } => {
                let anchor = match pointer_x {
                    Some(x) if area.width > 0.0 => (x / area.width).clamp(0.0, 1.0),
                    _ => 0.5,
                };
                self.zoom(factor, anchor);
            }
            Gesture::Scale { factor } => self.scale(factor),
        }
    }

    /// Moves the window by `seconds` (positive is later).
    pub fn pan(&mut self, seconds: f64) {
        if !seconds.is_finite() || seconds == 0.0 {
            return;
        }
        self.direction = if seconds > 0.0 {
            Direction::Forward
        } else {
            Direction::Backward
        };
        self.start += seconds;
        self.clamp();
    }

    /// Moves the traces right by `points` in an area `width` points wide, as dragging does.
    pub fn pan_points(&mut self, points: f64, width: f64) {
        if width > 0.0 {
            self.pan(-points / width * self.span);
        }
    }

    /// Magnifies time by `factor` (above 1 zooms in), keeping the time at `anchor` (0 is
    /// the left edge, 1 the right) in place unless the recording's edges stop it.
    pub fn zoom(&mut self, factor: f64, anchor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let anchor = anchor.clamp(0.0, 1.0);
        let at = self.start + anchor * self.span;
        self.span /= factor;
        self.clamp_span();
        self.start = at - anchor * self.span;
        self.clamp();
    }

    /// Makes traces `factor` times taller.
    pub fn scale(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            for amplitude in &mut self.amplitudes {
                *amplitude /= factor;
            }
        }
    }

    pub fn set_dc_removed(&mut self, on: bool) {
        self.dc = on;
    }

    pub fn toggle_bad(&mut self, channel: ChannelId) {
        if !self.bad.remove(&channel) && (channel.0 as usize) < self.kinds.len() {
            self.bad.insert(channel);
        }
    }

    /// Shows `rows` rows (at least 1, at most all), keeping the top row.
    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows;
        self.clamp();
    }

    /// Scrolls so that row `top` (fractional for smooth scrolling) is first on screen.
    pub fn set_top(&mut self, top: f64) {
        if !top.is_nan() {
            self.top = top;
            self.clamp();
        }
    }

    /// The time at `x` points from the left edge of an area `width` points wide.
    pub fn time_at(&self, x: f64, width: f64) -> f64 {
        self.start + x / width * self.span
    }

    /// Points from the left edge at which time `t` lies.
    pub fn x_at(&self, t: f64, width: f64) -> f64 {
        (t - self.start) / self.span * width
    }

    /// Height of one row in an area `height` points tall.
    pub fn row_height(&self, height: f64) -> f64 {
        height / self.rows as f64
    }

    /// The (fractional) row index at `y` points from the top.
    pub fn row_at(&self, y: f64, height: f64) -> f64 {
        self.top + y / self.row_height(height)
    }

    /// Points from the top of row `row`'s upper edge.
    pub fn y_of_row(&self, row: f64, height: f64) -> f64 {
        (row - self.top) * self.row_height(height)
    }

    /// The channel whose row is at `y` points from the top.
    pub fn channel_at(&self, y: f64, height: f64) -> Option<ChannelId> {
        let row = self.row_at(y, height);
        if row < 0.0 {
            return None;
        }
        self.rows_order.get(row.floor() as usize).copied()
    }

    /// What the tiles must cover for a trace area `width_px` physical pixels wide.
    pub fn tile_view(&self, width_px: u32) -> View {
        View {
            version: SignalVersion::RAW,
            time: self.time(),
            width_px,
            channels: self.visible_channels().to_vec(),
        }
    }

    /// The tiles to ask for when the tile view changed since the last call: a new
    /// generation and the keys from [`prefetch`], looking ahead in the direction of the
    /// last pan. `None` while the view stays the same.
    pub fn request(
        &mut self,
        recording: &Recording,
        width_px: u32,
        max_bytes: usize,
    ) -> Option<(u64, Vec<TileKey>)> {
        let view = self.tile_view(width_px);
        if self.requested.as_ref() == Some(&view) {
            return None;
        }
        let keys = prefetch(recording, &view, self.direction, max_bytes);
        self.requested = Some(view);
        self.generation += 1;
        Some((self.generation, keys))
    }

    fn clamp_span(&mut self) {
        let shortest = self.min_span.min(self.duration);
        self.span = if self.span.is_nan() {
            self.duration
        } else {
            self.span.clamp(shortest, self.duration)
        };
    }

    fn clamp(&mut self) {
        self.clamp_span();
        let latest = self.duration - self.span;
        self.start = if self.start.is_nan() {
            0.0
        } else {
            self.start.clamp(0.0, latest.max(0.0))
        };
        let n = self.rows_order.len();
        self.rows = self.rows.clamp(1, n.max(1));
        let lowest = n.saturating_sub(self.rows) as f64;
        self.top = self.top.clamp(0.0, lowest);
    }
}
