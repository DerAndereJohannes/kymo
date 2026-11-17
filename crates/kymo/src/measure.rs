//! Measuring by ⇧-dragging the traces: the interval between two times, snapped to the
//! samples of the channel the drag started on, and that channel's readings at both ends.
//! A measurement is display state: never saved, gone with the recording.

use kymo_core::{ChannelId, Grid, Recording};

use crate::ui::readout::Reading;
use crate::ui::status::{format_value, reading_text, time_decimals};

/// What an end without a reading shows (off screen, not cached yet, in a gap).
pub const NO_READING: &str = "\u{2013}";

/// A measurement from the time a ⇧-drag pressed at to the time it reached (in either
/// order), on the grid of `channel`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub channel: ChannelId,
    pub press: f64,
    pub reach: f64,
}

/// A measurement's ends as measured: on a grid with sample times, the samples nearest to
/// its times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    /// The earlier end's time and the later one's, in seconds from the start.
    pub start: f64,
    pub end: f64,
    /// `end - start`: on a regular run, the samples between the ends over its rate, exact
    /// to the sample.
    pub dt: f64,
    /// The samples between the ends (the later's index less the earlier's); `None` on an
    /// irregular grid, whose ends are the pointer's times.
    pub samples: Option<u64>,
    /// The grid's rate (an irregular grid's average), for the time's decimals.
    pub rate: f64,
}

impl Measurement {
    pub fn span(&self, recording: &Recording) -> Span {
        self.span_on(recording.grid(recording.channel(self.channel).grid))
    }

    /// [`span`](Self::span) on `grid`, its channel's.
    pub fn span_on(&self, grid: &Grid) -> Span {
        let (a, b) = (self.press.min(self.reach), self.press.max(self.reach));
        if grid.times().is_some() || grid.is_empty() {
            return Span {
                start: a,
                end: b,
                dt: b - a,
                samples: None,
                rate: grid.rate(),
            };
        }
        let (first, last) = (nearest_sample(grid, a), nearest_sample(grid, b));
        let samples = last - first;
        let dt = match (grid.regular_run(first), grid.regular_run(last)) {
            (Some(run), Some(other)) if run == other => samples as f64 / run.rate,
            // Across pieces of a segmented grid, whose break isn't whole samples.
            _ => grid.time_of(last) - grid.time_of(first),
        };
        Span {
            start: grid.time_of(first),
            end: grid.time_of(last),
            dt,
            samples: Some(samples),
            rate: grid.rate(),
        }
    }
}

/// The sample of a non-empty `grid` nearest to `time`, its first or last beyond them.
pub fn nearest_sample(grid: &Grid, time: f64) -> u64 {
    let last = grid.len() - 1;
    let s = grid.sample_at(time).min(last);
    let next = (s + 1).min(last);
    if (grid.time_of(next) - time).abs() < (time - grid.time_of(s)).abs() {
        next
    } else {
        s
    }
}

/// "Δt 1.250 s · 320 samples · 0.8000 Hz": the interval, to the grid's sample at its
/// rate's decimals; its samples where the grid has them; its frequency unless it is 0.
pub fn interval_text(span: &Span) -> String {
    let decimals = time_decimals(span.rate);
    let mut parts = vec![format!("\u{394}t {:.decimals$} s", span.dt)];
    match span.samples {
        Some(1) => parts.push("1 sample".to_owned()),
        Some(n) => parts.push(format!("{n} samples")),
        None => {}
    }
    if span.dt > 0.0 {
        parts.push(format_value(1.0 / span.dt, "Hz"));
    }
    parts.join(" \u{b7} ")
}

/// "Fz: 12.30 µV → −4.100 µV · Δ −16.40 µV": a channel's readings at the earlier end and
/// the later, in `unit`, and their difference where both are values (an envelope's has
/// none).
pub fn values_text(name: &str, unit: &str, start: Option<Reading>, end: Option<Reading>) -> String {
    let text = |r: Option<Reading>| r.map_or(NO_READING.to_owned(), |r| reading_text(r, unit));
    let mut out = format!("{name}: {} \u{2192} {}", text(start), text(end));
    if let (Some(Reading::Value(a)), Some(Reading::Value(b))) = (start, end) {
        out += " \u{b7} \u{394} ";
        out += &format_value(b - a, unit);
    }
    out
}
