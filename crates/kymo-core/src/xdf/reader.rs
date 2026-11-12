//! Opening XDF files: streams to grids and annotations, and the block reader over a
//! read-only map of the file.

use std::collections::HashMap;
use std::fs::File;
use std::ops::Range;
use std::path::Path;

use memmap2::Mmap;

use super::XdfError;
use super::scan::{self, Chunk, Info, MAGIC, Mode, Stamps, Stream, ValueFormat};
use super::time::{self, Clock, Corrector, Dejitter, Layout, Segment};
use crate::decode::{ByteOrder, Decoder, frames_into};
use crate::mapped::{self, map_read_only};
use crate::{
    AnnotationData, BlockReader, Channel, ChannelId, ChannelKind, Grid, GridId, Header, OpenError,
    Opened, Origin, ReadError, Sample, SampleType, SamplesMut, Timebase, Unit,
};

/// Reads samples straight from the mapped file, chunk by chunk.
#[derive(Debug)]
pub struct XdfReader {
    map: Mmap,
    file: File,
    /// One per grid.
    grids: Vec<GridStream>,
}

/// A stream shown as channels: its chunks and how its samples decode.
#[derive(Debug)]
struct GridStream {
    /// Per segment of the stream: its first slot in the grid, its first sample in the
    /// stream, its length; in order.
    map: Vec<(u64, u64, u64)>,
    chunks: Vec<Chunk>,
    positions: Vec<u32>,
    /// Bytes of a sample without a timestamp: the flag and the values.
    frame: usize,
    /// Bytes per value.
    size: usize,
    native: SampleType,
    decoders: Vec<Decoder>,
    /// Its channels' ids are this and the following ones.
    first_channel: u32,
    /// Stim channels of float type, whose NaN reads as 0.
    stim: Vec<bool>,
}

/// Opens an XDF file. It is memory-mapped, never loaded; its chunks are walked once at open
/// to index the samples of streams shown as channels and to read the annotations.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |source| OpenError::Io {
        path: path.to_owned(),
        source,
    };
    let error = |source| OpenError::Xdf {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(io)?;
    let map = map_read_only(&file).map_err(io)?;
    if !map.starts_with(MAGIC) {
        return Err(error(XdfError::NotXdf));
    }
    // The scan touches every page of the file (each sample's timestamp flag), so a helper
    // reads the file into the cache ahead of it: plain reads fill the cache much faster
    // than the scan's page faults would (a cold 620 MB file: 0.73 s without).
    let mut scan = std::thread::scope(|s| {
        s.spawn(|| mapped::prefetch(&file, map.len() as u64, [0..map.len() as u64]));
        scan::scan(&map)
    });
    scan.finish();
    let mut warnings = std::mem::take(&mut scan.warnings);
    let mut streams = scan.streams;
    streams.sort_by_key(|s| s.id);

    // Time stamps: each stream's clock correction, and the segments of those shown.
    let clocks: Vec<Option<Clock>> = streams
        .iter()
        .map(|s| Clock::new(&s.clock_offsets, &s.describe(), &mut warnings))
        .collect();
    let timing: Vec<Option<Timing>> = streams
        .iter()
        .zip(&clocks)
        .map(|(s, clock)| {
            (s.mode == Mode::Channels && s.len > 0).then(|| timing(s, &map, clock.as_ref()))
        })
        .collect();

    let mut extra = Vec::new();
    if let Some(version) = &scan.version {
        extra.push(("XDF version".to_owned(), version.clone()));
    }
    for ((stream, clock), timing) in streams.iter().zip(&clocks).zip(&timing) {
        extra.push((
            format!("stream {}", stream.id),
            describe(stream, clock.as_ref(), timing.as_ref()),
        ));
        match &stream.mode {
            Mode::Unreadable(reason) if stream.info.is_some() => {
                warnings.push(format!("{} is not read: {reason}", stream.describe()));
            }
            Mode::Irregular => warnings.push(format!(
                "{} has an irregular rate and several channels, which kymo doesn't show yet; \
                 it is left out",
                stream.describe()
            )),
            _ => {}
        }
        if stream.info.is_some() && !stream.footer {
            warnings.push(format!(
                "{} has no footer; the recording may have been cut off",
                stream.describe()
            ));
        }
        if timing.is_some() && stream.info.as_ref().is_some_and(|i| i.can_drop) {
            warnings.push(format!(
                "{} can drop samples, so pyxdf doesn't dejitter it; kymo does until it reads \
                 such streams on their own timestamps",
                stream.describe()
            ));
        }
    }

    let shown: Vec<(&Stream, &Timing)> = streams
        .iter()
        .zip(&timing)
        .filter_map(|(s, t)| Some((s, t.as_ref()?)))
        .collect();
    if shown.is_empty() {
        let listed: Vec<String> = streams
            .iter()
            .zip(&clocks)
            .map(|(s, c)| format!("{}: {}", s.describe(), describe(s, c.as_ref(), None)))
            .collect();
        let listed = if listed.is_empty() {
            String::new()
        } else {
            format!(" ({})", listed.join("; "))
        };
        return Err(error(XdfError::NoChannels(listed)));
    }

    // t = 0 is the earliest first (synchronized, dejittered) time of the streams shown.
    let t0 = shown
        .iter()
        .filter_map(|(_, t)| t.segments.first().filter(|_| t.stamped).map(|s| s.start))
        .fold(f64::INFINITY, f64::min);
    let t0 = if t0.is_finite() { t0 } else { 0.0 };
    for (stream, timing) in &shown {
        if !timing.stamped {
            warnings.push(format!(
                "{} has no timestamps; it is placed at the start",
                stream.describe()
            ));
        }
    }

    let streams_shown: Vec<&Stream> = shown.iter().map(|(s, _)| *s).collect();
    let names = labels(&streams_shown);
    let mut channels = Vec::new();
    let mut grids = Vec::new();
    let mut readers = Vec::new();
    let mut unknown_units: Vec<String> = Vec::new();
    for (g, (stream, timing)) in shown.iter().enumerate() {
        let Some(info) = &stream.info else { continue };
        let ValueFormat::Numeric(encoding) = info.format else {
            continue;
        };
        let grid_id = GridId(g as u32);
        let first_channel = channels.len() as u32;
        let event_stream = scan::is_event_type(&info.kind);
        // A stream without time stamps starts at t = 0.
        let origin = if timing.stamped { t0 } else { 0.0 };
        let layout = time::layout(&timing.segments, timing.rate, origin);
        if layout.damaged > 0 {
            warnings.push(format!(
                "{}: {} of its segments start more than 30 days after the previous one, or \
                 at a time that isn't a number, which is taken for damaged time stamps; each \
                 follows the previous one directly",
                stream.describe(),
                layout.damaged
            ));
        }
        if layout.moved > 0 {
            warnings.push(format!(
                "{}: {} of its segments start before the previous one ends; each follows the \
                 previous one directly",
                stream.describe(),
                layout.moved
            ));
        }
        let reader = GridStream {
            map: layout.map.clone(),
            chunks: stream.chunks.clone(),
            positions: stream.positions.clone(),
            frame: 1 + info.count * encoding.size(),
            size: encoding.size(),
            native: encoding.native(),
            decoders: Vec::new(),
            first_channel,
            stim: Vec::new(),
        };
        let mut decoders = Vec::with_capacity(info.count);
        let mut stim = Vec::with_capacity(info.count);
        for (c, name) in names[g].iter().enumerate() {
            let described = info.channels.get(c);
            let mut channel = Channel::new(name.clone(), grid_id);
            channel.kind = kind(event_stream, described.map_or("", |d| d.kind.as_str()));
            let unit = described.map_or("", |d| d.unit.trim());
            if !unit.is_empty() && unit != "NA" {
                channel.unit = Unit::from_label(unit);
                // Stim channels show codes, without a unit.
                if channel.unit.to_si.is_none()
                    && channel.kind != ChannelKind::Stim
                    && !unscaled_unit(unit)
                    && !unknown_units.iter().any(|u| u == unit)
                {
                    unknown_units.push(unit.to_owned());
                }
            }
            let decoder = Decoder::new(encoding, ByteOrder::Little);
            let reference = if channel.kind == ChannelKind::Stim {
                0.0
            } else {
                reader
                    .first_value(&map, c)
                    .map_or(0.0, |bytes| decoder.reference_from(bytes))
            };
            let decoder = decoder.with_reference(reference);
            channel.offset = decoder.reference();
            stim.push(channel.kind == ChannelKind::Stim && reader.native == SampleType::F32);
            decoders.push(decoder);
            channels.push(channel);
        }
        let grid = grid_of(
            &layout,
            timing.rate,
            reader.native,
            (first_channel..channels.len() as u32)
                .map(ChannelId)
                .collect(),
        )
        .map_err(|e| error(e.into()))?;
        grids.push(grid);
        readers.push(GridStream {
            decoders,
            stim,
            ..reader
        });
    }
    if !unknown_units.is_empty() {
        warnings.push(format!(
            "units not recognized, so their channels are not scaled: {}",
            unknown_units.join(", ")
        ));
    }

    // Annotations at their synchronized times (irregular streams aren't dejittered).
    let mut annotations = Vec::new();
    for (stream, clock) in streams.iter().zip(&clocks) {
        if stream.mode != Mode::Annotations {
            continue;
        }
        let mut corrector = Corrector::new(clock.as_ref());
        let before = annotations.len();
        annotations.extend(stream.events.iter().filter_map(|(t, text)| {
            let onset = corrector.correct(*t) - t0;
            onset.is_finite().then(|| AnnotationData {
                onset,
                duration: 0.0,
                description: text.clone(),
                channels: Vec::new(),
                origin: Origin::File,
            })
        }));
        let lost = stream.events.len() - (annotations.len() - before);
        if lost > 0 {
            warnings.push(format!(
                "{}: {lost} annotations at times that aren't numbers are left out",
                stream.describe()
            ));
        }
    }

    let start_time = scan
        .datetime
        .as_deref()
        .and_then(|text| start_time(text, &mut extra, &mut warnings));

    Ok(Opened {
        header: Header {
            format: "XDF",
            start_time,
            channels,
            grids,
            extra,
            bad_channels: Vec::new(),
        },
        annotations,
        warnings,
        sources: vec![path.to_owned()],
        reader: Box::new(XdfReader {
            map,
            file,
            grids: readers,
        }),
    })
}

/// The time stamps of a stream shown as channels: its dejittered segments (times absolute)
/// and its effective rate.
struct Timing {
    segments: Vec<Segment>,
    rate: f64,
    /// Whether the stream has time stamps at all; without, one segment from 0 at the
    /// nominal rate.
    stamped: bool,
}

/// Synchronizes and dejitters the time stamps of a stream shown as channels.
fn timing(stream: &Stream, map: &[u8], clock: Option<&Clock>) -> Timing {
    let nominal = stream.info.as_ref().map_or(1.0, |i| i.rate);
    let frame = stream.info.as_ref().map_or(1, |i| match i.format {
        ValueFormat::Numeric(e) => 1 + i.count * e.size(),
        ValueFormat::String => 1,
    });
    if stream.first_stamp.is_none() {
        return Timing {
            segments: vec![Segment {
                first: 0,
                len: stream.len,
                start: 0.0,
                step: Some(1.0 / nominal),
            }],
            rate: nominal,
            stamped: false,
        };
    }
    let mut corrector = Corrector::new(clock);
    let mut dejitter = Dejitter::new(nominal);
    timestamps(stream, map, frame, 1.0 / nominal, |i, t| {
        dejitter.push(i, corrector.correct(t));
    });
    let segments = dejitter.segments();
    let rate = time::effective_rate(&segments).unwrap_or(nominal);
    Timing {
        segments,
        rate,
        stamped: true,
    }
}

/// Feeds `f` the raw time stamp of each sample of a stream shown as channels, in order: its
/// own where it has one, else the previous sample's plus a sample period (as pyxdf deduces
/// them); samples before the first time stamp count back from it.
fn timestamps(stream: &Stream, map: &[u8], frame: usize, tdiff: f64, mut f: impl FnMut(u64, f64)) {
    let Some((q, first)) = stream.first_stamp else {
        return;
    };
    let mut previous: Option<f64> = None;
    for chunk in &stream.chunks {
        let positions: &[u32] = match &chunk.stamps {
            Stamps::Some(r) => &stream.positions[r.clone()],
            _ => &[],
        };
        let mut next = 0;
        for k in 0..chunk.len {
            let i = chunk.first + k;
            let before = match &chunk.stamps {
                Stamps::All => Some(k as usize),
                Stamps::None => None,
                Stamps::Some(_) => (positions.get(next) == Some(&(k as u32))).then(|| {
                    next += 1;
                    next - 1
                }),
            };
            // A time stamp that isn't a number is deduced instead.
            let own = before
                .and_then(|b| scan::f64_at(map, chunk.offset + k as usize * frame + 8 * b + 1))
                .filter(|t| t.is_finite());
            let t = match (own, previous) {
                (Some(t), _) => t,
                (None, Some(p)) => p + tdiff,
                (None, None) => first - (q - i) as f64 * tdiff,
            };
            f(i, t);
            previous = Some(t);
        }
    }
}

/// The grid of a stream laid out as `layout`: a regular time base for one segment, a
/// segmented one for several.
fn grid_of(
    layout: &Layout,
    rate: f64,
    native: SampleType,
    channels: Vec<ChannelId>,
) -> Result<Grid, crate::ModelError> {
    let timebase = match layout.pieces.as_slice() {
        [one] => Timebase::Regular {
            rate: one.rate,
            start: one.start,
        },
        pieces => Timebase::Segmented {
            rate,
            pieces: pieces.to_vec(),
        },
    };
    let segments = layout
        .map
        .iter()
        .map(|&(slot, _, len)| slot..slot + len)
        .collect();
    Grid::new(timebase, layout.len, segments, native, channels)
}

/// A stream for the info view: name, type, channels, rate, samples and how it is read;
/// for streams shown as channels, the effective rate, segments and clock resets.
fn describe(stream: &Stream, clock: Option<&Clock>, timing: Option<&Timing>) -> String {
    let Some(info) = &stream.info else {
        return "no header".to_owned();
    };
    let name = if info.kind.is_empty() {
        format!("{:?}", info.name)
    } else {
        format!("{:?} ({})", info.name, info.kind)
    };
    let how = match &stream.mode {
        Mode::Unreadable(reason) => return format!("{name}, not read: {reason}"),
        _ if stream.len == 0 => "no samples",
        Mode::Channels => "channels",
        Mode::Annotations => "annotations",
        Mode::Irregular => "left out (irregular channels)",
    };
    let rate = if info.rate > 0.0 {
        format!("{} Hz", info.rate)
    } else {
        "irregular".to_owned()
    };
    let mut text = format!(
        "{name}, {} × {}, {rate}, {} samples: {how}",
        info.count,
        info.format.name(),
        stream.len
    );
    if let Some(timing) = timing.filter(|t| t.stamped) {
        text.push_str(&format!(
            "; effective rate {:.6} Hz, {} segment{}",
            timing.rate,
            timing.segments.len(),
            if timing.segments.len() == 1 { "" } else { "s" }
        ));
    }
    match clock.map(Clock::resets) {
        Some(0) | None => {}
        Some(1) => text.push_str(", 1 clock reset"),
        Some(n) => text.push_str(&format!(", {n} clock resets")),
    }
    text
}

/// The kind of a channel: stim in streams of an event type and for the types "stim" and
/// "trigger(s)", otherwise its MNE type, misc if it has none.
fn kind(event_stream: bool, channel_type: &str) -> ChannelKind {
    let lower = channel_type.trim().to_ascii_lowercase();
    if event_stream || matches!(lower.as_str(), "stim" | "trigger" | "triggers") {
        ChannelKind::Stim
    } else {
        ChannelKind::from_mne(&lower).unwrap_or(ChannelKind::Misc)
    }
}

/// Each shown stream's channel labels: as described (`"{stream name}_{n}"` where empty or
/// missing), a label that several streams have prefixed by its stream's qualifier.
fn labels(shown: &[&Stream]) -> Vec<Vec<String>> {
    let mut labels: Vec<Vec<String>> = shown
        .iter()
        .map(|s| {
            let Some(info) = &s.info else {
                return Vec::new();
            };
            (0..info.count)
                .map(|c| match info.channels.get(c) {
                    Some(d) if !d.label.is_empty() => d.label.clone(),
                    _ => format!("{}_{c}", info.name),
                })
                .collect()
        })
        .collect();
    let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
    for (g, names) in labels.iter().enumerate() {
        for name in names {
            let owner = owners.entry(name.as_str()).or_default();
            if !owner.contains(&g) {
                owner.push(g);
            }
        }
    }
    let colliding: Vec<(usize, String)> = labels
        .iter()
        .enumerate()
        .flat_map(|(g, names)| {
            names
                .iter()
                .filter(|n| owners.get(n.as_str()).is_some_and(|o| o.len() > 1))
                .map(move |n| (g, n.clone()))
        })
        .collect();
    let qualifiers: Vec<String> = (0..shown.len()).map(|g| qualifier(shown, g)).collect();
    for (g, name) in colliding {
        for label in labels[g].iter_mut().filter(|l| **l == name) {
            *label = format!("{} {name}", qualifiers[g]);
        }
    }
    labels
}

/// What tells stream `g` apart from the other shown streams: its name if no other has it,
/// else its type if no other of that name has it, else its id.
fn qualifier(shown: &[&Stream], g: usize) -> String {
    let info = |k: usize| shown[k].info.as_ref();
    let field = |k: usize, f: fn(&Info) -> &str| info(k).map_or("", f).to_owned();
    let name = field(g, |i| &i.name);
    let others = (0..shown.len()).filter(|&k| k != g);
    let same_name: Vec<usize> = others.filter(|&k| field(k, |i| &i.name) == name).collect();
    if !name.is_empty() && same_name.is_empty() {
        return name;
    }
    let kind = field(g, |i| &i.kind);
    if !kind.is_empty() && same_name.iter().all(|&k| field(k, |i| &i.kind) != kind) {
        return kind;
    }
    format!("stream {}", shown[g].id)
}

/// Units mnextend recognizes that have no quantity in kymo (lengths, angles, temperatures,
/// times, frequencies, pixels): their channels keep their values without a warning.
fn unscaled_unit(unit: &str) -> bool {
    let unit = unit.trim().replace('\u{3bc}', "\u{b5}");
    const SYMBOLS: [&str; 9] = ["m", "rad", "\u{b0}C", "K", "s", "Hz", "deg", "\u{b0}", "px"];
    const PREFIXABLE: [&str; 6] = ["m", "rad", "\u{b0}C", "K", "s", "Hz"];
    const PREFIXES: [char; 8] = ['f', 'p', 'n', '\u{b5}', 'u', 'm', 'c', 'k'];
    const WORDS: [&str; 23] = [
        "meter",
        "meters",
        "metre",
        "metres",
        "centimeter",
        "centimeters",
        "centimetre",
        "centimetres",
        "millimeter",
        "millimeters",
        "millimetre",
        "millimetres",
        "radian",
        "radians",
        "degree",
        "degrees",
        "celsius",
        "degrees celsius",
        "kelvin",
        "second",
        "seconds",
        "hertz",
        "pixels",
    ];
    if SYMBOLS.contains(&unit.as_str()) {
        return true;
    }
    let mut chars = unit.chars();
    if let Some(p) = chars.next()
        && PREFIXES.contains(&p)
        && PREFIXABLE.contains(&chars.as_str())
    {
        return true;
    }
    let lower = unit.to_lowercase();
    WORDS.contains(&lower.as_str())
        || matches!(lower.as_str(), "millisecond" | "milliseconds" | "pixel")
}

/// The file header's `datetime` (`2021-12-15T10:46:02+0100`, the offset as `+HH:MM`,
/// `+HHMM` or `Z`, or none) as the start time in the file's local time, with its time zone
/// in `extra`.
fn start_time(
    text: &str,
    extra: &mut Vec<(String, String)>,
    warnings: &mut Vec<String>,
) -> Option<jiff::civil::DateTime> {
    let text = text.trim();
    let date_end = text.find('T').unwrap_or(0);
    let (local, offset) = if let Some(local) = text.strip_suffix('Z') {
        (local, Some(Some(0)))
    } else {
        match text[date_end..].rfind(['+', '-']) {
            Some(k) => {
                let k = date_end + k;
                (&text[..k], Some(utc_offset(&text[k..])))
            }
            None => (text, None),
        }
    };
    let parsed = local.parse::<jiff::civil::DateTime>().ok();
    let Some(time) = parsed.filter(|_| offset != Some(None)) else {
        warnings.push(format!(
            "the recording's date and time {text:?} can't be read; it has no start time"
        ));
        return None;
    };
    if let Some(Some(seconds)) = offset {
        let zone = if seconds == 0 {
            "UTC".to_owned()
        } else {
            let sign = if seconds < 0 { '-' } else { '+' };
            let minutes = seconds.unsigned_abs() / 60;
            format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
        };
        extra.push(("time zone".to_owned(), zone));
    }
    Some(time)
}

/// `+HH:MM`, `+HHMM` or `+HH` in seconds.
fn utc_offset(text: &str) -> Option<i32> {
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
    let minutes: i32 = match digits.get(2..) {
        Some(m) if !m.is_empty() => m.parse().ok()?,
        _ => 0,
    };
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

/// A run of samples within a chunk that are laid out alike: all with a timestamp or all
/// without.
struct Run {
    /// Byte position of its first sample.
    byte: usize,
    stamped: bool,
    /// Its first sample within the chunk.
    start: u64,
    len: u64,
}

impl GridStream {
    /// How many samples before `k` in `chunk` have a timestamp.
    fn stamped_before(&self, chunk: &Chunk, k: u64) -> u64 {
        match &chunk.stamps {
            Stamps::None => 0,
            Stamps::All => k,
            Stamps::Some(r) => {
                self.positions[r.clone()].partition_point(|&p| u64::from(p) < k) as u64
            }
        }
    }

    /// Byte position of sample `k` of `chunk` (its timestamp flag).
    fn byte_of(&self, chunk: &Chunk, k: u64) -> u64 {
        chunk.offset as u64 + k * self.frame as u64 + 8 * self.stamped_before(chunk, k)
    }

    /// The bytes of channel `c`'s first value, if the stream has samples.
    fn first_value<'a>(&self, map: &'a [u8], c: usize) -> Option<&'a [u8]> {
        let chunk = self.chunks.first()?;
        let stamped = self.stamped_before(chunk, 1) == 1;
        let at = chunk.offset + if stamped { 9 } else { 1 } + c * self.size;
        map.get(at..at + self.size)
    }

    /// Calls `f` with the runs that make up `range` of `chunk`, in order.
    fn runs(
        &self,
        chunk: &Chunk,
        range: Range<u64>,
        mut f: impl FnMut(Run) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        let len = range.end - range.start;
        match &chunk.stamps {
            Stamps::None | Stamps::All => {
                let stamped = chunk.stamps == Stamps::All;
                f(Run {
                    byte: self.byte_of(chunk, range.start) as usize,
                    stamped,
                    start: range.start,
                    len,
                })
            }
            Stamps::Some(r) => {
                let positions = &self.positions[r.clone()];
                let at = |q: usize| positions.get(q).map(|&p| u64::from(p));
                let mut k = range.start;
                let mut q = positions.partition_point(|&p| u64::from(p) < k);
                while k < range.end {
                    let byte = chunk.offset as u64 + k * self.frame as u64 + 8 * q as u64;
                    let stamped = at(q) == Some(k);
                    let mut e = k;
                    if stamped {
                        while e < range.end && at(q) == Some(e) {
                            e += 1;
                            q += 1;
                        }
                    } else {
                        e = at(q).map_or(range.end, |p| p.min(range.end));
                    }
                    f(Run {
                        byte: byte as usize,
                        stamped,
                        start: k,
                        len: e - k,
                    })?;
                    k = e;
                }
                Ok(())
            }
        }
    }

    /// The stream's samples in grid slots `range`: (their first slot, the samples), segment
    /// by segment; slots in gaps have none.
    fn samples_in(&self, range: Range<u64>) -> impl Iterator<Item = (u64, Range<u64>)> + '_ {
        let first = self
            .map
            .partition_point(|&(slot, _, len)| slot + len <= range.start);
        self.map[first..]
            .iter()
            .take_while(move |&&(slot, _, _)| slot < range.end)
            .filter_map(move |&(slot, sample, len)| {
                let from = range.start.max(slot);
                let to = range.end.min(slot + len);
                (from < to).then(|| (from, sample + (from - slot)..sample + (to - slot)))
            })
    }

    /// Decodes grid slots `range` of the channels at `locals` into `dst`, channel-major,
    /// segment by segment.
    fn copy<T: Sample>(
        &self,
        map: &[u8],
        grid: GridId,
        locals: &[usize],
        range: Range<u64>,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let n = (range.end - range.start) as usize;
        let mut covered = 0;
        for (slot, samples) in self.samples_in(range.clone()) {
            covered += samples.end - samples.start;
            let at = (slot - range.start) as usize;
            self.copy_samples(map, grid, locals, samples, dst, n, at)?;
        }
        if covered < range.end - range.start {
            return Err(ReadError::NotPresent { grid, range });
        }
        Ok(())
    }

    /// Decodes the stream's samples `range` of the channels at `locals` into `dst`, channel
    /// `k`'s at `dst[k * stride + at..]`, chunk by chunk and run by run.
    #[allow(clippy::too_many_arguments)]
    fn copy_samples<T: Sample>(
        &self,
        map: &[u8],
        grid: GridId,
        locals: &[usize],
        range: Range<u64>,
        dst: &mut [T],
        stride: usize,
        base: usize,
    ) -> Result<(), ReadError> {
        let columns = |base: usize| -> Vec<(usize, Decoder)> {
            locals
                .iter()
                .map(|&c| (base + c * self.size, self.decoders[c]))
                .collect()
        };
        let (plain, stamped) = (columns(1), columns(9));
        let not_present = || ReadError::NotPresent {
            grid,
            range: range.clone(),
        };
        let mut b = self
            .chunks
            .partition_point(|c| c.first + c.len <= range.start);
        let mut i = range.start;
        while i < range.end {
            let chunk = self
                .chunks
                .get(b)
                .filter(|c| c.first <= i)
                .ok_or_else(not_present)?;
            let j = i - chunk.first;
            let take = (chunk.len - j).min(range.end - i);
            self.runs(chunk, j..j + take, |run| {
                let (frame, columns) = if run.stamped {
                    (self.frame + 8, &stamped)
                } else {
                    (self.frame, &plain)
                };
                let data = map.get(run.byte..).unwrap_or_default();
                let at = base + (chunk.first + run.start - range.start) as usize;
                frames_into(data, frame, columns, 0..run.len, dst, stride, at)
            })?;
            i += take;
            b += 1;
        }
        Ok(())
    }
}

impl BlockReader for XdfReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        let stream = self
            .grids
            .get(grid.0 as usize)
            .ok_or(ReadError::UnknownGrid(grid))?;
        let locals = channels
            .iter()
            .map(|&channel| {
                channel
                    .0
                    .checked_sub(stream.first_channel)
                    .map(|c| c as usize)
                    .filter(|&c| c < stream.decoders.len())
                    .ok_or(ReadError::ChannelNotInGrid { channel, grid })
            })
            .collect::<Result<Vec<usize>, ReadError>>()?;
        let n = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(usize::MAX);
        let expected = channels.len().saturating_mul(n);
        match out {
            SamplesMut::I16(dst) if dst.len() == expected && stream.native == SampleType::I16 => {
                stream.copy(&self.map, grid, &locals, range, dst)
            }
            SamplesMut::I32(dst) if dst.len() == expected && stream.native == SampleType::I32 => {
                stream.copy(&self.map, grid, &locals, range, dst)
            }
            SamplesMut::F32(dst) if dst.len() == expected && stream.native == SampleType::F32 => {
                stream.copy(&self.map, grid, &locals, range, dst)?;
                // Stim channels' NaN is "no event".
                for (k, &c) in locals.iter().enumerate() {
                    if stream.stim[c] {
                        for v in &mut dst[k * n..(k + 1) * n] {
                            if v.is_nan() {
                                *v = 0.0;
                            }
                        }
                    }
                }
                Ok(())
            }
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type: stream.native,
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        let Some(stream) = self.grids.get(grid.0 as usize) else {
            return;
        };
        let mut ranges: Vec<Range<u64>> = Vec::new();
        for (_, samples) in stream.samples_in(range) {
            let first = stream
                .chunks
                .partition_point(|c| c.first + c.len <= samples.start);
            ranges.extend(
                stream.chunks[first..]
                    .iter()
                    .take_while(|c| c.first < samples.end)
                    .map(|c| {
                        let from = samples.start.max(c.first) - c.first;
                        let to = samples.end.min(c.first + c.len) - c.first;
                        stream.byte_of(c, from)..stream.byte_of(c, to)
                    }),
            );
        }
        mapped::prefetch(&self.file, self.map.len() as u64, ranges);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_mnextend_scales_but_kymo_has_no_quantity_for() {
        for unit in [
            "mm", "cm", "rad", "deg", "°", "°C", "K", "ms", "Hz", "kHz", "px",
        ] {
            assert!(unscaled_unit(unit), "{unit}");
        }
        for unit in ["Meters", "degrees Celsius", "pixel", "Milliseconds"] {
            assert!(unscaled_unit(unit), "{unit}");
        }
        for unit in ["kiloohm", "bpm", "%", "pxx", "kpx", "integer"] {
            assert!(!unscaled_unit(unit), "{unit}");
        }
    }
}
