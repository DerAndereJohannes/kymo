//! The header file (`.bvrh`): JSON with the sampling rate, the sample type and the channels
//! (section 2 of the specification), and what the info view shows.

use std::collections::HashMap;

use serde_json::Value;

use super::BvrfError;
use crate::decode::Encoding;
use crate::{Channel, ChannelKind, GridId, Quantity, Unit};

/// What the reader needs from a header.
#[derive(Debug)]
pub(super) struct ParsedHeader {
    pub(super) encoding: Encoding,
    pub(super) rate: f64,
    pub(super) channels: Vec<Channel>,
    /// Each channel's participant (`ParticipantId`), `None` for all participants.
    pub(super) participant: Vec<Option<String>>,
    /// The `Participants` section's ids, in its order.
    pub(super) participants: Vec<String>,
    pub(super) extra: Vec<(String, String)>,
}

/// At most this many channels, as for BrainVision.
const MAX_CHANNELS: usize = 1 << 16;

/// The value at `path` below `value`; `None` where a key is missing or the value is null
/// (the schema allows null for most optional fields).
fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |v, key| v.get(key))
        .filter(|v| !v.is_null())
}

/// The text at `path`, trimmed; `None` if missing, null, empty or not a string.
fn text<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether a parsed JSON file is a BVRF header: an object with an `EEGModality` or a `BVRF`
/// section. Other JSON is some other file.
pub(super) fn is_bvrf(json: &Value) -> bool {
    json.get("EEGModality").is_some() || json.get("BVRF").is_some()
}

/// Parses a header.
pub(super) fn parse(json: &Value, warnings: &mut Vec<String>) -> Result<ParsedHeader, BvrfError> {
    let invalid = |field: &'static str, value: &Value| BvrfError::Invalid {
        field,
        value: value.to_string(),
    };
    let version = text(json, &["BVRF", "Version"]);
    match version.map(major) {
        None => warnings.push("the header has no BVRF version; read as 1.0.0".into()),
        Some(Some(1)) => {}
        Some(Some(m)) if m > 1 => warnings.push(format!(
            "BVRF version {} is newer than 1.0.0, which kymo reads; read as 1.0.0",
            version.unwrap_or_default()
        )),
        Some(_) => warnings.push(format!(
            "invalid BVRF version {:?}; read as 1.0.0",
            version.unwrap_or_default()
        )),
    }
    let eeg = at(json, &["EEGModality"]).ok_or(BvrfError::Missing("EEGModality section"))?;

    let data_type = at(eeg, &["BVRFFiles", "DataFile", "NumericDataType"])
        .ok_or(BvrfError::Missing("NumericDataType"))?;
    let encoding = match data_type.as_str() {
        Some("Int16") => Encoding::I16,
        Some("Int32") => Encoding::I32,
        Some("Single") => Encoding::F32,
        Some("Double") => Encoding::F64,
        Some(other) => {
            return Err(BvrfError::Unsupported(format!(
                "numeric data type {other:?} (Int16, Int32, Single and Double are read)"
            )));
        }
        None => return Err(invalid("NumericDataType", data_type)),
    };

    let rate = at(eeg, &["DataSpecific", "SamplingFrequencyInHertz"])
        .ok_or(BvrfError::Missing("SamplingFrequencyInHertz"))?;
    let rate = rate
        .as_f64()
        .filter(|r| r.is_finite() && *r > 0.0)
        .ok_or_else(|| invalid("SamplingFrequencyInHertz", rate))?;

    let list = at(eeg, &["Channels"]).ok_or(BvrfError::Missing("Channels section"))?;
    let list = list
        .as_array()
        .filter(|l| (1..=MAX_CHANNELS).contains(&l.len()))
        .ok_or_else(|| BvrfError::Invalid {
            field: "Channels",
            value: match list.as_array() {
                Some(l) => format!("{} channels (1 to {MAX_CHANNELS} are read)", l.len()),
                None => "not a list".into(),
            },
        })?;
    let filters = filters(eeg);
    let mut unnamed = 0;
    let mut unknown_filters = 0;
    let mut channels = Vec::with_capacity(list.len());
    let mut participant = Vec::with_capacity(list.len());
    for (i, item) in list.iter().enumerate() {
        participant.push(text(item, &["ParticipantId"]).map(str::to_owned));
        let name = match text(item, &["Name"]) {
            Some(name) => name.to_owned(),
            None => {
                unnamed += 1;
                format!("Ch{}", i + 1)
            }
        };
        let fail = |reason: String| BvrfError::InvalidChannel {
            index: i + 1,
            name: name.clone(),
            reason,
        };
        let resolution = match at(item, &["ResolutionPerBit"]) {
            None => 1.0,
            Some(r) => r
                .as_f64()
                .filter(|r| r.is_finite() && *r != 0.0)
                .ok_or_else(|| fail(format!("invalid ResolutionPerBit {r}")))?,
        };
        let unit_label = text(item, &["Unit"]).unwrap_or_default();
        let (unit, gain, offset) = match coefficients(item).map_err(fail)? {
            Scaling::Linear { factor, offset } => (unit_label, resolution * factor, offset),
            Scaling::Rational => {
                let sensor = text(item, &["ResolutionUnit"]).unwrap_or_default();
                warnings.push(format!(
                    "channel {name:?}: its coefficients aren't linear, so it shows the sensor \
                     output{} rather than {unit_label}",
                    if sensor.is_empty() {
                        String::new()
                    } else {
                        format!(" in {sensor}")
                    }
                ));
                (sensor, resolution, 0.0)
            }
        };
        if !(gain.is_finite() && offset.is_finite()) {
            return Err(fail("its coefficients give no finite scaling".into()));
        }
        let unit = Unit::from_label(unit);
        let kind = match unit.quantity() {
            Some(Quantity::MagneticField) => ChannelKind::Mag,
            Some(Quantity::FieldGradient) => ChannelKind::Grad,
            _ => text(item, &["Type"])
                .and_then(ChannelKind::from_mne)
                .unwrap_or(ChannelKind::Misc),
        };
        let mut prefilter = Vec::new();
        for id in at(item, &["FilterIds"])
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match id.as_str().and_then(|id| filters.get(id.trim())) {
                Some(Some(filter)) => prefilter.push(filter.as_str()),
                Some(None) => {}
                None => unknown_filters += 1,
            }
        }
        channels.push(Channel {
            kind,
            unit,
            gain,
            offset,
            prefilter: prefilter.join(" "),
            ..Channel::new(name, GridId(0))
        });
    }
    if unnamed > 0 {
        warnings.push(format!(
            "{unnamed} channels have no name; they are named by their number (Ch1, ...)"
        ));
    }
    if unknown_filters > 0 {
        warnings.push(format!(
            "channels name {unknown_filters} filters the Filters section doesn't have"
        ));
    }

    let mut extra = Vec::new();
    if let Some(version) = version {
        extra.push(("version".to_owned(), version.to_owned()));
    }
    extra.push((
        "numeric data type".to_owned(),
        data_type.as_str().unwrap_or_default().to_owned(),
    ));
    info(json, eeg, &mut extra);
    let participants = at(json, &["Participants"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| text(p, &["Id"]).map(str::to_owned))
        .collect();
    Ok(ParsedHeader {
        encoding,
        rate,
        channels,
        participant,
        participants,
        extra,
    })
}

/// The major version of `"i.j.k"`.
fn major(version: &str) -> Option<u64> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.parse::<u64>().is_err()) {
        return None;
    }
    parts[0].parse().ok()
}

/// How a channel's numbers become its values (section 2.4).
#[derive(Debug, PartialEq)]
enum Scaling {
    /// `value × ResolutionPerBit × factor + offset`, in `Unit`.
    Linear { factor: f64, offset: f64 },
    /// A rational function that isn't linear, which a gain and an offset can't express.
    Rational,
}

/// The channel's `Coefficients`: the rational polynomial F(y) = (N0 + N1 y + ...) /
/// (D0 + D1 y + ...) of the sensor output y = value × resolution, numerator [0, 1] and
/// denominator [1] where missing. It is linear where the numerator has at most two terms
/// and the denominator one (trailing zeros left out), the denominator's divides the
/// numerator's.
fn coefficients(channel: &Value) -> Result<Scaling, String> {
    let Some(c) = at(channel, &["Coefficients"]) else {
        return Ok(Scaling::Linear {
            factor: 1.0,
            offset: 0.0,
        });
    };
    let terms = |key: &str, default: &[f64]| -> Result<Vec<f64>, String> {
        let Some(list) = at(c, &[key]) else {
            return Ok(default.to_vec());
        };
        let numbers: Option<Vec<f64>> = list
            .as_array()
            .map(|l| l.iter().map(Value::as_f64).collect())
            .unwrap_or_default();
        let mut numbers = numbers
            .filter(|n| n.iter().all(|x| x.is_finite()))
            .ok_or_else(|| format!("invalid coefficients {key} {list}"))?;
        while numbers.last() == Some(&0.0) {
            numbers.pop();
        }
        Ok(numbers)
    };
    let (num, denom) = (terms("Num", &[0.0, 1.0])?, terms("Denom", &[1.0])?);
    Ok(match (num.as_slice(), denom.as_slice()) {
        (n, [d]) if n.len() <= 2 => Scaling::Linear {
            factor: n.get(1).copied().unwrap_or(0.0) / d,
            offset: n.first().copied().unwrap_or(0.0) / d,
        },
        _ => Scaling::Rational,
    })
}

/// Each filter's part of a prefilter text in EDF's notation ("HP:0.1Hz LP:260Hz N:50Hz"),
/// by id; `None` for a filter of an unknown type.
fn filters(eeg: &Value) -> HashMap<String, Option<String>> {
    let hz = |x: f64| {
        if x == 0.0 {
            "DC".to_owned()
        } else {
            format!("{x}Hz")
        }
    };
    let mut filters = HashMap::new();
    for filter in at(eeg, &["Filters"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = text(filter, &["Id"]) else {
            continue;
        };
        let one = at(filter, &["FrequencyInHertz"]).and_then(Value::as_f64);
        let two = at(filter, &["FrequenciesInHertz"])
            .and_then(Value::as_array)
            .and_then(|f| Some((f.first()?.as_f64()?, f.get(1)?.as_f64()?)));
        let kind = text(filter, &["Type"]).map(str::to_ascii_lowercase);
        let part = match (kind.as_deref(), one, two) {
            (Some("lowpass"), Some(f), _) => Some(format!("LP:{}", hz(f))),
            (Some("highpass"), Some(f), _) => Some(format!("HP:{}", hz(f))),
            (Some("notch"), Some(f), _) => Some(format!("N:{}", hz(f))),
            (Some("bandpass"), _, Some((lo, hi))) => Some(format!("HP:{} LP:{}", hz(lo), hz(hi))),
            (Some("bandstop"), _, Some((lo, hi))) => Some(format!("N:{lo}-{hi}Hz")),
            _ => None,
        };
        filters.insert(id.to_owned(), part);
    }
    filters
}

/// The info view's fields: software, task, institution, amplifiers, caps, power line,
/// participants, references and grounds.
fn info(json: &Value, eeg: &Value, extra: &mut Vec<(String, String)>) {
    let mut push = |key: &str, value: String| {
        if !value.is_empty() {
            extra.push((key.to_owned(), value));
        }
    };
    let joined = |value: &Value, keys: &[&str], separator: &str| {
        keys.iter()
            .filter_map(|k| text(value, &[k]))
            .collect::<Vec<_>>()
            .join(separator)
    };
    let list = |path: &[&str]| -> Vec<&Value> {
        at(eeg, path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|v| v.is_object())
            .collect()
    };
    // Each item of a list prefixed by its participant, where it has one.
    let of = |item: &Value, text_of_item: String| match text(item, &["ParticipantId"]) {
        Some(p) => format!("{p}: {text_of_item}"),
        None => text_of_item,
    };
    if let Some(software) = at(json, &["RecordingSoftware"]) {
        push(
            "recording software",
            joined(software, &["Name", "Version"], " "),
        );
    }
    if let Some(task) = at(json, &["Task"]) {
        push("task", joined(task, &["Name"], ""));
        push("task description", joined(task, &["Description"], ""));
        push("task instructions", joined(task, &["Instructions"], ""));
    }
    if let Some(institution) = at(json, &["Institution"]) {
        push(
            "institution",
            joined(institution, &["Name", "Department", "Address"], ", "),
        );
    }
    let amplifiers: Vec<String> = list(&["Amplifiers"])
        .into_iter()
        .map(|a| {
            let model = joined(a, &["Model"], "");
            let serial = text(a, &["SerialNumber"]).map(|s| format!("serial {s}"));
            let text_of = match (model.is_empty(), serial) {
                (false, Some(s)) => format!("{model} ({s})"),
                (true, Some(s)) => s,
                (_, None) => model,
            };
            match text(a, &["Id"]) {
                Some(id) => format!("{id}: {text_of}"),
                None => text_of,
            }
        })
        .collect();
    push("amplifiers", amplifiers.join("; "));
    let caps: Vec<String> = list(&["Caps"])
        .into_iter()
        .map(|c| {
            let name = joined(c, &["CapManufacturer", "CapManufacturersModelName"], " ");
            let scheme = text(c, &["EEGPlacementScheme"]);
            of(
                c,
                match scheme {
                    Some(s) if name.is_empty() => s.to_owned(),
                    Some(s) => format!("{name}, {s}"),
                    None => name,
                },
            )
        })
        .collect();
    push("caps", caps.join("; "));
    if let Some(line) =
        at(eeg, &["DataSpecific", "PowerLineFrequencyInHertz"]).and_then(Value::as_f64)
    {
        push("power line frequency", format!("{line} Hz"));
    }
    let participants: Vec<String> = at(json, &["Participants"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = text(p, &["Id"])?;
            let mut details = Vec::new();
            if let Some(head) = at(p, &["HeadCircumferenceInCentiMeter"]).and_then(Value::as_f64) {
                details.push(format!("head circumference {head} cm"));
            }
            details.extend(text(p, &["ArtefactDescription"]).map(str::to_owned));
            Some(if details.is_empty() {
                id.to_owned()
            } else {
                format!("{id} ({})", details.join("; "))
            })
        })
        .collect();
    push("participants", participants.join(", "));
    for (key, section) in [("references", "References"), ("grounds", "Grounds")] {
        let items: Vec<String> = list(&[section])
            .into_iter()
            .filter_map(|r| {
                let name = text(r, &["Name"])?;
                let parts: Vec<String> = at(r, &["Composition"])
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| {
                        let electrode = text(part, &["Name"])?;
                        Some(match at(part, &["Factor"]).and_then(Value::as_f64) {
                            Some(f) if f != 1.0 => format!("{f} \u{d7} {electrode}"),
                            _ => electrode.to_owned(),
                        })
                    })
                    .collect();
                Some(of(
                    r,
                    if parts.is_empty() || parts == [name] {
                        name.to_owned()
                    } else {
                        format!("{name} = {}", parts.join(" + "))
                    },
                ))
            })
            .collect();
        push(key, items.join("; "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaling(coefficients: &str) -> Result<Scaling, String> {
        let json: Value =
            serde_json::from_str(&format!(r#"{{"Coefficients": {coefficients}}}"#)).unwrap();
        super::coefficients(&json)
    }

    #[test]
    fn coefficients_are_linear_up_to_two_terms_over_a_constant() {
        let linear = |factor, offset| Ok(Scaling::Linear { factor, offset });
        assert_eq!(scaling("null"), linear(1.0, 0.0));
        assert_eq!(scaling("{}"), linear(1.0, 0.0));
        assert_eq!(scaling(r#"{"Num": [0, 0.001]}"#), linear(0.001, 0.0));
        assert_eq!(
            scaling(r#"{"Num": [2, 4], "Denom": [2]}"#),
            linear(2.0, 1.0)
        );
        assert_eq!(
            scaling(r#"{"Num": [1, 3, 0], "Denom": [1, 0]}"#),
            linear(3.0, 1.0)
        );
        assert_eq!(scaling(r#"{"Num": [5]}"#), linear(0.0, 5.0));
        assert_eq!(scaling(r#"{"Num": null, "Denom": null}"#), linear(1.0, 0.0));
        assert_eq!(
            scaling(r#"{"Num": [0, 1], "Denom": [1, 0.5]}"#),
            Ok(Scaling::Rational)
        );
        assert_eq!(scaling(r#"{"Num": [0, 0, 1]}"#), Ok(Scaling::Rational));
        assert_eq!(scaling(r#"{"Denom": [0]}"#), Ok(Scaling::Rational));
        assert!(scaling(r#"{"Num": "1"}"#).is_err());
        assert!(scaling(r#"{"Num": [1, "x"]}"#).is_err());
    }

    #[test]
    fn versions_have_three_numbers() {
        assert_eq!(major("1.0.0"), Some(1));
        assert_eq!(major("12.3.4"), Some(12));
        assert_eq!(major("1.0"), None);
        assert_eq!(major("1.x.0"), None);
    }
}
