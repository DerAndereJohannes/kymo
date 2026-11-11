//! Scale bars: one per channel type on screen, beside its first shown row that isn't bad,
//! as long as a round amplitude (1, 2 or 5 times a power of ten) in the unit its readings
//! are shown in.

use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::ui::readout::display_unit;
use crate::view::ViewState;

/// A channel type's scale bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleBar {
    pub kind: ChannelKind,
    /// The channel whose row it stands beside.
    pub channel: ChannelId,
    /// That row's index among the shown rows (as [`ViewState::y_of_row`] counts them).
    pub row: usize,
    /// How much it stands for, in `unit`: the largest round value up to the row's
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

/// The scale bars of the rows `view` shows: for each channel type but stim (whose rows
/// hold codes, not amplitudes), beside the first row of that type that isn't bad and whose
/// middle is on screen. A type without such a row, or without a usable amplitude, gets
/// none.
pub fn scale_bars(recording: &Recording, view: &ViewState) -> Vec<ScaleBar> {
    let rows = view.rows() as f64;
    let mut bars: Vec<ScaleBar> = Vec::new();
    for (row, &channel) in view.visible_rows().zip(view.visible_channels()) {
        let middle = row as f64 + 0.5 - view.top();
        let kind = view.kind_of(channel);
        if !(0.0..rows).contains(&middle)
            || kind == ChannelKind::Stim
            || view.is_bad(channel)
            || bars.iter().any(|b| b.kind == kind)
        {
            continue;
        }
        let (unit, factor) = display_unit(recording.channel(channel));
        let span = 2.0 * view.channel_amplitude(channel) * factor;
        let Some(value) = round_below(span) else {
            continue;
        };
        bars.push(ScaleBar {
            kind,
            channel,
            row,
            value,
            unit,
            fraction: value / span,
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
