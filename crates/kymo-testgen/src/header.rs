//! Spec validation, the EDF/BDF header and TAL encoding.

use std::io::Write;

use crate::{Error, Spec, Tal};

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

/// Shortest round-trip formatting: parsing the field gives back exactly `x`.
fn num(x: f64) -> String {
    format!("{x}")
}

fn check_text(what: &str, value: &str, width: usize) -> Result<(), Error> {
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(invalid(format!("{what} {value:?} must be printable ASCII")));
    }
    if value.len() > width {
        return Err(invalid(format!(
            "{what} {value:?} is longer than {width} characters"
        )));
    }
    Ok(())
}

fn check_number(what: &str, x: f64) -> Result<(), Error> {
    if !x.is_finite() {
        return Err(invalid(format!("{what} {x} is not finite")));
    }
    check_text(what, &num(x), 8)
}

/// The `recording` header field as written.
fn recording_field(spec: &Spec) -> String {
    if spec.format.is_plus() {
        let d = spec.start.date();
        format!(
            "Startdate {:02}-{}-{:04} {}",
            d.day(),
            MONTHS[d.month() as usize - 1],
            d.year(),
            spec.recording
        )
    } else {
        spec.recording.clone()
    }
}

pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let format = spec.format;
    if spec.channels.is_empty() {
        return Err(invalid("at least one data channel is needed"));
    }
    if spec.signal_count() > 9999 {
        return Err(invalid("more than 9999 signals"));
    }
    if spec.record_duration.is_nan() || spec.record_duration <= 0.0 {
        return Err(invalid(format!(
            "record duration {} must be > 0",
            spec.record_duration
        )));
    }
    check_number("record duration", spec.record_duration)?;
    check_text("number of records", &spec.n_records.to_string(), 8)?;
    if !(1985..=2084).contains(&spec.start.year()) {
        return Err(invalid(format!(
            "start year {} is outside 1985..=2084",
            spec.start.year()
        )));
    }
    check_text("patient", &spec.patient, 80)?;
    check_text("recording", &recording_field(spec), 80)?;
    if format.is_plus() {
        if !(spec.first_onset.is_finite() && spec.first_onset >= 0.0) {
            return Err(invalid(format!(
                "first onset {} must be finite and >= 0",
                spec.first_onset
            )));
        }
    } else if spec.first_onset != 0.0 {
        return Err(invalid("first onset needs EDF+ or BDF+"));
    }

    let (lo, hi) = format.digital_limits();
    for (i, c) in spec.channels.iter().enumerate() {
        let what = |field: &str| format!("channel {i} {field}");
        check_text(&what("label"), &c.label, 16)?;
        if c.label.trim_end() == annotation_label(spec) {
            return Err(invalid(format!("channel {i} uses the annotation label")));
        }
        check_text(&what("transducer"), &c.transducer, 80)?;
        check_text(&what("dimension"), &c.dimension, 8)?;
        check_text(&what("prefilter"), &c.prefilter, 80)?;
        check_number(&what("physical minimum"), c.physical_min)?;
        check_number(&what("physical maximum"), c.physical_max)?;
        if c.physical_min == c.physical_max {
            return Err(invalid(format!(
                "channel {i}: physical minimum equals maximum"
            )));
        }
        if !(lo <= c.digital_min && c.digital_max <= hi && c.digital_max - c.digital_min >= 2) {
            return Err(invalid(format!(
                "channel {i}: digital range {}..={} must lie in {lo}..={hi} and span at least 3 values",
                c.digital_min, c.digital_max
            )));
        }
        let half_range = (f64::from(c.digital_max) - f64::from(c.digital_min)) / 2.0;
        let reach = c.amplitude / c.gain().abs();
        if !c.trigger && !(c.amplitude > 0.0 && reach <= 0.6 * half_range) {
            return Err(invalid(format!(
                "channel {i}: amplitude {} must be > 0 and at most 60% of the half-range",
                c.amplitude
            )));
        }
        if c.samples_per_record == 0 {
            return Err(invalid(format!("channel {i}: no samples per record")));
        }
        check_text(
            &what("samples per record"),
            &c.samples_per_record.to_string(),
            8,
        )?;
    }

    if !spec.gaps.is_empty() && !format.is_discontinuous() {
        return Err(invalid("gaps need EDF+D or BDF+D"));
    }
    let mut previous = 0;
    for g in &spec.gaps {
        if !(previous < g.before_record && g.before_record < spec.n_records) {
            return Err(invalid(format!(
                "gap before record {} must be sorted and in 1..{}",
                g.before_record, spec.n_records
            )));
        }
        if !(g.seconds.is_finite() && g.seconds > 0.0) {
            return Err(invalid(format!("gap of {} s must be > 0", g.seconds)));
        }
        previous = g.before_record;
    }

    if !spec.annotations.is_empty() && !format.is_plus() {
        return Err(invalid("annotations need EDF+ or BDF+"));
    }
    for (i, a) in spec.annotations.iter().enumerate() {
        if !a.onset.is_finite() {
            return Err(invalid(format!("annotation {i}: onset is not finite")));
        }
        if let Some(d) = a.duration
            && !(d.is_finite() && d >= 0.0)
        {
            return Err(invalid(format!(
                "annotation {i}: duration {d} must be >= 0"
            )));
        }
        if a.texts.is_empty() {
            return Err(invalid(format!("annotation {i}: no text")));
        }
        if a.texts.iter().any(|t| t.contains(['\x00', '\x14', '\x15'])) {
            return Err(invalid(format!(
                "annotation {i}: text contains a TAL separator"
            )));
        }
    }
    Ok(())
}

pub(crate) fn annotation_label(spec: &Spec) -> &'static str {
    if spec.format.is_bdf() {
        "BDF Annotations"
    } else {
        "EDF Annotations"
    }
}

/// Appends `value` left-aligned and space-padded to `width` (validated before).
fn field(out: &mut Vec<u8>, value: &str, width: usize) {
    debug_assert!(value.len() <= width, "{value:?} wider than {width}");
    out.extend_from_slice(value.as_bytes());
    out.resize(out.len() + width - value.len(), b' ');
}

/// The complete header. `annotation_spr` is the annotation signal's samples per record
/// (EDF+/BDF+ only).
pub(crate) fn header(spec: &Spec, annotation_spr: Option<u32>) -> Vec<u8> {
    let format = spec.format;
    let mut out = Vec::with_capacity(spec.header_bytes() as usize);
    if format.is_bdf() {
        out.push(0xff);
        field(&mut out, "BIOSEMI", 7);
    } else {
        field(&mut out, "0", 8);
    }
    field(&mut out, &spec.patient, 80);
    field(&mut out, &recording_field(spec), 80);
    let (d, t) = (spec.start.date(), spec.start.time());
    field(
        &mut out,
        &format!("{:02}.{:02}.{:02}", d.day(), d.month(), d.year() % 100),
        8,
    );
    field(
        &mut out,
        &format!("{:02}.{:02}.{:02}", t.hour(), t.minute(), t.second()),
        8,
    );
    field(&mut out, &spec.header_bytes().to_string(), 8);
    let reserved = match format {
        crate::Format::Edf => "",
        crate::Format::Bdf => "24BIT",
        other => other.name(),
    };
    field(&mut out, reserved, 44);
    field(&mut out, &spec.n_records.to_string(), 8);
    field(&mut out, &num(spec.record_duration), 8);
    field(&mut out, &spec.signal_count().to_string(), 4);

    // Per-signal fields, each for all signals in turn; the annotation signal comes last.
    const WIDTHS: [usize; 10] = [16, 80, 8, 8, 8, 8, 8, 80, 8, 32];
    let mut signals: Vec<[String; 10]> = spec
        .channels
        .iter()
        .map(|c| {
            [
                c.label.clone(),
                c.transducer.clone(),
                c.dimension.clone(),
                num(c.physical_min),
                num(c.physical_max),
                c.digital_min.to_string(),
                c.digital_max.to_string(),
                c.prefilter.clone(),
                c.samples_per_record.to_string(),
                String::new(),
            ]
        })
        .collect();
    if let Some(spr) = annotation_spr {
        let (lo, hi) = format.digital_limits();
        signals.push([
            annotation_label(spec).into(),
            String::new(),
            String::new(),
            "-1".into(),
            "1".into(),
            lo.to_string(),
            hi.to_string(),
            String::new(),
            spr.to_string(),
            String::new(),
        ]);
    }
    for (column, width) in WIDTHS.into_iter().enumerate() {
        for s in &signals {
            field(&mut out, &s[column], width);
        }
    }
    debug_assert_eq!(out.len() as u64, spec.header_bytes());
    out
}

/// Appends a record's time-keeping TAL: `+onset\x14\x14\0`.
pub(crate) fn timekeeping_tal(onset: f64, out: &mut Vec<u8>) {
    // Writing to a Vec cannot fail.
    let _ = write!(out, "{onset:+}\x14\x14\0");
}

/// Appends a TAL: `+onset[\x15duration]\x14text\x14[text\x14...]\0`.
pub(crate) fn tal(t: &Tal, out: &mut Vec<u8>) {
    let _ = write!(out, "{:+}", t.onset);
    if let Some(d) = t.duration {
        let _ = write!(out, "\x15{d}");
    }
    out.push(0x14);
    for text in &t.texts {
        out.extend_from_slice(text.as_bytes());
        out.push(0x14);
    }
    out.push(0);
}
