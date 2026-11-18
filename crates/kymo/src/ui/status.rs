//! The status line: its texts (build progress and the reading under the pointer), and the
//! toggle chips at its right for what changes the traces.

use std::collections::HashMap;

use eframe::egui::{Button, Color32, Response, RichText, TextStyle, Ui, vec2};

use kymo_core::service::BuildProgress;

use super::axis::{Clock, decimals, format_time};
use super::readout::Reading;

/// A toggle chip: a pill with a short label, filled in the selection color while `on`,
/// outlined while off, announced as a toggle to screen readers. As tall as egui's other
/// controls, the window range beside it included, so the line keeps its height.
pub fn chip(ui: &mut Ui, on: bool, label: &str) -> Response {
    let height = ui.spacing().interact_size.y;
    // As wide as a pill twice the small text's height, whatever the label.
    let width = 2.0 * (ui.text_style_height(&TextStyle::Small) + 4.0);
    let mut button = Button::new(RichText::new(label).small())
        .selected(on)
        .corner_radius(height / 2.0)
        .min_size(vec2(width, height));
    if !on {
        let outline = ui.visuals().widgets.noninteractive.bg_stroke;
        button = button.fill(Color32::TRANSPARENT).stroke(outline);
    }
    // Wider padding than buttons have, as befits a pill.
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(width / 5.0, 0.0);
        ui.add(button)
    })
    .inner
}

/// Significant digits of values under the pointer.
const DIGITS: i32 = 4;

/// The pyramid build's state, or `None` once it is complete.
pub fn build_text(progress: &BuildProgress) -> Option<String> {
    if let Some(error) = &progress.error {
        return Some(format!("Overview stopped: {error}"));
    }
    if progress.complete {
        return None;
    }
    // Floored, so 100% only shows when complete.
    let percent = (progress.fraction() * 100.0).floor().clamp(0.0, 99.0);
    Some(format!("Building overview {percent:.0}%"))
}

/// `value` with [`DIGITS`] significant digits (whole numbers in full), and its unit.
pub fn format_value(value: f64, unit: &str) -> String {
    format_significant(value, DIGITS, unit)
}

/// `value` with `digits` significant digits (whole numbers in full), and its unit.
pub fn format_significant(value: f64, digits: i32, unit: &str) -> String {
    let number = format_number(value, digits);
    if unit.is_empty() {
        number
    } else {
        format!("{number} {unit}")
    }
}

fn format_number(value: f64, digits: i32) -> String {
    if !value.is_finite() {
        return format!("{value}");
    }
    let decimals = if value == 0.0 {
        0
    } else {
        (digits - 1 - value.abs().log10().floor() as i32).clamp(0, 12) as usize
    };
    let text = format!("{:.decimals$}", value.abs());
    // A rounded zero has no sign.
    if value < 0.0 && text.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        format!("−{text}")
    } else {
        text
    }
}

/// A reading as text: "−12.34 µV", or "−20.1 … 15.27 µV" for an envelope.
pub fn reading_text(reading: Reading, unit: &str) -> String {
    match reading {
        Reading::Value(v) => format_value(v, unit),
        Reading::Range { min, max } => format_value(min, "") + " … " + &format_value(max, unit),
    }
}

/// Decimals of a time readout at sampling rate `rate`: enough to tell samples apart.
pub fn time_decimals(rate: f64) -> usize {
    if rate > 1.0 && rate.is_finite() {
        (rate.log10().ceil() as usize).min(6)
    } else {
        0
    }
}

/// Decimals of the window's start and end in the status line, for a window `span` seconds
/// long: tenths for 20 s, none from 100 s up, hundredths below 10 s.
pub fn range_decimals(span: f64) -> usize {
    decimals(span / 100.0)
}

/// The window from `start` to `end` on `clock`: "0:12.5 to 0:32.5".
pub fn range_text(start: f64, end: f64, decimals: usize, clock: Clock) -> String {
    format!(
        "{} to {}",
        format_time(start, decimals, clock),
        format_time(end, decimals, clock)
    )
}

/// "12.345 s · Fp1 −12.34 µV": the time under the pointer, and the channel's reading
/// there if there is one.
pub fn pointer_text(
    time: f64,
    decimals: usize,
    clock: Clock,
    channel: Option<(&str, Option<String>)>,
) -> String {
    let mut text = format_time(time, decimals, clock);
    if let Some((name, reading)) = channel {
        text += " · ";
        text += name;
        if let Some(reading) = reading {
            text += " ";
            text += &reading;
        }
    }
    text
}

/// The selected annotation: "BAD at 0:12.345 for 1.500 s" ("for 1:35.000" from a minute
/// up), or "spike at 0:12.345" for an event.
pub fn selection_text(
    description: &str,
    onset: f64,
    duration: f64,
    decimals: usize,
    clock: Clock,
) -> String {
    let start = format_time(onset, decimals, clock);
    if duration > 0.0 {
        // A length, on its own clock rather than the recording's.
        let length = format_time(duration, decimals, Clock::for_end(duration));
        format!("{description} at {start} for {length}")
    } else {
        format!("{description} at {start}")
    }
}

/// Descriptions named in [`annotations_text`] before the rest are counted.
const DESCRIPTIONS: usize = 3;

/// The annotations under the pointer by description, in order of first appearance, with a
/// count if there are several: "spike ×3, sleep stage N2, +2 more".
pub fn annotations_text<'a>(descriptions: impl IntoIterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut index = HashMap::new();
    for description in descriptions {
        let i = *index.entry(description).or_insert_with(|| {
            counts.push((description, 0));
            counts.len() - 1
        });
        counts[i].1 += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .take(DESCRIPTIONS)
        .map(|&(d, n)| {
            if n > 1 {
                format!("{d} ×{n}")
            } else {
                d.to_owned()
            }
        })
        .collect();
    if counts.len() > DESCRIPTIONS {
        parts.push(format!("+{} more", counts.len() - DESCRIPTIONS));
    }
    parts.join(", ")
}
