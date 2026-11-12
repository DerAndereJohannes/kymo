//! Opening FIFF raw recordings: the measurement info, the table of data buffers, and the
//! block reader over read-only maps of the file.

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::FiffError;
use super::info::{self, Info};
use super::tag::{self, Node, Tag, block, kind, types};
use crate::decode::{ByteOrder, Decoder, Encoding, frames_into};
use crate::mapped::{self, map_read_only};
use crate::{
    BlockReader, Channel, ChannelId, ChannelKind, Grid, GridId, Header, OpenError, Opened,
    ReadError, Sample, SampleType, SamplesMut, Timebase,
};

/// One data buffer: a run of whole frames of one encoding.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Buffer {
    /// Its first sample in the grid.
    start: u64,
    /// Samples (frames) in it.
    len: u64,
    /// The file (split part) holding it.
    part: usize,
    /// Byte position of its first frame.
    offset: usize,
    encoding: Encoding,
}

/// Reads samples straight from the mapped files, buffer by buffer.
#[derive(Debug)]
pub struct FiffReader {
    /// Each file of the recording, mapped, with the file for reading ahead.
    parts: Vec<(Mmap, File)>,
    /// Sorted by start, not overlapping.
    buffers: Vec<Buffer>,
    channels: usize,
    native: SampleType,
    decoding: Decoding,
}

/// How buffers decode to the grid's native type.
#[derive(Debug)]
enum Decoding {
    /// Every buffer has the same encoding: a decoder per channel (each with its own
    /// reference, for float64).
    Uniform(Vec<Decoder>),
    /// Buffers of different encodings, decoded value by value to the type that holds them
    /// all, each channel relative to its reference (0 unless some buffer is float64).
    Mixed(Vec<f64>),
}

/// Opens a FIFF raw recording. The file is memory-mapped, never loaded; the tags are walked
/// once at open to find the measurement info and the data buffers.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| OpenError::Io { path, source }
    };
    let error = |source| OpenError::Fiff {
        path: path.to_owned(),
        source,
    };
    let mut warnings = Vec::new();
    let file = File::open(path).map_err(io(path))?;
    let map = map_read_only(&file).map_err(io(path))?;
    if !tag::is_fiff(&map) {
        return Err(error(FiffError::NotFiff));
    }
    let tree = tag::tree(&map, &mut warnings);
    let part = Part::read(&tree, &map, &mut warnings).map_err(error)?;
    let info = part.info;
    let first_samp = part.first_samp;
    let parts = vec![(map, file)];
    let sources: Vec<PathBuf> = vec![path.to_owned()];

    let mut buffers = part.buffers;
    let len = buffers.last().map_or(0, |b| b.start + b.len);
    if buffers.is_empty() {
        warnings.push("the file has no data buffers".to_owned());
    }
    let segments = segments(&buffers);
    let n = info.channels.len();

    // The grid's type holds every buffer's; a reference per channel for float64.
    let mut encodings: Vec<Encoding> = buffers.iter().map(|b| b.encoding).collect();
    encodings.sort_by_key(|e| e.size());
    encodings.dedup();
    let native = match encodings.as_slice() {
        [] => SampleType::F32,
        [one] => one.native(),
        many if many.iter().all(|e| e.native() != SampleType::F32) => SampleType::I32,
        _ => SampleType::F32,
    };
    let mut channels: Vec<Channel> = info
        .channels
        .iter()
        .map(|ch| {
            let mut channel = Channel::new(ch.name.clone(), GridId(0));
            channel.kind = ch.kind();
            channel.unit = ch.unit();
            channel.gain = ch.calibration();
            channel.prefilter = prefilter(info.highpass, info.lowpass);
            channel
        })
        .collect();
    for channel in &mut channels {
        if !channel.gain.is_finite() {
            warnings.push(format!(
                "channel {:?} has a calibration of {}; its stored values are shown",
                channel.name, channel.gain
            ));
            channel.gain = 1.0;
        }
    }
    let float64 = buffers
        .iter()
        .find(|b| b.encoding == Encoding::F64)
        .copied();
    let reference = |c: usize, channel: &Channel| -> f64 {
        let Some(b) = float64 else { return 0.0 };
        if channel.kind == ChannelKind::Stim {
            return 0.0;
        }
        let decoder = Decoder::new(Encoding::F64, ByteOrder::Big).with_reference(0.0);
        let at = b.offset + c * 8;
        let bytes = parts[b.part].0.get(at..).unwrap_or_default();
        decoder.reference_from(bytes)
    };
    let references: Vec<f64> = channels
        .iter()
        .enumerate()
        .map(|(c, channel)| reference(c, channel))
        .collect();
    for (channel, r) in channels.iter_mut().zip(&references) {
        channel.offset = r * channel.gain;
    }
    let decoding = match encodings.as_slice() {
        [one] => Decoding::Uniform(
            references
                .iter()
                .map(|&r| Decoder::new(*one, ByteOrder::Big).with_reference(r))
                .collect(),
        ),
        [] => Decoding::Uniform(vec![Decoder::new(Encoding::F32, ByteOrder::Big); n]),
        _ => Decoding::Mixed(references),
    };
    buffers.shrink_to_fit();

    let grid = Grid::new(
        Timebase::Regular {
            rate: info.sfreq,
            start: 0.0,
        },
        len,
        segments,
        native,
        (0..n as u32).map(ChannelId).collect(),
    )
    .map_err(|e| error(e.into()))?;

    let mut extra = info.extra.clone();
    if part.maxshield {
        warnings.push(
            "this is a MaxShield (Internal Active Shielding) recording, which may be \
             distorted until processed with MaxFilter"
                .to_owned(),
        );
        extra.push(("MaxShield".to_owned(), "yes".to_owned()));
    }
    let start_time = start_time(&info, first_samp, &mut extra, &mut warnings);
    extra.push(("first sample".to_owned(), first_samp.to_string()));

    Ok(Opened {
        header: Header {
            format: "FIFF",
            start_time,
            channels,
            grids: vec![grid],
            extra,
        },
        annotations: Vec::new(),
        warnings,
        sources,
        reader: Box::new(FiffReader {
            parts,
            buffers,
            channels: n,
            native,
            decoding,
        }),
    })
}

/// What one file of a recording holds.
struct Part {
    info: Info,
    first_samp: u64,
    /// Starting at sample 0 of this part.
    buffers: Vec<Buffer>,
    maxshield: bool,
}

impl Part {
    fn read(tree: &Node, map: &[u8], warnings: &mut Vec<String>) -> Result<Part, FiffError> {
        let meas = tree.find(block::MEAS).ok_or(FiffError::NoMeasurement)?;
        let info_node = meas.find(block::MEAS_INFO).ok_or(FiffError::NoInfo)?;
        let mut info = info::read(info_node, map, warnings)?;
        if info.meas_date.is_none() {
            // As MNE: the time of the measurement's id, or of the file's.
            info.meas_date = [
                info_node.tag(kind::PARENT_BLOCK_ID),
                info_node.tag(kind::BLOCK_ID),
                meas.tag(kind::BLOCK_ID),
                meas.tag(kind::PARENT_BLOCK_ID),
                tree.tag(kind::FILE_ID),
            ]
            .into_iter()
            .flatten()
            .find_map(|t| t.id_time(map))
            // An id without a time (0, the epoch) is no date either.
            .filter(|&date| date != info::DATE_NONE && date.0 != 0);
        }
        let (raw, maxshield) = [block::RAW_DATA, block::CONTINUOUS_DATA]
            .iter()
            .find_map(|&b| meas.find(b).map(|n| (n, false)))
            .or_else(|| meas.find(block::IAS_RAW_DATA).map(|n| (n, true)))
            .ok_or_else(|| {
                FiffError::NoRawData(if meas.find(block::MNE_EPOCHS).is_some() {
                    " (it holds epochs)"
                } else if meas.find(block::EVOKED).is_some() {
                    " (it holds evoked responses)"
                } else {
                    ""
                })
            })?;
        let (first_samp, buffers) = buffers(raw, map, info.channels.len(), warnings)?;
        Ok(Part {
            info,
            first_samp,
            buffers,
            maxshield,
        })
    }
}

/// The encoding of a data buffer's type.
fn encoding(tag: &Tag) -> Result<Encoding, FiffError> {
    let invalid = |reason: String| FiffError::InvalidBuffer {
        position: tag.at as u64,
        reason,
    };
    match tag.typ {
        types::DAU_PACK16 | types::SHORT => Ok(Encoding::I16),
        types::INT => Ok(Encoding::I32),
        types::FLOAT => Ok(Encoding::F32),
        types::DOUBLE => Ok(Encoding::F64),
        types::COMPLEX_FLOAT | types::COMPLEX_DOUBLE => {
            Err(invalid("complex samples are not supported".into()))
        }
        other => Err(invalid(format!("data type {other} is not supported"))),
    }
}

/// `first_samp` and the data buffers of a raw data block, the first starting at sample 0.
///
/// A skip of `n` stands for `n` buffers of the length of the next one. A skip before the
/// first buffer moves `first_samp` (as in MNE); later ones are gaps; one after the last
/// buffer is ignored.
fn buffers(
    raw: &Node,
    map: &[u8],
    channels: usize,
    warnings: &mut Vec<String>,
) -> Result<(u64, Vec<Buffer>), FiffError> {
    let mut first_samp: i64 = 0;
    let mut buffers: Vec<Buffer> = Vec::new();
    let mut skip: u64 = 0;
    let mut at = 0u64;
    for tag in &raw.tags {
        match tag.kind {
            kind::FIRST_SAMPLE if buffers.is_empty() => {
                first_samp = tag.int(map).map_or(0, i64::from);
            }
            kind::DATA_SKIP => {
                skip = tag
                    .int(map)
                    .and_then(|n| u64::try_from(n).ok())
                    .unwrap_or(0);
            }
            kind::DATA_BUFFER => {
                let encoding = encoding(tag)?;
                let frame = channels * encoding.size();
                let len = (tag.size / frame) as u64;
                let rest = tag.size % frame;
                if tag.truncated {
                    warnings.push(format!(
                        "the file ends inside its last data buffer; the {len} whole samples \
                         there are read"
                    ));
                } else if rest != 0 {
                    warnings.push(format!(
                        "the data buffer at byte {} ends in {rest} bytes of an incomplete \
                         sample, which are ignored",
                        tag.at
                    ));
                }
                if skip > 0 {
                    let skipped = skip.saturating_mul(len);
                    if buffers.is_empty() {
                        first_samp = first_samp.saturating_add(skipped as i64);
                    } else {
                        at = at.saturating_add(skipped);
                    }
                    skip = 0;
                }
                if len > 0 {
                    buffers.push(Buffer {
                        start: at,
                        len,
                        part: 0,
                        offset: tag.data,
                        encoding,
                    });
                    at += len;
                }
            }
            _ => {}
        }
    }
    let first_samp = u64::try_from(first_samp).map_err(|_| FiffError::InvalidField {
        field: "first sample",
        value: first_samp.to_string(),
    })?;
    Ok((first_samp, buffers))
}

/// The ranges of samples the buffers hold, touching ones merged.
fn segments(buffers: &[Buffer]) -> Vec<Range<u64>> {
    let mut segments: Vec<Range<u64>> = Vec::new();
    for b in buffers {
        match segments.last_mut() {
            Some(last) if last.end == b.start => last.end = b.start + b.len,
            _ => segments.push(b.start..b.start + b.len),
        }
    }
    segments
}

/// The filters in EDF's notation ("HP:0.1Hz LP:172.2Hz"), "HP:DC" for a high-pass of 0.
fn prefilter(highpass: Option<f64>, lowpass: Option<f64>) -> String {
    let mut parts = Vec::new();
    match highpass {
        Some(0.0) => parts.push("HP:DC".to_owned()),
        Some(f) if f > 0.0 => parts.push(format!("HP:{}Hz", f as f32)),
        _ => {}
    }
    if let Some(f) = lowpass.filter(|f| *f > 0.0) {
        parts.push(format!("LP:{}Hz", f as f32));
    }
    parts.join(" ")
}

/// The wall-clock time of the first sample: `meas_date + first_samp / sfreq`, in the
/// file's local time if it stores its UTC offset, in UTC otherwise; adds the time zone to
/// `extra`.
fn start_time(
    info: &Info,
    first_samp: u64,
    extra: &mut Vec<(String, String)>,
    warnings: &mut Vec<String>,
) -> Option<jiff::civil::DateTime> {
    let (secs, usecs) = info.meas_date?;
    let offset = match info.utc_offset.as_deref() {
        None => Some(jiff::tz::Offset::UTC),
        Some(text) => {
            let parsed = utc_offset(text);
            if parsed.is_none() {
                warnings.push(format!(
                    "the UTC offset {text:?} is not +HH:MM; the start time is in UTC"
                ));
            }
            parsed
        }
    };
    let offset = offset.unwrap_or(jiff::tz::Offset::UTC);
    let nanos = (first_samp as f64 / info.sfreq * 1e9).round();
    let time = jiff::Timestamp::new(secs, 0)
        .ok()?
        .checked_add(jiff::SignedDuration::from_micros(usecs))
        .ok()?
        .checked_add(jiff::SignedDuration::from_nanos(nanos as i64))
        .ok()?;
    let seconds = offset.seconds();
    let zone = if seconds == 0 {
        "UTC".to_owned()
    } else {
        let sign = if seconds < 0 { '-' } else { '+' };
        let minutes = seconds.unsigned_abs() / 60;
        format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
    };
    extra.push(("time zone".to_owned(), zone));
    Some(offset.to_datetime(time))
}

/// `+HH:MM`, `+HHMM` or `+HH` as an offset.
fn utc_offset(text: &str) -> Option<jiff::tz::Offset> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) || ![2, 4].contains(&digits.len()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits
        .get(2..)
        .filter(|m| !m.is_empty())
        .map_or(Ok(0), str::parse)
        .ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    jiff::tz::Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()
}

/// Values cast from f64 to a native type, for buffers of mixed encodings.
trait Cast: Sample {
    fn cast(v: f64) -> Self;
}

impl Cast for i16 {
    fn cast(v: f64) -> Self {
        v as i16
    }
}

impl Cast for i32 {
    fn cast(v: f64) -> Self {
        v as i32
    }
}

impl Cast for f32 {
    fn cast(v: f64) -> Self {
        v as f32
    }
}

impl BlockReader for FiffReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        if grid != GridId(0) {
            return Err(ReadError::UnknownGrid(grid));
        }
        if let Some(&channel) = channels.iter().find(|c| c.0 as usize >= self.channels) {
            return Err(ReadError::ChannelNotInGrid { channel, grid });
        }
        let n = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(usize::MAX);
        let expected = channels.len().saturating_mul(n);
        match out {
            SamplesMut::I16(dst) if dst.len() == expected && self.native == SampleType::I16 => {
                self.copy(channels, range, dst)
            }
            SamplesMut::I32(dst) if dst.len() == expected && self.native == SampleType::I32 => {
                self.copy(channels, range, dst)
            }
            SamplesMut::F32(dst) if dst.len() == expected && self.native == SampleType::F32 => {
                self.copy(channels, range, dst)
            }
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type: self.native,
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        if grid != GridId(0) {
            return;
        }
        let first = self
            .buffers
            .partition_point(|b| b.start + b.len <= range.start);
        for b in self.buffers[first..]
            .iter()
            .take_while(|b| b.start < range.end)
        {
            let frame = (self.channels * b.encoding.size()) as u64;
            let from = range.start.max(b.start) - b.start;
            let to = range.end.min(b.start + b.len) - b.start;
            let (map, file) = &self.parts[b.part];
            let offset = b.offset as u64;
            mapped::prefetch(
                file,
                map.len() as u64,
                [offset + from * frame..offset + to * frame],
            );
        }
    }
}

impl FiffReader {
    /// Decodes `range` of `channels` into `dst`, channel-major, buffer by buffer.
    fn copy<T: Cast>(
        &self,
        channels: &[ChannelId],
        range: Range<u64>,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let n = (range.end - range.start) as usize;
        let not_present = || ReadError::NotPresent {
            grid: GridId(0),
            range: range.clone(),
        };
        let mut b = self
            .buffers
            .partition_point(|b| b.start + b.len <= range.start);
        let mut i = range.start;
        while i < range.end {
            let buffer = self
                .buffers
                .get(b)
                .filter(|x| x.start <= i)
                .ok_or_else(not_present)?;
            let j = i - buffer.start;
            let take = (buffer.len - j).min(range.end - i);
            let size = buffer.encoding.size();
            let frame = self.channels * size;
            let data = self.parts[buffer.part]
                .0
                .get(buffer.offset..)
                .unwrap_or_default();
            let at = (i - range.start) as usize;
            match &self.decoding {
                Decoding::Uniform(decoders) => {
                    let columns: Vec<(usize, Decoder)> = channels
                        .iter()
                        .map(|c| (c.0 as usize * size, decoders[c.0 as usize]))
                        .collect();
                    frames_into(data, frame, &columns, j..j + take, dst, n, at)?;
                }
                Decoding::Mixed(references) => {
                    let decoder = Decoder::new(buffer.encoding, ByteOrder::Big);
                    let outside =
                        || ReadError::Corrupt("samples beyond the end of the data".into());
                    for (k, c) in channels.iter().enumerate() {
                        let c = c.0 as usize;
                        for s in 0..take as usize {
                            let pos = (j as usize + s) * frame + c * size;
                            let v = decoder
                                .value(data.get(pos..).ok_or_else(outside)?)
                                .ok_or_else(outside)?;
                            dst[k * n + at + s] = T::cast(v - references[c]);
                        }
                    }
                }
            }
            i += take;
            b += 1;
        }
        Ok(())
    }
}
