//! The open scan: XDF has no index, so the file is walked once, chunk by chunk by each
//! chunk's length. Stream headers say how each stream's samples are laid out; Samples
//! chunks of streams shown as channels are indexed (where their samples start and which of
//! them carry a timestamp, so any sample's position follows), those of streams read as
//! annotations are decoded.

use std::collections::HashMap;
use std::ops::Range;

use super::xml::{self, Element};
use crate::decode::{ByteOrder, Decoder, Encoding};

/// Chunk tags.
mod tag {
    pub const FILE_HEADER: u16 = 1;
    pub const STREAM_HEADER: u16 = 2;
    pub const SAMPLES: u16 = 3;
    pub const CLOCK_OFFSET: u16 = 4;
    pub const BOUNDARY: u16 = 5;
    pub const STREAM_FOOTER: u16 = 6;
}

/// The bytes a file starts with.
pub(crate) const MAGIC: &[u8] = b"XDF:";

/// Stream types (lower case) whose samples are events: their numeric streams are discrete
/// (stim) channels at a regular rate and annotations at an irregular one.
const EVENT_TYPES: [&str; 7] = [
    "marker", "markers", "event", "events", "trigger", "triggers", "stim",
];

/// Whether a stream type is one of the event types.
pub(crate) fn is_event_type(kind: &str) -> bool {
    EVENT_TYPES.contains(&kind.trim().to_ascii_lowercase().as_str())
}

/// How a stream stores its values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ValueFormat {
    Numeric(Encoding),
    String,
}

impl ValueFormat {
    fn parse(text: &str) -> Option<ValueFormat> {
        Some(match text.trim() {
            "int8" => ValueFormat::Numeric(Encoding::I8),
            "int16" => ValueFormat::Numeric(Encoding::I16),
            "int32" => ValueFormat::Numeric(Encoding::I32),
            "int64" => ValueFormat::Numeric(Encoding::I64),
            "float32" => ValueFormat::Numeric(Encoding::F32),
            "double64" => ValueFormat::Numeric(Encoding::F64),
            "string" => ValueFormat::String,
            _ => return None,
        })
    }

    /// The format's name in stream headers.
    pub fn name(self) -> &'static str {
        match self {
            ValueFormat::Numeric(Encoding::I8) => "int8",
            ValueFormat::Numeric(Encoding::I16) => "int16",
            ValueFormat::Numeric(Encoding::I32) => "int32",
            ValueFormat::Numeric(Encoding::I64) => "int64",
            ValueFormat::Numeric(Encoding::F64) => "double64",
            ValueFormat::Numeric(_) => "float32",
            ValueFormat::String => "string",
        }
    }
}

/// A channel as the stream header describes it; empty fields where it has none.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChannelInfo {
    pub label: String,
    pub kind: String,
    pub unit: String,
}

/// A stream's header.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Info {
    pub name: String,
    /// The stream type ("EEG", "Markers", ...).
    pub kind: String,
    pub format: ValueFormat,
    pub count: usize,
    /// The nominal rate; 0 for an irregular stream.
    pub rate: f64,
    /// One per channel, or none if the header describes none (or not as many as it has).
    pub channels: Vec<ChannelInfo>,
    /// The header says the stream can drop samples (`desc/synchronization/can_drop_samples`),
    /// so pyxdf doesn't dejitter it.
    pub can_drop: bool,
    /// Seconds to subtract from the stream's synchronized time stamps
    /// (`desc/synchronization/offset_mean`, "subtracted from XDF timestamps before comparing
    /// streams"); 0 without one.
    pub offset_mean: f64,
}

/// How a stream is read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Mode {
    /// Numeric at a regular rate: one grid.
    Channels,
    /// Strings, or numeric at an irregular rate of an event type: an annotation per sample.
    Annotations,
    /// Numeric at an irregular rate, not of an event type: one grid on its own timestamps.
    Irregular,
    /// The header can't be read: why.
    Unreadable(String),
}

/// Which samples of a chunk carry a timestamp (8 bytes after the sample's flag byte).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stamps {
    None,
    All,
    /// Those at the positions (within the chunk, ascending) in this range of
    /// [`Stream::positions`].
    Some(Range<usize>),
}

/// A Samples chunk of a stream shown as channels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chunk {
    /// Its first sample in the stream.
    pub first: u64,
    /// Samples in it.
    pub len: u64,
    /// Byte position of its first sample (the timestamp flag).
    pub offset: usize,
    pub stamps: Stamps,
}

/// Everything the scan found about one stream.
#[derive(Clone, Debug)]
pub(crate) struct Stream {
    pub id: u32,
    /// `None` without a header.
    pub info: Option<Info>,
    pub mode: Mode,
    pub footer: bool,
    /// Streams shown as channels: their Samples chunks, in order.
    pub chunks: Vec<Chunk>,
    /// The positions of timestamped samples in chunks that have some.
    pub positions: Vec<u32>,
    /// Samples: indexed, decoded or (for streams not read) counted.
    pub len: u64,
    /// Streams shown as channels: the first sample with a timestamp, and the timestamp.
    pub first_stamp: Option<(u64, f64)>,
    /// Streams read as annotations: each sample's time and text.
    pub events: Vec<(f64, String)>,
    /// ClockOffset chunks: (collection time, offset).
    pub clock_offsets: Vec<(f64, f64)>,
    /// The last timestamp of an annotation stream, from which the next sample's follows.
    last_stamp: Option<f64>,
    /// Annotation samples without a timestamp before the first one, left out.
    unstamped: u64,
}

impl Stream {
    fn new(id: u32) -> Self {
        Stream {
            id,
            info: None,
            mode: Mode::Unreadable("it has no header".into()),
            footer: false,
            chunks: Vec::new(),
            positions: Vec::new(),
            len: 0,
            first_stamp: None,
            events: Vec::new(),
            clock_offsets: Vec::new(),
            last_stamp: None,
            unstamped: 0,
        }
    }

    /// The stream's name, for messages.
    pub fn describe(&self) -> String {
        match &self.info {
            Some(info) if !info.name.is_empty() => format!("stream {} ({})", self.id, info.name),
            _ => format!("stream {}", self.id),
        }
    }
}

/// What the scan found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// The file header's `version` and `datetime`.
    pub version: Option<String>,
    pub datetime: Option<String>,
    /// In order of first appearance.
    pub streams: Vec<Stream>,
    pub warnings: Vec<String>,
    /// Chunks of tags XDF 1.0 doesn't define, skipped: how many of each.
    unknown: std::collections::BTreeMap<u16, u64>,
}

/// A variable-length integer at `at`: a byte giving its width (1, 4 or 8), then the value,
/// little-endian. The value and the position after it.
fn varlen(data: &[u8], at: usize) -> Option<(u64, usize)> {
    let width = usize::from(*data.get(at)?);
    if !matches!(width, 1 | 4 | 8) {
        return None;
    }
    let bytes = data.get(at + 1..at + 1 + width)?;
    let mut value = [0u8; 8];
    value[..width].copy_from_slice(bytes);
    Some((u64::from_le_bytes(value), at + 1 + width))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn f64_at(data: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// A Boundary chunk's content, which lets a reader find its place again in a damaged file.
const BOUNDARY: [u8; 16] = [
    0x43, 0xA5, 0x46, 0xDC, 0xCB, 0xF5, 0x41, 0x0F, 0xB3, 0x0E, 0xD5, 0x46, 0x73, 0x83, 0xCB, 0xE4,
];

/// The position after the first Boundary chunk's content after `from`, if there is one.
fn after_boundary(map: &[u8], from: usize) -> Option<usize> {
    map.get(from..)?
        .windows(BOUNDARY.len())
        .position(|w| w == BOUNDARY)
        .map(|k| from + k + BOUNDARY.len())
}

/// Walks the chunks of `map` (which starts with [`MAGIC`]), passing over those of tags XDF
/// 1.0 doesn't define. Where the walk loses its place (an invalid length), it goes on after
/// the next Boundary chunk.
pub(crate) fn scan(map: &[u8]) -> Scan {
    let mut scan = Scan::default();
    let mut index: HashMap<u32, usize> = HashMap::new();
    let mut headless: Vec<u32> = Vec::new();
    let mut pos = MAGIC.len();
    let lost = |pos: usize, what: &str, warnings: &mut Vec<String>| -> Option<usize> {
        let next = after_boundary(map, pos + 1);
        warnings.push(match next {
            Some(next) => format!(
                "the chunk at byte {pos} has {what}; the {} bytes up to and including the \
                 next boundary chunk are skipped",
                next - pos
            ),
            None => format!("the chunk at byte {pos} has {what}; the rest of the file is not read"),
        });
        next
    };
    while pos < map.len() {
        let Some((len, content)) = varlen(map, pos).filter(|&(len, _)| len >= 2) else {
            match lost(pos, "an invalid length", &mut scan.warnings) {
                Some(next) => {
                    pos = next;
                    continue;
                }
                None => break,
            }
        };
        let end = content.saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
        let truncated = end > map.len();
        let end = end.min(map.len());
        let Some(tag) = map
            .get(content..content + 2)
            .filter(|_| content + 2 <= end)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
        else {
            scan.warnings
                .push("the file ends inside its last chunk, which is ignored".into());
            break;
        };
        // A tag XDF 1.0 doesn't define: a later version's chunk, passed over by its length
        // ("so that programs can read through the chunks without having to parse the
        // content structure").
        if !(tag::FILE_HEADER..=tag::STREAM_FOOTER).contains(&tag) {
            *scan.unknown.entry(tag).or_default() += 1;
        }
        let body = &map[content + 2..end];
        let body_at = content + 2;
        let mut stream_of = |scan: &mut Scan, id: u32| -> usize {
            *index.entry(id).or_insert_with(|| {
                scan.streams.push(Stream::new(id));
                scan.streams.len() - 1
            })
        };
        match tag {
            tag::FILE_HEADER if !truncated => match xml::parse(body) {
                Ok(root) => {
                    scan.version = root.text_of("version").map(|v| v.trim().to_owned());
                    scan.datetime = root.text_of("datetime").map(|v| v.trim().to_owned());
                }
                Err(e) => scan
                    .warnings
                    .push(format!("the file header can't be read ({e})")),
            },
            tag::STREAM_HEADER if !truncated => {
                if let Some(id) = u32_at(body, 0) {
                    let s = stream_of(&mut scan, id);
                    let stream = &mut scan.streams[s];
                    if stream.info.is_some() {
                        scan.warnings.push(format!(
                            "{} has a second header, which is ignored",
                            stream.describe()
                        ));
                    } else {
                        header(stream, &body[4..], &mut scan.warnings);
                    }
                }
            }
            tag::SAMPLES => {
                if let Some(id) = u32_at(body, 0) {
                    let s = stream_of(&mut scan, id);
                    let stream = &mut scan.streams[s];
                    if stream.info.is_none() {
                        if !headless.contains(&id) {
                            headless.push(id);
                            scan.warnings.push(format!(
                                "samples of stream {id} come before its header (or it has \
                                 none) and are skipped"
                            ));
                        }
                    } else {
                        samples(stream, map, body_at + 4, end, truncated, &mut scan.warnings);
                    }
                }
            }
            tag::CLOCK_OFFSET if !truncated => {
                if let (Some(id), Some(time), Some(offset)) =
                    (u32_at(body, 0), f64_at(body, 4), f64_at(body, 12))
                {
                    let s = stream_of(&mut scan, id);
                    scan.streams[s].clock_offsets.push((time, offset));
                }
            }
            tag::STREAM_FOOTER if !truncated => {
                if let Some(id) = u32_at(body, 0) {
                    let s = stream_of(&mut scan, id);
                    scan.streams[s].footer = true;
                }
            }
            tag::BOUNDARY => {}
            // Chunks the file ends inside, and unknown ones.
            _ => {}
        }
        if truncated {
            let reported = tag == tag::SAMPLES
                && u32_at(body, 0)
                    .and_then(|id| index.get(&id))
                    .is_some_and(|&s| {
                        let stream = &scan.streams[s];
                        stream.info.is_some()
                            && matches!(
                                stream.mode,
                                Mode::Channels | Mode::Irregular | Mode::Annotations
                            )
                    });
            if !reported {
                scan.warnings
                    .push("the file ends inside its last chunk, which is ignored".into());
            }
            break;
        }
        pos = end;
    }
    scan
}

/// Reads a stream header's XML into `stream`'s info and mode.
fn header(stream: &mut Stream, xml_bytes: &[u8], warnings: &mut Vec<String>) {
    let root = match xml::parse(xml_bytes) {
        Ok(root) => root,
        Err(e) => {
            stream.mode = Mode::Unreadable(format!("its header can't be read ({e})"));
            stream.info = Some(placeholder(String::new(), String::new()));
            return;
        }
    };
    let text = |name: &str| root.text_of(name).unwrap_or("").trim().to_owned();
    let name = root.text_of("name").unwrap_or("").to_owned();
    let kind = text("type");
    let Some(format) = ValueFormat::parse(&text("channel_format")) else {
        stream.mode = Mode::Unreadable(format!(
            "its channel format {:?} is not supported",
            text("channel_format")
        ));
        stream.info = Some(placeholder(name, kind));
        return;
    };
    let count = text("channel_count")
        .parse::<usize>()
        .ok()
        .filter(|&n| n > 0);
    let rate = text("nominal_srate")
        .parse::<f64>()
        .ok()
        .filter(|r| r.is_finite() && *r >= 0.0);
    let (Some(count), Some(rate)) = (count, rate) else {
        stream.mode = Mode::Unreadable(format!(
            "its channel count ({:?}) or rate ({:?}) is invalid",
            text("channel_count"),
            text("nominal_srate")
        ));
        stream.info = Some(placeholder(name, kind));
        return;
    };
    let mut channels: Vec<ChannelInfo> = root
        .child("desc")
        .and_then(|d| d.child("channels"))
        .map(|c| c.children("channel").map(channel).collect())
        .unwrap_or_default();
    if !channels.is_empty() && channels.len() != count {
        warnings.push(format!(
            "the header of stream {} ({name}) describes {} channels, but the stream has \
             {count}; their labels are not used",
            stream.id,
            channels.len()
        ));
        channels.clear();
    }
    stream.mode = match format {
        ValueFormat::String => Mode::Annotations,
        ValueFormat::Numeric(_) if rate > 0.0 => Mode::Channels,
        ValueFormat::Numeric(_) if !is_event_type(&kind) => Mode::Irregular,
        ValueFormat::Numeric(_) => Mode::Annotations,
    };
    let sync = root.child("desc").and_then(|d| d.child("synchronization"));
    let can_drop = sync
        .and_then(|s| s.text_of("can_drop_samples"))
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let offset_mean = match sync.and_then(|s| s.text_of("offset_mean")).map(str::trim) {
        None | Some("") => 0.0,
        Some(text) => match text.parse::<f64>() {
            Ok(v) if v.is_finite() => v,
            _ => {
                warnings.push(format!(
                    "stream {} ({name}): its offset_mean {text:?} isn't a number and is \
                     ignored",
                    stream.id
                ));
                0.0
            }
        },
    };
    stream.info = Some(Info {
        name,
        kind,
        format,
        count,
        rate,
        channels,
        can_drop,
        offset_mean,
    });
}

/// Info of a stream whose header can't be read, for its name in messages.
fn placeholder(name: String, kind: String) -> Info {
    Info {
        name,
        kind,
        format: ValueFormat::String,
        count: 0,
        rate: 0.0,
        channels: Vec::new(),
        can_drop: false,
        offset_mean: 0.0,
    }
}

fn channel(e: &Element) -> ChannelInfo {
    let text = |name: &str| e.text_of(name).unwrap_or("").to_owned();
    ChannelInfo {
        label: text("label"),
        kind: text("type"),
        unit: text("unit"),
    }
}

/// Reads a Samples chunk of `stream` whose sample count starts at `at`, the chunk ending at
/// `end` (cut there if `truncated`).
fn samples(
    stream: &mut Stream,
    map: &[u8],
    at: usize,
    end: usize,
    truncated: bool,
    warnings: &mut Vec<String>,
) {
    let data = &map[..end];
    let Some((count, start)) = varlen(data, at) else {
        warnings.push(format!(
            "a Samples chunk of {} has an invalid sample count and is skipped",
            stream.describe()
        ));
        return;
    };
    let Some(info) = stream.info.clone() else {
        return;
    };
    match (&stream.mode, info.format) {
        (Mode::Channels | Mode::Irregular, ValueFormat::Numeric(encoding)) => {
            let payload = info.count * encoding.size();
            index_chunk(stream, data, start, count, payload, truncated, warnings);
        }
        (Mode::Annotations, format) => {
            decode_events(stream, &info, format, data, start, count, warnings);
        }
        _ => stream.len = stream.len.saturating_add(count),
    }
}

/// Indexes a chunk of `count` samples of `payload` value bytes each, from `data[start]`.
fn index_chunk(
    stream: &mut Stream,
    data: &[u8],
    start: usize,
    count: u64,
    payload: usize,
    truncated: bool,
    warnings: &mut Vec<String>,
) {
    let frame = 1 + payload as u64;
    let count = count.min(u64::from(u32::MAX));
    let bytes = (data.len() - start.min(data.len())) as u64;
    // The number of timestamps follows from the chunk's length, if it is consistent.
    let stamped = (!truncated)
        .then(|| count.checked_mul(frame).and_then(|b| bytes.checked_sub(b)))
        .flatten()
        .filter(|rest| rest % 8 == 0 && rest / 8 <= count)
        .map(|rest| rest / 8);
    let flag = data.get(start).copied();
    let (len, stamps, first, problem) = match stamped {
        _ if count == 0 => (0, Stamps::None, None, None),
        Some(0) if flag == Some(0) => (count, Stamps::None, None, None),
        Some(m) if m == count && flag == Some(8) => {
            let first = f64_at(data, start + 1)
                .filter(|t| t.is_finite())
                .map(|t| (0, t));
            (count, Stamps::All, first, None)
        }
        _ => {
            let walk = walk(data, start, count, frame as usize, stamped);
            let stamps = if walk.positions.is_empty() {
                Stamps::None
            } else if walk.positions.len() as u64 == walk.count {
                Stamps::All
            } else {
                let from = stream.positions.len();
                stream.positions.extend_from_slice(&walk.positions);
                Stamps::Some(from..stream.positions.len())
            };
            let problem = walk.problem.or_else(|| {
                if truncated {
                    None
                } else if walk.count < count {
                    Some(format!("it holds fewer samples than its count ({count})"))
                } else if stamped.is_none() && walk.end < data.len() {
                    Some(format!(
                        "{} bytes after its last sample are ignored",
                        data.len() - walk.end
                    ))
                } else {
                    None
                }
            });
            (walk.count, stamps, walk.first, problem)
        }
    };
    if let Some(problem) = problem {
        warnings.push(format!(
            "a Samples chunk of {} at byte {start}: {problem}; its {len} whole samples \
             before that are read",
            stream.describe()
        ));
    } else if truncated {
        warnings.push(format!(
            "the file ends inside a Samples chunk of {}; its {len} whole samples are read",
            stream.describe()
        ));
    }
    if stream.first_stamp.is_none()
        && let Some((k, t)) = first
    {
        stream.first_stamp = Some((stream.len + k, t));
    }
    if len > 0 {
        stream.chunks.push(Chunk {
            first: stream.len,
            len,
            offset: start,
            stamps,
        });
        stream.len += len;
    }
}

/// The samples of a chunk, walked one by one.
struct Walk {
    /// Whole samples, up to a problem.
    count: u64,
    /// The positions of those with a timestamp.
    positions: Vec<u32>,
    /// The first with a timestamp, and the timestamp.
    first: Option<(u64, f64)>,
    /// Where the walk ended.
    end: usize,
    problem: Option<String>,
}

/// Walks `count` samples of `frame` bytes (plus 8 with a timestamp) from `data[start]`.
/// With `stamped` known (from the chunk's length), it stops once it has found that many
/// timestamps: the rest have none.
fn walk(data: &[u8], start: usize, count: u64, frame: usize, stamped: Option<u64>) -> Walk {
    let mut w = Walk {
        count: 0,
        positions: Vec::new(),
        first: None,
        end: start,
        problem: None,
    };
    let mut pos = start;
    let mut i = 0;
    while i < count {
        if stamped == Some(w.positions.len() as u64) {
            i = count;
            break;
        }
        let size = match data.get(pos) {
            Some(0) => frame,
            Some(8) => frame + 8,
            Some(other) => {
                w.problem = Some(format!(
                    "sample {i} has an invalid timestamp flag ({other})"
                ));
                break;
            }
            None => break,
        };
        if pos + size > data.len() {
            break;
        }
        if size > frame {
            w.positions.push(i as u32);
            if w.first.is_none() {
                w.first = f64_at(data, pos + 1)
                    .filter(|t| t.is_finite())
                    .map(|t| (i, t));
            }
        }
        pos += size;
        i += 1;
    }
    w.count = i;
    w.end = pos;
    w
}

/// Decodes a chunk of an annotation stream: one event per non-empty string, or per sample
/// of a numeric stream with its values (NaN left out) joined by commas.
fn decode_events(
    stream: &mut Stream,
    info: &Info,
    format: ValueFormat,
    data: &[u8],
    start: usize,
    count: u64,
    warnings: &mut Vec<String>,
) {
    let step = if info.rate > 0.0 {
        1.0 / info.rate
    } else {
        0.0
    };
    let mut pos = start;
    let mut i = 0;
    let mut texts = Vec::with_capacity(info.count);
    while i < count {
        let Some(&flag) = data.get(pos) else { break };
        let stamp = match flag {
            0 => None,
            // A time stamp that isn't a number is deduced instead.
            8 => match f64_at(data, pos + 1) {
                Some(t) => Some(t).filter(|t| t.is_finite()),
                None => break,
            },
            _ => {
                warnings.push(format!(
                    "a Samples chunk of {} at byte {start}: sample {i} has an invalid \
                     timestamp flag ({flag}); the samples from there on are skipped",
                    stream.describe()
                ));
                stream.len += i;
                return;
            }
        };
        pos += if flag == 8 { 9 } else { 1 };
        texts.clear();
        let mut complete = true;
        for _ in 0..info.count {
            match format {
                ValueFormat::String => match varlen(data, pos) {
                    Some((n, after)) => {
                        let n = usize::try_from(n).unwrap_or(usize::MAX);
                        match data.get(after..after.saturating_add(n)) {
                            Some(bytes) => {
                                texts.push(String::from_utf8_lossy(bytes).into_owned());
                                pos = after + n;
                            }
                            None => complete = false,
                        }
                    }
                    None => complete = false,
                },
                ValueFormat::Numeric(encoding) => match data.get(pos..pos + encoding.size()) {
                    Some(bytes) => {
                        if let Some(text) = value_text(encoding, bytes) {
                            texts.push(text);
                        }
                        pos += encoding.size();
                    }
                    None => complete = false,
                },
            }
            if !complete {
                break;
            }
        }
        if !complete {
            break;
        }
        i += 1;
        let time = match stamp {
            Some(t) => Some(t),
            None => stream.last_stamp.map(|t| t + step),
        };
        let Some(time) = time else {
            stream.unstamped += 1;
            continue;
        };
        stream.last_stamp = Some(time);
        match format {
            ValueFormat::String => stream.events.extend(
                texts
                    .iter()
                    .filter(|t| !t.is_empty())
                    .map(|t| (time, t.clone())),
            ),
            ValueFormat::Numeric(_) if !texts.is_empty() => {
                stream.events.push((time, texts.join(",")));
            }
            ValueFormat::Numeric(_) => {}
        }
    }
    stream.len += i;
    if i < count {
        warnings.push(format!(
            "a Samples chunk of {} at byte {start} ends inside sample {i}; the {i} samples \
             before it are read",
            stream.describe()
        ));
    }
}

impl Scan {
    /// Warnings about streams that only show once the whole file is scanned.
    pub fn finish(&mut self) {
        if !self.unknown.is_empty() {
            let count: u64 = self.unknown.values().sum();
            let tags: Vec<String> = self.unknown.keys().map(u16::to_string).collect();
            self.warnings.push(format!(
                "{count} chunk{} of tags XDF 1.0 doesn't define ({}) {} skipped",
                if count == 1 { "" } else { "s" },
                tags.join(", "),
                if count == 1 { "is" } else { "are" }
            ));
        }
        for stream in &self.streams {
            if stream.unstamped > 0 {
                self.warnings.push(format!(
                    "{} has {} samples before its first timestamp, which are left out",
                    stream.describe(),
                    stream.unstamped
                ));
            }
        }
    }
}

/// A numeric value as numpy prints it (pyxdf's arrays, as mnextend's descriptions show
/// them): integers as they are, floats in the shortest form that reads back as the same
/// float32 or float64; `None` for NaN.
pub(crate) fn value_text(encoding: Encoding, bytes: &[u8]) -> Option<String> {
    match encoding {
        Encoding::I64 => Some(i64::from_le_bytes(bytes.get(..8)?.try_into().ok()?).to_string()),
        Encoding::F32 => {
            let v = f32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
            (!v.is_nan()).then(|| numpy_float(&shortest(v), f64::from(v), 1e6))
        }
        Encoding::F64 => {
            let v = f64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
            (!v.is_nan()).then(|| numpy_float(&shortest(v), v, 1e16))
        }
        other => {
            let v = Decoder::new(other, ByteOrder::Little).value(bytes)?;
            Some((v as i64).to_string())
        }
    }
}

/// The shortest exponent form of `v` that reads back as `v` (`"1.2345e4"`), the one nearest
/// to `v` where two are as short, ties to an even last digit, as Python and numpy choose:
/// Rust's shortest form has the right length but may round a tie up, so `v` is formatted
/// again to that many digits, which rounds exactly.
fn shortest<T: std::fmt::LowerExp>(v: T) -> String {
    let sci = format!("{v:e}");
    let mantissa = sci.split('e').next().unwrap_or("");
    let digits = mantissa.chars().filter(char::is_ascii_digit).count();
    format!("{v:.*e}", digits.saturating_sub(1))
}

/// numpy's `str` of a float `value` from Rust's shortest exponent form of it (`"1.2345e4"`):
/// positional for 0 and from 1e-4 up to `cutoff` (1e16 for float64, as Python's `repr`; 1e6
/// for float32), as `"12345.0"` or `"0.0001"`; otherwise with an exponent of at least two
/// digits (`"1e-05"`, `"1.5e+16"`). The value decides, not its digits: float32's 0.0001 is
/// just below 1e-4, so it is `"1e-04"`.
pub(crate) fn numpy_float(sci: &str, value: f64, cutoff: f64) -> String {
    let (sign, body) = match sci.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", sci),
    };
    let Some((mantissa, exp)) = body.split_once('e') else {
        return sci.to_owned(); // "inf"
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return sci.to_owned();
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let positional = value == 0.0 || (1e-4..cutoff).contains(&value.abs());
    let text = if positional {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let mut int = digits.chars().take(int_len).collect::<String>();
            while int.len() < int_len {
                int.push('0');
            }
            let frac = digits.get(int_len..).unwrap_or("");
            format!("{int}.{}", if frac.is_empty() { "0" } else { frac })
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let dot = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{first}{dot}e{esign}{:02}", exp.unsigned_abs())
    };
    format!("{sign}{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_as_numpy_does() {
        let cases: [(f64, &str); 12] = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (0.1, "0.1"),
            (1e-4, "0.0001"),
            (1.5e-5, "1.5e-05"),
            (1e16, "1e+16"),
            (123456789012345.6, "123456789012345.6"),
            (-2.5e-300, "-2.5e-300"),
            (f64::INFINITY, "inf"),
            (0.10047566890716553, "0.10047566890716553"),
        ];
        for (v, expected) in cases {
            assert_eq!(numpy_float(&shortest(v), v, 1e16), expected, "{v}");
        }
        // Halfway between two shortest forms (a float32 value as float64): the even one.
        let tie = f64::from(-0.132_862_09_f32);
        assert_eq!(
            numpy_float(&shortest(tie), tie, 1e16),
            "-0.13286209106445312"
        );
        let f32s: [(f32, &str); 7] = [
            (11092.734, "11092.734"),
            (1e-5, "1e-05"),
            (1e-4, "1e-04"),
            (1.0001e-4, "0.00010001"),
            (999_999.0, "999999.0"),
            (1e6, "1e+06"),
            (123_456_789.0, "1.2345679e+08"),
        ];
        for (v, expected) in f32s {
            let text = numpy_float(&shortest(v), f64::from(v), 1e6);
            assert_eq!(text, expected, "{v}");
        }
    }
}
