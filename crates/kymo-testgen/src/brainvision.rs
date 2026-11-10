//! The BrainVision writer: header (`.vhdr`), samples (`.eeg`) and markers (`.vmrk`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::header::{check_amplitude, invalid, num};
use crate::signal::ChannelSignal;
use crate::{Binary, Error, Format, Orientation, Spec, Tal};

/// Checks what the BrainVision writer needs.
pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let Format::BrainVision(..) = spec.format else {
        unreachable!("called for BrainVision only");
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
    let (lo, hi) = spec.format.digital_limits();
    for (i, c) in spec.channels.iter().enumerate() {
        if c.samples_per_record != first.samples_per_record || c.samples_per_record == 0 {
            return Err(invalid(format!(
                "channel {i}: BrainVision has one rate, so every channel needs the same \
                 samples per record (> 0)"
            )));
        }
        for (field, text) in [("label", &c.label), ("dimension", &c.dimension)] {
            if text.contains(['\n', '\r']) || (field == "dimension" && text.contains(',')) {
                return Err(invalid(format!(
                    "channel {i} {field} {text:?} contains a line break or comma"
                )));
            }
        }
        if !(lo <= c.digital_min
            && c.digital_max <= hi
            && i64::from(c.digital_max) - i64::from(c.digital_min) >= 2)
        {
            return Err(invalid(format!(
                "channel {i}: digital range {}..={} must lie in {lo}..={hi} and span at least 3 \
                 values",
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
                "channel {i}: BrainVision has no offsets, but physical {}..{} over digital \
                 {}..{} needs {}",
                c.physical_min,
                c.physical_max,
                c.digital_min,
                c.digital_max,
                c.offset()
            )));
        }
        check_amplitude(i, c)?;
    }
    let rate = spec.rate(0);
    let whole = |x: f64| (x - x.round()).abs() < 1e-6;
    for (i, a) in spec.annotations.iter().enumerate() {
        if !(a.onset.is_finite() && a.onset >= 0.0 && whole(a.onset * rate)) {
            return Err(invalid(format!(
                "annotation {i}: onset {} must be >= 0 and on a sample",
                a.onset
            )));
        }
        if let Some(d) = a.duration
            && !(d.is_finite() && d >= 0.0 && whole(d * rate) && (d * rate).round() != 1.0)
        {
            return Err(invalid(format!(
                "annotation {i}: duration {d} must be 0 or at least 2 whole samples (a \
                 marker of one sample marks a point in time)"
            )));
        }
        if a.texts.is_empty() {
            return Err(invalid(format!("annotation {i}: no text")));
        }
        if a.texts.iter().any(|t| t.contains(['\n', '\r'])) {
            return Err(invalid(format!(
                "annotation {i}: text contains a line break"
            )));
        }
    }
    Ok(())
}

/// Escapes commas as BrainVision does (`\1`).
fn escape(text: &str) -> String {
    text.replace(',', "\\1")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Writes the header to `path`, the samples and markers beside it.
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let Format::BrainVision(orientation, binary) = spec.format else {
        unreachable!("validated");
    };
    let [_, data, markers] = <[_; 3]>::try_from(spec.format.files(path))
        .unwrap_or_else(|_| unreachable!("BrainVision writes three files"));
    let rate = spec.rate(0);
    let len = spec.len(0);

    let mut h = String::new();
    let mut line = |text: &str| {
        h.push_str(text);
        h.push_str("\r\n");
    };
    line("Brain Vision Data Exchange Header File Version 1.0");
    line("; Written by kymo-testgen");
    line("");
    line("[Common Infos]");
    line("Codepage=UTF-8");
    line(&format!("DataFile={}", file_name(&data)));
    line(&format!("MarkerFile={}", file_name(&markers)));
    line("DataFormat=BINARY");
    line(match orientation {
        Orientation::Multiplexed => "DataOrientation=MULTIPLEXED",
        Orientation::Vectorized => "DataOrientation=VECTORIZED",
    });
    line(&format!("NumberOfChannels={}", spec.channels.len()));
    line(&format!("DataPoints={len}"));
    line("; Sampling interval in microseconds");
    line(&format!("SamplingInterval={}", num(1e6 / rate)));
    line("");
    line("[Binary Infos]");
    line(&format!("BinaryFormat={}", binary.name()));
    line("");
    line("[Channel Infos]");
    for (i, c) in spec.channels.iter().enumerate() {
        line(&format!(
            "Ch{}={},,{},{}",
            i + 1,
            escape(&c.label),
            num(c.gain()),
            c.dimension
        ));
    }
    std::fs::write(path, h)?;

    write_data(spec, &data, orientation, binary, len)?;

    let mut m = String::new();
    let mut line = |text: &str| {
        m.push_str(text);
        m.push_str("\r\n");
    };
    line("Brain Vision Data Exchange Marker File, Version 1.0");
    line("");
    line("[Common Infos]");
    line("Codepage=UTF-8");
    line(&format!("DataFile={}", file_name(&data)));
    line("");
    line("[Marker Infos]");
    let (d, t) = (spec.start.date(), spec.start.time());
    line(&format!(
        "Mk1=New Segment,,1,1,0,{:04}{:02}{:02}{:02}{:02}{:02}{:06}",
        d.year(),
        d.month(),
        d.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.subsec_nanosecond() / 1000
    ));
    let mut k = 1;
    for a in &spec.annotations {
        for (kind, description) in a.texts.iter().map(|text| marker_text(text)) {
            k += 1;
            line(&format!(
                "Mk{k}={},{},{},{},0",
                escape(kind),
                escape(description),
                (a.onset * rate).round() as u64 + 1,
                size(a, rate)
            ));
        }
    }
    std::fs::write(markers, m)?;
    Ok(())
}

/// A marker's type and description from an annotation text: split at the first "/", as
/// kymo and MNE join them ("Stimulus/S  1"). A text without one gets no type, so kymo reads
/// it back as it is.
fn marker_text(text: &str) -> (&str, &str) {
    text.split_once('/').unwrap_or(("", text))
}

/// Marker size in samples: 1 (a point in time) without a duration.
fn size(a: &Tal, rate: f64) -> u64 {
    a.duration.map_or(1, |d| (d * rate).round() as u64)
}

fn write_data(
    spec: &Spec,
    path: &Path,
    orientation: Orientation,
    binary: Binary,
    len: u64,
) -> Result<(), Error> {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let signals: Vec<(ChannelSignal, Vec<f64>)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table)
        })
        .collect();
    let value = |c: usize, i: u64| {
        let (signal, table) = &signals[c];
        signal.value(i, table[(i % signal.sine_period) as usize])
    };
    let mut bytes = Vec::with_capacity(4);
    let mut put = |out: &mut BufWriter<File>, v: i32| -> std::io::Result<()> {
        bytes.clear();
        match binary {
            Binary::Int16 => bytes.extend_from_slice(&(v as i16).to_le_bytes()),
            Binary::UInt16 => bytes.extend_from_slice(&(v as u16).to_le_bytes()),
            Binary::Int32 => bytes.extend_from_slice(&v.to_le_bytes()),
            Binary::Float32 => bytes.extend_from_slice(&(v as f32).to_le_bytes()),
        }
        out.write_all(&bytes)
    };
    let n = spec.channels.len();
    match orientation {
        Orientation::Multiplexed => {
            for i in 0..len {
                for c in 0..n {
                    put(&mut out, value(c, i))?;
                }
            }
        }
        Orientation::Vectorized => {
            for c in 0..n {
                for i in 0..len {
                    put(&mut out, value(c, i))?;
                }
            }
        }
    }
    out.flush()?;
    Ok(())
}
