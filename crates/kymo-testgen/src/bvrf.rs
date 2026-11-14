//! The BVRF writer: header (`.bvrh`, JSON), samples (`.bvrd`, multiplexed) and markers
//! (`.bvrm`, tab-separated), all text UTF-8 with a byte-order mark as the specification has
//! it, lines ended by CRLF as Brain Products' Recorder writes them.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::header::{check_amplitude, invalid};
use crate::signal::ChannelSignal;
use crate::{BvrfData, BvrfScaling, Error, Format, Spec, Tal};

/// The UTF-8 byte-order mark.
const BOM: &str = "\u{feff}";

/// A channel's scaling as [`BvrfOptions::scaling`](crate::BvrfOptions::scaling) gives it.
fn scaling(spec: &Spec, channel: usize) -> BvrfScaling {
    spec.bvrf.scaling.get(channel).copied().unwrap_or_default()
}

/// The channel's offset, or 0 where it is only the rounding of its range (the usual
/// symmetric ranges give offsets of about 1e-10).
fn offset(spec: &Spec, channel: usize) -> f64 {
    let c = &spec.channels[channel];
    let size = c.physical_min.abs().max(c.physical_max.abs());
    if c.offset().abs() > 1e-9 * size {
        c.offset()
    } else {
        0.0
    }
}

/// Checks what the BVRF writer needs.
pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let Format::Bvrf(_) = spec.format else {
        unreachable!("called for BVRF only");
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
    if spec.bvrf.scaling.len() > spec.channels.len() {
        return Err(invalid("more BVRF scalings than channels"));
    }
    let (lo, hi) = spec.format.digital_limits();
    for (i, c) in spec.channels.iter().enumerate() {
        if c.samples_per_record != first.samples_per_record || c.samples_per_record == 0 {
            return Err(invalid(format!(
                "channel {i}: BVRF has one rate, so every channel needs the same samples per \
                 record (> 0)"
            )));
        }
        for (field, text) in [
            ("label", &c.label),
            ("dimension", &c.dimension),
            ("kind", &c.kind),
        ] {
            if text.contains(['\t', '\n', '\r']) {
                return Err(invalid(format!(
                    "channel {i} {field} {text:?} contains a tab or line break"
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
        match scaling(spec, i) {
            BvrfScaling::Resolution => {}
            BvrfScaling::Linear(factor) => {
                if !(factor.is_finite() && factor != 0.0) {
                    return Err(invalid(format!(
                        "channel {i}: the factor must be finite and non-zero"
                    )));
                }
            }
            BvrfScaling::Rational => {
                if offset(spec, i) != 0.0 {
                    return Err(invalid(format!(
                        "channel {i}: a channel with rational coefficients shows its sensor \
                         output, which has no offset"
                    )));
                }
            }
        }
        check_amplitude(i, c)?;
    }
    let o = &spec.bvrf;
    let participants = o.participants.len();
    if o.participants
        .iter()
        .any(|p| p.is_empty() || p.contains(['\t', '\n', '\r']))
    {
        return Err(invalid(
            "participant ids must be non-empty, without tabs or line breaks",
        ));
    }
    if o.channel_participants.len() > spec.channels.len()
        || o.targets.len() > spec.annotations.len()
    {
        return Err(invalid(
            "more channel participants or targets than channels or annotations",
        ));
    }
    let owner = |c: usize| o.channel_participants.get(c).copied().flatten();
    let mut names = std::collections::HashSet::new();
    for (i, c) in spec.channels.iter().enumerate() {
        if owner(i).is_some_and(|p| p >= participants) {
            return Err(invalid(format!("channel {i}: no such participant")));
        }
        if !names.insert((owner(i), c.label.as_str())) {
            return Err(invalid(format!(
                "channel {i}: {:?} twice for one participant",
                c.label
            )));
        }
    }
    for (i, t) in o.targets.iter().enumerate() {
        if t.participant.is_some_and(|p| p >= participants) {
            return Err(invalid(format!("annotation {i}: no such participant")));
        }
        if let Some(name) = &t.channel
            && !spec.channels.iter().enumerate().any(|(c, ch)| {
                ch.label == *name && t.participant.is_none_or(|p| owner(c) == Some(p))
            })
        {
            return Err(invalid(format!(
                "annotation {i}: no channel {name:?} for its participant"
            )));
        }
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
            && !(d.is_finite() && d >= 0.0 && whole(d * rate))
        {
            return Err(invalid(format!(
                "annotation {i}: duration {d} must be 0 or whole samples"
            )));
        }
        if a.texts.is_empty() {
            return Err(invalid(format!("annotation {i}: no text")));
        }
        for text in &a.texts {
            let (kind, _, _) = marker(text);
            if text.contains(['\t', '\n', '\r']) || kind.is_empty() || kind == "Recording Info" {
                return Err(invalid(format!(
                    "annotation {i}: text {text:?} contains a tab or line break, has no type \
                     before its \"/\", or is of the type Recording Info"
                )));
            }
        }
    }
    Ok(())
}

/// A marker's type, code and value from an annotation text "Type/CodeValue", as kymo joins
/// them ("Stimulus/S12" is type "Stimulus", code "S", value "12"): the type before the
/// first "/", the value the digits at the end (without a leading zero, so it reads back as
/// written). A text without "/" is a type alone.
fn marker(text: &str) -> (&str, &str, &str) {
    let Some((kind, rest)) = text.split_once('/') else {
        return (text, "", "");
    };
    let code = rest.trim_end_matches(|c: char| c.is_ascii_digit());
    let value = &rest[code.len()..];
    if value.len() > 1 && value.starts_with('0') {
        (kind, rest, "")
    } else {
        (kind, code, value)
    }
}

/// The marker file's date and time `seconds` after the start, an hour ahead of UTC.
fn date_time(spec: &Spec, seconds: f64) -> String {
    let at = jiff::SignedDuration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|d| spec.start.checked_add(d).ok())
        .unwrap_or(spec.start);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}+01:00",
        at.year(),
        at.month(),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.subsec_nanosecond() / 1000
    )
}

/// The `k`th pair's `StartEndId`, a GUID in braces as Recorder writes them.
fn guid(spec: &Spec, k: usize) -> String {
    format!("{{{:08x}-0000-4000-8000-{k:012x}}}", spec.seed as u32)
}

/// The header's filters (its `Filters` section) and each channel's ids among them, from
/// the channels' prefilters in EDF's notation ("HP:0.1Hz LP:100Hz N:50Hz"); other parts of
/// a prefilter are left out.
fn filters(spec: &Spec) -> (Vec<Value>, Vec<Vec<String>>) {
    let mut filters: Vec<(String, f64)> = Vec::new();
    let ids = spec
        .channels
        .iter()
        .map(|c| {
            c.prefilter
                .split_whitespace()
                .filter_map(|part| {
                    let (kind, frequency) = part.split_once(':')?;
                    let kind = match kind {
                        "HP" => "Highpass",
                        "LP" => "Lowpass",
                        "N" => "Notch",
                        _ => return None,
                    };
                    let hz: f64 = frequency.strip_suffix("Hz")?.parse().ok()?;
                    let k = filters
                        .iter()
                        .position(|(t, f)| t == kind && *f == hz)
                        .unwrap_or_else(|| {
                            filters.push((kind.to_owned(), hz));
                            filters.len() - 1
                        });
                    Some((k + 1).to_string())
                })
                .collect()
        })
        .collect();
    let filters = filters
        .into_iter()
        .enumerate()
        .map(|(k, (kind, hz))| {
            json!({
                "Id": (k + 1).to_string(),
                "Type": kind,
                "FrequencyInHertz": hz,
                "Domain": "Analog",
            })
        })
        .collect();
    (filters, ids)
}

/// Writes the header to `path`, the samples and markers beside it.
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let Format::Bvrf(data) = spec.format else {
        unreachable!("validated");
    };
    let [_, data_path, markers_path] = <[_; 3]>::try_from(spec.format.files(path))
        .unwrap_or_else(|_| unreachable!("BVRF writes three files"));
    let rate = spec.rate(0);
    let len = spec.len(0);

    let (filter_list, filter_ids) = filters(spec);
    let channels: Vec<Value> = spec
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut channel = json!({
                "Name": c.label,
                "Type": if c.kind.is_empty() { "EEG" } else { c.kind.as_str() },
                "Unit": c.dimension,
            });
            let fields = channel.as_object_mut().unwrap_or_else(|| unreachable!());
            if let Some(p) = spec.bvrf.channel_participants.get(i).copied().flatten() {
                fields.insert("ParticipantId".into(), json!(spec.bvrf.participants[p]));
            }
            let offset = offset(spec, i);
            let (resolution, coefficients) = match scaling(spec, i) {
                BvrfScaling::Resolution if offset == 0.0 => (c.gain(), None),
                BvrfScaling::Resolution => (c.gain(), Some(json!({"Num": [offset, 1.0]}))),
                BvrfScaling::Linear(factor) => {
                    (c.gain() / factor, Some(json!({"Num": [offset, factor]})))
                }
                BvrfScaling::Rational => {
                    fields.insert("Unit".into(), json!("\u{b5}S"));
                    fields.insert("ResolutionUnit".into(), json!(c.dimension));
                    (c.gain(), Some(json!({"Num": [0, 1], "Denom": [1, 0.5]})))
                }
            };
            fields.insert("ResolutionPerBit".into(), json!(resolution));
            if let Some(coefficients) = coefficients {
                fields.insert("Coefficients".into(), coefficients);
            }
            if !filter_ids[i].is_empty() {
                fields.insert("FilterIds".into(), json!(filter_ids[i]));
            }
            channel
        })
        .collect();
    let mut eeg = json!({
        "BVRFFiles": {
            "DataFile": {"Version": "1.0.0", "NumericDataType": data.name()},
            "MarkerFile": {"Version": "1.0.0"},
        },
        "DataSpecific": {
            "SamplingFrequencyInHertz": rate,
            "PowerLineFrequencyInHertz": 50,
        },
        "Amplifiers": [{"Model": "kymo-testgen", "SerialNumber": "0"}],
        "Channels": channels,
    });
    if !filter_list.is_empty()
        && let Some(eeg) = eeg.as_object_mut()
    {
        eeg.insert("Filters".into(), Value::Array(filter_list));
    }
    let mut header = json!({
        "$schema": "https://www.brainproducts.com/schemas/json/BVRF/BVRFHeader-1.0.0.json",
        "BVRF": {"Version": "1.0.0"},
        "BVRFHeaderFile": {"Version": "1.0.0"},
        "RecordingSoftware": {"Name": "kymo-testgen", "Version": env!("CARGO_PKG_VERSION")},
        "EEGModality": eeg,
    });
    if !spec.bvrf.participants.is_empty()
        && let Some(header) = header.as_object_mut()
    {
        let list: Vec<Value> = spec
            .bvrf
            .participants
            .iter()
            .map(|id| json!({"Id": id}))
            .collect();
        header.insert("Participants".into(), Value::Array(list));
    }
    let text = serde_json::to_string_pretty(&header)
        .map_err(|e| invalid(format!("the header can't be written: {e}")))?;
    std::fs::write(path, format!("{BOM}{}\r\n", text.replace('\n', "\r\n")))?;

    write_data(spec, &data_path, data, len)?;

    // The ParticipantId column only with several participants, as Recorder writes it.
    let several = spec.bvrf.participants.len() > 1;
    let mut m = format!(
        "{BOM}Sample\tType\tCode\tValue\t{}Channel\tDateTime\tStartEndId\tComment\r\n",
        if several { "ParticipantId\t" } else { "" }
    );
    let mut line = |[sample, kind, code, value, participant, channel, at, id]: [&str; 8]| {
        let mut fields = vec![sample, kind, code, value];
        if several {
            fields.push(participant);
        }
        fields.extend([channel, at, id, ""]);
        m.push_str(&fields.join("\t"));
        m.push_str("\r\n");
    };
    let saving = guid(spec, 0);
    let start = date_time(spec, 0.0);
    line(["0", "Recording Info", "Saving", "", "", "", &start, &saving]);
    let mut pairs = 0;
    for (k, a) in spec.annotations.iter().enumerate() {
        let sample = (a.onset * rate).round() as u64;
        let at = date_time(spec, a.onset);
        let target = spec.bvrf.targets.get(k);
        let participant = target
            .and_then(|t| t.participant)
            .map_or("", |p| spec.bvrf.participants[p].as_str());
        let channel = target.and_then(|t| t.channel.as_deref()).unwrap_or("");
        for text in &a.texts {
            let (kind, code, value) = marker(text);
            let id = match a.duration {
                Some(d) if d > 0.0 => {
                    pairs += 1;
                    guid(spec, pairs)
                }
                _ => String::new(),
            };
            let sample_text = sample.to_string();
            line([
                &sample_text,
                kind,
                code,
                value,
                participant,
                channel,
                &at,
                &id,
            ]);
            if let Some(end) = end_of(a, rate).filter(|_| !id.is_empty()) {
                let end_at = date_time(spec, end as f64 / rate);
                let end = end.to_string();
                line([&end, kind, "", "", participant, channel, &end_at, &id]);
            }
        }
    }
    let last = len.saturating_sub(1);
    let end = date_time(spec, last as f64 / rate);
    let last = last.to_string();
    line([&last, "Recording Info", "", "", "", "", &end, &saving]);
    std::fs::write(markers_path, m)?;
    Ok(())
}

/// The sample of an annotation's end marker, where it has a duration.
fn end_of(a: &Tal, rate: f64) -> Option<u64> {
    a.duration
        .filter(|&d| d > 0.0)
        .map(|d| ((a.onset + d) * rate).round() as u64)
}

fn write_data(spec: &Spec, path: &Path, data: BvrfData, len: u64) -> Result<(), Error> {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let signals: Vec<(ChannelSignal, Vec<f64>)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table)
        })
        .collect();
    for i in 0..len {
        for (signal, table) in &signals {
            let v = signal.value(i, table[(i % signal.sine_period) as usize]);
            match data {
                BvrfData::Int16 => out.write_all(&(v as i16).to_le_bytes())?,
                BvrfData::Int32 => out.write_all(&v.to_le_bytes())?,
                BvrfData::Single => out.write_all(&(v as f32).to_le_bytes())?,
                BvrfData::Double => out.write_all(&f64::from(v).to_le_bytes())?,
            }
        }
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::marker;

    #[test]
    fn texts_split_into_type_code_and_value() {
        assert_eq!(marker("Stimulus/S12"), ("Stimulus", "S", "12"));
        assert_eq!(marker("Stimulus/S0"), ("Stimulus", "S", "0"));
        assert_eq!(marker("Stimulus/S01"), ("Stimulus", "S01", ""));
        assert_eq!(
            marker("Annotation/Eyes open"),
            ("Annotation", "Eyes open", "")
        );
        assert_eq!(marker("Comment/a/b"), ("Comment", "a/b", ""));
        assert_eq!(marker("Reconnection"), ("Reconnection", "", ""));
        assert_eq!(marker("Response/12"), ("Response", "", "12"));
    }
}
