//! GDF files: samples against the truth function for both versions and every sample type,
//! and generated files patched byte by byte for each rule of the header and event table.

use std::ops::Range;
use std::path::PathBuf;

use crate::common::{TempDir, TempFile};
use kymo_core::gdf::GdfError;
use kymo_core::{
    AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType, SamplesMut,
};
use kymo_testgen::{ChannelSpec, Format, GdfVersion, Spec, Stored, Tal};
use proptest::prelude::*;

const V1: Format = Format::Gdf(GdfVersion::V1);
const V2: Format = Format::Gdf(GdfVersion::V2);
const TAGGED: Format = Format::Gdf(GdfVersion::V2Tagged);

/// Sample types read relative to the channel's first sample (int32 too where its limits
/// make it float32).
fn relative(stored: Stored) -> bool {
    matches!(
        stored,
        Stored::I32 | Stored::U32 | Stored::I64 | Stored::U64 | Stored::F64
    )
}

/// The smallest and largest value of an integer type; `None` for floats.
fn int_range(stored: Stored) -> Option<(i64, i64)> {
    Some(match stored {
        Stored::I8 => (-128, 127),
        Stored::U8 => (0, 255),
        Stored::I16 => (-32_768, 32_767),
        Stored::U16 => (0, 65_535),
        Stored::I24 => (-8_388_608, 8_388_607),
        Stored::U24 => (0, 16_777_215),
        Stored::I32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
        Stored::U32 => (0, i64::from(u32::MAX)),
        Stored::I64 => (i64::MIN, i64::MAX),
        Stored::U64 => (0, i64::MAX),
        Stored::F32 | Stored::F64 => return None,
    })
}

/// Whether samples at or beyond channel `c`'s digital limits read as missing (biosig's
/// overflow detection): for channels other than stim channels, where the limits leave out
/// a value of the type.
fn limited(spec: &Spec, c: usize, stim: bool) -> bool {
    let s = &spec.channels[c];
    !stim
        && int_range(s.stored.unwrap()).is_none_or(|(min, max)| {
            i64::from(s.digital_min) >= min || i64::from(s.digital_max) <= max
        })
}

/// The native type of channel `c`: float32 where samples can be missing, otherwise as its
/// type decodes.
fn native(spec: &Spec, c: usize, stim: bool) -> SampleType {
    if limited(spec, c, stim) {
        return SampleType::F32;
    }
    match spec.channels[c].stored.unwrap() {
        Stored::I8 | Stored::U8 | Stored::I16 => SampleType::I16,
        Stored::U16 | Stored::I24 | Stored::U24 | Stored::I32 => SampleType::I32,
        _ => SampleType::F32,
    }
}

fn is_stim(rec: &Recording, c: usize) -> bool {
    rec.channels()[c].kind == ChannelKind::Stim
}

fn physical(rec: &Recording, grid: GridId, channels: &[ChannelId], range: Range<u64>) -> Vec<f32> {
    let mut out = vec![0.0f32; channels.len() * (range.end - range.start) as usize];
    rec.read_physical(grid, channels, range, &mut out).unwrap();
    out
}

/// One f32 step at `x`.
fn step(x: f64) -> f64 {
    let x = (x as f32).abs();
    f64::from(f32::from_bits(x.to_bits() + 1) - x)
}

/// Whether `got` (channel-major) matches the truth exactly or, for channels read relative
/// to their first sample, within the steps of both roundings; NaN where the truth lies at
/// or beyond the channel's digital limits (biosig's overflow detection).
fn matches(
    rec: &Recording,
    spec: &Spec,
    channels: &[ChannelId],
    range: Range<u64>,
    got: &[f32],
) -> bool {
    let n = (range.end - range.start) as usize;
    got.len() == channels.len() * n
        && got.iter().enumerate().all(|(k, &g)| {
            let c = channels[k / n.max(1)].0 as usize;
            let i = range.start + (k % n.max(1)) as u64;
            let s = &spec.channels[c];
            let digital = spec.digital(c, i);
            if limited(spec, c, is_stim(rec, c)) {
                let missing = digital <= s.digital_min || digital >= s.digital_max;
                if missing || g.is_nan() {
                    return missing && g.is_nan();
                }
            }
            let e = spec.physical(c, i);
            if g == e as f32 {
                return true;
            }
            let stored = spec.channels[c].stored.unwrap_or(Stored::I16);
            relative(stored) && (f64::from(g) - e).abs() <= step(e - spec.physical(c, 0)) + step(e)
        })
}

fn open(path: &std::path::Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

fn bytes(spec: &Spec) -> Vec<u8> {
    std::fs::read(TempFile::written(spec).path()).unwrap()
}

/// A file written by hand, as `rec.gdf` in its own directory.
struct Patched {
    _dir: TempDir,
    path: PathBuf,
}

impl Patched {
    fn new(bytes: &[u8]) -> Patched {
        let dir = TempDir::new();
        let path = dir.path().join("rec.gdf");
        std::fs::write(&path, bytes).unwrap();
        Patched { _dir: dir, path }
    }

    fn open(&self) -> (Recording, Vec<AnnotationData>) {
        open(&self.path)
    }

    fn error(&self) -> GdfError {
        match kymo_core::open(&self.path) {
            Err(OpenError::Gdf { source, .. }) => source,
            other => panic!("expected a GDF error, got {other:?}"),
        }
    }
}

/// Byte offsets of the per-channel fields, for `ns` channels (both versions).
struct Fields {
    ns: usize,
}

impl Fields {
    fn label(&self, i: usize) -> usize {
        256 + 16 * i
    }
    /// GDF 2: the obsolete text unit; GDF 1: the unit.
    fn dimension(&self, i: usize) -> usize {
        256 + 96 * self.ns + 6 * i
    }
    /// GDF 2 only.
    fn unit_code(&self, i: usize) -> usize {
        256 + 102 * self.ns + 2 * i
    }
    fn digital_min(&self, i: usize) -> usize {
        256 + 120 * self.ns + 8 * i
    }
    fn digital_max(&self, i: usize) -> usize {
        256 + 128 * self.ns + 8 * i
    }
    fn samples(&self, i: usize) -> usize {
        256 + 216 * self.ns + 4 * i
    }
    fn sample_type(&self, i: usize) -> usize {
        256 + 220 * self.ns + 4 * i
    }
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

/// Where the event table of `spec`'s file starts.
fn table_at(spec: &Spec) -> usize {
    let events: usize = spec.annotations.iter().map(|a| a.texts.len()).sum();
    let entry = if spec.format == V1 { 6 } else { 12 };
    spec.file_size().unwrap() as usize - 8 - entry * events
}

#[derive(Debug, Clone)]
struct Case {
    spec: Spec,
    reads: Vec<(prop::sample::Index, prop::sample::Index)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::select(Format::GDF.to_vec()),
        proptest::collection::vec(
            (
                proptest::sample::select(Stored::ALL.to_vec()),
                proptest::sample::select(vec![1u32, 3, 50, 256]),
            ),
            1..6,
        ),
        1u64..6,
        proptest::sample::select(vec![1.0, 0.5]),
        any::<u64>(),
        proptest::collection::vec(any::<(prop::sample::Index, prop::sample::Index)>(), 1..4),
    )
        .prop_map(|(format, channels, records, duration, seed, reads)| {
            let channels = channels
                .into_iter()
                .enumerate()
                .map(|(i, (stored, spr))| ChannelSpec::gdf(format!("E{i}"), spr, stored))
                .collect();
            let mut spec = Spec::new(format, channels, duration, records);
            spec.seed = seed;
            // Spikes often, so the digital limits are read too.
            spec.spike_every = 7;
            Case { spec, reads }
        })
}

proptest! {
    /// Both versions and every sample type read back the truth function in physical units,
    /// for any range of each grid; grids split by rate and native type.
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let spec = &case.spec;
        let file = TempFile::written(spec);
        let (rec, _) = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        for (c, channel) in rec.channels().iter().enumerate() {
            let grid = rec.grid(channel.grid);
            prop_assert_eq!(grid.sample_type(), native(spec, c, is_stim(&rec, c)));
            prop_assert_eq!(grid.rate(), spec.rate(c));
            prop_assert_eq!(grid.len(), spec.len(c));
        }
        // Channels share a grid exactly when they share rate and native type.
        for (a, ca) in rec.channels().iter().enumerate() {
            for (b, cb) in rec.channels().iter().enumerate() {
                let same = spec.rate(a) == spec.rate(b)
                    && native(spec, a, is_stim(&rec, a)) == native(spec, b, is_stim(&rec, b));
                prop_assert_eq!(ca.grid == cb.grid, same);
            }
        }
        for (g, grid) in rec.grids().iter().enumerate() {
            let ids = grid.channels().to_vec();
            for (a, b) in &case.reads {
                let (a, b) = (a.index(grid.len() as usize + 1) as u64, b.index(grid.len() as usize + 1) as u64);
                let range = a.min(b)..a.max(b);
                let got = physical(&rec, GridId(g as u32), &ids, range.clone());
                prop_assert!(matches(&rec, spec, &ids, range.clone(), &got), "grid {} {:?}: {:?}", g, range, got);
                rec.prefetch(GridId(g as u32), range);
            }
        }
    }
}

#[test]
fn presets_open_with_their_channels_and_events() {
    use ChannelKind::*;
    for preset in ["gdf1-float64", "gdf2-int32", "gdf2-mixed"] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let (rec, annotations) = open(file.path());
        assert!(rec.warnings().is_empty(), "{preset}: {:?}", rec.warnings());
        assert_eq!(rec.format(), "GDF");
        assert_eq!(rec.start_time(), Some(spec.start), "{preset}");
        assert_eq!(rec.sources(), [file.path().to_owned()]);
        for (g, grid) in rec.grids().iter().enumerate() {
            let ids = grid.channels().to_vec();
            let c = ids[0].0 as usize;
            assert_eq!(grid.rate(), spec.rate(c));
            let got = physical(&rec, GridId(g as u32), &ids, 0..grid.len());
            assert!(
                matches(&rec, &spec, &ids, 0..grid.len(), &got),
                "{preset} grid {g}"
            );
        }
        for (channel, c) in rec.channels().iter().zip(&spec.channels) {
            assert_eq!(channel.name, c.label);
            assert_eq!(channel.transducer, c.transducer);
        }
        // Events: mode 1 (GDF 1) has no durations; the type code is the description.
        let expected: Vec<(f64, f64, String)> = spec
            .annotations
            .iter()
            .flat_map(|a| {
                let duration = a.duration.unwrap_or(0.0);
                a.texts.iter().map(move |t| (a.onset, duration, t.clone()))
            })
            .collect();
        assert_eq!(annotations.len(), expected.len(), "{preset}");
        for (a, e) in annotations.iter().zip(&expected) {
            assert!(
                (a.onset - e.0).abs() < 1e-9 && (a.duration - e.1).abs() < 1e-9,
                "{a:?} {e:?}"
            );
            assert_eq!(a.description, e.2);
            assert!(a.channels.is_empty());
        }
        let extra: Vec<(&str, &str)> = rec
            .extra()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let version = if preset == "gdf1-float64" {
            "1.25"
        } else {
            "2.20"
        };
        let mut fields = vec![
            ("version", version),
            ("patient", "X X X X"),
            ("recording", "kymo-testgen"),
        ];
        if preset == "gdf2-mixed" {
            let [name, model, v, serial] = kymo_testgen::GDF_MANUFACTURER;
            fields.extend([
                ("manufacturer", name),
                ("model", model),
                ("version (equipment)", v),
                ("serial number", serial),
            ]);
        }
        assert_eq!(extra, fields, "{preset}");
    }

    let summary = |preset: &str| {
        let rec = open(TempFile::preset(preset).path()).0;
        let channels: Vec<_> = rec
            .channels()
            .iter()
            .map(|c| (c.kind, c.unit.label.clone(), c.prefilter.clone()))
            .collect();
        (channels, rec)
    };
    let filtered = "HP:0.1Hz LP:100Hz".to_owned();
    let ch = |kind, unit: &str, filter: &str| (kind, unit.to_owned(), filter.to_owned());
    let (gdf1, rec) = summary("gdf1-float64");
    assert_eq!(
        gdf1,
        [
            ch(Eeg, "uV", &filtered),
            ch(Eeg, "uV", &filtered),
            ch(Eeg, "uV", &filtered),
            // "EOG:L" reads as EDF's "EOG L".
            ch(Eog, "uV", &filtered),
            ch(Eeg, "uV", &filtered),
            ch(Ecg, "mV", &filtered),
            ch(Stim, "Boolean", "No filtering"),
        ]
    );
    // The DC channel is read relative to its first sample, folded into the offset; the
    // STATUS channel keeps its codes.
    assert!((rec.channels()[4].offset - 1e6).abs() < 500.0);
    assert_eq!(rec.channels()[6].offset, 0.0);
    assert_eq!(rec.grids()[1].rate(), 128.0);

    // GDF 2: units from their codes, filters from numbers.
    let (gdf2, rec) = summary("gdf2-int32");
    let uv = |kind| ch(kind, "\u{b5}V", &filtered);
    assert_eq!(
        gdf2,
        [
            uv(Eeg),
            uv(Eeg),
            uv(Eeg),
            uv(Eeg),
            uv(Eeg),
            ch(Ecg, "mV", &filtered),
            ch(Eeg, "%", "LP:1Hz"),
            ch(Stim, "Boolean", ""),
        ]
    );
    assert_eq!(rec.channels()[0].unit.to_si, Some(1e-6));
    // The stim channel stays an integer, so it has a grid of its own.
    assert_eq!(rec.grids().len(), 2);
    assert_eq!(rec.grids()[1].sample_type(), SampleType::I32);
}

/// A small GDF 2 file with events, for patching.
fn small(format: Format) -> Spec {
    let channels = vec![
        ChannelSpec::gdf("Fz", 8, Stored::I16),
        ChannelSpec::gdf("Cz", 8, Stored::F32),
        ChannelSpec::gdf("Pz", 4, Stored::I24),
    ];
    let mut spec = Spec::new(format, channels, 1.0, 4);
    let duration = (format != V1).then_some(0.5);
    spec.annotations = vec![
        Tal {
            onset: 0.25,
            duration,
            texts: vec!["768".into()],
        },
        Tal {
            onset: 1.0,
            duration: None,
            texts: vec!["769".into(), "33537".into()],
        },
        Tal {
            onset: 3.875,
            duration,
            texts: vec!["32766".into()],
        },
    ];
    spec
}

fn summary(annotations: &[AnnotationData]) -> Vec<(f64, f64, &str, Vec<u32>)> {
    annotations
        .iter()
        .map(|a| {
            let channels = a.channels.iter().map(|c| c.0).collect();
            (a.onset, a.duration, a.description.as_str(), channels)
        })
        .collect()
}

#[test]
fn version_boundaries() {
    let spec = small(V2);
    let original = bytes(&spec);
    let (_, events) = Patched::new(&original).open();
    // From 1.90 on, GDF 2's layout.
    let mut b = original.clone();
    put(&mut b, 0, b"GDF 1.99");
    let (rec, got) = Patched::new(&b).open();
    assert_eq!(summary(&got), summary(&events));
    assert_eq!(rec.channels().len(), 3);
    assert_eq!(rec.extra()[0].1, "1.99");
    // Before 1.94, the event table's header has 3 bytes of rate and 4 of count.
    put(&mut b, 0, b"GDF 1.93");
    let at = table_at(&spec);
    let mut old = vec![3, 8, 0, 0];
    old.extend_from_slice(&4u32.to_le_bytes());
    put(&mut b, at, &old);
    let (_, got) = Patched::new(&b).open();
    assert_eq!(summary(&got), summary(&events));
    // From 2.40, bytes 254 and 255 hold the offset to UTC in minutes, for the info view;
    // before, they are reserved.
    let zone = |rec: &Recording| {
        rec.extra()
            .iter()
            .find(|(k, _)| k == "time zone")
            .map(|(_, v)| v.clone())
    };
    let mut z = original.clone();
    put(&mut z, 254, &(-330i16).to_le_bytes());
    assert_eq!(zone(&Patched::new(&z).open().0), None);
    put(&mut z, 0, b"GDF 2.50");
    let (rec, got) = Patched::new(&z).open();
    assert_eq!(zone(&rec).as_deref(), Some("UTC-05:30"));
    assert_eq!(rec.start_time(), Some(spec.start));
    // GDF stores local time, as MNE reads it (and writes it in its CSV), so the offset is
    // only shown.
    assert_eq!(rec.utc_offset(), None);
    assert_eq!(summary(&got), summary(&events));
    put(&mut z, 254, &60i16.to_le_bytes());
    assert_eq!(
        zone(&Patched::new(&z).open().0).as_deref(),
        Some("UTC+01:00")
    );
    // GDF 1.x (below 1.90) and 3.x aren't read as GDF 2.
    for version in [b"GDF 0.50", b"GDF 3.00", b"GDF x.yz"] {
        put(&mut b, 0, version);
        assert!(kymo_core::open(&Patched::new(&b).path).is_err());
    }
}

#[test]
fn record_durations_are_rational() {
    let mut spec = small(V2);
    spec.annotations.clear();
    spec.record_duration = 0.25;
    let mut b = bytes(&spec);
    let rec = Patched::new(&b).open().0;
    assert_eq!(rec.grids()[0].rate(), 32.0);
    // 3/7 s per record.
    put(&mut b, 244, &3u32.to_le_bytes());
    put(&mut b, 248, &7u32.to_le_bytes());
    let rec = Patched::new(&b).open().0;
    assert_eq!(rec.grids()[0].rate(), 8.0 * 7.0 / 3.0);
    // 0 s: 1 s, with a warning (as MNE).
    put(&mut b, 244, &0u32.to_le_bytes());
    let rec = Patched::new(&b).open().0;
    assert_eq!(rec.grids()[0].rate(), 8.0);
    assert!(
        rec.warnings()[0].contains("record duration"),
        "{:?}",
        rec.warnings()
    );
}

#[test]
fn record_counts() {
    for format in [V1, V2] {
        let mut spec = small(format);
        let with_events = bytes(&spec);
        spec.annotations.clear();
        let b = bytes(&spec);
        let at = 236;
        // -1: the records the file holds, no event table.
        let mut unknown = b.clone();
        put(&mut unknown, at, &(-1i64).to_le_bytes());
        let (rec, events) = Patched::new(&unknown).open();
        assert_eq!(rec.grids()[0].len(), 32);
        assert!(events.is_empty());
        assert!(
            rec.warnings()[0].contains("unknown (-1)"),
            "{:?}",
            rec.warnings()
        );
        // Truncated in the third record: two records, no event table.
        let record = 8 * 2 + 8 * 4 + 4 * 3;
        let cut = with_events.len() - (with_events.len() - b.len()) - record - record / 2;
        let (rec, events) = Patched::new(&with_events[..cut]).open();
        assert_eq!(rec.grids()[0].len(), 16);
        assert!(events.is_empty());
        assert!(
            rec.warnings()[0].contains("truncated"),
            "{:?}",
            rec.warnings()
        );
        let ids = rec.grids()[0].channels().to_vec();
        let got = physical(&rec, GridId(0), &ids, 0..16);
        assert!(matches(&rec, &spec, &ids, 0..16, &got));
        // Below -1: an error.
        put(&mut unknown, at, &(-2i64).to_le_bytes());
        assert!(matches!(
            Patched::new(&unknown).error(),
            GdfError::InvalidField { .. }
        ));
    }
}

#[test]
fn units_from_codes_and_texts() {
    let mut spec = small(V2);
    spec.annotations.clear();
    let b = bytes(&spec);
    let fields = Fields { ns: 3 };
    let unit = |code: u16, text: &[u8]| {
        let mut b = b.clone();
        put(&mut b, fields.unit_code(0), &code.to_le_bytes());
        let mut field = [0u8; 6];
        field[..text.len()].copy_from_slice(text);
        put(&mut b, fields.dimension(0), &field);
        let rec = Patched::new(&b).open().0;
        let c = &rec.channels()[0];
        (
            c.unit.label.clone(),
            c.unit.to_si,
            c.kind,
            rec.warnings().to_vec(),
        )
    };
    assert_eq!(
        unit(4256 + 18, b""),
        ("mV".into(), Some(1e-3), ChannelKind::Eeg, vec![])
    );
    assert_eq!(
        unit(4256, b""),
        ("V".into(), Some(1.0), ChannelKind::Eeg, vec![])
    );
    assert_eq!(
        unit(4256 + 20, b""),
        ("nV".into(), Some(1e-9), ChannelKind::Eeg, vec![])
    );
    // Teslas make a magnetometer.
    assert_eq!(
        unit(65504 + 22, b""),
        ("fT".into(), Some(1e-15), ChannelKind::Mag, vec![])
    );
    assert_eq!(
        unit(512, b""),
        (String::new(), None, ChannelKind::Eeg, vec![])
    );
    assert_eq!(unit(544, b""), ("%".into(), None, ChannelKind::Eeg, vec![]));
    assert_eq!(
        unit(6048, b""),
        ("\u{b0}C".into(), None, ChannelKind::Eeg, vec![])
    );
    // No code: the obsolete text field.
    assert_eq!(
        unit(0, b"uV"),
        ("uV".into(), Some(1e-6), ChannelKind::Eeg, vec![])
    );
    // An unknown code or prefix: the text, with a warning.
    for code in [4256 + 11, 32, 512 + 18] {
        let (label, _, _, warnings) = unit(code, b"mV");
        assert_eq!(label, "mV");
        assert!(
            warnings[0].contains(&format!("code {code}")),
            "{warnings:?}"
        );
    }

    // GDF 1: text, Latin-1 if it isn't UTF-8.
    let mut spec = small(V1);
    spec.annotations.clear();
    let mut b = bytes(&spec);
    put(&mut b, fields.dimension(0), b"\xb5V\0\0\0\0\0\0");
    put(&mut b, 256 + 96 * 3 + 8, b"\xc2\xb5V\0\0\0\0\0");
    let rec = Patched::new(&b).open().0;
    assert_eq!(rec.channels()[0].unit.label, "\u{b5}V");
    assert_eq!(rec.channels()[1].unit.label, "\u{b5}V");
    assert_eq!(rec.channels()[1].unit.to_si, Some(1e-6));
}

/// Integer Status channels (BioSemi's status word in BDF files converted to GDF) show the
/// codes in their low 16 bits, as in BDF; other stim channels, and float ones, every bit.
#[test]
fn status_channels_are_masked() {
    let trigger = |label: &str, stored| ChannelSpec {
        label: label.into(),
        stored: Some(stored),
        ..ChannelSpec::trigger(64, V2)
    };
    let channels = vec![
        ChannelSpec::gdf("Fz", 64, Stored::I16),
        trigger("Status", Stored::I32),
        trigger("TRIGGER", Stored::I32),
        trigger("STATUS", Stored::F32),
    ];
    let spec = Spec::new(V2, channels, 1.0, 8);
    let mut b = bytes(&spec);
    // Status bits above the codes, as BioSemi's amplifier sets them (bit 20 and 23).
    let header = 256 * 5;
    let record = 64 * (2 + 4 + 4 + 4);
    for r in 0..8 {
        for j in 0..64 {
            let at = header + r * record + 64 * 2 + 4 * j;
            let v = i32::from_le_bytes(b[at..at + 4].try_into().unwrap()) | 0x0090_0000;
            put(&mut b, at, &v.to_le_bytes());
        }
    }
    let rec = Patched::new(&b).open().0;
    let masks: Vec<_> = rec.channels().iter().map(|c| c.trigger_mask).collect();
    assert_eq!(masks, [None, Some(0xffff), None, None]);
    // The stim scan and the tiles see the codes.
    let status = ChannelId(1);
    let grid = rec.channel(status).grid;
    let pyramid = kymo_core::pyramid::Pyramid::build(&rec, grid).unwrap();
    let changes = pyramid
        .stim_changes(&rec)
        .into_iter()
        .find(|s| s.channel == status)
        .unwrap()
        .changes;
    let mut previous = None;
    for i in 0..spec.len(1) {
        let code = spec.digital(1, i);
        if previous != Some(code) {
            assert!(
                changes.iter().any(|c| c.sample == i && c.value == code),
                "sample {i}: {code}"
            );
            previous = Some(code);
        }
    }
    assert!(changes.iter().all(|c| (0..256).contains(&c.value)));
}

#[test]
fn kinds_from_labels() {
    use ChannelKind::*;
    let labels = [
        ("EEG:C3", Eeg),
        ("EOG:ch01", Eog),
        ("EOG L", Eog),
        ("EMG:chin", Emg),
        ("ECG", Ecg),
        ("STATUS", Stim),
        ("Trigger", Stim),
        ("C3", Eeg),
    ];
    let channels = labels
        .iter()
        .map(|(l, _)| ChannelSpec::gdf(*l, 4, Stored::I16))
        .collect();
    let spec = Spec::new(V2, channels, 1.0, 2);
    let rec = open(TempFile::written(&spec).path()).0;
    let kinds: Vec<_> = rec.channels().iter().map(|c| c.kind).collect();
    assert_eq!(kinds, labels.map(|(_, k)| k));
    // Names keep their labels.
    assert_eq!(rec.channels()[1].name, "EOG:ch01");
}

#[test]
fn start_times() {
    let mut spec = small(V1);
    spec.annotations.clear();
    let b = bytes(&spec);
    let start = |text: &[u8; 16]| {
        let mut b = b.clone();
        put(&mut b, 168, text);
        let rec = Patched::new(&b).open().0;
        (rec.start_time(), rec.warnings().to_vec())
    };
    let at = |s, ns| jiff::civil::date(2007, 3, 2).at(12, 7, s, ns);
    assert_eq!(start(b"2007030212075900"), (Some(at(59, 0)), vec![]));
    assert_eq!(
        start(b"2007030212075950"),
        (Some(at(59, 500_000_000)), vec![])
    );
    assert_eq!(start(b"20070302120759  "), (Some(at(59, 0)), vec![]));
    assert_eq!(start(b"                "), (None, vec![]));
    let (none, warnings) = start(b"2007130212075900");
    assert_eq!(none, None);
    assert!(warnings[0].contains("invalid start date"), "{warnings:?}");

    let mut spec = small(V2);
    spec.annotations.clear();
    let b = bytes(&spec);
    let start = |value: u64| {
        let mut b = b.clone();
        put(&mut b, 168, &value.to_le_bytes());
        Patched::new(&b).open().0.start_time()
    };
    assert_eq!(start(0), None);
    // Day 367 is 0001-01-01; a step of the fraction is about 20 µs, rounded to whole
    // seconds, milliseconds or microseconds where one lies within half a step.
    let day = 732_006u64; // MATLAB's datenum(2004, 2, 29)
    let date = jiff::civil::date(2004, 2, 29);
    assert_eq!(start(day << 32), Some(date.at(0, 0, 0, 0)));
    assert_eq!(start((day << 32) + (1 << 31)), Some(date.at(12, 0, 0, 0)));
    let fraction = |s: f64| (s / 86_400.0 * 2f64.powi(32)).round() as u64;
    assert_eq!(
        start((day << 32) + fraction(34_511.25)),
        Some(date.at(9, 35, 11, 250_000_000))
    );
    assert_eq!(
        start((day << 32) + fraction(34_511.0)),
        Some(date.at(9, 35, 11, 0))
    );
    // The value of MNE's test_gdf_1.99.gdf, which MNE and biosig read as this.
    assert_eq!(
        start(3_146_537_706_281_130),
        Some(jiff::civil::date(2005, 10, 25).at(9, 35, 11, 149_965_000))
    );
}

#[test]
fn tag_length_value_header() {
    let spec = small(TAGGED);
    let b = bytes(&spec);
    let (rec, events) = Patched::new(&b).open();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(
        rec.extra()[3],
        ("manufacturer".into(), "kymo-testgen".into())
    );
    // Four entries, one of them the end of another.
    assert_eq!(events.len(), 3);
    // A tag running past the header: a warning, the rest skipped.
    let mut damaged = b.clone();
    let at = 256 * 4;
    assert_eq!(damaged[at], 3);
    put(&mut damaged, at + 1, &[0xff, 0xff, 0x00]);
    let rec = Patched::new(&damaged).open().0;
    assert!(
        rec.warnings()[0].contains("header tag 3"),
        "{:?}",
        rec.warnings()
    );
    assert_eq!(rec.extra().len(), 3);
    let ids = rec.grids()[0].channels().to_vec();
    let got = physical(&rec, GridId(0), &ids, 0..32);
    assert!(matches(&rec, &spec, &ids, 0..32, &got));
    // A header length shorter than the channels need is an error.
    let mut short = b.clone();
    put(&mut short, 184, &3u16.to_le_bytes());
    assert!(matches!(
        Patched::new(&short).error(),
        GdfError::InvalidField { .. }
    ));
}

#[test]
fn channels_kymo_cannot_scale_or_read() {
    let mut spec = small(V2);
    spec.annotations.clear();
    let b = bytes(&spec);
    let fields = Fields { ns: 3 };
    // A digital range of 0: the stored values as they are, with a warning.
    let mut flat = b.clone();
    put(
        &mut flat,
        fields.digital_max(0),
        &f64::from(spec.channels[0].digital_min).to_le_bytes(),
    );
    let rec = Patched::new(&flat).open().0;
    assert_eq!(
        (rec.channels()[0].gain, rec.channels()[0].offset),
        (1.0, 0.0)
    );
    assert!(
        rec.warnings()[0].contains("no scaling"),
        "{:?}",
        rec.warnings()
    );
    let mut nan = b.clone();
    put(&mut nan, fields.digital_min(1), &f64::NAN.to_le_bytes());
    assert_eq!(Patched::new(&nan).open().0.channels()[1].gain, 1.0);

    // Sample types kymo doesn't read.
    for (code, reason) in [
        (18u32, "float128"),
        (255 + 12, "12-bit"),
        (511 + 4, "unsigned 4-bit"),
        (9, "unknown"),
    ] {
        let mut b = b.clone();
        put(&mut b, fields.sample_type(1), &code.to_le_bytes());
        match Patched::new(&b).error() {
            GdfError::InvalidChannel {
                index: 1,
                label,
                reason: r,
            } => {
                assert_eq!(label, "Cz");
                assert!(r.contains(reason), "{r}");
            }
            other => panic!("{other:?}"),
        }
    }
    // Negative samples per record.
    let mut b2 = b.clone();
    put(&mut b2, fields.samples(0), &(-1i32).to_le_bytes());
    assert!(matches!(
        Patched::new(&b2).error(),
        GdfError::InvalidChannel { index: 0, .. }
    ));

    // A channel without samples in the records (sparse samples) is left out.
    let mut sparse = b.clone();
    put(&mut sparse, fields.samples(2), &0i32.to_le_bytes());
    let rec = Patched::new(&sparse).open().0;
    let names: Vec<_> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Fz", "Cz"]);
    assert!(
        rec.warnings().iter().any(|w| w.contains("Pz")),
        "{:?}",
        rec.warnings()
    );
    // Without any channel with samples: an error.
    for i in 0..2 {
        put(&mut sparse, fields.samples(i), &0i32.to_le_bytes());
    }
    assert!(matches!(
        Patched::new(&sparse).error(),
        GdfError::NoChannels
    ));
    // Labels are trimmed at their first NUL.
    let mut label = b.clone();
    put(&mut label, fields.label(0), b"Fz\0junk  ");
    assert_eq!(Patched::new(&label).open().0.channels()[0].name, "Fz");
}

#[test]
fn events_of_both_modes() {
    // Mode 1 (GDF 1): points in time; mode 3 (GDF 2): durations, all channels. 769 and its
    // end (33537 = 769 | 0x8000) at the same position are one event of duration 0.
    let (_, mode1) = Patched::new(&bytes(&small(V1))).open();
    assert_eq!(
        summary(&mode1),
        [
            (0.25, 0.0, "768", vec![]),
            (1.0, 0.0, "769", vec![]),
            (3.875, 0.0, "32766", vec![]),
        ]
    );
    let spec = small(V2);
    let b = bytes(&spec);
    let (_, mode3) = Patched::new(&b).open();
    assert_eq!(
        summary(&mode3),
        [
            (0.25, 0.5, "768", vec![]),
            (1.0, 0.0, "769", vec![]),
            (3.875, 0.5, "32766", vec![]),
        ]
    );

    let at = table_at(&spec);
    let n = 4;
    let (pos, typ, chn) = (at + 8, at + 8 + 4 * n, at + 8 + 6 * n);
    let patched = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = b.clone();
        f(&mut b);
        let (rec, events) = Patched::new(&b).open();
        (
            summary(&events)
                .into_iter()
                .map(|(o, d, s, c)| (o, d, s.to_owned(), c))
                .collect::<Vec<_>>(),
            rec.warnings().to_vec(),
        )
    };
    // A rate of 0: the highest channel rate (8 Hz here, as written).
    let (events, warnings) = patched(&|b| put(b, at + 4, &0f32.to_le_bytes()));
    assert_eq!(events[0].0, 0.25);
    assert!(warnings.is_empty());
    // Another rate: positions and durations count its samples.
    let (events, _) = patched(&|b| put(b, at + 4, &16f32.to_le_bytes()));
    assert_eq!((events[0].0, events[0].1), (0.125, 0.25));
    // Channels, counted from 1; 0 or a channel the file lacks are all channels. An end on
    // another channel than its start ends nothing.
    let (events, warnings) = patched(&|b| {
        put(b, chn, &2u16.to_le_bytes());
        put(b, chn + 2, &3u16.to_le_bytes());
        put(b, chn + 4, &9u16.to_le_bytes());
    });
    assert_eq!(events[0].3, [1]);
    assert_eq!(events[1].3, [2]);
    assert_eq!(events[2].2, "33537");
    assert!(events[2].3.is_empty());
    assert!(
        warnings[0].contains("1 events name a channel"),
        "{warnings:?}"
    );
    // Position 0 (before the first sample) is left out; type 0x7FFF holds a sparse
    // sample, not an event.
    let (events, warnings) = patched(&|b| {
        put(b, pos, &0u32.to_le_bytes());
        put(b, typ + 2, &0x7fffu16.to_le_bytes());
    });
    assert_eq!(events.len(), 2);
    assert!(warnings[0].contains("position 0"), "{warnings:?}");
    // An end later than its start (position 17 at 8 Hz, 2 s): the start lasts until it.
    let (events, _) = patched(&|b| put(b, pos + 8, &17u32.to_le_bytes()));
    assert_eq!(
        events[1..3],
        [
            (1.0, 1.0, "769".to_owned(), vec![]),
            (3.875, 0.5, "32766".to_owned(), vec![])
        ]
    );
    // An end before any start of its type stays a point in time (events in file order).
    let (events, _) = patched(&|b| {
        put(b, pos + 4, &17u32.to_le_bytes());
        put(b, pos + 8, &9u32.to_le_bytes());
    });
    assert_eq!(events[1], (2.0, 0.0, "769".to_owned(), vec![]));
    assert_eq!(events[2], (1.0, 0.0, "33537".to_owned(), vec![]));
    // Each end closes the oldest open start of its type: 769, 769, end, end.
    let (events, _) = patched(&|b| {
        put(b, pos, &9u32.to_le_bytes());
        put(b, typ, &769u16.to_le_bytes());
        put(b, at + 8 + 8 * n, &0u32.to_le_bytes()); // no duration of its own
        put(b, pos + 4, &13u32.to_le_bytes());
        put(b, pos + 8, &17u32.to_le_bytes());
        put(b, typ + 6, &33537u16.to_le_bytes());
        put(b, pos + 12, &25u32.to_le_bytes());
        put(b, at + 8 + 8 * n + 12, &0u32.to_le_bytes());
    });
    assert_eq!(
        events,
        [
            (1.0, 1.0, "769".to_owned(), vec![]),
            (1.5, 1.5, "769".to_owned(), vec![])
        ]
    );
    // Modes 5 and 7: modes 1 and 3 with a timestamp per event after the table, skipped.
    let (events, warnings) = patched(&|b| {
        b[at] = 7;
        b.extend_from_slice(&[0xab; 8 * 4]);
    });
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], (0.25, 0.5, "768".to_owned(), vec![]));
    assert!(warnings.is_empty(), "{warnings:?}");
    let spec1 = small(V1);
    let mut b5 = bytes(&spec1);
    b5[table_at(&spec1)] = 5;
    b5.extend_from_slice(&[0xab; 8 * 4]);
    let (rec, events) = Patched::new(&b5).open();
    assert_eq!(summary(&events), summary(&mode1));
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    // Another mode: no events, with a warning.
    let (events, warnings) = patched(&|b| b[at] = 2);
    assert!(events.is_empty());
    assert!(warnings[0].contains("mode 2"), "{warnings:?}");
    // A truncated table: the events whose last field is there.
    let (events, warnings) = patched(&|b| b.truncate(b.len() - 6));
    assert_eq!(events.len(), 2);
    assert!(warnings[0].contains("truncated"), "{warnings:?}");
    let (events, _) = patched(&|b| b.truncate(at + 5));
    assert!(events.is_empty());
    // GDF 1's mode 1 with the old header form and a 3-byte rate.
    let spec1 = small(V1);
    let mut b1 = bytes(&spec1);
    let at1 = table_at(&spec1);
    put(&mut b1, at1 + 1, &[16, 0, 0]);
    let (_, events) = Patched::new(&b1).open();
    assert_eq!(events[0].onset, 0.125);
}

#[test]
fn missing_samples_are_nan() {
    let types = [
        Stored::I8,
        Stored::I16,
        Stored::I24,
        Stored::I32,
        Stored::I64,
        Stored::U16,
        Stored::F32,
        Stored::F64,
    ];
    let mut channels: Vec<ChannelSpec> = types
        .iter()
        .map(|&s| ChannelSpec::gdf(format!("{s:?}"), 4, s))
        .collect();
    channels.push(ChannelSpec {
        stored: Some(Stored::I16),
        ..ChannelSpec::trigger(4, V2)
    });
    let spec = Spec::new(V2, channels, 1.0, 3);
    let mut b = bytes(&spec);
    // Sample 1 of every channel: the type's smallest value, or NaN for floats.
    let header = 256 * (spec.channels.len() + 1);
    let mut offset = header; // record 0
    for c in &spec.channels {
        let s = c.stored.unwrap();
        let at = offset + s.size();
        let value: Vec<u8> = match s {
            Stored::I8 => vec![0x80],
            Stored::I16 => 0x8000u16.to_le_bytes().to_vec(),
            Stored::I24 => vec![0, 0, 0x80],
            Stored::I32 => 0x8000_0000u32.to_le_bytes().to_vec(),
            Stored::I64 => (1u64 << 63).to_le_bytes().to_vec(),
            Stored::U16 => 0x8000u16.to_le_bytes().to_vec(),
            Stored::F32 => f32::NAN.to_le_bytes().to_vec(),
            Stored::F64 => f64::NAN.to_le_bytes().to_vec(),
            _ => unreachable!(),
        };
        put(&mut b, at, &value);
        offset += 4 * s.size();
    }
    let rec = Patched::new(&b).open().0;
    for (c, channel) in rec.channels().iter().enumerate() {
        let got = physical(&rec, channel.grid, &[ChannelId(c as u32)], 0..3);
        let s = spec.channels[c].stored.unwrap();
        let stim = channel.kind == ChannelKind::Stim;
        // Unsigned values and stim codes have no missing value.
        assert_eq!(got[1].is_nan(), !stim && s != Stored::U16, "{s:?} {got:?}");
        assert!(!got[0].is_nan() && !got[2].is_nan(), "{s:?} {got:?}");
        if stim {
            assert_eq!(rec.grid(channel.grid).sample_type(), SampleType::I16);
            let mut native = [0i16; 3];
            rec.read_native(
                channel.grid,
                &[ChannelId(c as u32)],
                0..3,
                SamplesMut::I16(&mut native),
            )
            .unwrap();
            assert_eq!(native[1], i16::MIN);
        }
    }
}

fn corruptible() -> Vec<Vec<u8>> {
    [V1, TAGGED].into_iter().map(|f| bytes(&small(f))).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Damaged files never panic: opening fails, or the recording reads (or fails to).
    #[test]
    fn damaged_files_never_panic(
        which in 0usize..2,
        changes in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<prop::sample::Index>()),
    ) {
        thread_local! {
            static FILES: Vec<Vec<u8>> = corruptible();
        }
        let mut bytes = FILES.with(|f| f[which].clone());
        for (at, value) in changes {
            let at = at.index(bytes.len());
            bytes[at] = value;
        }
        if let Some(cut) = cut {
            bytes.truncate(cut.index(bytes.len() + 1));
        }
        let patched = Patched::new(&bytes);
        if let Ok(opened) = kymo_core::open(&patched.path)
            && let Ok((rec, _)) = Recording::new(opened)
        {
            for (g, grid) in rec.grids().iter().enumerate() {
                let g = GridId(g as u32);
                let ids = grid.channels().to_vec();
                let len = grid.len().min(1 << 16);
                let mut out = vec![0.0f32; ids.len() * len as usize];
                let _ = rec.read_physical(g, &ids, 0..len, &mut out);
                rec.prefetch(g, 0..len);
            }
        }
    }
}
