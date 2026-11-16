//! EDF/BDF header parsing: bytes to the recording model plus the record layout.

use crate::{
    Channel, ChannelId, ChannelKind, Grid, GridId, Header, ModelError, Quantity, SampleType,
    Timebase, Unit,
};

/// Widths of the general header fields, in order.
const GENERAL: [usize; 10] = [8, 80, 80, 8, 8, 8, 44, 8, 8, 4];
/// Widths of the per-signal fields; each field holds all signals in turn.
const SIGNAL: [usize; 10] = [16, 80, 8, 8, 8, 8, 8, 80, 8, 32];

/// The header is malformed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum EdfError {
    #[error("not an EDF or BDF file (version field {0:?})")]
    NotEdf(String),
    #[error("header truncated: it needs {needed} bytes, the file has {found}")]
    Truncated { needed: u64, found: u64 },
    #[error("invalid {field}: {value:?}")]
    InvalidField { field: String, value: String },
    #[error("signal {index} ({label:?}): {reason}")]
    InvalidSignal {
        index: usize,
        label: String,
        reason: String,
    },
    #[error("the file has no data signals, only annotations")]
    NoDataSignals,
    #[error("data record {record} {reason}")]
    InvalidRecord { record: u64, reason: String },
    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Where one signal's samples sit inside a data record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Byte offset from the start of the record.
    pub offset: u64,
    /// Samples per record.
    pub samples: u64,
}

/// The byte layout of the data records, for the reader.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordLayout {
    /// 2 (EDF, int16) or 3 (BDF, int24).
    pub bytes_per_sample: usize,
    /// EDF+D/BDF+D: records may be separated by gaps (given by their time-keeping TALs).
    pub discontinuous: bool,
    pub header_bytes: u64,
    pub record_bytes: u64,
    /// Complete records the reader may use (see the warnings when this differs from the
    /// header's count).
    pub n_records: u64,
    /// Seconds.
    pub record_duration: f64,
    /// One per data channel, in [`ChannelId`] order.
    pub channels: Vec<Block>,
    /// The annotation signals ("EDF Annotations"/"BDF Annotations"), in file order.
    pub annotations: Vec<Block>,
}

/// A parsed header.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedHeader {
    /// Data channels only. Grids are continuous; EDF+D gaps come from the TALs.
    pub header: Header,
    pub layout: RecordLayout,
    pub warnings: Vec<String>,
}

/// Parses the header at the start of `bytes`. `file_len` is the length of the whole file,
/// which decides how many records are usable.
///
/// Never panics, whatever the input.
pub fn parse_header(bytes: &[u8], file_len: u64) -> Result<ParsedHeader, EdfError> {
    let mut warnings = Vec::new();
    let mut general = Vec::with_capacity(GENERAL.len());
    let mut pos = 0;
    need(bytes, 256)?;
    for width in GENERAL {
        general.push(&bytes[pos..pos + width]);
        pos += width;
    }

    let version = general[0];
    let bdf = version[0] == 0xff;
    if !bdf && text(version) != "0" {
        return Err(EdfError::NotEdf(text(version)));
    }
    let reserved = text(general[6]);
    let plus = reserved.starts_with("EDF+") || reserved.starts_with("BDF+");
    let discontinuous = plus && reserved[4..].starts_with('D');
    let format = match (bdf, plus, discontinuous) {
        (false, false, _) => "EDF",
        (false, true, false) => "EDF+C",
        (false, true, true) => "EDF+D",
        (true, false, _) => "BDF",
        (true, true, false) => "BDF+C",
        (true, true, true) => "BDF+D",
    };

    let patient = text(general[1]);
    let recording = text(general[2]);
    let start_time = start_time(
        &text(general[3]),
        &text(general[4]),
        plus.then_some(recording.as_str()),
        &mut warnings,
    );

    let ns = integer("number of signals", general[9])?;
    if !(1..=9999).contains(&ns) {
        return Err(invalid("number of signals", general[9]));
    }
    let ns = ns as usize;
    let header_bytes = 256 * (ns as u64 + 1);
    let stated = integer("number of bytes in the header", general[5])?;
    if stated != header_bytes as i64 {
        return Err(EdfError::InvalidField {
            field: format!("number of bytes in the header (expected {header_bytes})"),
            value: text(general[5]),
        });
    }
    need(bytes, header_bytes)?;
    if file_len < header_bytes {
        return Err(EdfError::Truncated {
            needed: header_bytes,
            found: file_len,
        });
    }

    // Per-signal fields: column c of signal i.
    let column = |c: usize, i: usize| {
        let start = 256 + ns * SIGNAL[..c].iter().sum::<usize>() + i * SIGNAL[c];
        &bytes[start..start + SIGNAL[c]]
    };
    let bytes_per_sample = if bdf { 3 } else { 2 };
    let sample_type = if bdf {
        SampleType::I32
    } else {
        SampleType::I16
    };

    let mut channels = Vec::new();
    let mut blocks = Vec::new();
    let mut annotations = Vec::new();
    let mut record_bytes: u64 = 0;
    for i in 0..ns {
        let label = text(column(0, i));
        let field = |name: &str| format!("{name} of signal {i} ({label:?})");
        let samples = integer(&field("samples per record"), column(8, i))?;
        if !(0..=99_999_999).contains(&samples) {
            return Err(invalid(&field("samples per record"), column(8, i)));
        }
        let block = Block {
            offset: record_bytes,
            samples: samples as u64,
        };
        record_bytes += block.samples * bytes_per_sample as u64;

        if label == "EDF Annotations" || label == "BDF Annotations" {
            annotations.push(block);
            continue;
        }
        let signal_error = |reason: &str| EdfError::InvalidSignal {
            index: i,
            label: label.clone(),
            reason: reason.into(),
        };
        if samples == 0 {
            return Err(signal_error("no samples per record"));
        }
        let physical_min = float(&field("physical minimum"), column(3, i))?;
        let physical_max = float(&field("physical maximum"), column(4, i))?;
        let digital_min = integer(&field("digital minimum"), column(5, i))?;
        let digital_max = integer(&field("digital maximum"), column(6, i))?;
        if digital_min >= digital_max {
            return Err(signal_error("digital minimum is not below the maximum"));
        }
        if physical_min == physical_max {
            warnings.push(format!(
                "signal {i} ({label:?}): physical minimum equals maximum, so all values are {physical_min}"
            ));
        }
        let (dmin, dmax) = (digital_min as f64, digital_max as f64);
        let gain = (physical_max - physical_min) / (dmax - dmin);
        let offset = physical_min - gain * dmin;
        let unit = Unit::from_label(text(column(2, i)));
        let kind = kind(&label, &unit);
        channels.push(Channel {
            kind,
            unit,
            gain,
            offset,
            grid: GridId(0), // assigned below
            prefilter: text(column(7, i)),
            transducer: text(column(1, i)),
            physical_range: Some((physical_min, physical_max)),
            digital_range: Some((dmin, dmax)),
            // BioSemi keeps the trigger codes in the low 16 bits of the status channel; the
            // upper 8 are the amplifier's own (CMS in range, battery, ...).
            trigger_mask: (bdf && kind == ChannelKind::Stim).then_some(0xffff),
            events: true,
            in_stream: None,
            // TODO(later): duplicate labels stay as stored ("POL", "POL", ...). MNE renames
            // them "POL-0", "POL-1", ...; the Python round trip (bads by name) may need the
            // same names.
            name: label,
        });
        blocks.push(block);
    }
    if channels.is_empty() {
        return Err(EdfError::NoDataSignals);
    }

    let record_duration = float("record duration", general[8])?;
    if record_duration <= 0.0 {
        return Err(invalid("record duration", general[8]));
    }
    let n_records = usable_records(
        integer("number of records", general[7])
            .map_err(|_| invalid("number of records", general[7]))?,
        file_len - header_bytes,
        record_bytes,
        &mut warnings,
    )
    .ok_or_else(|| invalid("number of records", general[7]))?;

    // One grid per distinct samples-per-record, in order of first appearance.
    let mut grid_samples: Vec<u64> = Vec::new();
    let mut grid_channels: Vec<Vec<ChannelId>> = Vec::new();
    for (i, (channel, block)) in channels.iter_mut().zip(&blocks).enumerate() {
        let g = match grid_samples.iter().position(|&s| s == block.samples) {
            Some(g) => g,
            None => {
                grid_samples.push(block.samples);
                grid_channels.push(Vec::new());
                grid_samples.len() - 1
            }
        };
        channel.grid = GridId(g as u32);
        grid_channels[g].push(ChannelId(i as u32));
    }
    let grids = grid_samples
        .iter()
        .zip(grid_channels)
        .map(|(&samples, ids)| {
            let rate = samples as f64 / record_duration;
            Grid::continuous(
                Timebase::Regular { rate, start: 0.0 },
                n_records * samples,
                sample_type,
                ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ParsedHeader {
        header: Header {
            format,
            start_time,
            channels,
            grids,
            extra: vec![("patient".into(), patient), ("recording".into(), recording)],
            bad_channels: Vec::new(),
            streams: Vec::new(),
            utc_offset: None,
        },
        layout: RecordLayout {
            bytes_per_sample,
            discontinuous,
            header_bytes,
            record_bytes,
            n_records,
            record_duration,
            channels: blocks,
            annotations,
        },
        warnings,
    })
}

fn need(bytes: &[u8], needed: u64) -> Result<(), EdfError> {
    if (bytes.len() as u64) < needed {
        Err(EdfError::Truncated {
            needed,
            found: bytes.len() as u64,
        })
    } else {
        Ok(())
    }
}

/// Decodes a field as Latin-1 and trims spaces and NULs at both ends (as MNE does; fields
/// are left-aligned, so leading spaces carry no meaning).
fn text(field: &[u8]) -> String {
    let s: String = field.iter().map(|&b| char::from(b)).collect();
    s.trim_matches([' ', '\0']).to_owned()
}

fn invalid(field: &str, value: &[u8]) -> EdfError {
    EdfError::InvalidField {
        field: field.into(),
        value: text(value),
    }
}

fn integer(field: &str, value: &[u8]) -> Result<i64, EdfError> {
    text(value)
        .trim()
        .parse()
        .map_err(|_| invalid(field, value))
}

/// Parses a decimal number; a comma is accepted as the decimal separator.
fn float(field: &str, value: &[u8]) -> Result<f64, EdfError> {
    text(value)
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
        .ok_or_else(|| invalid(field, value))
}

/// The number of records to use, or `None` if the header's count is invalid.
fn usable_records(
    stated: i64,
    data_bytes: u64,
    record_bytes: u64,
    warnings: &mut Vec<String>,
) -> Option<u64> {
    let complete = data_bytes / record_bytes;
    let partial = data_bytes % record_bytes;
    let partial_note = if partial > 0 {
        format!(" and {partial} bytes of an incomplete one, which are ignored")
    } else {
        String::new()
    };
    match stated {
        -1 => {
            warnings.push(format!(
                "the number of records is unknown (-1); using the {complete} complete records \
                 in the file{partial_note}"
            ));
            Some(complete)
        }
        n if n < -1 => None,
        n if n as u64 > complete => {
            warnings.push(format!(
                "the file is truncated: the header lists {n} records, the file holds \
                 {complete}{partial_note}"
            ));
            Some(complete)
        }
        n => {
            let extra = data_bytes - n as u64 * record_bytes;
            if extra > 0 {
                warnings.push(format!(
                    "the file has {extra} bytes after its {n} records, which are ignored"
                ));
            }
            Some(n as u64)
        }
    }
}

/// Channel type from the label's EDF+ type prefix ("EEG Fpz-Cz", "SaO2 finger"), as MNE
/// reads them, and BioSemi's status channel. A magnetic unit makes a MEG channel whatever
/// the label (T: magnetometer, T/m: gradiometer). Anything else is EEG, as in MNE.
///
/// TODO(later): also recognize the type prefix without a space ("EOG-L", "EMG_chin",
/// "ECGII"), and decide whether the channel name keeps the full label or drops the prefix
/// ("L"). Until then such channels are EEG.
pub(crate) fn kind(label: &str, unit: &Unit) -> ChannelKind {
    match unit.quantity() {
        Some(Quantity::MagneticField) => return ChannelKind::Mag,
        Some(Quantity::FieldGradient) => return ChannelKind::Grad,
        _ => {}
    }
    let upper = label.trim().to_ascii_uppercase();
    if upper == "STATUS" || upper == "TRIGGER" {
        return ChannelKind::Stim;
    }
    match upper.split(' ').next() {
        Some("EOG") => ChannelKind::Eog,
        Some("ECG" | "EKG") => ChannelKind::Ecg,
        Some("EMG") => ChannelKind::Emg,
        Some("SEEG") => ChannelKind::Seeg,
        Some("ECOG") => ChannelKind::Ecog,
        Some("DBS") => ChannelKind::Dbs,
        Some("RESP") => ChannelKind::Resp,
        Some("BIO" | "SAO2") => ChannelKind::Bio,
        Some("STIM") => ChannelKind::Stim,
        Some("TEMP" | "MISC") => ChannelKind::Misc,
        _ => ChannelKind::Eeg,
    }
}

/// Parses `dd.mm.yy` and `hh.mm.ss`. Two-digit years are 1985-2084; for EDF+/BDF+ the
/// four-digit year in the recording field (`Startdate dd-MMM-yyyy`) wins, so years past 2084
/// work too. An invalid date gives a warning, not an error.
fn start_time(
    date: &str,
    time: &str,
    recording: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<jiff::civil::DateTime> {
    let parts = |s: &str| -> Option<[i64; 3]> {
        let mut it = s.split(['.', ':']).map(|p| p.trim().parse::<i64>().ok());
        let parsed = [it.next()??, it.next()??, it.next()??];
        it.next().is_none().then_some(parsed)
    };
    let long_year = recording.and_then(|r| {
        let date = r.strip_prefix("Startdate ")?.split(' ').next()?;
        date.split('-').nth(2)?.parse::<i64>().ok()
    });
    let (day, month, short_year) = match date.split('.').collect::<Vec<_>>()[..] {
        [d, m, y] => (
            d.trim().parse().ok(),
            m.trim().parse().ok(),
            y.trim().parse().ok(),
        ),
        _ => (None, None, None),
    };
    let year = long_year.or(short_year.map(|y: i64| if y >= 85 { 1900 + y } else { 2000 + y }));
    let result = (|| {
        let [h, mi, s] = parts(time)?;
        let narrow = |x: i64| i8::try_from(x).ok();
        jiff::civil::DateTime::new(
            i16::try_from(year?).ok()?,
            narrow(month?)?,
            narrow(day?)?,
            narrow(h)?,
            narrow(mi)?,
            narrow(s)?,
            0,
        )
        .ok()
    })();
    if result.is_none() {
        warnings.push(format!(
            "invalid start date or time {date:?} {time:?}; the start time is unknown"
        ));
    }
    result
}
