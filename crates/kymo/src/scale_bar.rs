//! Scale bars: one per channel type on screen, as long as a round amplitude (1, 2 or 5
//! times a power of ten) in the unit its readings are shown in. Each keeps to the top of
//! the visible part of its type's first block of rows, as a list's section header does:
//! scrolling within the block leaves it where it is, and the block's end pushes it out.

use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::ui::readout::display_unit;
use crate::view::ViewState;

/// A channel type's scale bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleBar {
    pub kind: ChannelKind,
    /// The first channel on screen of its block, whose unit it is in.
    pub channel: ChannelId,
    /// Where its middle is, in rows from the top of the trace area (0.5 is the middle of
    /// the top row).
    pub middle: f64,
    /// How much it stands for, in `unit`: the largest round value up to the rows'
    /// amplitude span (±amplitude spans a row).
    pub value: f64,
    /// The unit its readings are shown in ("µV", "fT"); empty if the channel has none.
    pub unit: String,
    /// Its length as a fraction of the row height, in (0.4, 1].
    pub fraction: f64,
}

impl ScaleBar {
    /// Its label: the value with as many decimals as it needs, then the unit ("20 µV",
    /// "0.5 mV", "200").
    pub fn label(&self) -> String {
        let value = format_value(self.value);
        if self.unit.is_empty() {
            value
        } else {
            format!("{value} {}", self.unit)
        }
    }
}

/// The scale bars of the rows `view` shows, one for each channel type but stim (whose rows
/// hold codes, not amplitudes). A type's bar stands beside the first block of consecutive
/// rows of that type on screen where it fits whole: in the middle of the block's top row
/// on screen, or of its top row slot while the block goes on above the screen, and no
/// lower in the block than the middle of its last row, so a block leaving at the top takes
/// the bar along. A type without such a block, or without a usable amplitude, gets none.
pub fn scale_bars(recording: &Recording, view: &ViewState) -> Vec<ScaleBar> {
    let top = view.top();
    let rows = view.rows() as f64;
    let first = view.visible_rows().start;
    let channels = view.visible_channels();
    let mut bars: Vec<ScaleBar> = Vec::new();
    let mut start = 0;
    while start < channels.len() {
        let kind = view.kind_of(channels[start]);
        let end = start
            + channels[start..]
                .iter()
                .take_while(|&&c| view.kind_of(c) == kind)
                .count();
        let block = ((first + start) as f64, (first + end) as f64);
        let channel = channels[start];
        start = end;
        if kind == ChannelKind::Stim || bars.iter().any(|b| b.kind == kind) {
            continue;
        }
        let (unit, factor) = display_unit(recording.channel(channel));
        let span = 2.0 * view.channel_amplitude(channel) * factor;
        let Some(value) = round_below(span) else {
            continue;
        };
        let fraction = value / span;
        let middle = (block.0.max(top) - top + 0.5).min(block.1 - top - 0.5);
        if middle < fraction / 2.0 || middle + fraction / 2.0 > rows {
            continue;
        }
        bars.push(ScaleBar {
            kind,
            channel,
            middle,
            value,
            unit,
            fraction,
        });
    }
    bars
}

/// The largest of 1, 2 or 5 times a power of ten up to `limit`; `None` unless `limit` is
/// positive and finite.
pub fn round_below(limit: f64) -> Option<f64> {
    if !(limit.is_finite() && limit > 0.0) {
        return None;
    }
    let power = 10f64.powf(limit.log10().floor());
    // log10 may round across a power of ten; the steps below correct for it.
    [10.0, 5.0, 2.0, 1.0, 0.5]
        .into_iter()
        .map(|step| step * power)
        .find(|&v| v <= limit * (1.0 + 1e-12))
}

/// `value`, a round value as [`round_below`] gives, with the decimals it needs and no
/// more: "20", "0.5", "0.002".
fn format_value(value: f64) -> String {
    // Rounded up with a little slack, as log10 of a power of ten can miss it slightly.
    let decimals = (-value.log10() - 1e-9).ceil().max(0.0) as usize;
    format!("{value:.decimals$}")
}
