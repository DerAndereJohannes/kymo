//! The header file (`.vhdr`): data layout, channels, and the amplifier's filters.

use super::BrainVisionError;
use super::text::{Ini, Repairs, unescape};
use crate::channel::kind_from_name;
use crate::decode::{ByteOrder, Encoding};
use crate::{Channel, ChannelKind, GridId, Quantity, Unit};

/// How samples are ordered in the data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Orientation {
    /// Sample-major: all channels of a sample, then the next sample.
    Multiplexed,
    /// Channel-major: all samples of a channel, then the next channel.
    Vectorized,
}

/// What the header says about the recording.
#[derive(Debug)]
pub(super) struct ParsedHeader {
    /// `DataFile` as written (`$b` replaced by the header's base name).
    pub(super) data_file: String,
    /// `MarkerFile` as written, if there is one.
    pub(super) marker_file: Option<String>,
    pub(super) orientation: Orientation,
    pub(super) encoding: Encoding,
    /// Little-endian, as the spec has it, unless `UseBigEndianOrder=YES` (a key of
    /// Analyzer's generic data reader).
    pub(super) order: ByteOrder,
    /// Hz.
    pub(super) rate: f64,
    /// `DataPoints`, if given.
    pub(super) data_points: Option<u64>,
    /// In grid 0.
    pub(super) channels: Vec<Channel>,
    pub(super) extra: Vec<(String, String)>,
}

const COMMON: &str = "Common Infos";
const BINARY: &str = "Binary Infos";
const CHANNELS: &str = "Channel Infos";

/// More channels than any amplifier has; a header asking for more is broken.
const MAX_CHANNELS: usize = 1 << 16;

/// Parses a header file's text. `base` is the header's file name without extension, which
/// `$b` in file names stands for.
pub(super) fn parse(
    ini: &Ini,
    base: &str,
    warnings: &mut Vec<String>,
) -> Result<ParsedHeader, BrainVisionError> {
    let version = version(&ini.first_line);
    if version.is_none() {
        warnings.push(format!(
            "unknown header version in {:?}; read as version 1.0",
            ini.first_line
        ));
    }
    let required = |section: &'static str, key: &'static str| {
        ini.get(section, key)
            .ok_or(BrainVisionError::Missing { section, key })
    };
    let invalid = |key: &'static str, value: &str| BrainVisionError::Invalid {
        key,
        value: value.to_owned(),
    };

    let data_type = ini.get(COMMON, "DataType").unwrap_or("TIMEDOMAIN");
    if !data_type.eq_ignore_ascii_case("TIMEDOMAIN") {
        return Err(BrainVisionError::Unsupported(format!(
            "data type {data_type:?} (only time-domain data is read)"
        )));
    }
    let data_format = required(COMMON, "DataFormat")?;
    if data_format.eq_ignore_ascii_case("ASCII") {
        return Err(BrainVisionError::Unsupported(
            "ASCII data (only binary data is read)".into(),
        ));
    }
    if !data_format.eq_ignore_ascii_case("BINARY") {
        return Err(invalid("DataFormat", data_format));
    }
    let orientation = match required(COMMON, "DataOrientation")? {
        o if o.eq_ignore_ascii_case("MULTIPLEXED") => Orientation::Multiplexed,
        o if o.eq_ignore_ascii_case("VECTORIZED") => Orientation::Vectorized,
        o => return Err(invalid("DataOrientation", o)),
    };
    let encoding = match required(BINARY, "BinaryFormat")? {
        f if f.eq_ignore_ascii_case("INT_16") => Encoding::I16,
        f if f.eq_ignore_ascii_case("UINT_16") => Encoding::U16,
        f if f.eq_ignore_ascii_case("INT_32") => Encoding::I32,
        f if f.eq_ignore_ascii_case("IEEE_FLOAT_32") => Encoding::F32,
        f => {
            return Err(BrainVisionError::Unsupported(format!(
                "binary format {f:?} (INT_16, UINT_16, INT_32 and IEEE_FLOAT_32 are read)"
            )));
        }
    };
    let order = match ini.get(BINARY, "UseBigEndianOrder") {
        Some(v) if v.eq_ignore_ascii_case("YES") => ByteOrder::Big,
        Some(v) if v.is_empty() || v.eq_ignore_ascii_case("NO") => ByteOrder::Little,
        Some(v) => {
            warnings.push(format!(
                "invalid UseBigEndianOrder {v:?} (YES or NO); read as little-endian"
            ));
            ByteOrder::Little
        }
        None => ByteOrder::Little,
    };
    let interval = required(COMMON, "SamplingInterval")?;
    let rate = interval
        .parse::<f64>()
        .ok()
        .filter(|&us| us.is_finite() && us > 0.0)
        .map(|us| 1e6 / us)
        .filter(|rate| rate.is_finite())
        .ok_or_else(|| invalid("SamplingInterval", interval))?;
    let count = required(COMMON, "NumberOfChannels")?;
    let n = count
        .parse::<usize>()
        .ok()
        .filter(|&n| (1..=MAX_CHANNELS).contains(&n))
        .ok_or_else(|| invalid("NumberOfChannels", count))?;
    let data_points = match ini.get(COMMON, "DataPoints") {
        Some(points) => Some(
            points
                .parse::<u64>()
                .map_err(|_| invalid("DataPoints", points))?,
        ),
        None => None,
    };
    let file_name = |name: &str| name.replace("$b", base);
    let data_file = file_name(required(COMMON, "DataFile")?);
    let marker_file = ini
        .get(COMMON, "MarkerFile")
        .filter(|name| !name.is_empty())
        .map(file_name);

    let channels = channels(ini, n, warnings)?;

    let mut extra = Vec::new();
    let mut push = |key: &str, value: &str| extra.push((key.to_owned(), value.to_owned()));
    push(
        "version",
        match version {
            Some(2) => "2.0",
            _ => "1.0",
        },
    );
    if let Some(created) = ini.preamble.first().filter(|c| !c.is_empty()) {
        push("created", created);
    }
    push("data file", &data_file);
    if let Some(marker_file) = &marker_file {
        push("marker file", marker_file);
    }
    push(
        "orientation",
        match orientation {
            Orientation::Multiplexed => "multiplexed",
            Orientation::Vectorized => "vectorized",
        },
    );
    push("binary format", required(BINARY, "BinaryFormat")?);
    if order == ByteOrder::Big {
        push("byte order", "big-endian");
    }
    if let Some(codepage) = ini.get(COMMON, "Codepage") {
        push("codepage", codepage);
    }

    Ok(ParsedHeader {
        data_file,
        marker_file,
        orientation,
        encoding,
        order,
        rate,
        data_points,
        channels,
        extra,
    })
}

/// The format version of an identification line such as "Brain Vision Data Exchange Header
/// File Version 1.0" or "BrainVision Core Data Format 2.0 Header File, Version 2.0".
fn version(line: &str) -> Option<u8> {
    match line.split_whitespace().last()? {
        "1.0" => Some(1),
        "2.0" => Some(2),
        _ => None,
    }
}

/// The channels `Ch1` to `Ch{n}`, with kinds, units and filters.
fn channels(
    ini: &Ini,
    n: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<Channel>, BrainVisionError> {
    let mut lines: Vec<Option<&str>> = vec![None; n];
    let mut ignored = 0;
    for (key, value) in ini.section(CHANNELS) {
        match channel_number(key) {
            Some(k) if k <= n => {
                if lines[k - 1].is_none() {
                    lines[k - 1] = Some(value);
                }
            }
            Some(_) => ignored += 1,
            None => {}
        }
    }
    if ignored > 0 {
        warnings.push(format!(
            "{ignored} channel lines beyond NumberOfChannels ({n}) are ignored"
        ));
    }
    let coordinates = coordinates(ini, n);
    // Radius 0 is an unknown position (A2-Radius): it tells a channel off the scalp only
    // where the montage places others.
    let placed = coordinates.iter().any(|r| r.is_some_and(|r| r != 0.0));
    let mut channels = Vec::with_capacity(n);
    let mut names = Repairs::new("channel names");
    let mut units = Repairs::new("channel units");
    for (i, line) in lines.into_iter().enumerate() {
        let Some(line) = line else {
            return Err(BrainVisionError::MissingChannel(i + 1));
        };
        let mut fields = line.split(',');
        let name = names.apply(unescape(fields.next().unwrap_or_default().trim()));
        let name = if name.is_empty() {
            format!("Ch{}", i + 1)
        } else {
            name
        };
        let _reference = fields.next();
        let resolution = fields.next().unwrap_or_default().trim();
        let gain = if resolution.is_empty() {
            1.0
        } else {
            resolution
                .parse::<f64>()
                .ok()
                .filter(|r| r.is_finite())
                .ok_or_else(|| BrainVisionError::InvalidChannel {
                    index: i + 1,
                    name: name.clone(),
                    reason: format!("invalid resolution {resolution:?}"),
                })?
        };
        let unit = fields.next().unwrap_or_default().trim();
        // The spec's default.
        let unit = match unit {
            "" => "\u{b5}V".to_owned(),
            u => units.apply(u.to_owned()),
        };
        let unit = Unit::from_label(unit);
        let kind = match unit.quantity() {
            Some(Quantity::Voltage) => {
                let kind = kind_from_name(&name);
                // A scalp channel the montage can't place, where it places others, is not on
                // the scalp.
                if kind == ChannelKind::EEG && placed && coordinates[i] == Some(0.0) {
                    ChannelKind::MISC
                } else {
                    kind
                }
            }
            Some(Quantity::MagneticField) => ChannelKind::MAG,
            Some(Quantity::FieldGradient) => ChannelKind::GRAD,
            _ => ChannelKind::MISC,
        };
        channels.push(Channel {
            kind,
            unit,
            gain,
            ..Channel::new(name, GridId(0))
        });
    }
    names.warn(warnings);
    units.warn(warnings);
    let filters = filters(&ini.comment, &channels);
    for (channel, filter) in channels.iter_mut().zip(filters) {
        channel.prefilter = filter;
    }
    Ok(channels)
}

/// `k` of a `Ch{k}` key, 1-based.
fn channel_number(key: &str) -> Option<usize> {
    let digits = key
        .get(..2)?
        .eq_ignore_ascii_case("ch")
        .then(|| &key[2..])?;
    digits.parse().ok().filter(|&k| k >= 1)
}

/// The radius of each channel's position in `[Coordinates]`, if it has one.
fn coordinates(ini: &Ini, n: usize) -> Vec<Option<f64>> {
    let mut radii = vec![None; n];
    for (key, value) in ini.section("Coordinates") {
        if let Some(k) = channel_number(key).filter(|&k| k <= n) {
            radii[k - 1] = value
                .split(',')
                .next()
                .and_then(|r| r.trim().parse::<f64>().ok());
        }
    }
    radii
}

/// Each channel's filters, in EDF's notation ("HP:0.016Hz LP:140Hz N:50Hz"), from the
/// amplifier tables Recorder writes into the comment: the hardware filters of the
/// "Channels" table and, if enabled, the "Software Filters" applied while recording. Empty
/// for channels these tables don't describe; tables of another shape are ignored.
fn filters(comment: &str, channels: &[Channel]) -> Vec<String> {
    let lines: Vec<&str> = comment.lines().collect();
    let mut hardware = vec![None; channels.len()];
    let mut software = vec![None; channels.len()];
    for (at, line) in lines.iter().enumerate() {
        let title = line.trim();
        let underlined = lines
            .get(at + 1)
            .is_some_and(|l| l.trim().starts_with("---") || l.trim().starts_with("==="));
        if !underlined {
            continue;
        }
        let rows = &lines[at + 2..];
        if title == "Channels" {
            table(rows, channels, true, &mut hardware);
        } else if title
            .replace(' ', "")
            .eq_ignore_ascii_case("SoftwareFilters")
        {
            table(rows, channels, false, &mut software);
        }
    }
    hardware
        .into_iter()
        .zip(software)
        .map(|(hardware, software)| match (hardware, software) {
            (Some(h), Some(s)) => format!("{h}; software {s}"),
            (Some(h), None) => h,
            (None, Some(s)) => format!("software {s}"),
            (None, None) => String::new(),
        })
        .collect()
}

/// Reads one filter table: a `#` header line naming the columns, then a row per channel
/// up to a blank line. Rows of the hardware table repeat the channel's name, physical
/// channel, resolution and unit before the filters; rows of the software table hold only
/// the filters.
fn table(rows: &[&str], channels: &[Channel], named: bool, out: &mut [Option<String>]) {
    let Some(header) = rows.first().filter(|h| h.trim_start().starts_with('#')) else {
        return;
    };
    let low_in_seconds = if header.contains("Low Cutoff [s]") {
        true
    } else if header.contains("Low Cutoff [Hz]") {
        false
    } else {
        return;
    };
    for row in &rows[1..] {
        if row.trim().is_empty() {
            break;
        }
        let row = row.trim_start();
        let Some((number, rest)) = row.split_once(char::is_whitespace) else {
            continue;
        };
        let Some(k) = number
            .parse::<usize>()
            .ok()
            .filter(|&k| (1..=out.len()).contains(&k))
        else {
            continue;
        };
        let rest = rest.trim_start();
        let filters = if named {
            // Names may hold spaces, so match the channel's name rather than splitting it.
            let Some(after) = rest.strip_prefix(channels[k - 1].name.as_str()) else {
                continue;
            };
            let columns: Vec<&str> = after.split_whitespace().collect();
            // Physical channel, resolution, unit, then the filters.
            columns.get(3..6).map(<[&str]>::to_vec)
        } else {
            let columns: Vec<&str> = rest.split_whitespace().collect();
            columns.get(..3).map(<[&str]>::to_vec)
        };
        if let Some([low, high, notch]) = filters.as_deref() {
            out[k - 1] = filter_text(low, high, notch, low_in_seconds);
        }
    }
}

/// "HP:… LP:… N:…" from a table's cells; `None` if a cell is neither a number nor
/// "DC"/"Off".
fn filter_text(low: &str, high: &str, notch: &str, low_in_seconds: bool) -> Option<String> {
    let number = |cell: &str| cell.replace(',', ".").parse::<f64>().ok();
    let off = |cell: &str| cell.eq_ignore_ascii_case("DC") || cell.eq_ignore_ascii_case("Off");
    let mut parts = Vec::new();
    if off(low) {
        parts.push("HP:DC".to_owned());
    } else {
        let value = number(low).filter(|v| *v > 0.0)?;
        let hz = if low_in_seconds {
            // A time constant τ: the cutoff is 1 / (2πτ).
            1.0 / (std::f64::consts::TAU * value)
        } else {
            value
        };
        parts.push(format!("HP:{}Hz", significant(hz)));
    }
    if !off(high) {
        parts.push(format!("LP:{}Hz", significant(number(high)?)));
    }
    if !off(notch) {
        parts.push(format!("N:{}Hz", significant(number(notch)?)));
    }
    Some(parts.join(" "))
}

/// `x` rounded to 3 significant digits, without trailing zeros.
fn significant(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let decimals = (2 - x.abs().log10().floor() as i32).max(0) as usize;
    let text = format!("{x:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}
