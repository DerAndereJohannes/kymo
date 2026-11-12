//! XDF writer: a file header, the stream headers, the Samples chunks of every stream
//! interleaved by time (with clock offsets and boundary chunks among them, if asked for),
//! and the stream footers. Also [`Chunks`], which writes single chunks for tests.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::Path;

use crate::header::invalid;
use crate::{Error, Spec};

/// The value format of a numeric XDF stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XdfFormat {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl XdfFormat {
    pub const ALL: [XdfFormat; 6] = [
        XdfFormat::I8,
        XdfFormat::I16,
        XdfFormat::I32,
        XdfFormat::I64,
        XdfFormat::F32,
        XdfFormat::F64,
    ];

    /// The `channel_format` of the stream header.
    pub fn name(self) -> &'static str {
        match self {
            XdfFormat::I8 => "int8",
            XdfFormat::I16 => "int16",
            XdfFormat::I32 => "int32",
            XdfFormat::I64 => "int64",
            XdfFormat::F32 => "float32",
            XdfFormat::F64 => "double64",
        }
    }

    pub fn size(self) -> usize {
        match self {
            XdfFormat::I8 => 1,
            XdfFormat::I16 => 2,
            XdfFormat::I32 | XdfFormat::F32 => 4,
            XdfFormat::I64 | XdfFormat::F64 => 8,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, XdfFormat::F32 | XdfFormat::F64)
    }

    /// The digital values a channel of this format may use (for float32, the integers it
    /// stores exactly).
    pub fn limits(self) -> (i32, i32) {
        match self {
            XdfFormat::I8 => (-128, 127),
            XdfFormat::I16 => (-32_768, 32_767),
            XdfFormat::F32 => (-16_777_216, 16_777_216),
            _ => (i32::MIN, i32::MAX),
        }
    }

    /// Writes `value` little-endian (integers rounded, NaN as 0).
    pub fn put(self, value: f64, out: &mut Vec<u8>) {
        match self {
            XdfFormat::I8 => out.push(value as i8 as u8),
            XdfFormat::I16 => out.extend_from_slice(&(value as i16).to_le_bytes()),
            XdfFormat::I32 => out.extend_from_slice(&(value as i32).to_le_bytes()),
            XdfFormat::I64 => out.extend_from_slice(&(value as i64).to_le_bytes()),
            XdfFormat::F32 => out.extend_from_slice(&(value as f32).to_le_bytes()),
            XdfFormat::F64 => out.extend_from_slice(&value.to_le_bytes()),
        }
    }
}

/// Which samples of a stream carry a timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdfStamps {
    All,
    /// The first of each chunk, as liblsl writes them.
    FirstOfChunk,
    /// Every `n`th sample of the stream (the first included).
    Every(u64),
    /// None at all.
    None,
}

/// A regular stream of the spec's channels.
#[derive(Clone, Debug, PartialEq)]
pub struct XdfStream {
    pub id: u32,
    pub name: String,
    /// The stream type ("EEG", "Markers", ...).
    pub kind: String,
    pub format: XdfFormat,
    /// Indices into [`Spec::channels`], all of the same rate.
    pub channels: Vec<usize>,
    /// Seconds after the earliest stream's first sample at which its first sample is.
    pub start: f64,
    pub stamps: XdfStamps,
    /// Samples per chunk.
    pub chunk: u64,
    /// Chunks alternate between `chunk` samples and a third of that.
    pub varying_chunks: bool,
    /// The header describes the channels (label, type, unit).
    pub described: bool,
    pub footer: bool,
}

impl XdfStream {
    /// A described float32 stream of `channels` from the start, timestamped at the first
    /// sample of each chunk of 32 samples, with a footer.
    pub fn new(id: u32, name: &str, kind: &str, channels: Vec<usize>) -> Self {
        XdfStream {
            id,
            name: name.into(),
            kind: kind.into(),
            format: XdfFormat::F32,
            channels,
            start: 0.0,
            stamps: XdfStamps::FirstOfChunk,
            chunk: 32,
            varying_chunks: false,
            described: true,
            footer: true,
        }
    }
}

/// The values of a marker stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerValues {
    /// One string channel: each text of an annotation is a sample.
    Strings,
    /// Numeric channels: each text is a sample, its values separated by commas ("nan" for
    /// NaN).
    Numeric(XdfFormat, usize),
}

/// A stream of some of the spec's annotations, each sample at its onset.
#[derive(Clone, Debug, PartialEq)]
pub struct XdfMarkers {
    pub id: u32,
    pub name: String,
    pub kind: String,
    pub values: MarkerValues,
    /// The nominal rate, 0 for an irregular stream.
    pub rate: f64,
    /// Which of [`Spec::annotations`] it holds.
    pub annotations: Range<usize>,
}

/// XDF's own options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XdfOptions {
    /// The regular streams; none writes one described float32 stream per rate (ids from 1,
    /// "EEG").
    pub streams: Vec<XdfStream>,
    pub markers: Vec<XdfMarkers>,
    /// The file header's `datetime`, as written; none writes none.
    pub datetime: Option<String>,
    /// The LSL time (seconds) of the earliest stream's first sample.
    pub clock: f64,
    /// A ClockOffset chunk (offset 0) for each stream every 8 of its chunks.
    pub clock_offsets: bool,
    /// A Boundary chunk every 16 chunks.
    pub boundaries: bool,
    /// Chunk lengths and sample counts as 8-byte numbers, rather than the smallest width.
    pub wide_lengths: bool,
}

/// The regular streams written: [`XdfOptions::streams`], or one per rate.
pub fn streams(spec: &Spec) -> Vec<XdfStream> {
    if !spec.xdf.streams.is_empty() {
        return spec.xdf.streams.clone();
    }
    let mut rates: Vec<u32> = spec.channels.iter().map(|c| c.samples_per_record).collect();
    rates.sort_unstable();
    rates.dedup();
    rates
        .iter()
        .enumerate()
        .map(|(k, &spr)| {
            let channels = (0..spec.channels.len())
                .filter(|&c| spec.channels[c].samples_per_record == spr)
                .collect();
            XdfStream::new(
                k as u32 + 1,
                &format!("kymo-testgen {}", k + 1),
                "EEG",
                channels,
            )
        })
        .collect()
}

/// The spec's channels in the order kymo lists them: by stream id, then as in the stream.
pub fn channel_order(spec: &Spec) -> Vec<usize> {
    let mut streams = streams(spec);
    streams.sort_by_key(|s| s.id);
    streams.into_iter().flat_map(|s| s.channels).collect()
}

/// The stream of channel `c` and its index there.
pub fn stream_of(spec: &Spec, c: usize) -> Option<(XdfStream, usize)> {
    streams(spec).into_iter().find_map(|s| {
        s.channels
            .iter()
            .position(|&k| k == c)
            .map(|i| (s.clone(), i))
    })
}

pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    if spec.channels.is_empty() {
        return Err(invalid("at least one data channel is needed"));
    }
    if !spec.gaps.is_empty() || spec.first_onset != 0.0 {
        return Err(invalid("XDF has no gaps and no first onset"));
    }
    if !(spec.record_duration.is_finite() && spec.record_duration > 0.0) {
        return Err(invalid("the record duration must be > 0"));
    }
    if !spec.xdf.clock.is_finite() {
        return Err(invalid("the clock must be finite"));
    }
    let streams = streams(spec);
    let mut ids: Vec<u32> = streams
        .iter()
        .map(|s| s.id)
        .chain(spec.xdf.markers.iter().map(|m| m.id))
        .collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid("stream ids must be unique"));
    }
    let mut used = vec![false; spec.channels.len()];
    for s in &streams {
        if s.channels.is_empty() || s.chunk == 0 || !(s.start.is_finite() && s.start >= 0.0) {
            return Err(invalid(format!(
                "stream {} needs channels, chunks of at least one sample and a start >= 0",
                s.id
            )));
        }
        let spr = spec.channels[s.channels[0]].samples_per_record;
        for &c in &s.channels {
            let ch = spec
                .channels
                .get(c)
                .ok_or_else(|| invalid(format!("stream {} lists channel {c}", s.id)))?;
            if std::mem::replace(&mut used[c], true) {
                return Err(invalid(format!("channel {c} is in two streams")));
            }
            if ch.samples_per_record != spr {
                return Err(invalid(format!(
                    "stream {}'s channels differ in rate",
                    s.id
                )));
            }
            let (lo, hi) = s.format.limits();
            if ch.digital_min < lo || ch.digital_max > hi {
                return Err(invalid(format!(
                    "channel {:?} doesn't fit {}",
                    ch.label,
                    s.format.name()
                )));
            }
            if !s.format.is_float() && ch.gain() != 1.0 {
                return Err(invalid(format!(
                    "channel {:?} of an integer stream needs a gain of 1",
                    ch.label
                )));
            }
        }
    }
    if used.iter().any(|u| !u) {
        return Err(invalid("every channel must be in a stream"));
    }
    for m in &spec.xdf.markers {
        let annotations = spec
            .annotations
            .get(m.annotations.clone())
            .ok_or_else(|| invalid(format!("marker stream {} lists no annotations", m.id)))?;
        if let MarkerValues::Numeric(_, n) = m.values {
            for text in annotations.iter().flat_map(|a| &a.texts) {
                if numbers(text).is_none_or(|v| v.len() != n) {
                    return Err(invalid(format!(
                        "marker {text:?} of stream {} isn't {n} numbers",
                        m.id
                    )));
                }
            }
        }
    }
    Ok(())
}

fn numbers(text: &str) -> Option<Vec<f64>> {
    text.split(',')
        .map(|v| v.trim().parse::<f64>().ok())
        .collect()
}

/// Writes `spec` to a new file at `path`.
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let file = File::create(path)?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    write_to(spec, &mut out)?;
    out.flush()?;
    Ok(())
}

/// The size of the file [`write`] writes.
pub(crate) fn file_size(spec: &Spec) -> Result<u64, Error> {
    validate(spec)?;
    let mut counter = Counter(0);
    write_to(spec, &mut counter)?;
    Ok(counter.0)
}

struct Counter(u64);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Text for XML, with `&`, `<` and `>` escaped.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A stream header's XML, as LabRecorder writes it; `channels` (label, type, unit) are
/// described if given.
pub fn header_xml(
    name: &str,
    kind: &str,
    count: usize,
    rate: f64,
    format: &str,
    channels: Option<&[(String, String, String)]>,
) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n<info>\n\t<name>{}</name>\n\t<type>{}</type>\n\t\
         <channel_count>{count}</channel_count>\n\t<nominal_srate>{rate:.13}</nominal_srate>\n\t\
         <channel_format>{format}</channel_format>\n\t<source_id>kymo-testgen</source_id>\n\t\
         <version>1.1000000000000001</version>\n\t<created_at>0</created_at>\n\t\
         <uid>00000000-0000-0000-0000-000000000000</uid>\n\t<session_id>default</session_id>\n\t\
         <hostname>localhost</hostname>\n\t<v4address />\n\t<v4data_port>0</v4data_port>\n\t\
         <desc>",
        escape(name),
        escape(kind)
    );
    if let Some(channels) = channels {
        xml.push_str("\n\t\t<channels>");
        for (label, kind, unit) in channels {
            xml.push_str(&format!(
                "\n\t\t\t<channel>\n\t\t\t\t<label>{}</label>\n\t\t\t\t<type>{}</type>\n\t\t\t\t\
                 <unit>{}</unit>\n\t\t\t</channel>",
                escape(label),
                escape(kind),
                escape(unit)
            ));
        }
        xml.push_str("\n\t\t</channels>\n\t");
    }
    xml.push_str("</desc>\n</info>\n");
    xml
}

/// Writes single chunks, for files put together by hand in tests.
#[derive(Clone, Debug, Default)]
pub struct Chunks {
    pub bytes: Vec<u8>,
    /// Lengths and counts as 8-byte numbers.
    pub wide: bool,
}

/// Chunk tags.
pub mod tag {
    pub const FILE_HEADER: u16 = 1;
    pub const STREAM_HEADER: u16 = 2;
    pub const SAMPLES: u16 = 3;
    pub const CLOCK_OFFSET: u16 = 4;
    pub const BOUNDARY: u16 = 5;
    pub const STREAM_FOOTER: u16 = 6;
}

/// Boundary chunks' 16 bytes.
const BOUNDARY: [u8; 16] = [
    0x43, 0xA5, 0x46, 0xDC, 0xCB, 0xF5, 0x41, 0x0F, 0xB3, 0x0E, 0xD5, 0x46, 0x73, 0x83, 0xCB, 0xE4,
];

impl Chunks {
    /// The magic bytes and a file header of `version` 1.0 with `datetime`, if given.
    pub fn new(datetime: Option<&str>) -> Self {
        let mut c = Chunks {
            bytes: b"XDF:".to_vec(),
            wide: false,
        };
        let datetime = datetime.map_or(String::new(), |d| format!("<datetime>{d}</datetime>"));
        let xml = format!("<?xml version=\"1.0\"?><info><version>1.0</version>{datetime}</info>");
        c.raw(tag::FILE_HEADER, xml.as_bytes());
        c
    }

    /// A variable-length number: its width (1, 4 or 8), then it.
    pub fn varlen(&self, n: u64, out: &mut Vec<u8>) {
        if self.wide || n > u64::from(u32::MAX) {
            out.push(8);
            out.extend_from_slice(&n.to_le_bytes());
        } else if n < 256 {
            out.push(1);
            out.push(n as u8);
        } else {
            out.push(4);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }

    /// A chunk of `tag` with `content`.
    pub fn raw(&mut self, tag: u16, content: &[u8]) -> &mut Self {
        let mut head = Vec::new();
        self.varlen(content.len() as u64 + 2, &mut head);
        self.bytes.extend_from_slice(&head);
        self.bytes.extend_from_slice(&tag.to_le_bytes());
        self.bytes.extend_from_slice(content);
        self
    }

    pub fn stream_header(&mut self, id: u32, xml: &str) -> &mut Self {
        let mut content = id.to_le_bytes().to_vec();
        content.extend_from_slice(xml.as_bytes());
        self.raw(tag::STREAM_HEADER, &content)
    }

    pub fn footer(&mut self, id: u32, xml: &str) -> &mut Self {
        let mut content = id.to_le_bytes().to_vec();
        content.extend_from_slice(xml.as_bytes());
        self.raw(tag::STREAM_FOOTER, &content)
    }

    /// A Samples chunk of numeric samples: each its timestamp (or none) and its values.
    pub fn numeric(
        &mut self,
        id: u32,
        format: XdfFormat,
        samples: &[(Option<f64>, Vec<f64>)],
    ) -> &mut Self {
        let mut content = id.to_le_bytes().to_vec();
        self.varlen(samples.len() as u64, &mut content);
        for (stamp, values) in samples {
            put_stamp(*stamp, &mut content);
            for &v in values {
                format.put(v, &mut content);
            }
        }
        self.raw(tag::SAMPLES, &content)
    }

    /// A Samples chunk of string samples.
    pub fn strings(&mut self, id: u32, samples: &[(Option<f64>, Vec<String>)]) -> &mut Self {
        let mut content = id.to_le_bytes().to_vec();
        self.varlen(samples.len() as u64, &mut content);
        for (stamp, values) in samples {
            put_stamp(*stamp, &mut content);
            for v in values {
                self.varlen(v.len() as u64, &mut content);
                content.extend_from_slice(v.as_bytes());
            }
        }
        self.raw(tag::SAMPLES, &content)
    }

    pub fn clock_offset(&mut self, id: u32, time: f64, offset: f64) -> &mut Self {
        let mut content = id.to_le_bytes().to_vec();
        content.extend_from_slice(&time.to_le_bytes());
        content.extend_from_slice(&offset.to_le_bytes());
        self.raw(tag::CLOCK_OFFSET, &content)
    }

    pub fn boundary(&mut self) -> &mut Self {
        self.raw(tag::BOUNDARY, &BOUNDARY)
    }
}

fn put_stamp(stamp: Option<f64>, out: &mut Vec<u8>) {
    match stamp {
        Some(t) => {
            out.push(8);
            out.extend_from_slice(&t.to_le_bytes());
        }
        None => out.push(0),
    }
}

/// One chunk to write, by the time of its first sample.
struct Planned {
    time: f64,
    /// The data stream (index into the streams) and its sample range, or a marker stream
    /// (index into the markers) and its annotation.
    what: Plan,
}

enum Plan {
    Data(usize, Range<u64>, usize),
    Marker(usize, usize),
}

fn write_to<W: Write>(spec: &Spec, out: &mut W) -> Result<(), Error> {
    let streams = streams(spec);
    let opts = &spec.xdf;
    let mut chunks = Chunks::new(opts.datetime.as_deref());
    chunks.wide = opts.wide_lengths;
    let rate = |s: &XdfStream| spec.rate(s.channels[0]);
    for s in &streams {
        let channels: Vec<(String, String, String)> = s
            .channels
            .iter()
            .map(|&c| {
                let ch = &spec.channels[c];
                (ch.label.clone(), ch.kind.clone(), ch.dimension.clone())
            })
            .collect();
        let xml = header_xml(
            &s.name,
            &s.kind,
            s.channels.len(),
            rate(s),
            s.format.name(),
            s.described.then_some(channels.as_slice()),
        );
        chunks.stream_header(s.id, &xml);
    }
    for m in &opts.markers {
        let (count, format) = match m.values {
            MarkerValues::Strings => (1, "string"),
            MarkerValues::Numeric(f, n) => (n, f.name()),
        };
        chunks.stream_header(
            m.id,
            &header_xml(&m.name, &m.kind, count, m.rate, format, None),
        );
    }
    out.write_all(&chunks.bytes)?;
    chunks.bytes.clear();

    // Every chunk, by time.
    let mut plan = Vec::new();
    for (k, s) in streams.iter().enumerate() {
        let len = spec.len(s.channels[0]);
        let mut first = 0;
        let mut n = 0;
        while first < len {
            let size = if s.varying_chunks && n % 2 == 1 {
                (s.chunk / 3).max(1)
            } else {
                s.chunk
            };
            let end = (first + size).min(len);
            plan.push(Planned {
                time: s.start + first as f64 / rate(s),
                what: Plan::Data(k, first..end, n),
            });
            first = end;
            n += 1;
        }
    }
    for (k, m) in opts.markers.iter().enumerate() {
        for a in m.annotations.clone() {
            plan.push(Planned {
                time: spec.annotations[a].onset,
                what: Plan::Marker(k, a),
            });
        }
    }
    plan.sort_by(|a, b| a.time.total_cmp(&b.time));

    for (n, p) in plan.iter().enumerate() {
        if opts.boundaries && n % 16 == 0 {
            chunks.boundary();
        }
        match &p.what {
            Plan::Data(k, range, chunk) => {
                let s = &streams[*k];
                let r = rate(s);
                let samples: Vec<(Option<f64>, Vec<f64>)> = range
                    .clone()
                    .map(|i| {
                        let stamped = match s.stamps {
                            XdfStamps::All => true,
                            XdfStamps::FirstOfChunk => i == range.start,
                            XdfStamps::Every(e) => i % e.max(1) == 0,
                            XdfStamps::None => false,
                        };
                        let time = opts.clock + s.start + i as f64 / r;
                        let values = s.channels.iter().map(|&c| spec.physical(c, i)).collect();
                        (stamped.then_some(time), values)
                    })
                    .collect();
                chunks.numeric(s.id, s.format, &samples);
                if opts.clock_offsets && chunk % 8 == 0 {
                    chunks.clock_offset(s.id, opts.clock + p.time, 0.0);
                }
            }
            Plan::Marker(k, a) => {
                let m = &opts.markers[*k];
                let tal = &spec.annotations[*a];
                let time = Some(opts.clock + tal.onset);
                match m.values {
                    MarkerValues::Strings => {
                        let samples: Vec<_> =
                            tal.texts.iter().map(|t| (time, vec![t.clone()])).collect();
                        chunks.strings(m.id, &samples);
                    }
                    MarkerValues::Numeric(format, _) => {
                        let samples: Vec<_> = tal
                            .texts
                            .iter()
                            .map(|t| (time, numbers(t).unwrap_or_default()))
                            .collect();
                        chunks.numeric(m.id, format, &samples);
                    }
                }
            }
        }
        if chunks.bytes.len() > 1 << 20 {
            out.write_all(&chunks.bytes)?;
            chunks.bytes.clear();
        }
    }

    for s in streams.iter().filter(|s| s.footer) {
        let len = spec.len(s.channels[0]);
        let first = opts.clock + s.start;
        let last = first + len.saturating_sub(1) as f64 / rate(s);
        chunks.footer(s.id, &footer_xml(first, last, len));
    }
    for m in &opts.markers {
        let n: usize = spec.annotations[m.annotations.clone()]
            .iter()
            .map(|a| a.texts.len())
            .sum();
        chunks.footer(m.id, &footer_xml(0.0, 0.0, n as u64));
    }
    out.write_all(&chunks.bytes)?;
    Ok(())
}

fn footer_xml(first: f64, last: f64, count: u64) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<info>\n\t<first_timestamp>{first}</first_timestamp>\n\t\
         <last_timestamp>{last}</last_timestamp>\n\t<sample_count>{count}</sample_count>\n\t\
         <clock_offsets />\n</info>\n"
    )
}
