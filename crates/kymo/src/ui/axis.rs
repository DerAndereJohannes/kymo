//! The time axis: tick steps that leave room for their labels, and how times are written.
//!
//! Times are seconds from the recording start. A view ending before a minute writes them as
//! seconds ("12.5 s"); longer ones as minutes or hours ("2:30", "1:02:03.5"). The decimals
//! follow the tick step, so neighboring labels always differ.

use std::ops::Range;

/// Steps from one second up; shorter steps are 1, 2 or 5 × 10ᵏ s, longer ones whole days
/// times 1, 2 or 5 × 10ᵏ.
const STEPS: [f64; 18] = [
    1.0, 2.0, 5.0, 10.0, 15.0, 30.0, // seconds
    60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, // minutes
    3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0, // hours
];

/// The most ticks [`ticks`] returns, whatever the input.
pub const MAX_TICKS: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    /// Seconds from the recording start.
    pub time: f64,
    pub label: String,
}

/// How a view writes its times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// "12.5 s"
    Seconds,
    /// "2:30.5"
    Minutes,
    /// "1:02:03.5"
    Hours,
}

impl Clock {
    /// The clock for a view (or recording) ending at `end` seconds.
    pub fn for_end(end: f64) -> Clock {
        if end < 60.0 {
            Clock::Seconds
        } else if end < 3600.0 {
            Clock::Minutes
        } else {
            Clock::Hours
        }
    }
}

/// The smallest round step that puts ticks at least `min_spacing` apart on an axis `width`
/// long showing `span` seconds; `None` if there is nothing to divide.
pub fn step(span: f64, width: f64, min_spacing: f64) -> Option<f64> {
    if !(span > 0.0 && width > 0.0 && span.is_finite() && width.is_finite()) {
        return None;
    }
    let least = span * min_spacing.max(0.0) / width;
    if !least.is_finite() {
        return None;
    }
    let one_two_five = |least: f64, unit: f64| {
        let decade = 10f64.powf((least / unit).log10().floor()) * unit;
        [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| m * decade)
            .find(|&s| s >= least)
    };
    if least <= 0.0 {
        return None;
    }
    if least < 1.0 {
        return one_two_five(least, 1.0).map(|s| s.min(1.0));
    }
    STEPS
        .into_iter()
        .find(|&s| s >= least)
        .or_else(|| one_two_five(least, 86400.0))
}

/// Decimals that write every multiple of `step` exactly: none from a second up.
pub fn decimals(step: f64) -> usize {
    if step >= 1.0 || step.is_nan() || step <= 0.0 {
        0
    } else {
        // 0.1 → 1, 0.2 → 1, 0.05 → 2; the slack absorbs log10's rounding at powers of ten.
        (-step.log10() - 1e-9).ceil() as usize
    }
}

/// Ticks for the times `time` on an axis `width` long, at least `min_spacing` apart.
pub fn ticks(time: Range<f64>, width: f64, min_spacing: f64) -> Vec<Tick> {
    let span = time.end - time.start;
    let Some(step) = step(span, width, min_spacing) else {
        return Vec::new();
    };
    let clock = Clock::for_end(time.end);
    let decimals = decimals(step);
    let first = (time.start / step).ceil();
    let last = (time.end / step).floor();
    if !(first.is_finite() && last.is_finite()) || last < first {
        return Vec::new();
    }
    let count = ((last - first) as usize + 1).min(MAX_TICKS);
    (0..count)
        .map(|k| {
            let time = (first + k as f64) * step;
            Tick {
                time,
                label: format_time(time, decimals, clock),
            }
        })
        .collect()
}

/// `t` seconds written with `decimals` decimals on `clock`.
pub fn format_time(t: f64, decimals: usize, clock: Clock) -> String {
    if !t.is_finite() {
        return format!("{t}");
    }
    let sign = if t < 0.0 { "−" } else { "" };
    let decimals = decimals.min(9);
    let scale = 10u64.pow(decimals as u32);
    // Rounded once, in whole units of the last decimal, so 59.99 never shows as "0:60".
    let units = (t.abs() * scale as f64).round() as u64;
    let (whole, fraction) = (units / scale, units % scale);
    let fraction = if decimals == 0 {
        String::new()
    } else {
        format!(".{fraction:0decimals$}")
    };
    match clock {
        Clock::Seconds => format!("{sign}{whole}{fraction} s"),
        Clock::Minutes => format!("{sign}{}:{:02}{fraction}", whole / 60, whole % 60),
        Clock::Hours => format!(
            "{sign}{}:{:02}:{:02}{fraction}",
            whole / 3600,
            whole / 60 % 60,
            whole % 60
        ),
    }
}
