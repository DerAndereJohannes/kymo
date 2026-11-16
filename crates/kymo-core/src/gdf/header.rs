//! GDF header parsing: the fixed part, the part per channel and GDF 2's tag-length-value
//! fields, to the recording model plus the record layout.

use super::{GdfError, units};
use crate::decode::{ByteOrder, Decoder, Encoding};
use crate::edf::header::kind;
use crate::records::RecordBlock;
use crate::{Channel, ChannelId, ChannelKind, Grid, GridId, SampleType, Timebase, Unit};

/// A parsed header, with what the reader and the event table need.
pub(super) struct Parsed {
    /// The version number, e.g. 2.2 for "GDF 2.20".
    pub version: f64,
    pub start_time: Option<jiff::civil::DateTime>,
    /// Data channels only: channels without samples in the records are left out.
    pub channels: Vec<Channel>,
    /// One per data channel; decoders have no reference yet.
    pub blocks: Vec<RecordBlock>,
    pub grids: Vec<Grid>,
    /// For each channel of the file (numbered from 1 in the event table), its data channel.
    pub file_channels: Vec<Option<ChannelId>>,
    pub header_bytes: u64,
    pub record_bytes: u64,
    /// Complete records the reader may use.
    pub n_records: u64,
    /// Where the event table starts, if the file has room for one after the stated records.
    pub event_table: Option<u64>,
    pub extra: Vec<(String, String)>,
}

/// GDF 1 headers have 1.x versions below 1.90; 1.90 and later use GDF 2's layout.
const FIRST_GDF2: f64 = 1.90;

/// From GDF 2.40 bytes 254 and 255 hold the start time's offset to UTC in minutes (int16;
/// required from 2.50).
const FIRST_TIME_ZONE: f64 = 2.40;

/// Bounds-checked little-endian reads from the header.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn get<const N: usize>(&self, at: usize) -> [u8; N] {
        // Callers checked the header's length first.
        self.0
            .get(at..at + N)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; N])
    }
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.get(at))
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.get(at))
    }
    fn i32(&self, at: usize) -> i32 {
        i32::from_le_bytes(self.get(at))
    }
    fn i64(&self, at: usize) -> i64 {
        i64::from_le_bytes(self.get(at))
    }
    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.get(at))
    }
    fn f32(&self, at: usize) -> f32 {
        f32::from_le_bytes(self.get(at))
    }
    fn f64(&self, at: usize) -> f64 {
        f64::from_le_bytes(self.get(at))
    }
    fn text(&self, at: usize, len: usize) -> String {
        text(self.0.get(at..at + len).unwrap_or_default())
    }
}

/// A text field up to its first NUL: UTF-8 if valid, Latin-1 otherwise (GDF 1 writers put
/// "µV" in Latin-1), trimmed of spaces.
pub(super) fn text(field: &[u8]) -> String {
    let field = field.split(|&b| b == 0).next().unwrap_or_default();
    let s = match std::str::from_utf8(field) {
        Ok(s) => s.to_owned(),
        Err(_) => field.iter().map(|&b| char::from(b)).collect(),
    };
    s.trim_matches([' ', '\0']).to_owned()
}

fn invalid(field: &'static str, value: impl ToString) -> GdfError {
    GdfError::InvalidField {
        field,
        value: value.to_string(),
    }
}

fn need(bytes: &[u8], needed: u64) -> Result<(), GdfError> {
    if (bytes.len() as u64) < needed {
        return Err(GdfError::Truncated {
            needed,
            found: bytes.len() as u64,
        });
    }
    Ok(())
}

/// The encoding of a GDF type code.
fn encoding(code: u32) -> Result<Encoding, String> {
    Ok(match code {
        1 => Encoding::I8,
        2 => Encoding::U8,
        3 => Encoding::I16,
        4 => Encoding::U16,
        5 => Encoding::I32,
        6 => Encoding::U32,
        7 => Encoding::I64,
        8 => Encoding::U64,
        16 => Encoding::F32,
        17 => Encoding::F64,
        279 => Encoding::I24,
        535 => Encoding::U24,
        18 => return Err("float128 samples are not supported".into()),
        c if c > 255 && c < 511 => {
            return Err(format!("{}-bit integer samples are not supported", c - 255));
        }
        c if c > 511 && c < 767 => {
            return Err(format!(
                "unsigned {}-bit integer samples are not supported",
                c - 511
            ));
        }
        c => return Err(format!("unknown sample type {c}")),
    })
}

/// Parses the header at the start of `bytes`, the whole file.
///
/// Never panics, whatever the input.
pub(super) fn parse(bytes: &[u8], warnings: &mut Vec<String>) -> Result<Parsed, GdfError> {
    need(bytes, 256)?;
    let b = Bytes(bytes);
    let version_field = b.text(0, 8);
    let version: f64 = version_field
        .strip_prefix("GDF ")
        .and_then(|v| v.trim().parse().ok())
        .filter(|v: &f64| (1.0..3.0).contains(v))
        .ok_or_else(|| GdfError::NotGdf(version_field.clone()))?;
    let gdf2 = version >= FIRST_GDF2;

    let (header_bytes, ns) = if gdf2 {
        (u64::from(b.u16(184)) * 256, u64::from(b.u16(252)))
    } else {
        let h = b.i64(184);
        let h = u64::try_from(h).map_err(|_| invalid("header length", h))?;
        (h, u64::from(b.u32(252)))
    };
    let fixed_end = 256 * (ns + 1);
    if header_bytes < fixed_end {
        return Err(invalid(
            "header length",
            format!("{header_bytes} bytes, but {ns} channels need {fixed_end}"),
        ));
    }
    need(bytes, header_bytes)?;
    let ns = ns as usize;

    let (patient, recording) = if gdf2 {
        (b.text(8, 66), b.text(88, 64))
    } else {
        (b.text(8, 80), b.text(88, 80))
    };
    let start_time = if gdf2 {
        start_time_2(b.u64(168), warnings)
    } else {
        start_time_1(&b.text(168, 16), warnings)
    };

    let stated_records = b.i64(236);
    let (mut num, mut den) = (b.u32(244), b.u32(248));
    if num == 0 || den == 0 {
        warnings.push(format!(
            "the record duration {num}/{den} s is invalid; using 1 s, as MNE does"
        ));
        (num, den) = (1, 1);
    }

    // The per-channel part: each field holds all channels in turn.
    let mut column = 256;
    let mut field = |width: usize| {
        let at = column;
        column += width * ns;
        move |i: usize| at + width * i
    };
    let label_at = field(16);
    let transducer_at = field(80);
    let (dimension_at, code_at) = if gdf2 {
        let d = field(6);
        (d, Some(field(2)))
    } else {
        (field(8), None)
    };
    let pmin_at = field(8);
    let pmax_at = field(8);
    let dmin_at = field(8);
    let dmax_at = field(8);
    let (prefilter_at, filters_at) = if gdf2 {
        let _obsolete = field(68);
        let lowpass = field(4);
        let highpass = field(4);
        let notch = field(4);
        (None, Some((lowpass, highpass, notch)))
    } else {
        (Some(field(80)), None)
    };
    let spr_at = field(4);
    let type_at = field(4);

    let mut channels = Vec::new();
    let mut blocks = Vec::new();
    let mut file_channels = Vec::with_capacity(ns);
    let mut record_bytes: u64 = 0;
    let mut sparse = Vec::new();
    for i in 0..ns {
        let label = b.text(label_at(i), 16);
        let channel_error = |reason: String| GdfError::InvalidChannel {
            index: i,
            label: label.clone(),
            reason,
        };
        let spr = b.i32(spr_at(i));
        let spr =
            u64::try_from(spr).map_err(|_| channel_error(format!("{spr} samples per record")))?;
        if spr == 0 {
            // Sparse samples, stored in the event table: not read.
            sparse.push(label);
            file_channels.push(None);
            continue;
        }
        let encoding = encoding(b.u32(type_at(i))).map_err(channel_error)?;
        let offset = record_bytes;
        record_bytes = spr
            .checked_mul(encoding.size() as u64)
            .and_then(|n| n.checked_add(record_bytes))
            .ok_or_else(|| channel_error(format!("{spr} samples per record")))?;

        let (pmin, pmax) = (b.f64(pmin_at(i)), b.f64(pmax_at(i)));
        let (dmin, dmax) = if gdf2 {
            (b.f64(dmin_at(i)), b.f64(dmax_at(i)))
        } else {
            (b.i64(dmin_at(i)) as f64, b.i64(dmax_at(i)) as f64)
        };
        let gain = (pmax - pmin) / (dmax - dmin);
        let offset_value = pmin - gain * dmin;
        let (gain, offset_value) = if gain.is_finite() && gain != 0.0 && offset_value.is_finite() {
            (gain, offset_value)
        } else {
            warnings.push(format!(
                "channel {i} ({label:?}): physical range {pmin}..{pmax} over digital range \
                     {dmin}..{dmax} gives no scaling; reading the stored values as they are"
            ));
            (1.0, 0.0)
        };

        let unit_label = match code_at {
            Some(code_at) => {
                let code = b.u16(code_at(i));
                match units::label(code) {
                    Some(label) => label,
                    None => {
                        let text = b.text(dimension_at(i), 6);
                        if code != 0 {
                            warnings.push(format!(
                                "channel {i} ({label:?}): unknown physical dimension code \
                                 {code}; using {text:?}"
                            ));
                        }
                        text
                    }
                }
            }
            None => b.text(dimension_at(i), 8),
        };
        let unit = Unit::from_label(unit_label);
        // "EEG:C3", as BioSig writes the type, reads as EDF's "EEG C3".
        let kind = kind(&label.replacen(':', " ", 1), &unit);
        let prefilter = match (prefilter_at, filters_at) {
            (Some(at), _) => b.text(at(i), 80),
            (None, Some((lp, hp, notch))) => prefilter(b.f32(hp(i)), b.f32(lp(i)), b.f32(notch(i))),
            (None, None) => String::new(),
        };

        let mut decoder = Decoder::new(encoding, ByteOrder::Little);
        if kind != ChannelKind::STIM {
            // Overflow detection, as biosig reads GDF: samples at or beyond the digital
            // minimum and maximum are missing (the spec's invalid values, and the saturated
            // ones; biosig writes NaN as the type's smallest value). Stim channels keep their
            // codes; limits that aren't finite with DigMin < DigMax are ignored.
            decoder = decoder.with_limits(dmin, dmax);
        }
        // A Status channel holds BioSemi's status word, as in BDF files converted to GDF:
        // trigger codes in the low 16 bits, the amplifier's own status above. Float samples
        // get no mask, since masking a float32 can lose bits.
        let trigger_mask = (kind == ChannelKind::STIM
            && label.eq_ignore_ascii_case("status")
            && decoder.native() != SampleType::F32)
            .then_some(0xffff);
        file_channels.push(Some(ChannelId(channels.len() as u32)));
        channels.push(Channel {
            name: label,
            kind,
            unit,
            gain,
            offset: offset_value,
            grid: GridId(0), // assigned below
            prefilter,
            transducer: b.text(transducer_at(i), 80),
            physical_range: Some((pmin, pmax)),
            digital_range: Some((dmin, dmax)),
            trigger_mask,
            events: true,
            in_stream: None,
        });
        blocks.push(RecordBlock {
            offset,
            samples: spr,
            decoder,
        });
    }
    if !sparse.is_empty() {
        warnings.push(format!(
            "{} channels without samples in the data records (sparse samples in the event \
             table) are left out: {}",
            sparse.len(),
            sparse.join(", ")
        ));
    }
    if channels.is_empty() {
        return Err(GdfError::NoChannels);
    }

    let mut extra = vec![
        ("version".to_owned(), version_field[4..].trim().to_owned()),
        ("patient".to_owned(), patient),
        ("recording".to_owned(), recording),
    ];
    if version >= FIRST_TIME_ZONE {
        // Minutes east of UTC; the start time stays the local time the file stores.
        let minutes = i16::from_le_bytes(b.get(254));
        let (sign, minutes) = (if minutes < 0 { '-' } else { '+' }, minutes.unsigned_abs());
        extra.push((
            "time zone".to_owned(),
            format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60),
        ));
    }
    if gdf2 && header_bytes > fixed_end {
        let tags = &bytes[fixed_end as usize..header_bytes as usize];
        extra.extend(tags_3(tags, warnings));
    }

    let data_bytes = bytes.len() as u64 - header_bytes;
    let complete = data_bytes / record_bytes;
    let (n_records, event_table) = match stated_records {
        -1 => {
            warnings.push(format!(
                "the number of records is unknown (-1); using the {complete} complete records \
                 in the file, which leaves no room for an event table"
            ));
            (complete, None)
        }
        n if n < -1 => return Err(invalid("number of records", n)),
        n if n as u64 > complete => {
            warnings.push(format!(
                "the file is truncated: the header lists {n} records, the file holds \
                 {complete}; it has no event table"
            ));
            (complete, None)
        }
        n => {
            let end = header_bytes + n as u64 * record_bytes;
            (n as u64, (end < bytes.len() as u64).then_some(end))
        }
    };

    // One grid per samples per record and native type, in order of first appearance.
    let mut keys: Vec<(u64, SampleType)> = Vec::new();
    let mut members: Vec<Vec<ChannelId>> = Vec::new();
    for (i, (channel, block)) in channels.iter_mut().zip(&blocks).enumerate() {
        let key = (block.samples, block.decoder.native());
        let g = match keys.iter().position(|&k| k == key) {
            Some(g) => g,
            None => {
                keys.push(key);
                members.push(Vec::new());
                keys.len() - 1
            }
        };
        channel.grid = GridId(g as u32);
        members[g].push(ChannelId(i as u32));
    }
    let grids = keys
        .iter()
        .zip(members)
        .map(|(&(spr, native), ids)| {
            let rate = spr as f64 * f64::from(den) / f64::from(num);
            Grid::continuous(
                Timebase::Regular { rate, start: 0.0 },
                n_records * spr,
                native,
                ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Parsed {
        version,
        start_time,
        channels,
        blocks,
        grids,
        file_channels,
        header_bytes,
        record_bytes,
        n_records,
        event_table,
        extra,
    })
}

/// GDF 2's filters in EDF's notation ("HP:0.1Hz LP:100Hz N:50Hz"); values that aren't
/// positive and finite are left out.
fn prefilter(highpass: f32, lowpass: f32, notch: f32) -> String {
    [("HP", highpass), ("LP", lowpass), ("N", notch)]
        .into_iter()
        .filter(|(_, f)| f.is_finite() && *f > 0.0)
        .map(|(name, f)| format!("{name}:{f}Hz"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// GDF 1's start: `YYYYMMDDhhmmsscc` (cc in hundredths of a second, blank for 0); blank
/// for none.
fn start_time_1(text: &str, warnings: &mut Vec<String>) -> Option<jiff::civil::DateTime> {
    if text.is_empty() {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(r)?;
        if s.trim().is_empty() {
            return Some(0);
        }
        s.parse().ok()
    };
    let parsed = (|| {
        let narrow = |x: i64| i8::try_from(x).ok();
        jiff::civil::DateTime::new(
            i16::try_from(digits(0..4)?).ok()?,
            narrow(digits(4..6)?)?,
            narrow(digits(6..8)?)?,
            narrow(digits(8..10)?)?,
            narrow(digits(10..12)?)?,
            narrow(digits(12..14)?)?,
            i32::try_from(digits(14..16).unwrap_or(0) * 10_000_000).ok()?,
        )
        .ok()
    })();
    if parsed.is_none() {
        warnings.push(format!(
            "invalid start date {text:?}; the start time is unknown"
        ));
    }
    parsed
}

/// GDF 2's start: days since 0000-01-01 as fixed point with 32 fractional bits, as
/// MATLAB's `datenum` counts them (so 0001-01-01 is day 367); 0 for none. A step of the
/// fraction is about 20 µs, so the time of day is rounded to the coarsest of whole seconds,
/// milliseconds and microseconds that lies within half a step (12:00:00, not 11:59:59.99999).
fn start_time_2(value: u64, warnings: &mut Vec<String>) -> Option<jiff::civil::DateTime> {
    if value == 0 {
        return None;
    }
    let parsed = (|| {
        let days = i64::try_from(value >> 32).ok()? - 367;
        // The time of day in units of 2^-32 nanoseconds.
        const DAY_NS: u128 = 86_400_000_000_000;
        let exact = u128::from(value & 0xffff_ffff) * DAY_NS;
        let half_step = DAY_NS / 2;
        let nanos = [1_000_000_000u128, 1_000_000, 1_000]
            .into_iter()
            .find_map(|unit| {
                let scaled = unit << 32;
                let rounded = (exact + scaled / 2) / scaled * scaled;
                (rounded.abs_diff(exact) <= half_step).then_some(rounded >> 32)
            })
            .unwrap_or((exact + (1 << 31)) >> 32);
        let span = jiff::SignedDuration::from_secs(days.checked_mul(86_400)?)
            .checked_add(jiff::SignedDuration::from_nanos(i64::try_from(nanos).ok()?))?;
        jiff::civil::date(1, 1, 1)
            .at(0, 0, 0, 0)
            .checked_add(span)
            .ok()
    })();
    if parsed.is_none() {
        warnings.push(format!(
            "invalid start date {value:#x}; the start time is unknown"
        ));
    }
    parsed
}

/// GDF 2's tag-length-value fields (the header's third part), up to tag 0: only the
/// manufacturer (tag 3) is kept, as header fields; the others are skipped.
fn tags_3(mut bytes: &[u8], warnings: &mut Vec<String>) -> Vec<(String, String)> {
    let mut extra = Vec::new();
    while let Some((&tag, rest)) = bytes.split_first() {
        if tag == 0 {
            break;
        }
        let Some(len) = rest.first_chunk::<3>() else {
            warnings.push(format!("header tag {tag} is truncated; skipping the rest"));
            break;
        };
        let len = u32::from_le_bytes([len[0], len[1], len[2], 0]) as usize;
        let Some(value) = rest.get(3..3 + len) else {
            warnings.push(format!(
                "header tag {tag} needs {len} bytes, past the end of the header; skipping \
                 the rest"
            ));
            break;
        };
        if tag == 3 {
            // Manufacturer: name, model, version, serial number, each ending in a NUL.
            let mut parts = value.split(|&b| b == 0).map(text);
            for name in [
                "manufacturer",
                "model",
                "version (equipment)",
                "serial number",
            ] {
                if let Some(part) = parts.next().filter(|p| !p.is_empty()) {
                    extra.push((name.to_owned(), part));
                }
            }
        }
        bytes = &rest[3 + len..];
    }
    extra
}
