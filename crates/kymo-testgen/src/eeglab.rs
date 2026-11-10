//! The EEGLAB writer: a MATLAB v6 `.set` holding the `EEG` struct (or its fields as
//! variables), with the samples in it or in a `.fdt` beside it.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::header::{check_amplitude, invalid};
use crate::mat::{self, MX_INT32, Order, Value};
use crate::signal::ChannelSignal;
use crate::{Error, Format, Layout, Spec, Storage};

/// MAT v6 element sizes are 32-bit.
const MAX_ELEMENT: u64 = u32::MAX as u64 - 4096;

/// Checks what the EEGLAB writer needs.
pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let Format::Eeglab(storage, _) = spec.format else {
        unreachable!("called for EEGLAB only");
    };
    let Some(first) = spec.channels.first() else {
        return Err(invalid("at least one data channel is needed"));
    };
    if !(spec.record_duration.is_finite() && spec.record_duration > 0.0) {
        return Err(invalid(format!(
            "record duration {} must be > 0",
            spec.record_duration
        )));
    }
    if !spec.gaps.is_empty() {
        return Err(invalid("gaps need EDF+D or BDF+D"));
    }
    if spec.first_onset != 0.0 {
        return Err(invalid("first onset needs EDF+ or BDF+"));
    }
    for (i, c) in spec.channels.iter().enumerate() {
        if c.samples_per_record != first.samples_per_record || c.samples_per_record == 0 {
            return Err(invalid(format!(
                "channel {i}: EEGLAB has one rate, so every channel needs the same samples \
                 per record (> 0)"
            )));
        }
        if i64::from(c.digital_max) - i64::from(c.digital_min) < 2 {
            return Err(invalid(format!(
                "channel {i}: digital range {}..={} must span at least 3 values",
                c.digital_min, c.digital_max
            )));
        }
        let gain = c.gain();
        if !(gain.is_finite() && gain != 0.0) {
            return Err(invalid(format!(
                "channel {i}: the gain must be finite and non-zero"
            )));
        }
        let size = c.physical_min.abs().max(c.physical_max.abs());
        if c.offset().abs() > 1e-9 * size {
            return Err(invalid(format!(
                "channel {i}: EEGLAB has no offsets, but physical {}..{} over digital {}..{} \
                 needs {}",
                c.physical_min,
                c.physical_max,
                c.digital_min,
                c.digital_max,
                c.offset()
            )));
        }
        check_amplitude(i, c)?;
    }
    if storage != Storage::Fdt {
        let bytes =
            spec.len(0) * spec.channels.len() as u64 * spec.format.bytes_per_sample() as u64;
        if bytes > MAX_ELEMENT {
            return Err(invalid(format!(
                "{bytes} bytes of samples don't fit into a MAT v6 file; use a .fdt"
            )));
        }
    }
    for (i, a) in spec.annotations.iter().enumerate() {
        if !(a.onset.is_finite() && a.onset >= 0.0) {
            return Err(invalid(format!(
                "annotation {i}: onset {} must be >= 0",
                a.onset
            )));
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
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The stored value of every channel at each sample: the physical value (µV).
fn values(spec: &Spec) -> impl Fn(usize, u64) -> f64 {
    let signals: Vec<(ChannelSignal, Vec<f64>, f64)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table, spec.channels[c].gain())
        })
        .collect();
    move |c, i| {
        let (signal, table, gain) = &signals[c];
        f64::from(signal.value(i, table[(i % signal.sine_period) as usize])) * gain
    }
}

/// An event's type: a text that is a whole number is written as a number, as EEGLAB's
/// numeric event codes are.
fn event_type(text: &str) -> Value {
    match text.parse::<u32>() {
        Ok(code) if !text.starts_with('+') => Value::scalar(f64::from(code)),
        _ => Value::Char(text.into()),
    }
}

/// Writes the `.set` to `path`, and the `.fdt` beside it for [`Storage::Fdt`].
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let Format::Eeglab(storage, layout) = spec.format else {
        unreachable!("validated");
    };
    let rate = spec.rate(0);
    let n = spec.channels.len();
    let len = spec.len(0);
    let value = values(spec);

    let data = match storage {
        Storage::Fdt => {
            let fdt = path.with_extension("fdt");
            let mut out = BufWriter::with_capacity(1 << 20, File::create(&fdt)?);
            for i in 0..len {
                for c in 0..n {
                    out.write_all(&(value(c, i) as f32).to_le_bytes())?;
                }
            }
            out.flush()?;
            Value::Char(file_name(&fdt))
        }
        Storage::Single => Value::Single(
            vec![n, len as usize],
            (0..len)
                .flat_map(|i| (0..n).map(move |c| (c, i)))
                .map(|(c, i)| value(c, i) as f32)
                .collect(),
        ),
        Storage::Double => Value::Double(
            vec![n, len as usize],
            (0..len)
                .flat_map(|i| (0..n).map(move |c| (c, i)))
                .map(|(c, i)| value(c, i))
                .collect(),
        ),
    };
    let datfile = match &data {
        Value::Char(name) => Value::Char(name.clone()),
        _ => Value::Empty,
    };

    let chanlocs = Value::records(
        &["labels", "type", "theta", "radius", "X", "Y", "Z", "urchan"],
        spec.channels
            .iter()
            .enumerate()
            .map(|(k, c)| {
                vec![
                    Value::Char(c.label.clone()),
                    Value::Char(c.kind.clone()),
                    Value::Empty,
                    Value::Empty,
                    Value::Empty,
                    Value::Empty,
                    Value::Empty,
                    Value::scalar(k as f64 + 1.0),
                ]
            })
            .collect(),
    );
    let events: Vec<Vec<Value>> = spec
        .annotations
        .iter()
        .flat_map(|a| a.texts.iter().map(move |t| (a, t)))
        .enumerate()
        .map(|(k, (a, text))| {
            vec![
                event_type(text),
                Value::scalar(a.onset * rate + 1.0),
                Value::scalar(a.duration.unwrap_or(0.0) * rate),
                Value::scalar(k as f64 + 1.0),
            ]
        })
        .collect();
    let event = if events.is_empty() {
        Value::Empty
    } else {
        Value::records(&["type", "latency", "duration", "urevent"], events)
    };
    let times = Value::Double(
        vec![1, len as usize],
        (0..len).map(|i| i as f64 * 1000.0 / rate).collect(),
    );
    // Fields kymo skips.
    let etc = Value::record(vec![
        ("eeglabvers", Value::Char("2024.0".into())),
        (
            "rejected",
            Value::Logical((0..n).map(|c| c % 3 == 0).collect()),
        ),
        (
            "counts",
            Value::Raw {
                class: MX_INT32,
                storage: mat::MI_INT32,
                dims: vec![1, 2],
                bytes: [7i32, -7].iter().flat_map(|v| v.to_le_bytes()).collect(),
            },
        ),
        (
            "notes",
            Value::Cell(
                vec![1, 2],
                vec![Value::Char("kymo-testgen".into()), Value::scalar(0.5)],
            ),
        ),
    ]);
    let set = file_name(path);
    let fields = vec![
        ("setname", Value::Char("kymo-testgen".into())),
        ("filename", Value::Char(set.clone())),
        ("filepath", Value::Char(String::new())),
        ("subject", Value::Char("S01".into())),
        ("group", Value::Char(String::new())),
        ("condition", Value::Char("rest".into())),
        ("session", Value::scalar(1.0)),
        (
            "comments",
            Value::Rows(vec!["Written by".into(), "kymo-testgen".into()]),
        ),
        ("nbchan", Value::scalar(n as f64)),
        ("trials", Value::scalar(1.0)),
        ("pnts", Value::scalar(len as f64)),
        ("srate", Value::scalar(rate)),
        ("xmin", Value::scalar(0.0)),
        ("xmax", Value::scalar(len.saturating_sub(1) as f64 / rate)),
        ("times", times),
        ("data", data),
        ("icaact", Value::Empty),
        ("icawinv", Value::Empty),
        ("icasphere", Value::Empty),
        ("icaweights", Value::Empty),
        ("icachansind", Value::Empty),
        ("chanlocs", chanlocs),
        ("urchanlocs", Value::Empty),
        ("chaninfo", Value::record(vec![("plotrad", Value::Empty)])),
        ("ref", Value::Char("common".into())),
        ("event", event),
        ("urevent", Value::Empty),
        ("eventdescription", Value::Cell(vec![0, 0], Vec::new())),
        ("epoch", Value::Empty),
        ("epochdescription", Value::Cell(vec![0, 0], Vec::new())),
        ("history", Value::Char("EEG = pop_saveset(EEG);".into())),
        ("saved", Value::Char("yes".into())),
        ("etc", etc),
        ("datfile", datfile),
    ];
    let bytes = match layout {
        Layout::Struct => mat::file(&[("EEG", Value::record(fields))], Order::Little),
        Layout::Flat => mat::file(&fields, Order::Little),
    };
    std::fs::write(path, bytes)?;
    Ok(())
}
