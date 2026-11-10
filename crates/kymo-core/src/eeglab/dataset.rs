//! The `EEG` struct: rate, size, channels, events and where the samples are.

use super::EeglabError;
use super::mat::{Numeric, Struct, Value, Variables};
use crate::channel::kind_from_name;
use crate::{AnnotationData, Channel, ChannelKind, GridId, Origin, Unit};

/// More channels than any recording has; a dataset claiming more is broken.
const MAX_CHANNELS: u64 = 1 << 20;

/// Where the samples are.
#[derive(Debug)]
pub(super) enum Data<'a> {
    /// In a float32 file, named as the dataset names it (`EEG.data` or `EEG.datfile`).
    File(String),
    /// In the `.set`: a numeric array of nbchan × pnts.
    Embedded(Numeric<'a>),
}

/// What the dataset says about the recording.
#[derive(Debug)]
pub(super) struct Dataset<'a> {
    /// Hz.
    pub(super) rate: f64,
    /// Samples per channel.
    pub(super) samples: u64,
    pub(super) data: Data<'a>,
    /// In grid 0, `nbchan` of them.
    pub(super) channels: Vec<Channel>,
    pub(super) annotations: Vec<AnnotationData>,
    pub(super) extra: Vec<(String, String)>,
}

/// The dataset's fields: those of the `EEG` struct (or of a nested `EEG.EEG`), or the
/// file's variables, as newer EEGLAB versions save them.
enum Fields<'a> {
    Struct(Struct<'a>),
    Variables(Variables<'a>),
}

impl<'a> Fields<'a> {
    fn get(&self, name: &str) -> Result<Option<Value<'a>>, EeglabError> {
        match self {
            Fields::Struct(s) => s.field(0, name),
            Fields::Variables(v) => v.get(name).map(|e| e.value()).transpose(),
        }
    }
}

/// Reads the dataset from a MAT-file's bytes.
pub(super) fn read<'a>(
    bytes: &'a [u8],
    warnings: &mut Vec<String>,
) -> Result<Dataset<'a>, EeglabError> {
    let variables = Variables::read(bytes)?;
    let compressed = variables.compressed > 0;
    // A field that is missing may be in a compressed variable.
    let missing = |field: &'static str| {
        if compressed {
            EeglabError::Compressed
        } else {
            EeglabError::Missing(field)
        }
    };
    if variables.get("ALLEEG").is_some() {
        return Err(EeglabError::AllEeg);
    }
    let fields = match variables.get("EEG") {
        Some(eeg) => match eeg.value()? {
            Value::Struct(s) if s.len() == 1 => match s.field(0, "EEG")? {
                Some(Value::Struct(inner)) if inner.len() == 1 => Fields::Struct(inner),
                _ => Fields::Struct(s),
            },
            Value::Struct(s) => {
                return Err(EeglabError::Invalid {
                    field: "EEG",
                    value: format!("a struct array of {} datasets", s.len()),
                });
            }
            _ => return Err(EeglabError::NotEeglab),
        },
        None if ["nbchan", "pnts", "srate"]
            .iter()
            .all(|n| variables.get(n).is_none()) =>
        {
            return Err(if compressed {
                EeglabError::Compressed
            } else {
                EeglabError::NotEeglab
            });
        }
        None => Fields::Variables(variables),
    };

    let count = |field: &'static str| -> Result<Option<u64>, EeglabError> {
        let Some(value) = fields.get(field)? else {
            return Ok(None);
        };
        let n = value
            .scalar()
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n < 2f64.powi(53))
            .ok_or_else(|| EeglabError::Invalid {
                field,
                value: describe(&value),
            })?;
        Ok(Some(n as u64))
    };
    let trials = count("trials")?.unwrap_or(1);
    if trials > 1 {
        return Err(EeglabError::Epoched(trials));
    }
    let nbchan = count("nbchan")?.ok_or_else(|| missing("nbchan"))?;
    if !(1..=MAX_CHANNELS).contains(&nbchan) {
        return Err(EeglabError::Invalid {
            field: "nbchan",
            value: nbchan.to_string(),
        });
    }
    let nbchan = nbchan as usize;
    let samples = count("pnts")?.ok_or_else(|| missing("pnts"))?;
    let srate = fields.get("srate")?.ok_or_else(|| missing("srate"))?;
    let rate = srate
        .scalar()
        .filter(|r| r.is_finite() && *r > 0.0)
        .ok_or_else(|| EeglabError::Invalid {
            field: "srate",
            value: describe(&srate),
        })?;

    let data = match fields.get("data")? {
        Some(Value::Char(name)) if !name.trim().is_empty() => Data::File(name.trim().to_owned()),
        Some(Value::Numeric(n)) if n.len() > 0 => Data::Embedded(n),
        Some(other) if !other.is_empty() => {
            return Err(EeglabError::Invalid {
                field: "data",
                value: describe(&other),
            });
        }
        _ => match fields.get("datfile")?.and_then(|v| v.text()) {
            Some(name) if !name.trim().is_empty() => Data::File(name.trim().to_owned()),
            _ => return Err(missing("data")),
        },
    };
    match &data {
        Data::File(name) if name.to_ascii_lowercase().ends_with(".dat") => {
            return Err(EeglabError::Unsupported(format!(
                "EEGLAB's old data format ({name}); save the dataset again from EEGLAB"
            )));
        }
        Data::Embedded(n) => check_embedded(n, nbchan, samples)?,
        Data::File(_) => {}
    }

    let channels = channels(fields.get("chanlocs")?, nbchan, warnings)?;
    let annotations = events(fields.get("event")?, rate, warnings)?;
    let mut extra = Vec::new();
    for field in ["setname", "subject", "group", "condition", "session", "ref"] {
        if let Some(text) = fields.get(field)?.as_ref().and_then(text_or_number)
            && !text.trim().is_empty()
        {
            extra.push((field.to_owned(), text.trim().to_owned()));
        }
    }
    if let Data::File(name) = &data {
        extra.push(("data file".to_owned(), name.clone()));
    }
    Ok(Dataset {
        rate,
        samples,
        data,
        channels,
        annotations,
        extra,
    })
}

/// Samples in the `.set` must be a real nbchan × pnts array.
fn check_embedded(n: &Numeric, nbchan: usize, samples: u64) -> Result<(), EeglabError> {
    if n.complex {
        return Err(EeglabError::Unsupported("complex samples".into()));
    }
    let rows = n.dims.first().copied().unwrap_or(0);
    let columns = n
        .dims
        .iter()
        .skip(1)
        .fold(1u64, |p, &d| p.saturating_mul(d as u64));
    let fits = (rows == nbchan && columns == samples)
        || (nbchan == 1 && n.len() as u64 == samples && columns == 1);
    if !fits {
        return Err(EeglabError::Invalid {
            field: "data",
            value: format!(
                "an array of {:?} for {nbchan} channels of {samples} samples",
                n.dims
            ),
        });
    }
    Ok(())
}

/// A short description of a value, for errors.
fn describe(value: &Value) -> String {
    match value {
        Value::Numeric(n) if n.len() == 1 => n.get(0).map_or("?".into(), number),
        Value::Numeric(n) => format!("a numeric array of {:?}", n.dims),
        Value::Char(s) => format!("{s:?}"),
        Value::Struct(s) => format!("a struct of {:?}", s.dims),
        Value::Cell(_) => "a cell array".into(),
        Value::Other => "an array of a class kymo doesn't read".into(),
    }
}

/// A number as text, the shortest that reads back the same: whole numbers without
/// decimals ("12", as EEGLAB shows event codes, and MNE too), never an exponent.
fn number(x: f64) -> String {
    format!("{x}")
}

/// The text of a char array, or a number as text.
fn text_or_number(value: &Value) -> Option<String> {
    value.text().or_else(|| value.scalar().map(number))
}

/// `nbchan` channels, named and typed from `chanlocs` where it has them.
fn channels(
    chanlocs: Option<Value>,
    nbchan: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<Channel>, EeglabError> {
    let chanlocs = match chanlocs {
        Some(Value::Struct(s)) if s.len() > 0 => Some(s),
        _ => None,
    };
    if let Some(s) = &chanlocs
        && s.len() != nbchan
    {
        warnings.push(format!(
            "chanlocs describes {} channels, but the dataset has {nbchan}; {}",
            s.len(),
            if s.len() < nbchan {
                "the others get default names"
            } else {
                "the rest are ignored"
            }
        ));
    }
    let mut unknown: Vec<String> = Vec::new();
    let mut channels = Vec::with_capacity(nbchan);
    for k in 0..nbchan {
        let field = |name: &str| -> Result<Option<String>, EeglabError> {
            match &chanlocs {
                Some(s) if k < s.len() => Ok(s.field(k, name)?.as_ref().and_then(text_or_number)),
                _ => Ok(None),
            }
        };
        let label = field("labels")?.unwrap_or_default().trim().to_owned();
        let name = if label.is_empty() {
            format!("EEG {k:03}")
        } else {
            label
        };
        let kind_name = field("type")?.unwrap_or_default().trim().to_owned();
        let kind = match ChannelKind::from_mne(&kind_name) {
            Some(kind) => kind,
            None => {
                if !kind_name.is_empty() && !unknown.contains(&kind_name) {
                    unknown.push(kind_name);
                }
                kind_from_name(&name)
            }
        };
        let (unit, gain) = scaling(kind);
        channels.push(Channel {
            kind,
            unit: Unit::from_label(unit),
            gain,
            ..Channel::new(name, GridId(0))
        });
    }
    if !unknown.is_empty() {
        let types: Vec<String> = unknown.iter().map(|t| format!("{t:?}")).collect();
        warnings.push(format!(
            "channel types that aren't MNE's ({}) are read from the channel names",
            types.join(", ")
        ));
    }
    Ok(channels)
}

/// Unit and gain of a channel of `kind`: EEGLAB stores µV, which MNE reads as stored × 1e-6
/// in each type's SI unit; stim channels hold codes.
fn scaling(kind: ChannelKind) -> (&'static str, f64) {
    match kind {
        ChannelKind::Mag | ChannelKind::RefMeg => ("T", 1e-6),
        ChannelKind::Grad => ("T/m", 1e-6),
        ChannelKind::Stim => ("", 1.0),
        _ => ("\u{b5}V", 1.0),
    }
}

/// The events as annotations, as MNE reads them: onset (latency − 1) / rate, duration /
/// rate, the type as description.
fn events(
    event: Option<Value>,
    rate: f64,
    warnings: &mut Vec<String>,
) -> Result<Vec<AnnotationData>, EeglabError> {
    let Some(Value::Struct(s)) = event else {
        return Ok(Vec::new());
    };
    let (mut no_latency, mut before, mut bad_duration) = (0, 0, 0);
    let mut annotations = Vec::with_capacity(s.len());
    for k in 0..s.len() {
        let Some(latency) = s
            .field(k, "latency")?
            .and_then(|v| v.scalar())
            .filter(|l| l.is_finite())
        else {
            no_latency += 1;
            continue;
        };
        let onset = (latency - 1.0) / rate;
        if onset < 0.0 {
            before += 1;
            continue;
        }
        let duration = match s.field(k, "duration")? {
            None => 0.0,
            Some(v) if v.is_empty() => 0.0,
            Some(v) => match v.scalar() {
                Some(d) if d.is_finite() && d >= 0.0 => d / rate,
                _ => {
                    bad_duration += 1;
                    0.0
                }
            },
        };
        let description = s
            .field(k, "type")?
            .as_ref()
            .and_then(text_or_number)
            .unwrap_or_default();
        annotations.push(AnnotationData {
            onset,
            duration,
            description,
            channels: Vec::new(),
            origin: Origin::File,
        });
    }
    if no_latency > 0 {
        warnings.push(format!(
            "{no_latency} events without a latency (or a NaN one) are ignored"
        ));
    }
    if before > 0 {
        warnings.push(format!(
            "{before} events before the first sample are ignored"
        ));
    }
    if bad_duration > 0 {
        warnings.push(format!(
            "{bad_duration} events whose duration isn't a number >= 0 are read as points in time"
        ));
    }
    Ok(annotations)
}
