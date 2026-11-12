//! Opening FIFF raw recordings: the measurement info, the table of data buffers, and the
//! block reader over read-only maps of the file.

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::info::{self, Info};
use super::tag::{self, Node, Tag, block, kind, types};
use super::{FiffError, annotations};
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

/// Most files a split recording may have, against reference loops.
const MAX_PARTS: usize = 10_000;

/// Opens a FIFF raw recording, all its split parts: a later part opens the whole
/// recording, from the first part on. The files are memory-mapped, never loaded; the tags
/// of each are walked once at open to find the measurement info and the data buffers.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let error = |source| OpenError::Fiff {
        path: path.to_owned(),
        source,
    };
    let mut warnings = Vec::new();
    let parts = chain(Loaded::open(path)?, &mut warnings)?;
    let several = parts.len() > 1;
    for loaded in &parts {
        for w in &loaded.warnings {
            warnings.push(if several {
                format!("{}: {w}", file_name(&loaded.path))
            } else {
                w.clone()
            });
        }
    }

    // The parts' buffers on one grid, each part from its first sample on.
    let first = &parts[0].part;
    let info = &first.info;
    let first_samp = first.first_samp;
    let mut buffers = Vec::new();
    let mut end = 0;
    for (k, loaded) in parts.iter().enumerate() {
        let part = &loaded.part;
        if k > 0 {
            if let Some(reason) = mismatch(info, &part.info) {
                return Err(error(FiffError::SplitMismatch {
                    file: file_name(&loaded.path),
                    reason,
                }));
            }
            if part.first_samp < first_samp + end {
                return Err(error(FiffError::SplitMismatch {
                    file: file_name(&loaded.path),
                    reason: format!(
                        "it starts at sample {}, before the previous part ends ({})",
                        part.first_samp,
                        first_samp + end
                    ),
                }));
            }
        }
        let offset = part.first_samp - first_samp;
        buffers.extend(part.buffers.iter().map(|b| Buffer {
            start: b.start + offset,
            part: k,
            ..*b
        }));
        end = buffers.last().map_or(end, |b| b.start + b.len);
    }
    let len = end;
    if buffers.is_empty() {
        warnings.push("the file has no data buffers".to_owned());
    }
    let segments = segments(&buffers);
    let n = info.channels.len();
    let sources: Vec<PathBuf> = parts.iter().map(|l| l.path.clone()).collect();

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
    stim(&mut channels);
    let maps: Vec<&[u8]> = parts.iter().map(|l| &l.map[..]).collect();
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
        let bytes = maps[b.part].get(b.offset + c * 8..).unwrap_or_default();
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

    // Channels by name: the full name, or the struct's (cut to 15 bytes), which MNE writes
    // in bad channel lists when names are longer.
    let channel = |name: &str| -> Option<ChannelId> {
        let find = |f: &dyn Fn(&info::ChInfo) -> &str| {
            info.channels
                .iter()
                .position(|c| f(c) == name)
                .map(|i| ChannelId(i as u32))
        };
        find(&|c| &c.name).or_else(|| find(&|c| &c.struct_name))
    };
    let mut bad_channels = Vec::new();
    let mut unknown = Vec::new();
    for name in &first.bads {
        match channel(name) {
            Some(id) if !bad_channels.contains(&id) => bad_channels.push(id),
            Some(_) => {}
            None => unknown.push(name.as_str()),
        }
    }
    if !unknown.is_empty() {
        warnings.push(format!(
            "the file marks channels as bad that it doesn't have: {}",
            unknown.join(", ")
        ));
    }
    let annotations = match &first.annotations {
        Some(node) => annotations::read(
            node,
            &parts[0].map,
            first_samp as f64 / info.sfreq,
            len as f64 / info.sfreq,
            info.meas_date.map(|(s, us)| s as f64 + us as f64 * 1e-6),
            &channel,
            &mut warnings,
        ),
        None => Vec::new(),
    };

    let mut extra = info.extra.clone();
    if first.maxshield {
        warnings.push(
            "this is a MaxShield (Internal Active Shielding) recording, which may be \
             distorted until processed with MaxFilter"
                .to_owned(),
        );
        extra.push(("MaxShield".to_owned(), "yes".to_owned()));
    }
    let start_time = start_time(info, first_samp, &mut extra, &mut warnings);
    extra.push(("first sample".to_owned(), first_samp.to_string()));
    if several {
        extra.push(("split parts".to_owned(), parts.len().to_string()));
    }

    let parts = parts.into_iter().map(|l| (l.map, l.file)).collect();
    Ok(Opened {
        header: Header {
            format: "FIFF",
            start_time,
            channels,
            grids: vec![grid],
            extra,
            bad_channels,
        },
        annotations,
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

/// Stim channels: the combined channel (STI101, else STI 014), if there is one, holds the
/// codes in its low 16 bits (Neuromag's STI016 sets bit 15, which a 16-bit sample reads as
/// negative) and is the only one whose changes are events; the others are the bit lines it
/// combines. Without one, every stim channel's changes are events.
fn stim(channels: &mut [Channel]) {
    let combined = ["STI101", "STI 014"].iter().find_map(|name| {
        channels
            .iter()
            .position(|c| c.kind == ChannelKind::Stim && c.name == *name)
    });
    let Some(combined) = combined else { return };
    for (c, channel) in channels.iter_mut().enumerate() {
        if channel.kind == ChannelKind::Stim {
            channel.events = c == combined;
        }
    }
    channels[combined].trigger_mask = Some(0xffff);
}

/// Why `part`'s info can't continue `first`'s, if it can't.
fn mismatch(first: &Info, part: &Info) -> Option<String> {
    if part.sfreq != first.sfreq {
        return Some(format!(
            "its sampling rate ({} Hz) differs from the first part's ({} Hz)",
            part.sfreq, first.sfreq
        ));
    }
    if part.channels.len() != first.channels.len() {
        return Some(format!(
            "it has {} channels, the first part {}",
            part.channels.len(),
            first.channels.len()
        ));
    }
    let differ = |a: &info::ChInfo, b: &info::ChInfo| {
        a.name != b.name
            || a.kind != b.kind
            || a.unit != b.unit
            || a.calibration() != b.calibration()
    };
    first
        .channels
        .iter()
        .zip(&part.channels)
        .find(|(a, b)| differ(a, b))
        .map(|(a, _)| format!("its channel {:?} differs from the first part's", a.name))
}

/// The parts of the recording `given` belongs to, in order: back through the previous-part
/// references to the first, then forward through the next-part references. A missing or
/// unreadable part ends the chain there, with a warning.
fn chain(given: Loaded, warnings: &mut Vec<String>) -> Result<Vec<Loaded>, OpenError> {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    let mut first = given;
    let mut seen = vec![canonical(&first.path)];
    while let Some(r) = first.part.prev.clone() {
        let here = file_name(&first.path);
        match next_part(&first.path, &r, "previous", &here, &mut seen, warnings) {
            Some(loaded) => first = loaded,
            None => break,
        }
    }
    let mut seen = vec![canonical(&first.path)];
    let mut parts = vec![first];
    while let Some(last) = parts.last()
        && let Some(r) = last.part.next.clone()
    {
        let here = file_name(&last.path);
        let path = last.path.clone();
        match next_part(&path, &r, "next", &here, &mut seen, warnings) {
            Some(loaded) => parts.push(loaded),
            None => break,
        }
    }
    Ok(parts)
}

/// The part `r` names from the part at `path` (`which`: "previous" or "next"), if it can
/// be found and read and isn't one already in `seen`; a warning otherwise.
fn next_part(
    path: &Path,
    r: &Reference,
    which: &str,
    here: &str,
    seen: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Option<Loaded> {
    let shown = if which == "previous" {
        format!("the recording is read from {here} on")
    } else {
        format!("the recording ends with {here}")
    };
    let Some(found) = resolve(path, r) else {
        warnings.push(format!(
            "the {which} part of the recording ({}) is missing; {shown}",
            r.describe()
        ));
        return None;
    };
    let canonical = std::fs::canonicalize(&found).unwrap_or_else(|_| found.clone());
    if seen.contains(&canonical) || seen.len() >= MAX_PARTS {
        warnings.push(format!(
            "the parts of the recording refer to each other in a loop; {shown}"
        ));
        return None;
    }
    match Loaded::open(&found) {
        Ok(loaded) => {
            seen.push(canonical);
            Some(loaded)
        }
        Err(e) => {
            warnings.push(format!(
                "the {which} part of the recording can't be read ({e}); {shown}"
            ));
            None
        }
    }
}

/// Where the part `r` names is, beside the part at `path`: by its name (as written, then
/// its last path component beside `path`, since MNE may write a full path), else by its
/// number (Neuromag's names: `rec.fif`, `rec-1.fif`, `rec-2.fif`); `None` if no such file
/// exists.
fn resolve(path: &Path, r: &Reference) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let mut candidates = Vec::new();
    if let Some(name) = &r.name {
        let named = Path::new(name);
        if named.is_absolute() {
            candidates.push(named.to_owned());
        }
        let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
        candidates.push(dir.join(last));
    }
    if let (Some(num), Some(base)) = (r.num, path.file_name().and_then(|n| n.to_str())) {
        candidates.push(dir.join(numbered(base, num)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// `name` as Neuromag numbers split parts: the part number after a "-" before the first
/// ".", none for part 0 (`rec.fif`, `rec-1.fif`).
fn numbered(name: &str, num: i32) -> String {
    let dot = name.find('.').unwrap_or(name.len());
    let (stem, rest) = name.split_at(dot);
    let stem = match stem.rsplit_once('-') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => stem,
    };
    if num > 0 {
        format!("{stem}-{num}{rest}")
    } else {
        format!("{stem}{rest}")
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Another part of a split recording, as a reference block names it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reference {
    name: Option<String>,
    num: Option<i32>,
}

impl Reference {
    fn describe(&self) -> String {
        match (&self.name, self.num) {
            (Some(name), _) => format!("{name:?}"),
            (None, Some(num)) => format!("part {num}"),
            (None, None) => "unnamed".to_owned(),
        }
    }
}

/// One file of a recording, mapped and read.
struct Loaded {
    path: PathBuf,
    map: Mmap,
    file: File,
    part: Part,
    warnings: Vec<String>,
}

impl Loaded {
    fn open(path: &Path) -> Result<Loaded, OpenError> {
        let io = |source| OpenError::Io {
            path: path.to_owned(),
            source,
        };
        let error = |source| OpenError::Fiff {
            path: path.to_owned(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        let map = map_read_only(&file).map_err(io)?;
        if !tag::is_fiff(&map) {
            return Err(error(FiffError::NotFiff));
        }
        let mut warnings = Vec::new();
        let tree = tag::tree(&map, &mut warnings);
        let part = Part::read(&tree, &map, &mut warnings).map_err(error)?;
        Ok(Loaded {
            path: path.to_owned(),
            map,
            file,
            part,
            warnings,
        })
    }
}

/// What one file of a recording holds.
struct Part {
    info: Info,
    first_samp: u64,
    /// Starting at sample 0 of this part.
    buffers: Vec<Buffer>,
    maxshield: bool,
    /// The names of the channels the file marks as bad.
    bads: Vec<String>,
    /// MNE's annotations block, if any.
    annotations: Option<Node>,
    prev: Option<Reference>,
    next: Option<Reference>,
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

        // The last list of bad channels, as MNE reads them.
        let mut lists = Vec::new();
        meas.find_all(block::MNE_BAD_CHANNELS, &mut lists);
        let bads = lists
            .iter()
            .rev()
            .find_map(|b| {
                b.tag(kind::MNE_CH_NAME_LIST)
                    .or_else(|| b.tag(kind::BAD_CHS))
            })
            .map(|t| t.names(map))
            .unwrap_or_default();

        let (mut prev, mut next) = (None, None);
        let mut refs = Vec::new();
        tree.find_all(block::REF, &mut refs);
        for r in refs {
            let reference = Reference {
                name: r
                    .tag(kind::REF_FILE_NAME)
                    .map(|t| t.text(map))
                    .filter(|n| !n.is_empty()),
                num: r.tag(kind::REF_FILE_NUM).and_then(|t| t.int(map)),
            };
            match r.tag(kind::REF_ROLE).and_then(|t| t.int(map)) {
                Some(1) => prev = Some(reference),
                Some(2) => next = Some(reference),
                _ => {}
            }
        }
        Ok(Part {
            info,
            first_samp,
            buffers,
            maxshield,
            bads,
            annotations: tree.find(block::MNE_ANNOTATIONS).cloned(),
            prev,
            next,
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
