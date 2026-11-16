//! EEGLAB datasets: samples against the truth function in every storage and layout, and
//! MAT-files written by hand for each rule of the parser and the reader.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::common::{TempDir, TempFile};
use kymo_core::eeglab::EeglabError;
use kymo_core::{
    AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType, SamplesMut,
};
use kymo_testgen::mat::{self, Order, Value};
use kymo_testgen::{ChannelSpec, Format, Layout, Spec, Storage};
use proptest::prelude::*;

fn open(path: &Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

fn physical(rec: &Recording, channels: &[ChannelId], range: Range<u64>) -> Vec<f32> {
    let mut out = vec![0.0f32; channels.len() * (range.end - range.start) as usize];
    rec.read_physical(GridId(0), channels, range, &mut out)
        .unwrap();
    out
}

fn truth(spec: &Spec, channels: &[ChannelId], range: Range<u64>) -> Vec<f32> {
    channels
        .iter()
        .flat_map(|c| {
            range
                .clone()
                .map(move |i| spec.physical(c.0 as usize, i) as f32)
        })
        .collect()
}

/// One f32 step at `x`.
fn step(x: f64) -> f64 {
    let x = (x as f32).abs();
    f64::from(f32::from_bits(x.to_bits() + 1) - x)
}

/// Whether `got` matches the truth exactly or, for float64 samples (read relative to the
/// channel's first sample, then rounded again), within the steps of both roundings.
fn matches(
    spec: &Spec,
    channels: &[ChannelId],
    range: Range<u64>,
    got: &[f32],
    double: bool,
) -> bool {
    let expected = truth(spec, channels, range.clone());
    let n = (range.end - range.start) as usize;
    got.len() == expected.len()
        && got.iter().zip(&expected).enumerate().all(|(k, (g, e))| {
            if g == e {
                return true;
            }
            let c = channels[k / n.max(1)].0 as usize;
            let e = f64::from(*e);
            let reference = spec.physical(c, 0);
            double && (f64::from(*g) - e).abs() <= step(e - reference) + step(e)
        })
}

/// The native type of whole-number doubles: MATLAB stores them in the first of uint8,
/// int8, uint16, int16, uint32 and int32 that holds them all.
fn stored_type(spec: &Spec) -> SampleType {
    let values =
        (0..spec.channels.len()).flat_map(|c| (0..spec.len(0)).map(move |i| spec.physical(c, i)));
    let (lo, hi) = values.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let within = |a: f64, b: f64| a <= lo && hi <= b;
    if within(0.0, 255.0) || within(-128.0, 127.0) {
        SampleType::I16
    } else if within(0.0, 65_535.0) {
        SampleType::I32
    } else if within(-32_768.0, 32_767.0) {
        SampleType::I16
    } else if within(0.0, 4_294_967_295.0) {
        SampleType::F32
    } else {
        SampleType::I32
    }
}

#[derive(Debug, Clone)]
struct Case {
    spec: Spec,
    reads: Vec<(Vec<usize>, Range<u64>)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::select(Format::EEGLAB.to_vec()),
        1usize..7,
        proptest::sample::select(vec![1u32, 3, 50, 256]),
        1u64..8,
        any::<u64>(),
        // An int16 or a wide range of values, whole µV or tenths.
        any::<bool>(),
        any::<bool>(),
        proptest::collection::vec(
            (
                proptest::collection::vec(any::<prop::sample::Index>(), 1..5),
                (any::<prop::sample::Index>(), any::<prop::sample::Index>()),
            ),
            1..6,
        ),
    )
        .prop_map(|(format, n, spr, records, seed, wide, whole, picks)| {
            let (lo, hi) = if wide {
                (-8_388_608, 8_388_607)
            } else {
                (-32_768, 32_767)
            };
            let gain = if whole { 1.0 } else { 0.1 };
            let channels = (0..n)
                .map(|i| ChannelSpec {
                    digital_min: lo,
                    digital_max: hi,
                    physical_min: f64::from(lo) * gain,
                    physical_max: f64::from(hi) * gain,
                    ..ChannelSpec::eeg(format!("E{i}"), spr, format)
                })
                .collect();
            let mut spec = Spec::new(format, channels, 1.0, records);
            spec.seed = seed;
            // Spikes often, so the limits are read too.
            spec.spike_every = 7;
            let len = records * u64::from(spr);
            let reads = picks
                .into_iter()
                .map(|(chans, (a, b))| {
                    let chans = chans.iter().map(|c| c.index(n)).collect();
                    let (a, b) = (
                        a.index(len as usize + 1) as u64,
                        b.index(len as usize + 1) as u64,
                    );
                    (chans, a.min(b)..a.max(b))
                })
                .collect();
            Case { spec, reads }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Every storage and layout reads back the truth function in physical units, for any
    /// channels and range; doubles of whole numbers are read in the integer type MATLAB
    /// stored them in.
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let file = TempFile::written(&case.spec);
        let (rec, _) = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        let Format::Eeglab(storage, _) = case.spec.format else { unreachable!() };
        // As MATLAB, the writer stores doubles that are all whole in an integer type, which
        // a fractional gain can give too (a single sample of 0).
        let spec = &case.spec;
        let whole = (0..spec.channels.len()).all(|c| {
            (0..spec.len(0)).all(|i| {
                let v = spec.physical(c, i);
                v.is_finite() && v.fract() == 0.0 && !(v == 0.0 && v.is_sign_negative())
            })
        });
        let expected_type = if storage == Storage::Double && whole {
            stored_type(&case.spec)
        } else {
            SampleType::F32
        };
        prop_assert_eq!(rec.grid(GridId(0)).sample_type(), expected_type);
        for (chans, range) in &case.reads {
            let ids: Vec<ChannelId> = chans.iter().map(|&c| ChannelId(c as u32)).collect();
            let got = physical(&rec, &ids, range.clone());
            prop_assert!(
                matches(&case.spec, &ids, range.clone(), &got, storage == Storage::Double),
                "{:?} != {:?}", got, truth(&case.spec, &ids, range.clone())
            );
            rec.prefetch(GridId(0), range.clone());
        }
    }
}

#[test]
fn presets_open_with_their_channels_and_events() {
    for preset in ["eeglab-fdt", "eeglab-set-single", "eeglab-set-double"] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let (rec, annotations) = open(file.path());
        assert_eq!(rec.format(), "EEGLAB");
        assert_eq!(rec.start_time(), None);
        let mut sources = vec![file.path().to_owned()];
        if preset == "eeglab-fdt" {
            sources.push(file.companion("fdt").to_owned());
        }
        assert_eq!(rec.sources(), sources, "{preset}");
        let grid = rec.grid(GridId(0));
        assert_eq!(grid.len(), spec.len(0));
        assert_eq!(grid.rate(), spec.rate(0));
        let ids = grid.channels().to_vec();
        let double = preset == "eeglab-set-double";
        let got = physical(&rec, &ids, 0..grid.len());
        assert!(
            matches(&spec, &ids, 0..grid.len(), &got, double),
            "{preset}"
        );
        for (channel, c) in rec.channels().iter().zip(&spec.channels) {
            assert_eq!(channel.name, c.label);
        }
        let expected: Vec<(f64, f64, String)> = spec
            .annotations
            .iter()
            .flat_map(|a| {
                // A boundary's duration is the data removed there: a point in time.
                a.texts.iter().map(move |t| {
                    let duration = match t.as_str() {
                        "boundary" => 0.0,
                        _ => a.duration.unwrap_or(0.0),
                    };
                    (a.onset, duration, t.clone())
                })
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
        let mut fields = vec![
            ("setname", "kymo-testgen"),
            ("subject", "S01"),
            ("condition", "rest"),
            ("session", "1"),
            ("ref", "common"),
        ];
        let fdt = file.path().with_extension("fdt");
        let fdt_name = fdt.file_name().unwrap().to_string_lossy();
        if preset == "eeglab-fdt" {
            fields.push(("data file", &fdt_name));
        }
        assert_eq!(extra, fields, "{preset}");
    }

    let channels = |preset: &str| {
        let file = TempFile::preset(preset);
        let rec = open(file.path()).0;
        let channels: Vec<_> = rec
            .channels()
            .iter()
            .map(|c| (c.kind.clone(), c.unit.label.clone(), c.gain))
            .collect();
        (channels, rec.warnings().to_vec(), rec)
    };
    use crate::kinds::*;
    let uv = |kind| (kind, "\u{b5}V".to_owned(), 1.0);
    let (fdt, warnings, _) = channels("eeglab-fdt");
    assert_eq!(
        fdt,
        [
            uv(Eeg),
            uv(Eeg),
            uv(Eeg),
            uv(Eeg),
            // No type: from the name.
            uv(Eog),
            uv(Ecg),
            uv(Resp),
            // A type as written.
            uv(ChannelKind::named("Light")),
        ]
    );
    assert!(warnings.is_empty());
    let (single, warnings, _) = channels("eeglab-set-single");
    assert_eq!(single, [uv(Eeg), uv(Eeg), uv(Eog), uv(Emg)]);
    assert!(warnings.is_empty());
    let (double, warnings, rec) = channels("eeglab-set-double");
    assert_eq!(double[7], (Stim, String::new(), 1.0));
    assert!(warnings.is_empty());
    // The DC channel is read relative to its first sample, folded into the offset; the
    // stim channel keeps its codes.
    let dc = &rec.channels()[6];
    assert!((dc.offset - 1e6).abs() < 500.0, "{}", dc.offset);
    assert_eq!(rec.channels()[7].offset, 0.0);
    assert!(rec.channels()[0].offset.abs() < 1000.0);
}

/// A dataset written by hand into a directory: `rec.set`, and `rec.fdt` if given.
struct Written {
    dir: TempDir,
}

impl Written {
    fn bytes(set: &[u8]) -> Written {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("rec.set"), set).unwrap();
        Written { dir }
    }

    /// The fields in one `EEG` struct.
    fn new(fields: Vec<(&str, Value)>) -> Written {
        Written::bytes(&eeg(fields, Order::Little))
    }

    fn with_fdt(self, name: &str, bytes: &[u8]) -> Written {
        std::fs::write(self.dir.path().join(name), bytes).unwrap();
        self
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn open(&self) -> Result<(Recording, Vec<AnnotationData>), OpenError> {
        kymo_core::open(&self.path("rec.set")).map(|o| Recording::new(o).unwrap())
    }

    fn error(&self) -> EeglabError {
        match self.open() {
            Err(OpenError::Eeglab { source, .. }) => source,
            other => panic!("expected an EEGLAB error, got {other:?}"),
        }
    }
}

fn eeg(fields: Vec<(&str, Value)>, order: Order) -> Vec<u8> {
    mat::file(&[("EEG", Value::record(fields))], order)
}

fn chanlocs(channels: &[(&str, &str)]) -> Value {
    Value::records(
        &["labels", "type"],
        channels
            .iter()
            .map(|(l, t)| vec![Value::Char((*l).into()), Value::Char((*t).into())])
            .collect(),
    )
}

/// Two channels of four samples at 100 Hz, as singles inside the `.set`: Fz holds 1 to 4,
/// Cz −1 to −4.
fn fields() -> Vec<(&'static str, Value)> {
    vec![
        ("setname", Value::Char("test".into())),
        ("nbchan", Value::scalar(2.0)),
        ("trials", Value::scalar(1.0)),
        ("pnts", Value::scalar(4.0)),
        ("srate", Value::scalar(100.0)),
        (
            "data",
            Value::Single(vec![2, 4], vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0]),
        ),
        ("chanlocs", chanlocs(&[("Fz", "EEG"), ("Cz", "")])),
        ("event", Value::Empty),
    ]
}

/// [`fields`] with some replaced (or added), and [`removed`] ones left out.
fn with(changes: Vec<(&'static str, Value)>) -> Vec<(&'static str, Value)> {
    let mut f = fields();
    for (name, value) in changes {
        f.retain(|(n, _)| *n != name);
        if value != removed() {
            f.push((name, value));
        }
    }
    f
}

/// Marks a field for [`with`] to leave out.
fn removed() -> Value {
    Value::Opaque(0, Vec::new())
}

fn all_physical(rec: &Recording) -> Vec<f32> {
    let grid = rec.grid(GridId(0));
    physical(rec, grid.channels(), 0..grid.len())
}

const FOUR: [f32; 8] = [1.0, 2.0, 3.0, 4.0, -1.0, -2.0, -3.0, -4.0];

#[test]
fn layouts_and_byte_orders() {
    // One EEG struct, little- and big-endian.
    for order in [Order::Little, Order::Big] {
        let (rec, _) = Written::bytes(&eeg(fields(), order)).open().unwrap();
        assert_eq!(all_physical(&rec), FOUR, "{order:?}");
        assert_eq!(rec.grid(GridId(0)).rate(), 100.0);
    }
    // The fields as variables.
    let flat = mat::file(&fields(), Order::Little);
    let (rec, _) = Written::bytes(&flat).open().unwrap();
    assert_eq!(all_physical(&rec), FOUR);
    // A nested EEG.EEG.
    let nested = mat::file(
        &[("EEG", Value::record(vec![("EEG", Value::record(fields()))]))],
        Order::Little,
    );
    let (rec, _) = Written::bytes(&nested).open().unwrap();
    assert_eq!(all_physical(&rec), FOUR);
    // Variables and fields kymo doesn't read are skipped, whatever their class.
    let mut skipped = fields();
    skipped.push((
        "etc",
        Value::record(vec![
            ("handle", Value::Opaque(mat::MX_FUNCTION, vec![1; 13])),
            ("sparse", Value::Opaque(mat::MX_SPARSE, vec![2; 3])),
            ("flags", Value::Logical(vec![true, false])),
            (
                "notes",
                Value::Cell(vec![1, 2], vec![Value::Char("a".into()), Value::Empty]),
            ),
        ]),
    ));
    let mut bytes = eeg(skipped, Order::Little);
    bytes.extend(mat::variable(
        "other",
        &Value::Opaque(mat::MX_FUNCTION, vec![0; 40]),
        Order::Little,
    ));
    bytes.extend(mat::element(mat::MI_UINT8, &[9; 11], Order::Little));
    let (rec, _) = Written::bytes(&bytes).open().unwrap();
    assert_eq!(all_physical(&rec), FOUR);
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
}

#[test]
fn samples_in_every_storage_type() {
    use kymo_testgen::mat::*;
    // Doubles MATLAB stored in a smaller type, and every numeric class.
    let values: [f64; 8] = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 100.0];
    fn bytes<const N: usize>(values: &[f64], f: impl Fn(f64) -> [u8; N]) -> Vec<u8> {
        values.iter().flat_map(|&v| f(v)).collect()
    }
    let cases: Vec<(u32, Vec<u8>, SampleType)> = vec![
        (
            MI_INT8,
            bytes(&values, |v| [v as i8 as u8]),
            SampleType::I16,
        ),
        (MI_UINT8, bytes(&values, |v| [v as u8]), SampleType::I16),
        (
            MI_INT16,
            bytes(&values, |v| (v as i16).to_le_bytes()),
            SampleType::I16,
        ),
        (
            MI_UINT16,
            bytes(&values, |v| (v as u16).to_le_bytes()),
            SampleType::I32,
        ),
        (
            MI_INT32,
            bytes(&values, |v| (v as i32).to_le_bytes()),
            SampleType::I32,
        ),
        (
            MI_UINT32,
            bytes(&values, |v| (v as u32).to_le_bytes()),
            SampleType::F32,
        ),
        (
            MI_INT64,
            bytes(&values, |v| (v as i64).to_le_bytes()),
            SampleType::F32,
        ),
        (
            MI_UINT64,
            bytes(&values, |v| (v as u64).to_le_bytes()),
            SampleType::F32,
        ),
        (
            MI_SINGLE,
            bytes(&values, |v| (v as f32).to_le_bytes()),
            SampleType::F32,
        ),
        (MI_DOUBLE, bytes(&values, f64::to_le_bytes), SampleType::F32),
    ];
    for (storage, data, sample_type) in cases {
        for class in [MX_DOUBLE, MX_SINGLE] {
            let written = Written::new(with(vec![(
                "data",
                Value::Raw {
                    class,
                    storage,
                    dims: vec![2, 4],
                    bytes: data.clone(),
                },
            )]));
            let (rec, _) = written.open().unwrap();
            assert_eq!(rec.grid(GridId(0)).sample_type(), sample_type, "{storage}");
            assert_eq!(
                all_physical(&rec),
                [0.0, 2.0, 4.0, 6.0, 1.0, 3.0, 5.0, 100.0],
                "{storage}"
            );
        }
    }
}

#[test]
fn channels_from_chanlocs() {
    let locs = chanlocs(&[
        ("MEG 0111", "mag"),
        ("MEG 0112", "grad"),
        ("REF1", "ref_meg"),
        ("STI 014", "stim"),
        ("X1", "MISC"),
        ("EKG", ""),
        // Types as written: one kymo doesn't know, one it knows (EDF+'s).
        ("MEG", "meg"),
        ("Light", "LIGHT"),
        // The type wins over the name.
        ("HEOG", "EEG"),
        ("", "eog"),
        ("Pulse", " bio "),
    ]);
    let n = 11;
    let written = Written::new(with(vec![
        ("nbchan", Value::scalar(n as f64)),
        ("pnts", Value::scalar(1.0)),
        ("data", Value::Single(vec![n, 1], vec![0.0; n])),
        ("chanlocs", locs),
    ]));
    let (rec, _) = written.open().unwrap();
    let got: Vec<(&str, ChannelKind, &str, f64)> = rec
        .channels()
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.kind.clone(),
                c.unit.label.as_str(),
                c.gain,
            )
        })
        .collect();
    use crate::kinds::*;
    let uv = "\u{b5}V";
    assert_eq!(
        got,
        [
            ("MEG 0111", Mag, "T", 1e-6),
            ("MEG 0112", Grad, "T/m", 1e-6),
            ("REF1", RefMeg, "T", 1e-6),
            ("STI 014", Stim, "", 1.0),
            ("X1", Misc, uv, 1.0),
            ("EKG", Ecg, uv, 1.0),
            ("MEG", ChannelKind::named("meg"), uv, 1.0),
            ("Light", ChannelKind::named("Light"), uv, 1.0),
            ("HEOG", Eeg, uv, 1.0),
            ("EEG 009", Eog, uv, 1.0),
            ("Pulse", Bio, uv, 1.0),
        ]
    );
    assert_eq!(rec.channels()[6].kind.name(), "meg");
    assert_eq!(rec.channels()[7].kind.name(), "Light");
    assert!(rec.warnings().is_empty());

    // Without chanlocs, MNE's names.
    let (rec, _) = Written::new(with(vec![("chanlocs", Value::Empty)]))
        .open()
        .unwrap();
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["EEG 000", "EEG 001"]);
    assert!(rec.warnings().is_empty());
    let (rec, _) = Written::new(with(vec![("chanlocs", removed())]))
        .open()
        .unwrap();
    assert_eq!(rec.channels()[1].name, "EEG 001");

    // Fewer or more than nbchan.
    let (rec, _) = Written::new(with(vec![("chanlocs", chanlocs(&[("Fz", "")]))]))
        .open()
        .unwrap();
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Fz", "EEG 001"]);
    assert_eq!(
        rec.warnings(),
        ["chanlocs describes 1 channels, but the dataset has 2; the others get default names"]
    );
    let three = chanlocs(&[("A", ""), ("B", ""), ("C", "")]);
    let (rec, _) = Written::new(with(vec![("chanlocs", three)]))
        .open()
        .unwrap();
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
    assert_eq!(rec.warnings().len(), 1);

    // Labels that are numbers, or padded.
    let numbered = Value::records(
        &["labels"],
        vec![vec![Value::scalar(5.0)], vec![Value::Char(" Cz ".into())]],
    );
    let (rec, _) = Written::new(with(vec![("chanlocs", numbered)]))
        .open()
        .unwrap();
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["5", "Cz"]);
}

fn event(kind: Value, latency: f64, duration: Option<Value>) -> Vec<Value> {
    vec![
        kind,
        Value::scalar(latency),
        duration.unwrap_or(Value::Empty),
    ]
}

fn summary(annotations: &[AnnotationData]) -> Vec<(f64, f64, &str)> {
    annotations
        .iter()
        .map(|a| (a.onset, a.duration, a.description.as_str()))
        .collect()
}

#[test]
fn events_become_annotations() {
    let s = |x: f64| Some(Value::scalar(x));
    let events = Value::records(
        &["type", "latency", "duration"],
        vec![
            event(Value::Char("square".into()), 1.0, s(0.0)),
            event(Value::scalar(12.0), 11.0, s(5.0)),
            event(Value::scalar(1.5), 21.0, None),
            event(Value::Char("lost".into()), f64::NAN, s(0.0)),
            event(Value::Char("early".into()), 0.5, s(0.0)),
            event(Value::Char("odd".into()), 31.0, s(f64::NAN)),
            event(Value::Char("negative".into()), 32.0, s(-2.0)),
            event(Value::Char("boundary".into()), 40.5, s(250.0)),
            event(Value::Empty, 45.5, s(0.0)),
            event(Value::Char("boundary".into()), 46.5, s(f64::NAN)),
        ],
    );
    let data = Value::Single(vec![2, 50], vec![0.0; 100]);
    let written = Written::new(with(vec![
        ("pnts", Value::scalar(50.0)),
        ("data", data.clone()),
        ("event", events),
    ]));
    let (rec, annotations) = written.open().unwrap();
    assert_eq!(
        summary(&annotations),
        [
            (0.0, 0.0, "square"),
            (0.1, 0.05, "12"),
            (0.2, 0.0, "1.5"),
            (0.3, 0.0, "odd"),
            (0.31, 0.0, "negative"),
            // A boundary's duration is the data removed there, not data that is shown:
            // it is a point in time, its duration unknown (NaN) as it may be.
            (0.395, 0.0, "boundary"),
            (0.445, 0.0, ""),
            (0.455, 0.0, "boundary"),
        ]
    );
    assert_eq!(
        rec.warnings(),
        [
            "1 events without a latency (or a NaN one) are ignored",
            "1 events before the first sample are ignored",
            "2 events whose duration isn't a number >= 0 are read as points in time",
        ]
    );
    assert!(annotations.iter().all(|a| a.channels.is_empty()));

    // Events without a duration field are points in time.
    let events = Value::records(
        &["latency", "type"],
        vec![vec![Value::scalar(3.0), Value::Char("a".into())]],
    );
    let (_, annotations) = Written::new(with(vec![
        ("pnts", Value::scalar(50.0)),
        ("data", data),
        ("event", events),
    ]))
    .open()
    .unwrap();
    assert_eq!(summary(&annotations), [(0.02, 0.0, "a")]);
}

#[test]
fn header_fields() {
    let written = Written::new(with(vec![
        (
            "setname",
            Value::Rows(vec!["first line".into(), "second".into()]),
        ),
        ("subject", Value::Char(" S07 ".into())),
        ("group", Value::Char(String::new())),
        ("session", Value::scalar(2.0)),
        ("ref", Value::Char("average".into())),
    ]));
    let (rec, _) = written.open().unwrap();
    let extra: Vec<(&str, &str)> = rec
        .extra()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        extra,
        [
            ("setname", "first line\nsecond"),
            ("subject", "S07"),
            ("session", "2"),
            ("ref", "average"),
        ]
    );
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Samples of [`fields`], in a `.fdt`.
fn fdt() -> Vec<u8> {
    floats(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0])
}

#[test]
fn data_files_beside_the_set() {
    let in_fdt = |name: &str| with(vec![("data", Value::Char(name.into()))]);
    let written = Written::new(in_fdt("rec.fdt")).with_fdt("rec.fdt", &fdt());
    let (rec, _) = written.open().unwrap();
    assert_eq!(all_physical(&rec), FOUR);
    assert!(rec.warnings().is_empty());
    assert_eq!(
        rec.sources(),
        [written.path("rec.set"), written.path("rec.fdt")]
    );
    // Opening the .fdt opens its .set.
    let (rec, _) = Recording::new(kymo_core::open(&written.path("rec.fdt")).unwrap()).unwrap();
    assert_eq!(all_physical(&rec), FOUR);

    // `datfile` when `data` is empty.
    let datfile = with(vec![
        ("data", Value::Char(String::new())),
        ("datfile", Value::Char("rec.fdt".into())),
    ]);
    let (rec, _) = Written::new(datfile)
        .with_fdt("rec.fdt", &fdt())
        .open()
        .unwrap();
    assert_eq!(all_physical(&rec), FOUR);

    // A renamed dataset, and a path from another machine.
    for name in ["old.fdt", "C:\\data\\rec.fdt"] {
        let (rec, _) = Written::new(in_fdt(name))
            .with_fdt("rec.fdt", &fdt())
            .open()
            .unwrap();
        assert_eq!(all_physical(&rec), FOUR);
        assert_eq!(
            rec.warnings(),
            [format!("data file {name:?} not found; using \"rec.fdt\"")]
        );
    }
    // Missing under every name.
    let missing = Written::new(in_fdt("old.fdt"));
    assert_eq!(
        missing.error(),
        EeglabError::NoDataFile(missing.path("old.fdt"))
    );

    // A short file gives its whole frames, a long one the dataset's samples.
    let mut short = fdt();
    short.truncate(3 * 8 + 5);
    let (rec, _) = Written::new(in_fdt("rec.fdt"))
        .with_fdt("rec.fdt", &short)
        .open()
        .unwrap();
    assert_eq!(all_physical(&rec), [1.0, 2.0, 3.0, -1.0, -2.0, -3.0]);
    assert_eq!(
        rec.warnings(),
        ["the data file holds 3 samples per channel, fewer than the dataset's 4"]
    );
    let mut long = fdt();
    long.extend_from_slice(&[0; 6]);
    let (rec, _) = Written::new(in_fdt("rec.fdt"))
        .with_fdt("rec.fdt", &long)
        .open()
        .unwrap();
    assert_eq!(all_physical(&rec), FOUR);
    assert_eq!(
        rec.warnings(),
        ["the data file has 6 bytes after the dataset's 4 samples, which are ignored"]
    );
}

#[test]
fn datasets_kymo_cannot_read_are_errors() {
    let error = |fields: Vec<(&'static str, Value)>| Written::new(fields).error();
    assert_eq!(
        error(with(vec![("trials", Value::scalar(4.0))])),
        EeglabError::Epoched(4)
    );
    assert_eq!(
        error(with(vec![("nbchan", Value::scalar(0.0))])),
        EeglabError::Invalid {
            field: "nbchan",
            value: "0".into()
        }
    );
    assert_eq!(
        error(with(vec![("nbchan", Value::scalar(2.5))])),
        EeglabError::Invalid {
            field: "nbchan",
            value: "2.5".into()
        }
    );
    assert_eq!(
        error(with(vec![("srate", Value::scalar(0.0))])),
        EeglabError::Invalid {
            field: "srate",
            value: "0".into()
        }
    );
    assert_eq!(
        error(with(vec![("srate", removed())])),
        EeglabError::Missing("srate")
    );
    assert_eq!(
        error(with(vec![("data", removed())])),
        EeglabError::Missing("data")
    );
    assert!(matches!(
        error(with(vec![("data", Value::Char("rec.dat".into()))])),
        EeglabError::Unsupported(_)
    ));
    assert!(matches!(
        error(with(vec![(
            "data",
            Value::Single(vec![3, 4], vec![0.0; 12])
        )])),
        EeglabError::Invalid { field: "data", .. }
    ));
    assert!(matches!(
        error(with(vec![(
            "data",
            Value::Single(vec![2, 3], vec![0.0; 6])
        )])),
        EeglabError::Invalid { field: "data", .. }
    ));
    assert!(matches!(
        error(with(vec![("data", Value::record(vec![]))])),
        EeglabError::Invalid { field: "data", .. }
    ));
    assert!(matches!(
        error(with(vec![("data", Value::Char("gone.fdt".into()))])),
        EeglabError::NoDataFile(_)
    ));

    // Files that aren't EEGLAB datasets, or not as kymo reads them.
    let file =
        |variables: &[(&str, Value)]| Written::bytes(&mat::file(variables, Order::Little)).error();
    assert_eq!(file(&[("x", Value::scalar(1.0))]), EeglabError::NotEeglab);
    assert_eq!(file(&[("EEG", Value::scalar(1.0))]), EeglabError::NotEeglab);
    assert_eq!(
        file(&[
            ("ALLEEG", Value::record(fields())),
            ("EEG", Value::record(fields()))
        ]),
        EeglabError::AllEeg
    );
    assert!(matches!(
        file(&[(
            "EEG",
            Value::Struct(
                vec![1, 2],
                vec!["nbchan".into()],
                vec![vec![Value::Empty]; 2]
            )
        )]),
        EeglabError::Invalid { field: "EEG", .. }
    ));

    // Compressed variables: refused where they hold the dataset, skipped otherwise.
    let compressed = mat::element(mat::MI_COMPRESSED, &[0x78, 0x9c, 1, 2, 3], Order::Little);
    let mut bytes = mat::header(Order::Little);
    bytes.extend(&compressed);
    assert_eq!(Written::bytes(&bytes).error(), EeglabError::Compressed);
    let mut flat = mat::file(&with(vec![("srate", removed())]), Order::Little);
    flat.extend(&compressed);
    assert_eq!(Written::bytes(&flat).error(), EeglabError::Compressed);
    let mut beside = eeg(fields(), Order::Little);
    beside.extend(&compressed);
    let (rec, _) = Written::bytes(&beside).open().unwrap();
    assert_eq!(all_physical(&rec), FOUR);

    // MAT v7.3 (HDF5).
    let mut hdf5 = b"MATLAB 7.3 MAT-file, Platform: GLNXA64, HDF5 schema 1.00 .".to_vec();
    hdf5.resize(512, b' ');
    match Written::bytes(&hdf5).open() {
        Err(OpenError::Unsupported { reason, .. }) => {
            assert!(
                reason.contains("v7.3") && reason.contains("v6.5"),
                "{reason}"
            )
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_mat_files_are_errors() {
    let good = eeg(fields(), Order::Little);
    let mat_error = |bytes: &[u8]| match Written::bytes(bytes).error() {
        EeglabError::Mat(message) => message,
        other => panic!("expected a MAT error, got {other:?}"),
    };
    // Cut anywhere inside the struct.
    for cut in [130, 200, good.len() / 2, good.len() - 9] {
        let message = mat_error(&good[..cut]);
        assert!(message.contains("truncated"), "{cut}: {message}");
    }
    let mut endian = good.clone();
    endian[126..128].copy_from_slice(b"XX");
    assert!(mat_error(&endian).contains("endian"));
    assert!(mat_error(&good[..100]).contains("header"));
    // A struct claiming more elements than its bytes can hold.
    let mut huge = good.clone();
    // The EEG struct's dimensions (1 × 1) follow its tag (8), flags (16) and the
    // dimensions' tag (8).
    let dims = 128 + 8 + 16 + 8 + 4;
    huge[dims..dims + 4].copy_from_slice(&1_000_000_000u32.to_le_bytes());
    assert!(matches!(
        Written::bytes(&huge).error(),
        EeglabError::Mat(_) | EeglabError::Invalid { .. }
    ));
}

fn corruptible() -> Vec<Vec<u8>> {
    [Layout::Struct, Layout::Flat]
        .into_iter()
        .flat_map(|layout| {
            [Storage::Single, Storage::Double].map(|storage| {
                let format = Format::Eeglab(storage, layout);
                let channels = (0..3)
                    .map(|i| ChannelSpec::eeg(format!("E{i}"), 10, format))
                    .collect();
                let mut spec = Spec::new(format, channels, 1.0, 2);
                spec.annotations = Spec::preset("eeglab-set-single").unwrap().annotations[..3]
                    .iter()
                    .map(|a| kymo_testgen::Tal {
                        onset: a.onset / 30.0,
                        ..a.clone()
                    })
                    .collect();
                let file = TempFile::written(&spec);
                std::fs::read(file.path()).unwrap()
            })
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Damaged files never panic: opening fails, or the recording reads (or fails to).
    #[test]
    fn damaged_files_never_panic(
        which in 0usize..4,
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
        let written = Written::bytes(&bytes);
        if let Ok(opened) = kymo_core::open(&written.path("rec.set"))
            && let Ok((rec, _)) = Recording::new(opened)
        {
            let grid = rec.grid(GridId(0));
            let ids = grid.channels().to_vec();
            let mut out = vec![0.0f32; ids.len() * grid.len() as usize];
            let _ = rec.read_physical(GridId(0), &ids, 0..grid.len(), &mut out);
            let n = ids.len() * grid.len() as usize;
            let _ = match grid.sample_type() {
                SampleType::I16 => rec.read_native(GridId(0), &ids, 0..grid.len(), SamplesMut::I16(&mut vec![0; n])),
                SampleType::I32 => rec.read_native(GridId(0), &ids, 0..grid.len(), SamplesMut::I32(&mut vec![0; n])),
                _ => rec.read_native(GridId(0), &ids, 0..grid.len(), SamplesMut::F32(&mut vec![0.0; n])),
            };
        }
    }
}

/// An empty char matrix claiming a billion rows (a damaged `0 × n` one) reads as empty,
/// without allocating a line per row.
#[test]
fn empty_char_matrices_with_many_rows_read_as_empty() {
    let mut fields = fields();
    fields[0].1 = Value::Raw {
        class: mat::MX_CHAR,
        storage: mat::MI_UINT16,
        dims: vec![1_000_000_000, 0],
        bytes: Vec::new(),
    };
    let (rec, _) = Written::new(fields).open().unwrap();
    assert!(rec.extra().iter().all(|(key, _)| key != "setname"));
}
