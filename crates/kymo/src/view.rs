//! What the trace area shows and how it moves, without any GUI.
//!
//! Times are seconds from the recording start and positions are points (logical pixels)
//! within the trace area, all in f64. Every change is clamped to the recording.

use std::collections::BTreeSet;
use std::ops::Range;

use kymo_core::tile::{Direction, SignalVersion, TileKey, View, prefetch, samples_in};
use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::action::{Action, Gesture};

/// Seconds shown at first and after [`Action::DefaultWindow`], unless set otherwise.
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

/// Default amplitude per channel spacing in SI units, as MNE's `scalings` for `raw.plot`.
fn default_amplitude(kind: ChannelKind) -> f64 {
    match kind {
        ChannelKind::Eeg => 20e-6,
        ChannelKind::Eog => 150e-6,
        ChannelKind::Ecg => 5e-4,
        ChannelKind::Emg => 1e-3,
        ChannelKind::Mag | ChannelKind::RefMeg => 1e-12,
        ChannelKind::Grad => 4e-11,
        ChannelKind::Seeg | ChannelKind::Ecog | ChannelKind::Dbs => 1e-4,
        ChannelKind::Bio => 1e-6,
        _ => 1.0,
    }
}

/// The channel types, in the order the sidebar lists them: brain signals, other body
/// signals, then MEG references, stim and misc.
pub const KINDS: [ChannelKind; 14] = [
    ChannelKind::Mag,
    ChannelKind::Grad,
    ChannelKind::Eeg,
    ChannelKind::Seeg,
    ChannelKind::Ecog,
    ChannelKind::Dbs,
    ChannelKind::Eog,
    ChannelKind::Ecg,
    ChannelKind::Emg,
    ChannelKind::Resp,
    ChannelKind::Bio,
    ChannelKind::RefMeg,
    ChannelKind::Stim,
    ChannelKind::Misc,
];

/// The index of a channel type in [`KINDS`]; types not listed there count as `Misc`.
pub fn kind_index(kind: ChannelKind) -> usize {
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
    /// The rows, top to bottom: the shown channels in file order.
    rows_order: Vec<ChannelId>,
    /// Per channel (by id): whether it is hidden.
    hidden: Vec<bool>,
    /// Per channel (by id): its type and its unit's factor to SI.
    kinds: Vec<ChannelKind>,
    to_si: Vec<Option<f64>>,
    start: f64,
    span: f64,
    /// Seconds shown at first and after [`Action::DefaultWindow`].
    default_span: f64,
    /// The first row on screen; fractional while scrolling smoothly.
    top: f64,
    /// Rows on screen as last chosen; fewer while fewer channels are shown.
    rows: usize,
    /// Amplitude per channel spacing, SI units, per [`KINDS`].
    amplitudes: [f64; KINDS.len()],
    dc: bool,
    bad: BTreeSet<ChannelId>,
    direction: Direction,
    generation: u64,
    requested: Option<View>,
    /// The DC windows last asked for, while DC removal is on.
    dc_requested: Option<Vec<(ChannelId, Range<u64>)>>,
}

impl ViewState {
    /// [`DEFAULT_SPAN`] from the start (or the whole recording if shorter), the first
    /// [`DEFAULT_ROWS`] channels, default amplitudes, DC removal off, no bad channels.
    pub fn new(recording: &Recording) -> Self {
        Self::with_default_span(recording, DEFAULT_SPAN)
    }

    /// As [`new`](Self::new), showing `seconds` at first and after
    /// [`Action::DefaultWindow`].
    pub fn with_default_span(recording: &Recording, seconds: f64) -> Self {
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
            hidden: vec![false; channels.len()],
            kinds: channels.iter().map(|c| c.kind).collect(),
            to_si: channels.iter().map(|c| c.unit.to_si).collect(),
            start: 0.0,
            span: DEFAULT_SPAN,
            default_span: DEFAULT_SPAN,
            top: 0.0,
            rows: DEFAULT_ROWS,
            amplitudes: KINDS.map(default_amplitude),
            dc: false,
            bad: BTreeSet::new(),
            direction: Direction::Forward,
            generation: 0,
            requested: None,
            dc_requested: None,
        };
        state.set_default_span(seconds);
        state.span = state.default_span;
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

    /// All channel rows, on screen or not: the shown channels.
    pub fn row_count(&self) -> usize {
        self.rows_order.len()
    }

    /// Rows that fit on screen: as many as last chosen, at most all, at least 1.
    pub fn rows(&self) -> usize {
        self.rows.min(self.rows_order.len()).max(1)
    }

    /// Indices of the rows on screen, including partly visible ones.
    pub fn visible_rows(&self) -> Range<usize> {
        let n = self.rows_order.len();
        let first = (self.top.floor() as usize).min(n);
        let last = ((self.top + self.rows() as f64).ceil() as usize).min(n);
        first..last.max(first)
    }

    /// The row a channel is shown in, `None` if it is hidden.
    pub fn row_of(&self, channel: ChannelId) -> Option<usize> {
        // Rows are in file order.
        self.rows_order.binary_search(&channel).ok()
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
        self.amplitude(self.kind_of(channel)) / self.to_si(channel)
    }

    /// The factor from a channel's physical unit to SI; 1 for a unit without a known one.
    pub fn to_si(&self, channel: ChannelId) -> f64 {
        let i = channel.0 as usize;
        self.to_si.get(i).copied().flatten().unwrap_or(1.0)
    }

    /// Sets the amplitude per channel spacing of each type in [`KINDS`] order that has one
    /// (SI units); the others, and values that aren't positive and finite, stay as they are.
    pub fn set_amplitudes(&mut self, amplitudes: [Option<f64>; KINDS.len()]) {
        for (amplitude, new) in self.amplitudes.iter_mut().zip(amplitudes) {
            if let Some(new) = new.filter(|a| a.is_finite() && *a > 0.0) {
                *amplitude = new;
            }
        }
    }

    /// The channel's type (`Misc` for a channel not in the recording).
    pub fn kind_of(&self, channel: ChannelId) -> ChannelKind {
        self.kinds
            .get(channel.0 as usize)
            .copied()
            .unwrap_or(ChannelKind::Misc)
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

    /// Whether a channel has a row (on screen or not); `false` for unknown channels.
    pub fn is_shown(&self, channel: ChannelId) -> bool {
        self.hidden.get(channel.0 as usize) == Some(&false)
    }

    /// How many channels of a type are shown, and how many there are.
    pub fn kind_shown(&self, kind: ChannelKind) -> (usize, usize) {
        self.kinds
            .iter()
            .zip(&self.hidden)
            .filter(|&(&k, _)| k == kind)
            .fold((0, 0), |(shown, all), (_, &hidden)| {
                (shown + usize::from(!hidden), all + 1)
            })
    }

    /// Applies a view action. Returns `false` for actions outside the view (open, quit,
    /// ...).
    pub fn apply(&mut self, action: Action) -> bool {
        let rows = self.rows() as f64;
        match action {
            Action::PanLeft => self.pan(-self.span / 4.0),
            Action::PanRight => self.pan(self.span / 4.0),
            Action::PageLeft => self.pan(-self.span),
            Action::PageRight => self.pan(self.span),
            Action::ToStart => self.pan(-self.duration),
            Action::ToEnd => self.pan(self.duration),
            // From the window's start, as Default Window keeps it, so zooming in and out
            // returns to the same window unless the end of the recording moved it.
            Action::ZoomIn => self.zoom(ZOOM_STEP, 0.0),
            Action::ZoomOut => self.zoom(1.0 / ZOOM_STEP, 0.0),
            Action::DefaultWindow => {
                self.span = self.default_span;
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
                self.set_rows(more.max(self.rows() + 1));
            }
            Action::FewerChannels => {
                let fewer = (rows / ROWS_STEP).round() as usize;
                self.set_rows(fewer.min(self.rows().saturating_sub(1)));
            }
            Action::ScaleUp => self.scale(SCALE_STEP),
            Action::ScaleDown => self.scale(1.0 / SCALE_STEP),
            Action::DefaultScale => self.amplitudes = KINDS.map(default_amplitude),
            Action::ToggleDc => self.dc = !self.dc,
            Action::Open
            | Action::Close
            | Action::Quit
            | Action::Settings
            | Action::Shortcuts
            | Action::About
            | Action::ToggleSidebar
            | Action::ToggleAntialias
            | Action::AutoScale
            | Action::StimEveryChange
            | Action::StimFromZero
            | Action::StimIncreasing
            | Action::ToggleInitialCode
            | Action::ToggleEditing
            | Action::Undo
            | Action::Redo
            | Action::DeleteAnnotation
            | Action::MergeAnnotations
            | Action::Deselect
            | Action::LoadAnnotations
            | Action::SaveAnnotations
            | Action::ExportShownAnnotations => return false,
        }
        true
    }

    /// Applies a gesture made over a trace area of size `area`, with the pointer at
    /// `pointer_x` points from its left edge (zooming centers on the area without one).
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

    /// Seconds [`Action::DefaultWindow`] shows from now on; the window shown now stays.
    /// Values that aren't positive and finite are ignored.
    pub fn set_default_span(&mut self, seconds: f64) {
        if seconds.is_finite() && seconds > 0.0 {
            self.default_span = seconds;
        }
    }

    pub fn default_span(&self) -> f64 {
        self.default_span
    }

    pub fn toggle_bad(&mut self, channel: ChannelId) {
        if !self.bad.remove(&channel) && (channel.0 as usize) < self.kinds.len() {
            self.bad.insert(channel);
        }
    }

    /// Shows `rows` rows (at least 1, at most all), keeping the top row. Asking for all
    /// rows or more keeps a larger number chosen before, for when hidden channels return.
    pub fn set_rows(&mut self, rows: usize) {
        let n = self.rows_order.len();
        self.rows = if rows >= n { self.rows.max(n) } else { rows };
        self.clamp();
    }

    /// Shows or hides a channel's row. The first row on screen stays first while it is
    /// shown; the rows on screen stay as many as chosen while there are enough.
    pub fn set_shown(&mut self, channel: ChannelId, shown: bool) {
        if let Some(hidden) = self.hidden.get_mut(channel.0 as usize) {
            *hidden = !shown;
            self.rebuild_rows();
        }
    }

    /// Shows or hides every channel of a type, as [`set_shown`](Self::set_shown).
    pub fn set_kind_shown(&mut self, kind: ChannelKind, shown: bool) {
        for (hidden, &k) in self.hidden.iter_mut().zip(&self.kinds) {
            if k == kind {
                *hidden = !shown;
            }
        }
        self.rebuild_rows();
    }

    /// Scrolls so that a shown channel's row is first on screen, or as close to it as the
    /// end of the list allows. Returns `false` for a hidden channel.
    pub fn scroll_to(&mut self, channel: ChannelId) -> bool {
        match self.row_of(channel) {
            Some(row) => {
                self.set_top(row as f64);
                true
            }
            None => false,
        }
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
        height / self.rows() as f64
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

    /// The windows to ask for DC means over when they changed since the last call: each
    /// visible channel's samples in view, top to bottom, except stim channels, which DC
    /// removal leaves alone. `None` while DC removal is off or nothing changed.
    pub fn dc_request(&mut self, recording: &Recording) -> Option<Vec<(ChannelId, Range<u64>)>> {
        if !self.dc {
            self.dc_requested = None;
            return None;
        }
        let time = self.time();
        let windows: Vec<_> = self
            .visible_channels()
            .iter()
            .filter(|&&c| self.kind_of(c) != ChannelKind::Stim)
            .map(|&c| {
                let grid = recording.grid(recording.channel(c).grid);
                (c, samples_in(grid, &time))
            })
            .collect();
        if self.dc_requested.as_ref() == Some(&windows) {
            return None;
        }
        self.dc_requested = Some(windows.clone());
        Some(windows)
    }

    /// The rows after a change of what is shown, keeping the first channel on screen (or
    /// the next shown one after it) first.
    fn rebuild_rows(&mut self) {
        let row = self.top.floor();
        let anchor = self.rows_order.get(row as usize).copied();
        self.rows_order = (0..self.hidden.len() as u32)
            .map(ChannelId)
            .filter(|&c| self.is_shown(c))
            .collect();
        if let Some(anchor) = anchor {
            let top = match self.rows_order.binary_search(&anchor) {
                Ok(i) => i as f64 + (self.top - row),
                Err(i) => i as f64,
            };
            self.top = top;
        }
        self.clamp();
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
        self.rows = self.rows.max(1);
        let lowest = self.rows_order.len().saturating_sub(self.rows()) as f64;
        self.top = self.top.clamp(0.0, lowest);
    }
}
