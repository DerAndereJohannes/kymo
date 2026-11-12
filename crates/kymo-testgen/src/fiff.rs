//! The FIFF writer: a raw recording as MNE and Neuromag systems write one. Big-endian tags:
//! the file id, a measurement block with the measurement info (channels, rate, filters,
//! date) and a raw data block of data buffers (one per record, or two of different lengths
//! with [`FiffOptions::varying_buffers`]), with skips for gaps.
//!
//! [`Tags`] writes single tags, so tests can put together files of their own.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::header::{check_amplitude, invalid};
use crate::signal::{ChannelSignal, splitmix64};
use crate::{Error, FiffType, Format, Spec};

/// Tag kinds.
pub mod kind {
    pub const FILE_ID: i32 = 100;
    pub const DIR_POINTER: i32 = 101;
    pub const BLOCK_START: i32 = 104;
    pub const BLOCK_END: i32 = 105;
    pub const NOP: i32 = 108;
    pub const UTC_OFFSET: i32 = 159;
    pub const NCHAN: i32 = 200;
    pub const SFREQ: i32 = 201;
    pub const CH_INFO: i32 = 203;
    pub const MEAS_DATE: i32 = 204;
    pub const DESCRIPTION: i32 = 206;
    pub const FIRST_SAMPLE: i32 = 208;
    pub const EXPERIMENTER: i32 = 212;
    pub const LOWPASS: i32 = 219;
    pub const HIGHPASS: i32 = 223;
    pub const LINE_FREQ: i32 = 235;
    pub const CH_KIND: i32 = 252;
    pub const CH_RANGE: i32 = 253;
    pub const CH_CAL: i32 = 254;
    pub const CH_UNIT: i32 = 256;
    pub const CH_UNIT_MUL: i32 = 257;
    pub const CH_DACQ_NAME: i32 = 258;
    pub const CH_COIL_TYPE: i32 = 350;
    pub const DATA_BUFFER: i32 = 300;
    pub const DATA_SKIP: i32 = 301;
}

/// Block kinds.
pub mod block {
    pub const MEAS: i32 = 100;
    pub const MEAS_INFO: i32 = 101;
    pub const RAW_DATA: i32 = 102;
    pub const EVOKED: i32 = 104;
    pub const CONTINUOUS_DATA: i32 = 112;
    pub const CH_INFO: i32 = 113;
    pub const IAS_RAW_DATA: i32 = 119;
}

/// Data types.
pub mod types {
    pub const VOID: u32 = 0;
    pub const SHORT: u32 = 2;
    pub const INT: u32 = 3;
    pub const FLOAT: u32 = 4;
    pub const DOUBLE: u32 = 5;
    pub const STRING: u32 = 10;
    pub const DAU_PACK16: u32 = 16;
    pub const COMPLEX_FLOAT: u32 = 20;
    pub const CH_INFO_STRUCT: u32 = 30;
    pub const ID_STRUCT: u32 = 31;
}

/// A FIFF channel description: what a channel info struct holds, as [`Tags::ch_info`]
/// writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChInfo {
    pub name: String,
    pub kind: i32,
    pub range: f32,
    pub cal: f32,
    pub coil_type: i32,
    pub unit: i32,
    pub unit_mul: i32,
}

impl ChInfo {
    /// A channel of `kind` and `unit` with a calibration of `cal` (range 1).
    pub fn new(name: impl Into<String>, kind: i32, unit: i32, cal: f32) -> Self {
        ChInfo {
            name: name.into(),
            kind,
            range: 1.0,
            cal,
            coil_type: 0,
            unit,
            unit_mul: 0,
        }
    }
}

/// Tags, written one by one into bytes (big-endian, each followed by the next).
#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub bytes: Vec<u8>,
}

impl Tags {
    /// The file id and a directory pointer of -1 (no directory), as every FIFF file starts.
    pub fn new() -> Self {
        let mut tags = Tags::default();
        tags.id(kind::FILE_ID, 0).int(kind::DIR_POINTER, -1);
        tags
    }

    /// A tag with `next` = 0 (the next tag follows).
    pub fn tag(&mut self, kind: i32, typ: u32, data: &[u8]) -> &mut Self {
        self.tag_with_next(kind, typ, data, 0)
    }

    pub fn tag_with_next(&mut self, kind: i32, typ: u32, data: &[u8], next: i32) -> &mut Self {
        self.bytes.extend(header(kind, typ, data.len(), next));
        self.bytes.extend_from_slice(data);
        self
    }

    pub fn int(&mut self, kind: i32, value: i32) -> &mut Self {
        self.tag(kind, types::INT, &value.to_be_bytes())
    }

    pub fn ints(&mut self, kind: i32, values: &[i32]) -> &mut Self {
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
        self.tag(kind, types::INT, &data)
    }

    pub fn float(&mut self, kind: i32, value: f32) -> &mut Self {
        self.tag(kind, types::FLOAT, &value.to_be_bytes())
    }

    pub fn string(&mut self, kind: i32, value: &str) -> &mut Self {
        self.tag(kind, types::STRING, value.as_bytes())
    }

    /// An id struct: version, machine id, time (`secs` seconds since 1970).
    pub fn id(&mut self, kind: i32, secs: i32) -> &mut Self {
        let data: Vec<u8> = [0x0001_0003, 0, 0, secs, 0]
            .iter()
            .flat_map(|v: &i32| v.to_be_bytes())
            .collect();
        self.tag(kind, types::ID_STRUCT, &data)
    }

    pub fn start(&mut self, block: i32) -> &mut Self {
        self.int(kind::BLOCK_START, block)
    }

    pub fn end(&mut self, block: i32) -> &mut Self {
        self.int(kind::BLOCK_END, block)
    }

    /// A channel info struct; names longer than 15 bytes are cut, as MNE writes them (with
    /// the whole name in an extended channel info block, see [`Tags::extended`]).
    pub fn ch_info(&mut self, ch: &ChInfo) -> &mut Self {
        let mut data = Vec::with_capacity(96);
        for v in [0, 0, ch.kind] {
            data.extend(v.to_be_bytes());
        }
        data.extend(ch.range.to_be_bytes());
        data.extend(ch.cal.to_be_bytes());
        data.extend(ch.coil_type.to_be_bytes());
        data.extend([0u8; 48]);
        data.extend(ch.unit.to_be_bytes());
        data.extend(ch.unit_mul.to_be_bytes());
        let mut name = [0u8; 16];
        let cut = &ch.name.as_bytes()[..ch.name.len().min(15)];
        name[..cut.len()].copy_from_slice(cut);
        data.extend(name);
        self.tag(kind::CH_INFO, types::CH_INFO_STRUCT, &data)
    }

    /// An extended channel info block, as MNE writes for every channel when a name is
    /// longer than 15 bytes.
    pub fn extended(&mut self, ch: &ChInfo) -> &mut Self {
        self.start(block::CH_INFO)
            .int(kind::CH_KIND, ch.kind)
            .float(kind::CH_RANGE, ch.range)
            .float(kind::CH_CAL, ch.cal)
            .int(kind::CH_COIL_TYPE, ch.coil_type)
            .int(kind::CH_UNIT, ch.unit)
            .int(kind::CH_UNIT_MUL, ch.unit_mul)
            .string(kind::CH_DACQ_NAME, &ch.name)
            .end(block::CH_INFO)
    }

    /// The last tag: a NOP pointing nowhere, as MNE ends its files.
    pub fn finish(&mut self) -> &mut Self {
        self.tag_with_next(kind::NOP, types::VOID, &[], -1)
    }
}

fn header(kind: i32, typ: u32, size: usize, next: i32) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(&kind.to_be_bytes());
    h[4..8].copy_from_slice(&typ.to_be_bytes());
    h[8..12].copy_from_slice(&(size as i32).to_be_bytes());
    h[12..].copy_from_slice(&next.to_be_bytes());
    h
}

/// FIFF's channel kind and coil type for MNE's type name.
fn kind_of(c: &crate::ChannelSpec) -> Option<(i32, i32)> {
    if c.trigger {
        return Some((3, 0));
    }
    Some(match c.kind.to_ascii_lowercase().as_str() {
        "mag" => (1, 3022),
        "grad" => (1, 3012),
        "" | "eeg" => (2, 1),
        "stim" => (3, 0),
        "bio" => (102, 0),
        "eog" => (202, 0),
        "ref_meg" => (301, 5001),
        "emg" => (302, 0),
        "ecg" => (402, 0),
        "misc" => (502, 0),
        "resp" => (602, 0),
        "seeg" => (802, 0),
        "dbs" => (803, 0),
        "ecog" => (902, 0),
        "syst" => (900, 0),
        _ => return None,
    })
}

/// FIFF's unit code for a dimension.
fn unit_of(dimension: &str) -> Option<i32> {
    Some(match dimension {
        "V" => 107,
        "T" => 112,
        "T/m" => 201,
        "S" => 110,
        "\u{b0}C" => 114,
        "" | "Boolean" => 0,
        _ => return None,
    })
}

/// The buffer type of `buffer` (by index), for `typ`.
fn buffer_type(typ: FiffType, buffer: usize) -> FiffType {
    match typ {
        FiffType::Mixed => [
            FiffType::Short,
            FiffType::Int,
            FiffType::Float,
            FiffType::Double,
        ][buffer % 4],
        t => t,
    }
}

impl FiffType {
    /// Bytes per stored sample; for [`FiffType::Mixed`], of its widest type.
    pub fn size(self) -> usize {
        match self {
            FiffType::Pack16 | FiffType::Short => 2,
            FiffType::Int | FiffType::Float => 4,
            FiffType::Double | FiffType::Mixed => 8,
        }
    }

    fn code(self) -> u32 {
        match self {
            FiffType::Pack16 => types::DAU_PACK16,
            FiffType::Short => types::SHORT,
            FiffType::Int => types::INT,
            FiffType::Float => types::FLOAT,
            FiffType::Double => types::DOUBLE,
            FiffType::Mixed => unreachable!("buffers have one type each"),
        }
    }

    fn put(self, value: i32, out: &mut Vec<u8>) {
        match self {
            FiffType::Pack16 | FiffType::Short => out.extend((value as i16).to_be_bytes()),
            FiffType::Int => out.extend(value.to_be_bytes()),
            FiffType::Float => out.extend((value as f32).to_be_bytes()),
            FiffType::Double | FiffType::Mixed => out.extend(f64::from(value).to_be_bytes()),
        }
    }
}

fn fiff_type(spec: &Spec) -> FiffType {
    match spec.format {
        Format::Fiff(t) => t,
        _ => unreachable!("called for FIFF only"),
    }
}

/// One data buffer as written: after `skip` skipped buffers of its length, `len` samples
/// from grid sample `start`, of type `typ`.
#[derive(Clone, Copy, Debug)]
struct Buffer {
    skip: u64,
    start: u64,
    len: u64,
    typ: FiffType,
}

/// The data buffers of `spec`: each record one buffer, or split into two of different
/// lengths with varying buffers (except after a skip, whose length the next buffer gives).
fn buffers(spec: &Spec) -> Vec<Buffer> {
    let typ = fiff_type(spec);
    let spr = u64::from(spec.channels[0].samples_per_record);
    let mut buffers = Vec::new();
    let mut skips = std::collections::HashMap::new();
    for g in &spec.gaps {
        skips.insert(
            g.before_record,
            (g.seconds / spec.record_duration).round() as u64,
        );
    }
    for r in 0..spec.n_records {
        let start = spec.record_start(0, r);
        let skip = if r == 0 {
            spec.fiff.initial_skip
        } else {
            skips.get(&r).copied().unwrap_or(0)
        };
        let split = spec.fiff.varying_buffers && skip == 0 && spr >= 2;
        let first = if split {
            1 + splitmix64(spec.seed ^ r) % (spr - 1)
        } else {
            spr
        };
        for (k, (s, len)) in [(start, first), (start + first, spr - first)]
            .into_iter()
            .enumerate()
        {
            if len > 0 {
                let typ = buffer_type(typ, buffers.len());
                buffers.push(Buffer {
                    skip: if k == 0 { skip } else { 0 },
                    start: s,
                    len,
                    typ,
                });
            }
        }
    }
    buffers
}

/// Checks what the FIFF writer needs.
pub(crate) fn validate(spec: &Spec) -> Result<(), Error> {
    let Some(first) = spec.channels.first() else {
        return Err(invalid("at least one data channel is needed"));
    };
    let rate = spec.rate(0);
    if !(rate.is_finite() && rate > 0.0 && f64::from(rate as f32) == rate) {
        return Err(invalid(format!(
            "the rate {rate} must be positive and exact in float32 (FIFF's sfreq)"
        )));
    }
    let first_samp = spec.first_onset * rate;
    if !(first_samp >= 0.0 && first_samp.fract() == 0.0 && first_samp < 1e9) {
        return Err(invalid(format!(
            "first onset {} must be a whole number of samples >= 0",
            spec.first_onset
        )));
    }
    if (spec.fiff.initial_skip * u64::from(first.samples_per_record)) as f64 > first_samp {
        return Err(invalid(
            "the initial skip moves the first sample, so it must fit the first onset",
        ));
    }
    for g in &spec.gaps {
        let records = g.seconds / spec.record_duration;
        if !(records >= 1.0 && (records - records.round()).abs() < 1e-9)
            || g.before_record == 0
            || g.before_record >= spec.n_records
        {
            return Err(invalid(format!(
                "gap before record {}: a skip is a whole number of records inside the data",
                g.before_record
            )));
        }
    }
    if spec
        .gaps
        .windows(2)
        .any(|w| w[0].before_record >= w[1].before_record)
    {
        return Err(invalid("gaps must be sorted by record"));
    }
    let (lo, hi) = spec.format.digital_limits();
    for (i, c) in spec.channels.iter().enumerate() {
        if c.samples_per_record != first.samples_per_record || c.samples_per_record == 0 {
            return Err(invalid(format!(
                "channel {i}: FIFF has one rate, so every channel needs the same samples per \
                 record (> 0)"
            )));
        }
        if c.digital_min < lo
            || c.digital_max > hi
            || i64::from(c.digital_max) - i64::from(c.digital_min) < 2
        {
            return Err(invalid(format!(
                "channel {i}: digital range {}..={} must lie in {lo}..={hi} and span at least 3 \
                 values",
                c.digital_min, c.digital_max
            )));
        }
        let gain = c.gain();
        if !(gain.is_finite() && gain != 0.0 && (gain as f32).is_normal()) {
            return Err(invalid(format!(
                "channel {i}: the gain must be a normal float32"
            )));
        }
        let size = c.physical_min.abs().max(c.physical_max.abs());
        if c.offset().abs() > 1e-9 * size {
            return Err(invalid(format!("channel {i}: FIFF has no offsets")));
        }
        if kind_of(c).is_none() {
            return Err(invalid(format!("channel {i}: unknown type {:?}", c.kind)));
        }
        if unit_of(&c.dimension).is_none() {
            return Err(invalid(format!(
                "channel {i}: unknown unit {:?}",
                c.dimension
            )));
        }
        check_amplitude(i, c)?;
    }
    if !spec.annotations.is_empty() {
        return Err(invalid("FIFF annotations are not written yet"));
    }
    Ok(())
}

/// The calibration of channel `c`: its gain in float32, as range × cal with a range of 1/4
/// for integer buffers (as acquisition systems store a range), 1 for float buffers (as MNE
/// writes them). Both are exact, so `range × cal` is the gain in float32.
fn calibration(spec: &Spec, c: usize) -> (f32, f32) {
    let gain = spec.channels[c].gain() as f32;
    match fiff_type(spec) {
        FiffType::Pack16 | FiffType::Short | FiffType::Int => (0.25, gain * 4.0),
        _ => (1.0, gain),
    }
}

/// The channels as FIFF describes them.
fn ch_infos(spec: &Spec) -> Vec<ChInfo> {
    spec.channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (kind, coil_type) = kind_of(c).unwrap_or((2, 1));
            let (range, cal) = calibration(spec, i);
            ChInfo {
                name: c.label.clone(),
                kind,
                range,
                cal,
                coil_type,
                unit: unit_of(&c.dimension).unwrap_or(0),
                unit_mul: 0,
            }
        })
        .collect()
}

/// The measurement date: the start as UTC, moved by the UTC offset if any, so that the
/// local time is the start.
fn meas_date(spec: &Spec) -> (i32, i32) {
    let offset = spec
        .fiff
        .utc_offset
        .as_deref()
        .and_then(offset_seconds)
        .unwrap_or(0);
    let utc = spec
        .start
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map(|z| z.timestamp())
        .unwrap_or_default();
    let secs = utc.as_second() - i64::from(offset);
    (secs as i32, utc.subsec_microsecond())
}

/// `+HH:MM` in seconds.
fn offset_seconds(text: &str) -> Option<i32> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let digits: String = text[1..].chars().filter(|c| *c != ':').collect();
    let hours: i32 = digits.get(..2)?.parse().ok()?;
    let minutes: i32 = digits.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// The low-pass written: 100 Hz, or the Nyquist frequency if lower.
pub fn lowpass(spec: &Spec) -> f32 {
    (spec.rate(0) / 2.0).min(100.0) as f32
}

/// Everything before the data buffers: the file id, the measurement info, the start of the
/// raw data block and its first sample.
fn head(spec: &Spec) -> Vec<u8> {
    let mut t = Tags::new();
    let (secs, usecs) = meas_date(spec);
    t.start(block::MEAS).start(block::MEAS_INFO);
    t.ints(kind::MEAS_DATE, &[secs, usecs]);
    if let Some(offset) = &spec.fiff.utc_offset {
        t.string(kind::UTC_OFFSET, offset);
    }
    let infos = ch_infos(spec);
    t.int(kind::NCHAN, infos.len() as i32)
        .float(kind::SFREQ, spec.rate(0) as f32)
        .float(kind::LOWPASS, lowpass(spec))
        .float(kind::HIGHPASS, 0.1)
        .float(kind::LINE_FREQ, 50.0)
        .string(kind::EXPERIMENTER, "kymo-testgen")
        .string(kind::DESCRIPTION, &spec.recording);
    for ch in &infos {
        t.ch_info(ch);
    }
    if infos.iter().any(|c| c.name.len() > 15) {
        for ch in &infos {
            t.extended(ch);
        }
    }
    t.end(block::MEAS_INFO);
    let raw = if spec.fiff.maxshield {
        block::IAS_RAW_DATA
    } else {
        block::RAW_DATA
    };
    t.start(raw);
    let first_samp = (spec.first_onset * spec.rate(0)).round() as u64
        - spec.fiff.initial_skip * u64::from(spec.channels[0].samples_per_record);
    t.int(kind::FIRST_SAMPLE, first_samp as i32);
    t.bytes
}

/// Everything after the data buffers.
fn tail(spec: &Spec) -> Vec<u8> {
    let raw = if spec.fiff.maxshield {
        block::IAS_RAW_DATA
    } else {
        block::RAW_DATA
    };
    let mut t = Tags::default();
    t.end(raw).end(block::MEAS).finish();
    t.bytes
}

/// Size of the file in bytes.
pub(crate) fn file_size(spec: &Spec) -> Result<u64, Error> {
    validate(spec)?;
    let n = spec.channels.len() as u64;
    let data: u64 = buffers(spec)
        .iter()
        .map(|b| 16 + b.len * n * b.typ.size() as u64 + if b.skip > 0 { 20 } else { 0 })
        .sum();
    Ok(head(spec).len() as u64 + data + tail(spec).len() as u64)
}

/// Writes `spec` to `path`.
pub(crate) fn write(spec: &Spec, path: &Path) -> Result<(), Error> {
    validate(spec)?;
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    out.write_all(&head(spec))?;
    let signals: Vec<(ChannelSignal, Vec<f64>)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table)
        })
        .collect();
    let mut data = Vec::new();
    for b in buffers(spec) {
        if b.skip > 0 {
            let mut t = Tags::default();
            t.int(kind::DATA_SKIP, b.skip as i32);
            out.write_all(&t.bytes)?;
        }
        data.clear();
        for i in b.start..b.start + b.len {
            for (signal, table) in &signals {
                let v = signal.value(i, table[(i % signal.sine_period) as usize]);
                b.typ.put(v, &mut data);
            }
        }
        out.write_all(&header(kind::DATA_BUFFER, b.typ.code(), data.len(), 0))?;
        out.write_all(&data)?;
    }
    out.write_all(&tail(spec))?;
    out.flush()?;
    Ok(())
}
