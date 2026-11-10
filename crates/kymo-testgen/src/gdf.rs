//! The GDF writer: GDF 1.25 or 2.20, a header with a part per channel (and, for
//! [`GdfVersion::V2Tagged`], a tag-length-value part), data records with a sample type per
//! channel, and an event table of the annotations.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::header::{check_amplitude, invalid};
use crate::signal::ChannelSignal;
use crate::{Error, Format, GdfVersion, Spec, Stored};

/// GDF 2's physical dimension codes of the units the presets use.
const UNIT_CODES: &[(&str, u16)] = &[
    ("uV", 4256 + 19),
    ("\u{b5}V", 4256 + 19),
    ("mV", 4256 + 18),
    ("V", 4256),
    ("%", 544),
    ("Boolean", 7776),
    ("fT", 65504 + 22),
    ("pT", 65504 + 21),
    ("", 512),
];

/// The manufacturer [`GdfVersion::V2Tagged`] writes, in tag 3: name, model, version and
/// serial number.
pub const MANUFACTURER: [&str; 4] = ["kymo-testgen", "synthetic", "1", "0001"];

fn version(spec: &Spec) -> GdfVersion {
    match spec.format {
        Format::Gdf(v) => v,
        _ => unreachable!("called for GDF only"),
    }
}

fn stored(c: &crate::ChannelSpec) -> Stored {
    c.stored.unwrap_or(Stored::I16)
}

/// The record duration as a fraction of whole numbers (microseconds at most).
fn duration(spec: &Spec) -> Option<(u32, u32)> {
    let micros = spec.record_duration * 1e6;
    if !(micros.is_finite() && micros >= 1.0 && micros.fract() == 0.0 && micros < 4e15) {
        return None;
    }
    let (mut num, mut den) = (micros as u64, 1_000_000u64);
    let gcd = {
        let (mut a, mut b) = (num, den);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    num /= gcd;
    den /= gcd;
    Some((u32::try_from(num).ok()?, u32::try_from(den).ok()?))
}

/// The event table's rate: the highest channel rate.
fn event_rate(spec: &Spec) -> f64 {
    (0..spec.channels.len())
        .map(|c| spec.rate(c))
        .fold(0.0, f64::max)
}

/// The type code of an annotation text: a whole number in 1..=65535, except 0x7FFF
/// (GDF's sparse samples).
fn type_code(text: &str) -> Option<u16> {
    text.parse::<u16>().ok().filter(|&t| t != 0 && t != 0x7fff)
}

fn unit_code(dimension: &str) -> Option<u16> {
    UNIT_CODES
        .iter()
        .find(|(d, _)| *d == dimension)
        .map(|&(_, c)| c)
}

/// Checks what the GDF writer needs.
pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let version = version(spec);
    if spec.channels.is_empty() {
        return Err(invalid("at least one data channel is needed"));
    }
    if duration(spec).is_none() {
        return Err(invalid(format!(
            "record duration {} must be a positive whole number of microseconds",
            spec.record_duration
        )));
    }
    if !spec.gaps.is_empty() {
        return Err(invalid("gaps need EDF+D or BDF+D"));
    }
    if spec.first_onset != 0.0 {
        return Err(invalid("first onset needs EDF+ or BDF+"));
    }
    if !(1..=9999).contains(&spec.start.year()) {
        return Err(invalid("the start year must be in 1..=9999"));
    }
    let text = |what: String, value: &str, width: usize| {
        if value.len() > width || value.contains('\0') {
            Err(invalid(format!(
                "{what} {value:?} must fit {width} bytes without NULs"
            )))
        } else {
            Ok(())
        }
    };
    text("patient".into(), &spec.patient, 66)?;
    text("recording".into(), &spec.recording, 64)?;
    for (i, c) in spec.channels.iter().enumerate() {
        text(format!("channel {i} label"), &c.label, 16)?;
        text(format!("channel {i} transducer"), &c.transducer, 80)?;
        match version {
            GdfVersion::V1 => {
                text(format!("channel {i} dimension"), &c.dimension, 8)?;
                text(format!("channel {i} prefilter"), &c.prefilter, 80)?;
            }
            _ => {
                if unit_code(&c.dimension).is_none() {
                    text(format!("channel {i} dimension"), &c.dimension, 6)?;
                }
            }
        }
        let (lo, hi) = stored(c).limits();
        if !(lo <= c.digital_min
            && c.digital_max <= hi
            && i64::from(c.digital_max) - i64::from(c.digital_min) >= 2)
        {
            return Err(invalid(format!(
                "channel {i}: digital range {}..={} must lie in {lo}..={hi} ({:?}) and span \
                 at least 3 values",
                c.digital_min,
                c.digital_max,
                stored(c)
            )));
        }
        if !(c.physical_min.is_finite() && c.physical_max.is_finite())
            || c.physical_min == c.physical_max
        {
            return Err(invalid(format!(
                "channel {i}: the physical range must be finite and not empty"
            )));
        }
        check_amplitude(i, c)?;
        if c.samples_per_record == 0 || c.samples_per_record > i32::MAX as u32 {
            return Err(invalid(format!(
                "channel {i}: samples per record must be in 1..2^31"
            )));
        }
    }
    let rate = event_rate(spec);
    for (i, a) in spec.annotations.iter().enumerate() {
        if !(a.onset.is_finite() && a.onset >= 0.0 && a.onset * rate < 4e9) {
            return Err(invalid(format!(
                "annotation {i}: onset {} out of range",
                a.onset
            )));
        }
        let duration = a.duration.unwrap_or(0.0);
        if version == GdfVersion::V1 && duration != 0.0 {
            return Err(invalid(format!(
                "annotation {i}: GDF 1 events (mode 1) have no duration"
            )));
        }
        if !(duration.is_finite() && duration >= 0.0 && duration * rate < 4e9) {
            return Err(invalid(format!(
                "annotation {i}: duration {duration} out of range"
            )));
        }
        if let Some(t) = a.texts.iter().find(|t| type_code(t).is_none()) {
            return Err(invalid(format!(
                "annotation {i}: {t:?} is no GDF event type (1..=65535, not 32767)"
            )));
        }
    }
    let events: usize = spec.annotations.iter().map(|a| a.texts.len()).sum();
    if events >= 1 << 24 {
        return Err(invalid("too many events"));
    }
    Ok(())
}

/// Size of the written file.
pub(crate) fn file_size(spec: &Spec) -> Result<u64, Error> {
    validate(spec)?;
    let events: u64 = spec.annotations.iter().map(|a| a.texts.len() as u64).sum();
    let table = if events == 0 {
        0
    } else {
        let entry = if version(spec) == GdfVersion::V1 {
            6
        } else {
            12
        };
        8 + entry * events
    };
    Ok(header(spec).len() as u64 + spec.n_records * record_bytes(spec) + table)
}

fn record_bytes(spec: &Spec) -> u64 {
    spec.channels
        .iter()
        .map(|c| u64::from(c.samples_per_record) * stored(c).size() as u64)
        .sum()
}

/// A text field, NUL-padded to `width` (validated before).
fn field(out: &mut Vec<u8>, value: &str, width: usize) {
    out.extend_from_slice(value.as_bytes());
    out.resize(out.len() + width - value.len(), 0);
}

/// GDF 2's filters from EDF's notation ("HP:0.1Hz LP:100Hz N:50Hz"); NaN for none.
fn filters(prefilter: &str) -> (f32, f32, f32) {
    let mut f = (f32::NAN, f32::NAN, f32::NAN);
    for part in prefilter.split_whitespace() {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let Ok(value) = value.trim_end_matches("Hz").parse::<f32>() else {
            continue;
        };
        match name {
            "LP" => f.0 = value,
            "HP" => f.1 = value,
            "N" => f.2 = value,
            _ => {}
        }
    }
    f
}

/// The tag-length-value part of [`GdfVersion::V2Tagged`]: the manufacturer (tag 3), an
/// unknown tag that readers skip, and the closing tag 0.
fn tags() -> Vec<u8> {
    let mut out = Vec::new();
    let mut tag = |tag: u8, value: &[u8]| {
        out.push(tag);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes()[..3]);
        out.extend_from_slice(value);
    };
    let manufacturer: Vec<u8> = MANUFACTURER
        .iter()
        .flat_map(|s| s.bytes().chain([0]))
        .collect();
    tag(3, &manufacturer);
    tag(200, b"skipped by readers");
    out.push(0);
    out
}

/// The whole header, padded to its length.
fn header(spec: &Spec) -> Vec<u8> {
    let version = version(spec);
    let gdf2 = version != GdfVersion::V1;
    let ns = spec.channels.len();
    let (num, den) = duration(spec).unwrap_or((1, 1));
    let tags = if version == GdfVersion::V2Tagged {
        tags()
    } else {
        Vec::new()
    };
    let header_len = 256 * (ns + 1) + tags.len().div_ceil(256) * 256;

    let mut out = Vec::with_capacity(header_len);
    out.extend_from_slice(if gdf2 { b"GDF 2.20" } else { b"GDF 1.25" });
    let (d, t) = (spec.start.date(), spec.start.time());
    if gdf2 {
        field(&mut out, &spec.patient, 66);
        out.resize(out.len() + 10 + 4, 0); // reserved; patient's habits, weight, height, sex
        field(&mut out, &spec.recording, 64);
        out.resize(out.len() + 16, 0); // recording location
        // Days since 0000-01-01 (MATLAB's datenum: 0001-01-01 is day 367), 32 bits of
        // fraction.
        let days = spec
            .start
            .date()
            .since(jiff::civil::date(1, 1, 1))
            .map_or(0, |span| span.get_days() as i64)
            + 367;
        let seconds = f64::from(t.hour()) * 3600.0
            + f64::from(t.minute()) * 60.0
            + f64::from(t.second())
            + f64::from(t.subsec_nanosecond()) * 1e-9;
        let fraction = (seconds / 86_400.0 * 2f64.powi(32)).round() as u64;
        out.extend_from_slice(&(((days as u64) << 32) + fraction).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes()); // birthday
        out.extend_from_slice(&((header_len / 256) as u16).to_le_bytes());
        out.resize(out.len() + 6 + 8 + 6 + 6 + 12 + 12, 0);
        out.extend_from_slice(&(spec.n_records as i64).to_le_bytes());
        out.extend_from_slice(&num.to_le_bytes());
        out.extend_from_slice(&den.to_le_bytes());
        out.extend_from_slice(&(ns as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
    } else {
        field(&mut out, &spec.patient, 80);
        field(&mut out, &spec.recording, 80);
        let centis = t.subsec_nanosecond() / 10_000_000;
        let start = format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}{centis:02}",
            d.year(),
            d.month(),
            d.day(),
            t.hour(),
            t.minute(),
            t.second()
        );
        out.extend_from_slice(start.as_bytes());
        out.extend_from_slice(&(header_len as i64).to_le_bytes());
        out.resize(out.len() + 8 + 8 + 8 + 20, 0); // equipment, lab, technician, reserved
        out.extend_from_slice(&(spec.n_records as i64).to_le_bytes());
        out.extend_from_slice(&num.to_le_bytes());
        out.extend_from_slice(&den.to_le_bytes());
        out.extend_from_slice(&(ns as u32).to_le_bytes());
    }
    debug_assert_eq!(out.len(), 256);

    let channels = &spec.channels;
    for c in channels {
        field(&mut out, &c.label, 16);
    }
    for c in channels {
        field(&mut out, &c.transducer, 80);
    }
    if gdf2 {
        for c in channels {
            let text = if unit_code(&c.dimension).is_some() {
                ""
            } else {
                &c.dimension
            };
            field(&mut out, text, 6);
        }
        for c in channels {
            out.extend_from_slice(&unit_code(&c.dimension).unwrap_or(0).to_le_bytes());
        }
    } else {
        for c in channels {
            field(&mut out, &c.dimension, 8);
        }
    }
    for c in channels {
        out.extend_from_slice(&c.physical_min.to_le_bytes());
    }
    for c in channels {
        out.extend_from_slice(&c.physical_max.to_le_bytes());
    }
    for c in channels {
        if gdf2 {
            out.extend_from_slice(&f64::from(c.digital_min).to_le_bytes());
        } else {
            out.extend_from_slice(&i64::from(c.digital_min).to_le_bytes());
        }
    }
    for c in channels {
        if gdf2 {
            out.extend_from_slice(&f64::from(c.digital_max).to_le_bytes());
        } else {
            out.extend_from_slice(&i64::from(c.digital_max).to_le_bytes());
        }
    }
    if gdf2 {
        out.resize(out.len() + 68 * ns, 0);
        let f: Vec<_> = channels.iter().map(|c| filters(&c.prefilter)).collect();
        for x in &f {
            out.extend_from_slice(&x.0.to_le_bytes());
        }
        for x in &f {
            out.extend_from_slice(&x.1.to_le_bytes());
        }
        for x in &f {
            out.extend_from_slice(&x.2.to_le_bytes());
        }
    } else {
        for c in channels {
            field(&mut out, &c.prefilter, 80);
        }
    }
    for c in channels {
        out.extend_from_slice(&(c.samples_per_record as i32).to_le_bytes());
    }
    for c in channels {
        out.extend_from_slice(&stored(c).code().to_le_bytes());
    }
    // GDF 2: positions (3 float32) and impedances (20 bytes); GDF 1: reserved.
    out.resize(out.len() + 32 * ns, 0);
    debug_assert_eq!(out.len(), 256 * (ns + 1));
    out.extend_from_slice(&tags);
    out.resize(header_len, 0);
    out
}

/// The event table, empty without annotations: mode 1 (GDF 1) or 3 (GDF 2) at the highest
/// channel rate, each text an event.
fn event_table(spec: &Spec) -> Vec<u8> {
    let events: Vec<(u32, u16, u32)> = spec
        .annotations
        .iter()
        .flat_map(|a| {
            let rate = event_rate(spec);
            let position = (a.onset * rate).round() as u32 + 1;
            let duration = (a.duration.unwrap_or(0.0) * rate).round() as u32;
            a.texts
                .iter()
                .filter_map(move |t| Some((position, type_code(t)?, duration)))
        })
        .collect();
    if events.is_empty() {
        return Vec::new();
    }
    let n = events.len() as u32;
    let mut out = Vec::new();
    if version(spec) == GdfVersion::V1 {
        out.push(1);
        out.extend_from_slice(&(event_rate(spec).round() as u32).to_le_bytes()[..3]);
        out.extend_from_slice(&n.to_le_bytes());
    } else {
        out.push(3);
        out.extend_from_slice(&n.to_le_bytes()[..3]);
        out.extend_from_slice(&(event_rate(spec) as f32).to_le_bytes());
    }
    for e in &events {
        out.extend_from_slice(&e.0.to_le_bytes());
    }
    for e in &events {
        out.extend_from_slice(&e.1.to_le_bytes());
    }
    if version(spec) != GdfVersion::V1 {
        for _ in &events {
            out.extend_from_slice(&0u16.to_le_bytes()); // all channels
        }
        for e in &events {
            out.extend_from_slice(&e.2.to_le_bytes());
        }
    }
    out
}

/// Writes `spec` to `path`.
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    out.write_all(&header(spec))?;
    let signals: Vec<(ChannelSignal, Vec<f64>)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table)
        })
        .collect();
    let mut record = vec![0u8; record_bytes(spec) as usize];
    for r in 0..spec.n_records {
        let mut pos = 0;
        for (c, (signal, table)) in signals.iter().enumerate() {
            let spr = u64::from(spec.channels[c].samples_per_record);
            let stored = stored(&spec.channels[c]);
            let size = stored.size();
            let start = r * spr;
            for j in 0..spr {
                let i = start + j;
                let v = signal.value(i, table[(i % signal.sine_period) as usize]);
                stored.put(v, &mut record[pos..pos + size]);
                pos += size;
            }
        }
        out.write_all(&record)?;
    }
    out.write_all(&event_table(spec))?;
    out.flush()?;
    Ok(())
}
