//! FIFF raw recordings: samples against the truth function for every buffer type, the
//! presets, and files put together tag by tag for each rule of the tag walk, the
//! measurement info and the data buffers.

use std::ops::Range;
use std::path::PathBuf;

use crate::common::{TempDir, TempFile};
use kymo_core::fiff::FiffError;
use kymo_core::{AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType};
use kymo_testgen::fiff::{ChInfo, Tags, block, kind, types};
use kymo_testgen::{ChannelSpec, FiffSplit, FiffType, Format, GapSpec, Spec, SplitNaming};
use proptest::prelude::*;

/// Buffer types read relative to each channel's first sample (float64, and mixed files,
/// which have float64 buffers).
fn relative(format: Format) -> bool {
    matches!(
        format,
        Format::Fiff(FiffType::Double) | Format::Fiff(FiffType::Mixed)
    )
}

fn physical(rec: &Recording, channels: &[ChannelId], range: Range<u64>) -> Vec<f32> {
    let mut out = vec![0.0f32; channels.len() * (range.end - range.start) as usize];
    rec.read_physical(GridId(0), channels, range, &mut out)
        .unwrap();
    out
}

/// One f32 step at `x`.
fn step(x: f64) -> f64 {
    let x = (x as f32).abs();
    f64::from(f32::from_bits(x.to_bits() + 1) - x)
}

/// Whether `got` (channel-major, NaN in gaps) matches the truth exactly or, for channels
/// read relative to their first sample, within the steps of both roundings.
fn matches(spec: &Spec, channels: &[ChannelId], range: Range<u64>, got: &[f32]) -> bool {
    let n = (range.end - range.start) as usize;
    let segments = spec.segments(0);
    got.len() == channels.len() * n
        && got.iter().enumerate().all(|(k, &g)| {
            let c = channels[k / n.max(1)].0 as usize;
            let i = range.start + (k % n.max(1)) as u64;
            if !segments.iter().any(|s| s.contains(&i)) {
                return g.is_nan();
            }
            let e = spec.physical(c, i);
            g == e as f32
                || relative(spec.format)
                    && (f64::from(g) - e).abs() <= step(e - spec.physical(c, 0)) + step(e) * 2.0
        })
}

fn open(path: &std::path::Path) -> Recording {
    open_with(path).0
}

fn open_with(path: &std::path::Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

type Summary = Vec<(f64, f64, String, Vec<String>)>;

/// The annotations kymo should read from `spec`'s file, as (onset, duration, description,
/// channel names): one per text, onsets and ends stored as float32 from the start, onsets
/// from the first sample.
fn expected_annotations(spec: &Spec) -> Summary {
    let mut expected = Vec::new();
    for (i, a) in spec.annotations.iter().enumerate() {
        let onset = f64::from(a.onset as f32);
        let end = f64::from((a.onset + a.duration.unwrap_or(0.0)) as f32);
        let channels = spec
            .fiff
            .annotation_channels
            .get(i)
            .cloned()
            .unwrap_or_default();
        for text in &a.texts {
            expected.push((
                onset - spec.first_onset,
                end - onset,
                text.clone(),
                channels.clone(),
            ));
        }
    }
    expected
}

fn summary(rec: &Recording, annotations: &[AnnotationData]) -> Summary {
    annotations
        .iter()
        .map(|a| {
            let names = a
                .channels
                .iter()
                .map(|&c| rec.channel(c).name.clone())
                .collect();
            (a.onset, a.duration, a.description.clone(), names)
        })
        .collect()
}

fn same_annotations(got: &Summary, expected: &Summary) -> bool {
    got.len() == expected.len()
        && got.iter().zip(expected).all(|(g, e)| {
            (g.0 - e.0).abs() < 1e-9 && (g.1 - e.1).abs() < 1e-9 && g.2 == e.2 && g.3 == e.3
        })
}

#[derive(Debug, Clone)]
struct Case {
    spec: Spec,
    reads: Vec<(prop::sample::Index, prop::sample::Index)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::select(Format::FIFF.to_vec()),
        proptest::collection::vec(
            proptest::sample::select(vec!["eeg", "mag", "grad", "eog", "trigger"]),
            1..6,
        ),
        proptest::sample::select(vec![1u32, 3, 50, 256]),
        1u64..7,
        proptest::sample::select(vec![1.0, 0.5]),
        (
            0u64..3,
            0u64..3,
            any::<bool>(),
            proptest::option::of((1u64..6, 1u64..3)),
            proptest::option::of(1u64..4),
        ),
        any::<u64>(),
        proptest::collection::vec(any::<(prop::sample::Index, prop::sample::Index)>(), 1..4),
    )
        .prop_map(
            |(
                format,
                kinds,
                spr,
                records,
                duration,
                (skip, extra, varying, gap, split),
                seed,
                reads,
            )| {
                let channels = kinds
                    .iter()
                    .enumerate()
                    .map(|(i, &k)| match k {
                        "trigger" => ChannelSpec {
                            label: format!("STI {i}"),
                            ..ChannelSpec::trigger(spr, format)
                        },
                        k => ChannelSpec::fiff(format!("E{i}"), k, spr, format),
                    })
                    .collect();
                let mut spec = Spec::new(format, channels, duration, records);
                spec.seed = seed;
                // Spikes often, so the digital limits are read too.
                spec.spike_every = 7;
                spec.fiff.initial_skip = skip;
                spec.fiff.varying_buffers = varying;
                spec.fiff.split = split.map(|records| FiffSplit {
                    records,
                    naming: SplitNaming::Neuromag,
                });
                spec.first_onset = (skip + extra) as f64 * duration;
                if let Some((before, length)) = gap
                    && before < records
                {
                    spec.gaps = vec![GapSpec {
                        before_record: before,
                        seconds: length as f64 * duration,
                    }];
                }
                Case { spec, reads }
            },
        )
}

proptest! {
    /// Every buffer type (and mixed ones) reads back the truth function in physical units
    /// for any range, with skips as gaps, an initial skip moving the first sample, buffers
    /// of varying length, and split parts (a gap before a part's first record is a jump in
    /// its first sample).
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let spec = &case.spec;
        let file = TempFile::written(spec);
        let files = spec.files(file.path());
        let written: u64 = files.iter().map(|f| std::fs::metadata(f).unwrap().len()).sum();
        prop_assert_eq!(written, kymo_testgen::fiff::file_size_at(spec, file.path()).unwrap());
        if spec.fiff.split.is_none() {
            prop_assert_eq!(written, spec.file_size().unwrap());
        }
        let rec = open(file.path());
        prop_assert_eq!(rec.sources(), files.as_slice());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        prop_assert_eq!(rec.grids().len(), 1);
        let grid = &rec.grids()[0];
        let native = match spec.format {
            Format::Fiff(FiffType::Pack16 | FiffType::Short) => Some(SampleType::I16),
            Format::Fiff(FiffType::Int) => Some(SampleType::I32),
            Format::Fiff(FiffType::Float | FiffType::Double) => Some(SampleType::F32),
            _ => None,
        };
        if let Some(native) = native {
            prop_assert_eq!(grid.sample_type(), native);
        }
        prop_assert_eq!(grid.rate(), spec.rate(0));
        prop_assert_eq!(grid.len(), spec.len(0));
        let segments = spec.segments(0);
        prop_assert_eq!(grid.segments(), segments.as_slice());
        let first_samp = (spec.first_onset * spec.rate(0)).round() as u64;
        let first = rec.extra().iter().find(|(k, _)| k == "first sample").map(|(_, v)| v.clone());
        prop_assert_eq!(first, Some(first_samp.to_string()));
        let ids = grid.channels().to_vec();
        for (a, b) in &case.reads {
            let (a, b) = (a.index(grid.len() as usize + 1) as u64, b.index(grid.len() as usize + 1) as u64);
            let range = a.min(b)..a.max(b);
            if grid.present_in(range.clone()).next().is_none() {
                continue;
            }
            // Reads stay inside a segment; read each piece.
            for piece in grid.present_in(range.clone()).collect::<Vec<_>>() {
                let got = physical(&rec, &ids, piece.clone());
                prop_assert!(matches(spec, &ids, piece.clone(), &got), "{:?}: {:?}", piece, got);
            }
            rec.prefetch(GridId(0), range);
        }
    }
}

#[test]
fn presets_open_with_their_channels() {
    use ChannelKind::*;
    for preset in [
        "fif-float32-meg",
        "fif-int16-skips",
        "fif-int32-varying",
        "fif-double-dc",
        "fif-mixed",
        "fif-split",
    ] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let files = spec.files(file.path());
        let written: u64 = files
            .iter()
            .map(|f| std::fs::metadata(f).unwrap().len())
            .sum();
        let size = kymo_testgen::fiff::file_size_at(&spec, file.path()).unwrap();
        assert_eq!(written, size, "{preset}");
        let (rec, annotations) = open_with(file.path());
        assert!(rec.warnings().is_empty(), "{preset}: {:?}", rec.warnings());
        assert_eq!(rec.format(), "FIFF");
        assert_eq!(rec.sources(), files.as_slice());
        assert!(
            same_annotations(&summary(&rec, &annotations), &expected_annotations(&spec)),
            "{preset}: {:?}",
            summary(&rec, &annotations)
        );
        let bads: Vec<&str> = rec
            .bad_channels()
            .iter()
            .map(|&c| rec.channel(c).name.as_str())
            .collect();
        assert_eq!(bads, spec.fiff.bads, "{preset}");
        // The first sample's time, in local time where the file has a UTC offset.
        let start = spec
            .start
            .checked_add(jiff::SignedDuration::from_secs_f64(spec.first_onset))
            .unwrap();
        assert_eq!(rec.start_time(), Some(start), "{preset}");
        let grid = &rec.grids()[0];
        assert_eq!(grid.segments(), spec.segments(0).as_slice(), "{preset}");
        let ids = grid.channels().to_vec();
        for piece in spec.segments(0) {
            let got = physical(&rec, &ids, piece.clone());
            assert!(matches(&spec, &ids, piece, &got), "{preset}");
        }
        for (channel, c) in rec.channels().iter().zip(&spec.channels) {
            assert_eq!(channel.name, c.label, "{preset}");
            assert_eq!(channel.prefilter, "HP:0.1Hz LP:100Hz", "{preset}");
            // Float64 buffers are read relative to each channel's first sample.
            if !relative(spec.format) {
                assert_eq!(channel.offset, 0.0, "{preset}: {}", c.label);
            }
        }
        let extra = |key: &str| {
            rec.extra()
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(extra("experimenter"), Some("kymo-testgen"));
        assert_eq!(extra("description"), Some(spec.recording.as_str()));
        assert_eq!(extra("line frequency"), Some("50 Hz"));
        let zone = if preset == "fif-int16-skips" {
            "UTC+01:00"
        } else {
            "UTC"
        };
        assert_eq!(extra("time zone"), Some(zone), "{preset}");
    }

    let kinds = |preset: &str| -> Vec<(ChannelKind, String)> {
        open(TempFile::preset(preset).path())
            .channels()
            .iter()
            .map(|c| (c.kind, c.unit.label.clone()))
            .collect()
    };
    let k = |kind, unit: &str| (kind, unit.to_owned());
    assert_eq!(
        kinds("fif-float32-meg"),
        [
            k(Mag, "T"),
            k(Grad, "T/m"),
            k(Grad, "T/m"),
            k(Mag, "T"),
            k(Eeg, "V"),
            k(Eeg, "V"),
            k(Eeg, "V"),
            k(Eeg, "V"),
            k(Eog, "V"),
            k(Ecg, "V"),
            // A stim channel whose calibration isn't 1: a line in volts, read as misc.
            k(Misc, "V"),
            k(Stim, ""),
        ]
    );
    assert_eq!(
        kinds("fif-int32-varying"),
        [
            k(Eeg, "V"),
            k(Eeg, "V"),
            k(Seeg, "V"),
            k(Ecog, "V"),
            k(Emg, "V"),
            k(Ecg, "V"),
        ]
    );
    let rec = open(TempFile::preset("fif-float32-meg").path());
    let extra = rec.extra().iter().find(|(k, _)| k == "first sample");
    assert_eq!(extra.map(|(_, v)| v.as_str()), Some("25800"));
    // The DC channel is read relative to its first sample, folded into the offset; STI 014
    // keeps its codes.
    let rec = open(TempFile::preset("fif-double-dc").path());
    assert!((rec.channels()[2].offset - 1.0).abs() < 1e-3);
    assert_eq!(rec.channels()[3].offset, 0.0);
    assert_eq!(rec.channels()[3].gain, 1.0);
}

/// A FIFF file put together tag by tag, as `rec.fif` in its own directory.
struct Built {
    _dir: TempDir,
    path: PathBuf,
}

impl Built {
    fn new(bytes: &[u8]) -> Built {
        let dir = TempDir::new();
        let path = dir.path().join("rec.fif");
        std::fs::write(&path, bytes).unwrap();
        Built { _dir: dir, path }
    }

    fn open(&self) -> Recording {
        open(&self.path)
    }

    fn error(&self) -> FiffError {
        match kymo_core::open(&self.path) {
            Err(OpenError::Fiff { source, .. }) => source,
            other => panic!("expected a FIFF error, got {other:?}"),
        }
    }
}

/// Two EEG channels at 100 Hz, calibration 1e-6.
fn eeg_channels(t: &mut Tags) {
    t.int(kind::NCHAN, 2).float(kind::SFREQ, 100.0);
    for name in ["A", "B"] {
        t.ch_info(&ChInfo::new(name, 2, 107, 1e-6));
    }
}

/// A buffer of frames (one value per channel each), as int32.
fn buffer(t: &mut Tags, frames: &[[i32; 2]]) {
    let data: Vec<u8> = frames
        .iter()
        .flatten()
        .flat_map(|v| v.to_be_bytes())
        .collect();
    t.tag(kind::DATA_BUFFER, types::INT, &data);
}

/// A file with the measurement info from `info` and the raw data block from `raw`.
fn file(info: impl FnOnce(&mut Tags), raw: impl FnOnce(&mut Tags)) -> Vec<u8> {
    let mut t = Tags::new();
    t.start(block::MEAS).start(block::MEAS_INFO);
    info(&mut t);
    t.end(block::MEAS_INFO).start(block::RAW_DATA);
    raw(&mut t);
    t.end(block::RAW_DATA).end(block::MEAS).finish();
    t.bytes
}

/// Channel `c`'s physical values over the whole grid (NaN in gaps), as read in segments.
fn values(rec: &Recording, c: u32) -> Vec<f32> {
    let grid = &rec.grids()[0];
    let mut out = vec![f32::NAN; grid.len() as usize];
    for s in grid.segments() {
        let got = physical(rec, &[ChannelId(c)], s.clone());
        out[s.start as usize..s.end as usize].copy_from_slice(&got);
    }
    out
}

fn first_sample(rec: &Recording) -> Option<String> {
    rec.extra()
        .iter()
        .find(|(k, _)| k == "first sample")
        .map(|(_, v)| v.clone())
}

#[test]
fn skips_are_gaps_except_before_the_first_buffer() {
    let frames = |k: i32, n: i32| -> Vec<[i32; 2]> { (0..n).map(|i| [k * 10 + i, -k]).collect() };
    // A skip right after the first sample moves it; one inside is a gap of that many
    // buffers of the next one's length; one at the end is ignored.
    let bytes = file(eeg_channels, |t| {
        t.int(kind::FIRST_SAMPLE, 100).int(kind::DATA_SKIP, 2);
        buffer(t, &frames(1, 3));
        buffer(t, &frames(2, 2));
        t.int(kind::DATA_SKIP, 2);
        buffer(t, &frames(3, 4));
        t.int(kind::DATA_SKIP, 5);
    });
    let rec = Built::new(&bytes).open();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(first_sample(&rec).as_deref(), Some("106"));
    let grid = &rec.grids()[0];
    assert_eq!(grid.len(), 3 + 2 + 8 + 4);
    assert_eq!(grid.segments(), [0..5, 13..17]);
    let a = values(&rec, 0);
    let expected: Vec<f64> = [10, 11, 12, 20, 21]
        .into_iter()
        .chain([30, 31, 32, 33])
        .map(|v| f64::from(v) * 1e-6)
        .collect();
    let present: Vec<f64> = a
        .iter()
        .filter(|v| !v.is_nan())
        .map(|&v| f64::from(v))
        .collect();
    assert_eq!(present.len(), expected.len());
    for (g, e) in present.iter().zip(&expected) {
        assert!((g - e).abs() <= e.abs() * 1e-7, "{g} {e}");
    }
    assert!(a[5..13].iter().all(|v| v.is_nan()));

    // A skip before the first buffer moves the first sample even without a first-sample
    // tag; empty buffers hold nothing and don't take a pending skip.
    let bytes = file(eeg_channels, |t| {
        t.int(kind::DATA_SKIP, 3);
        buffer(t, &frames(1, 10));
        buffer(t, &[]);
        buffer(t, &frames(2, 1));
    });
    let rec = Built::new(&bytes).open();
    assert_eq!(first_sample(&rec).as_deref(), Some("30"));
    assert_eq!(rec.grids()[0].segments(), [0..11]);
}

#[test]
fn skips_in_samples_and_skips_in_a_row() {
    let frames = |k: i32, n: i32| -> Vec<[i32; 2]> { (0..n).map(|i| [k * 10 + i, -k]).collect() };
    // FIFF_DATA_SKIP_SAMP is a skip of that many samples; skips before one buffer add up.
    let bytes = file(eeg_channels, |t| {
        t.int(kind::FIRST_SAMPLE, 100).int(kind::DATA_SKIP_SAMP, 7);
        buffer(t, &frames(1, 3));
        t.int(kind::DATA_SKIP_SAMP, 5);
        buffer(t, &frames(2, 2));
        t.int(kind::DATA_SKIP, 1).int(kind::DATA_SKIP, 2);
        buffer(t, &frames(3, 4));
        t.int(kind::DATA_SKIP, 1).int(kind::DATA_SKIP_SAMP, 3);
        buffer(t, &frames(4, 2));
    });
    let rec = Built::new(&bytes).open();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(first_sample(&rec).as_deref(), Some("107"));
    // Gaps of 5 samples, 3 buffers of 4, and 1 buffer of 2 plus 3 samples.
    assert_eq!(rec.grids()[0].segments(), [0..3, 8..10, 22..26, 31..33]);
    let present: Vec<i64> = values(&rec, 0)
        .iter()
        .filter(|v| !v.is_nan())
        .map(|&v| (f64::from(v) * 1e6).round() as i64)
        .collect();
    assert_eq!(present, [10, 11, 12, 20, 21, 30, 31, 32, 33, 40, 41]);
}

#[test]
fn tags_are_followed_wherever_next_points() {
    // As a file edited in place: a channel tag jumps to tags appended at the end (the
    // second channel and the end of the info), which jump back to the raw data.
    let mut t = Tags::new();
    t.start(block::MEAS).start(block::MEAS_INFO);
    t.int(kind::NCHAN, 2).float(kind::SFREQ, 100.0);
    let mut first = Tags::default();
    first.ch_info(&ChInfo::new("A", 2, 107, 1.0));
    let jump_at = t.bytes.len();
    t.bytes.extend(&first.bytes);
    let back = t.bytes.len();
    t.start(block::RAW_DATA);
    buffer(&mut t, &[[1, 2], [3, 4]]);
    t.end(block::RAW_DATA).end(block::MEAS).finish();
    let appended = t.bytes.len();
    let mut tail = Tags::default();
    tail.ch_info(&ChInfo::new("B", 2, 107, 1.0));
    let n = tail.bytes.len();
    tail.end(block::MEAS_INFO);
    // The last appended tag points back to the raw data.
    tail.bytes[n + 12..n + 16].copy_from_slice(&(back as i32).to_be_bytes());
    t.bytes.extend(&tail.bytes);
    t.bytes[jump_at + 12..jump_at + 16].copy_from_slice(&(appended as i32).to_be_bytes());
    let rec = Built::new(&t.bytes).open();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
    assert_eq!(values(&rec, 1), [2.0, 4.0]);

    // A pointer back to a tag already jumped to is a loop: the walk stops with a warning.
    // Here the last tag (the NOP before the appended tags) points to them again.
    let mut bytes = t.bytes.clone();
    let at = appended - 16;
    bytes[at + 12..at + 16].copy_from_slice(&(appended as i32).to_be_bytes());
    let rec = Built::new(&bytes).open();
    assert!(
        rec.warnings().iter().any(|w| w.contains("not a tag")),
        "{:?}",
        rec.warnings()
    );
}

#[test]
fn damaged_and_unreadable_buffers() {
    // The file ends inside the last buffer: its whole frames are read.
    let mut bytes = file(eeg_channels, |t| buffer(t, &[[1, 2], [3, 4], [5, 6]]));
    // Without the closing tags (two block ends and the NOP) and 3 values.
    bytes.truncate(bytes.len() - (20 + 20 + 16) - 12);
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.grids()[0].len(), 1);
    assert!(
        rec.warnings()
            .iter()
            .any(|w| w.contains("inside its last data buffer"))
    );

    // A buffer whose size isn't whole frames: the rest is ignored.
    let bytes = file(eeg_channels, |t| {
        t.tag(kind::DATA_BUFFER, types::INT, &[0; 12]);
    });
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.grids()[0].len(), 1);
    assert!(
        rec.warnings()
            .iter()
            .any(|w| w.contains("incomplete sample"))
    );

    // Complex and unknown types are refused.
    for (typ, reason) in [(types::COMPLEX_FLOAT, "complex"), (99, "data type 99")] {
        let bytes = file(eeg_channels, |t| {
            t.tag(kind::DATA_BUFFER, typ, &[0; 16]);
        });
        match Built::new(&bytes).error() {
            FiffError::InvalidBuffer { reason: r, .. } => assert!(r.contains(reason), "{r}"),
            other => panic!("{other:?}"),
        }
    }

    // A file cut inside the first channel's description (after the file id, directory
    // pointer, two block starts, NCHAN and SFREQ): the walk stops there.
    let bytes = file(eeg_channels, |t| buffer(t, &[[1, 2]]));
    assert_eq!(Built::new(&bytes[..150]).error(), FiffError::NoChannels);
}

#[test]
fn files_without_raw_data_are_refused() {
    let mut t = Tags::new();
    t.int(kind::NOP, 0).finish();
    assert_eq!(Built::new(&t.bytes).error(), FiffError::NoMeasurement);

    let mut t = Tags::new();
    t.start(block::MEAS).end(block::MEAS).finish();
    assert_eq!(Built::new(&t.bytes).error(), FiffError::NoInfo);

    // Evoked responses: info but no raw data.
    let mut t = Tags::new();
    t.start(block::MEAS).start(block::MEAS_INFO);
    eeg_channels(&mut t);
    t.end(block::MEAS_INFO)
        .start(block::EVOKED)
        .end(block::EVOKED)
        .end(block::MEAS)
        .finish();
    let error = Built::new(&t.bytes).error();
    assert!(error.to_string().contains("evoked"), "{error}");

    // No channels, no rate.
    let bytes = file(
        |t| {
            t.float(kind::SFREQ, 100.0);
        },
        |_| {},
    );
    assert_eq!(Built::new(&bytes).error(), FiffError::NoChannels);
    let bytes = file(
        |t| {
            t.ch_info(&ChInfo::new("A", 2, 107, 1.0));
        },
        |_| {},
    );
    assert!(matches!(
        Built::new(&bytes).error(),
        FiffError::InvalidField {
            field: "sampling rate",
            ..
        }
    ));
    // A channel description of the wrong size.
    let bytes = file(
        |t| {
            t.float(kind::SFREQ, 100.0)
                .tag(kind::CH_INFO, types::CH_INFO_STRUCT, &[0; 90]);
        },
        |_| {},
    );
    assert!(matches!(
        Built::new(&bytes).error(),
        FiffError::InvalidChannel { index: 0, .. }
    ));
}

#[test]
fn maxshield_and_continuous_data_blocks() {
    for (raw, maxshield) in [(block::IAS_RAW_DATA, true), (block::CONTINUOUS_DATA, false)] {
        let mut t = Tags::new();
        t.start(block::MEAS).start(block::MEAS_INFO);
        eeg_channels(&mut t);
        t.end(block::MEAS_INFO).start(raw);
        buffer(&mut t, &[[1, 2]]);
        t.end(raw).end(block::MEAS).finish();
        let rec = Built::new(&t.bytes).open();
        assert_eq!(rec.grids()[0].len(), 1);
        assert_eq!(
            rec.warnings().iter().any(|w| w.contains("MaxShield")),
            maxshield
        );
        assert_eq!(rec.extra().iter().any(|(k, _)| k == "MaxShield"), maxshield);
    }
}

#[test]
fn channel_kinds_units_and_names() {
    use ChannelKind::*;
    // (FIFF kind, unit, calibration) and the expected kind and unit.
    let cases = [
        (1, 112, 1.0, Mag, "T"),
        (1, 201, 1.0, Grad, "T/m"),
        (301, 112, 1.0, RefMeg, "T"),
        (2, 107, 1.0, Eeg, "V"),
        (3, 107, 1.0, Stim, "V"),
        // A stim channel whose calibration isn't 1 is a line in volts or an analog input.
        (3, 107, 3.0517578e-4, Misc, "V"),
        (102, 0, 1.0, Bio, ""),
        (202, 107, 1.0, Eog, "V"),
        (302, 107, 1.0, Emg, "V"),
        (402, 107, 1.0, Ecg, "V"),
        (502, 107, 1.0, Misc, "V"),
        (602, 107, 1.0, Resp, "V"),
        (802, 107, 1.0, Seeg, "V"),
        (803, 107, 1.0, Dbs, "V"),
        (902, 107, 1.0, Ecog, "V"),
        (201, 112, 1.0, Misc, "T"),
        (900, -1, 1.0, Misc, ""),
        (1100, 114, 1.0, Misc, "\u{b0}C"),
        (502, 118, 1.0, Misc, "s\u{b2}"),
        // Eye tracking positions, in pixels.
        (1400, 210, 1.0, Misc, "px"),
    ];
    let bytes = file(
        |t| {
            t.int(kind::NCHAN, cases.len() as i32)
                .float(kind::SFREQ, 100.0);
            for (k, &(kind, unit, cal, ..)) in cases.iter().enumerate() {
                let mut ch = ChInfo::new(format!("C{k}"), kind, unit, cal);
                ch.unit_mul = if k % 2 == 0 { -1 } else { 0 };
                t.ch_info(&ch);
            }
        },
        |_| {},
    );
    let rec = Built::new(&bytes).open();
    for (channel, &(_, _, cal, kind, unit)) in rec.channels().iter().zip(&cases) {
        assert_eq!(channel.kind, kind, "{}", channel.name);
        assert_eq!(channel.unit.label, unit, "{}", channel.name);
        // The unit multiplier doesn't scale values, as in MNE.
        assert_eq!(channel.gain, f64::from(cal), "{}", channel.name);
    }

    // Extended channel info replaces the struct's fields (long names); a count that
    // doesn't match the channels is ignored with a warning, as is a wrong NCHAN.
    let long = "EEG Fp1-Reference-long";
    let bytes = file(
        |t| {
            t.int(kind::NCHAN, 1).float(kind::SFREQ, 100.0);
            let ch = ChInfo::new(long, 2, 107, 1.0);
            t.ch_info(&ch).extended(&ChInfo { kind: 202, ..ch });
        },
        |_| {},
    );
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.channels()[0].name, long);
    assert_eq!(rec.channels()[0].kind, Eog);
    let bytes = file(
        |t| {
            t.int(kind::NCHAN, 3).float(kind::SFREQ, 100.0);
            let ch = ChInfo::new(long, 2, 107, 1.0);
            t.ch_info(&ch).ch_info(&ch).extended(&ch);
        },
        |_| {},
    );
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.channels()[0].name, &long[..15]);
    for warning in [
        "states 3 channels but describes 2",
        "extended information for 1",
    ] {
        assert!(
            rec.warnings().iter().any(|w| w.contains(warning)),
            "{:?}",
            rec.warnings()
        );
    }
}

#[test]
fn start_times() {
    let date = |t: &mut Tags| {
        eeg_channels(t);
    };
    // No date: none, unless an id has a time (here the file's, as MNE falls back to).
    let bytes = file(date, |t| buffer(t, &[[0, 0]]));
    assert_eq!(Built::new(&bytes).open().start_time(), None);
    let mut with_id = bytes.clone();
    // The file id's time is at bytes 16 + 12.
    with_id[28..32].copy_from_slice(&1_000_000_000i32.to_be_bytes());
    let rec = Built::new(&with_id).open();
    assert_eq!(
        rec.start_time(),
        Some(jiff::civil::date(2001, 9, 9).at(1, 46, 40, 0))
    );

    let dated = |date: [i32; 2], offset: Option<&str>, first: i32| {
        file(
            |t| {
                t.ints(kind::MEAS_DATE, &date);
                if let Some(offset) = offset {
                    t.string(kind::UTC_OFFSET, offset);
                }
                eeg_channels(t);
            },
            |t| {
                t.int(kind::FIRST_SAMPLE, first);
                buffer(t, &[[0, 0]]);
            },
        )
    };
    let utc = jiff::civil::date(2001, 9, 9).at(1, 46, 40, 0);
    let cases = [
        // MNE's "no date".
        ([0, i32::MAX], None, 0, None, None),
        (
            [1_000_000_000, 250_000],
            None,
            0,
            Some(utc + jiff::SignedDuration::from_micros(250_000)),
            Some("UTC"),
        ),
        // First sample 150 at 100 Hz: 1.5 s later.
        (
            [1_000_000_000, 0],
            None,
            150,
            Some(utc + jiff::SignedDuration::from_millis(1500)),
            Some("UTC"),
        ),
        (
            [1_000_000_000, 0],
            Some("+02:00"),
            0,
            Some(utc + jiff::SignedDuration::from_hours(2)),
            Some("UTC+02:00"),
        ),
        (
            [1_000_000_000, 0],
            Some("-0530"),
            0,
            Some(utc - jiff::SignedDuration::from_mins(330)),
            Some("UTC-05:30"),
        ),
        ([1_000_000_000, 0], Some("CET"), 0, Some(utc), Some("UTC")),
    ];
    for (date, offset, first, expected, zone) in cases {
        let rec = Built::new(&dated(date, offset, first)).open();
        assert_eq!(rec.start_time(), expected, "{date:?} {offset:?}");
        let got = rec
            .extra()
            .iter()
            .find(|(k, _)| k == "time zone")
            .map(|(_, v)| v.as_str());
        assert_eq!(got, zone, "{date:?} {offset:?}");
        // The file's date is UTC, so the start time's offset from it is known.
        let seconds = match offset {
            Some("+02:00") => 7200,
            Some("-0530") => -19800,
            _ => 0,
        };
        assert_eq!(
            rec.utc_offset(),
            expected.map(|_| jiff::tz::Offset::from_seconds(seconds).unwrap()),
            "{date:?} {offset:?}"
        );
        assert_eq!(
            rec.warnings().iter().any(|w| w.contains("UTC offset")),
            offset == Some("CET")
        );
    }
}

fn corruptible() -> Vec<Vec<u8>> {
    ["fif-int16-skips", "fif-int32-varying", "fif-mixed"]
        .into_iter()
        .map(|p| {
            let mut spec = Spec::preset(p).unwrap();
            spec.n_records = spec.n_records.min(40);
            spec.gaps.retain(|g| g.before_record < spec.n_records);
            std::fs::read(TempFile::written(&spec).path()).unwrap()
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Damaged files never panic: opening fails, or the recording reads (or fails to).
    #[test]
    fn damaged_files_never_panic(
        which in 0usize..3,
        changes in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<prop::sample::Index>()),
    ) {
        thread_local! {
            static FILES: Vec<Vec<u8>> = corruptible();
        }
        let mut bytes = FILES.with(|f| f[which].clone());
        // Mostly in the head, where the structure is.
        for (at, value) in changes {
            let at = at.index(bytes.len().min(4096));
            bytes[at] = value;
        }
        if let Some(cut) = cut {
            bytes.truncate(cut.index(bytes.len() + 1));
        }
        let built = Built::new(&bytes);
        if let Ok(opened) = kymo_core::open(&built.path)
            && let Ok((rec, _)) = Recording::new(opened)
        {
            for (g, grid) in rec.grids().iter().enumerate() {
                let g = GridId(g as u32);
                let ids = grid.channels().to_vec();
                for s in grid.segments().iter().take(4) {
                    let s = s.start..s.end.min(s.start + (1 << 14));
                    let mut out = vec![0.0f32; ids.len() * (s.end - s.start) as usize];
                    let _ = rec.read_physical(g, &ids, s.clone(), &mut out);
                    rec.prefetch(g, s);
                }
            }
        }
    }
}

/// Writes `spec` (a split recording) in its own directory; the paths of its parts.
fn split_files(spec: &Spec, dir: &TempDir) -> Vec<PathBuf> {
    let path = dir.path().join("rec.fif");
    kymo_testgen::write(spec, &path).unwrap();
    spec.files(&path)
}

fn split_spec(naming: SplitNaming) -> Spec {
    let f = Format::Fiff(FiffType::Int);
    let channels = (0..3)
        .map(|i| ChannelSpec::fiff(format!("E{i}"), "eeg", 50, f))
        .collect();
    let mut spec = Spec::new(f, channels, 1.0, 10);
    spec.first_onset = 3.0;
    spec.spike_every = 7;
    spec.fiff.split = Some(FiffSplit { records: 4, naming });
    spec
}

fn error_of(path: &std::path::Path) -> FiffError {
    match kymo_core::open(path) {
        Err(OpenError::Fiff { source, .. }) => source,
        other => panic!("expected a FIFF error, got {other:?}"),
    }
}

#[test]
fn split_recordings_open_as_one_from_any_part() {
    for naming in [
        SplitNaming::Neuromag,
        SplitNaming::NumbersOnly,
        SplitNaming::Bids,
    ] {
        let mut spec = split_spec(naming);
        // A gap before the second part's first record: a jump in its first sample.
        spec.gaps = vec![GapSpec {
            before_record: 4,
            seconds: 2.0,
        }];
        let dir = TempDir::new();
        let files = split_files(&spec, &dir);
        assert_eq!(files.len(), 3);
        for (k, part) in files.iter().enumerate() {
            let rec = open(part);
            assert!(
                rec.warnings().is_empty(),
                "{naming:?} part {k}: {:?}",
                rec.warnings()
            );
            assert_eq!(rec.sources(), files.as_slice(), "{naming:?} part {k}");
            let grid = &rec.grids()[0];
            assert_eq!(grid.segments(), spec.segments(0).as_slice());
            assert_eq!(grid.segments(), [0..200, 300..600]);
            let ids = grid.channels().to_vec();
            for s in spec.segments(0) {
                let got = physical(&rec, &ids, s.clone());
                assert!(matches(&spec, &ids, s, &got), "{naming:?} part {k}");
            }
            assert_eq!(first_sample(&rec).as_deref(), Some("150"));
            let parts = rec.extra().iter().find(|(k, _)| k == "split parts");
            assert_eq!(parts.map(|(_, v)| v.as_str()), Some("3"));
        }
    }
}

#[test]
fn missing_and_mismatched_split_parts() {
    let spec = split_spec(SplitNaming::Neuromag);

    // A missing last part: the parts before it, with a warning.
    let dir = TempDir::new();
    let files = split_files(&spec, &dir);
    std::fs::remove_file(&files[2]).unwrap();
    let rec = open(&files[0]);
    assert_eq!(rec.sources(), &files[..2]);
    assert_eq!(rec.grids()[0].len(), 400);
    assert!(
        rec.warnings()
            .iter()
            .any(|w| w.contains("next part") && w.contains("missing")),
        "{:?}",
        rec.warnings()
    );

    // A missing first part: a later one is read from itself on.
    let dir = TempDir::new();
    let files = split_files(&spec, &dir);
    std::fs::remove_file(&files[0]).unwrap();
    let rec = open(&files[1]);
    assert_eq!(rec.sources(), &files[1..]);
    assert_eq!(first_sample(&rec).as_deref(), Some("350"));
    assert!(
        rec.warnings()
            .iter()
            .any(|w| w.contains("previous part") && w.contains("missing"))
    );

    // A part of another rate, or one starting before the previous part ends, is an error.
    let dir = TempDir::new();
    let files = split_files(&spec, &dir);
    let other = TempDir::new();
    let mut faster = split_spec(SplitNaming::Neuromag);
    for c in &mut faster.channels {
        c.samples_per_record = 100;
    }
    let faster = split_files(&faster, &other);
    std::fs::copy(&faster[1], &files[1]).unwrap();
    assert!(matches!(
        error_of(&files[0]),
        FiffError::SplitMismatch { .. }
    ));
    // The same rate and length, but a channel of another name.
    let renamed = TempDir::new();
    let mut other_names = split_spec(SplitNaming::Neuromag);
    other_names.channels[1].label = "X1".into();
    let other_names = split_files(&other_names, &renamed);
    std::fs::copy(&other_names[1], &files[1]).unwrap();
    match error_of(&files[0]) {
        FiffError::SplitMismatch { reason, .. } => assert!(reason.contains("E1"), "{reason}"),
        other => panic!("{other:?}"),
    }
    let earlier = TempDir::new();
    let mut early = split_spec(SplitNaming::Neuromag);
    early.first_onset = 0.0;
    let early = split_files(&early, &earlier);
    std::fs::copy(&early[1], &files[1]).unwrap();
    match error_of(&files[0]) {
        FiffError::SplitMismatch { reason, .. } => assert!(reason.contains("before"), "{reason}"),
        other => panic!("{other:?}"),
    }

    // Parts that refer to each other in a loop: read up to the loop, with a warning.
    let part = |next: &str, prev: Option<&str>| {
        file(eeg_channels, |t| {
            if let Some(prev) = prev {
                t.int(kind::FIRST_SAMPLE, 2).reference(1, Some(prev), None);
            }
            buffer(t, &[[1, 2], [3, 4]]);
            t.reference(2, Some(next), None);
        })
    };
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.fif"), part("b.fif", None)).unwrap();
    std::fs::write(dir.path().join("b.fif"), part("a.fif", Some("a.fif"))).unwrap();
    let rec = open(&dir.path().join("a.fif"));
    assert_eq!(rec.sources().len(), 2);
    assert_eq!(rec.grids()[0].len(), 4);
    assert!(
        rec.warnings().iter().any(|w| w.contains("loop")),
        "{:?}",
        rec.warnings()
    );

    // A part named by a full path elsewhere (as MNE may write the previous part's) is
    // found beside the file under its last component.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.fif"), part("/elsewhere/b.fif", None)).unwrap();
    let b = file(eeg_channels, |t| {
        t.int(kind::FIRST_SAMPLE, 2);
        buffer(t, &[[5, 6]]);
    });
    std::fs::write(dir.path().join("b.fif"), b).unwrap();
    let rec = open(&dir.path().join("a.fif"));
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(rec.grids()[0].len(), 3);
}

/// A file with two EEG channels at 100 Hz, an annotations block from `annotations`, and
/// 10 s of samples from `first`, its first sample, on; with a measurement date if `dated`.
fn annotated(dated: bool, first: i32, annotations: impl FnOnce(&mut Tags)) -> Vec<u8> {
    let mut t = Tags::new();
    t.start(block::MEAS).start(block::MEAS_INFO);
    if dated {
        t.ints(kind::MEAS_DATE, &[1_000_000_000, 0]);
    }
    eeg_channels(&mut t);
    t.end(block::MEAS_INFO).start(block::MNE_ANNOTATIONS);
    annotations(&mut t);
    t.end(block::MNE_ANNOTATIONS).start(block::RAW_DATA);
    t.int(kind::FIRST_SAMPLE, first);
    buffer(&mut t, &[[0, 0]; 1000]);
    t.end(block::RAW_DATA).end(block::MEAS).finish();
    t.bytes
}

#[test]
fn annotation_onsets_count_from_the_first_sample() {
    // First sample 200 at 100 Hz: 2 s after sample 0.
    let block = |orig: Option<f64>| {
        move |t: &mut Tags| {
            t.floats(kind::MNE_BASELINE_MIN, &[5.0, 7.5])
                .floats(kind::MNE_BASELINE_MAX, &[5.5, 7.5])
                .names(kind::COMMENT, &["BAD: x", " spaced "]);
            if let Some(orig) = orig {
                t.doubles(kind::MEAS_DATE, &[orig, 250_000.0]);
            }
        }
    };
    // (dated, the annotations' orig_time) and the first onset kymo reads.
    let cases = [
        // From the annotations' date (0.25 s after the second), which is the recording's.
        (true, Some(1e9), 5.0 + 0.25 - 2.0),
        // A date a second later than the recording's.
        (true, Some(1e9 + 1.0), 5.0 + 1.25 - 2.0),
        // Without their own date, MNE takes them from the first sample.
        (true, None, 5.0),
        // Without a recording date, from sample 0.
        (false, None, 5.0 - 2.0),
        (false, Some(1e9), 5.0 - 2.0),
    ];
    for (dated, orig, onset) in cases {
        let built = Built::new(&annotated(dated, 200, block(orig)));
        let (rec, annotations) = open_with(&built.path);
        assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        let got = summary(&rec, &annotations);
        assert_eq!(got.len(), 2);
        assert!((got[0].0 - onset).abs() < 1e-9, "{dated} {orig:?}: {got:?}");
        assert!((got[1].0 - got[0].0 - 2.5).abs() < 1e-9);
        assert_eq!((got[0].1, got[1].1), (0.5, 0.0));
        // Colons come back from "{COLON}", spaces stay.
        assert_eq!(
            (got[0].2.as_str(), got[1].2.as_str()),
            ("BAD: x", " spaced ")
        );
    }
}

#[test]
fn annotation_channels_counts_and_range() {
    let lists = |json: &str| {
        let json = json.to_owned();
        move |t: &mut Tags| {
            t.floats(kind::MNE_BASELINE_MIN, &[1.0, 2.0, 3.0])
                .floats(kind::MNE_BASELINE_MAX, &[1.0, 2.0, 3.0])
                .names(kind::COMMENT, &["a", "b", "c"])
                .string(kind::MNE_EPOCHS_DROP_LOG, &json);
        }
    };
    // Lists of names, with JSON's escapes; an unknown name means all channels.
    let built = Built::new(&annotated(
        false,
        0,
        lists(r#"[["A", "B"], [], ["B", "Z\"q"]]"#),
    ));
    let (rec, annotations) = open_with(&built.path);
    let got: Vec<Vec<String>> = summary(&rec, &annotations)
        .into_iter()
        .map(|a| a.3)
        .collect();
    assert_eq!(got, [vec!["A".to_owned(), "B".to_owned()], vec![], vec![]]);
    assert!(
        rec.warnings().iter().any(|w| w.contains("Z\"q")),
        "{:?}",
        rec.warnings()
    );
    // Not JSON: all channels, with a warning.
    let built = Built::new(&annotated(false, 0, lists(r#"[["A"]"#)));
    let (rec, annotations) = open_with(&built.path);
    assert!(annotations.iter().all(|a| a.channels.is_empty()));
    assert!(rec.warnings().iter().any(|w| w.contains("not valid JSON")));

    // Fewer ends than onsets: the first ones; outside the data (10 s): left out.
    let built = Built::new(&annotated(false, 0, |t| {
        t.floats(kind::MNE_BASELINE_MIN, &[-3.0, -0.5, 4.0, 10.5])
            .floats(kind::MNE_BASELINE_MAX, &[-2.0, 0.5, 4.0])
            .names(kind::COMMENT, &["gone", "kept", "kept too", "unended"]);
    }));
    let (rec, annotations) = open_with(&built.path);
    let got: Vec<String> = annotations.iter().map(|a| a.description.clone()).collect();
    assert_eq!(got, ["kept", "kept too"]);
    for warning in ["the first 3 are read", "1 annotation(s) outside"] {
        assert!(
            rec.warnings().iter().any(|w| w.contains(warning)),
            "{:?}",
            rec.warnings()
        );
    }
}

#[test]
fn bad_channels_by_name_and_struct_name() {
    let long = "EEG Fp1-Reference-long";
    let bytes = file(
        |t| {
            t.int(kind::NCHAN, 3).float(kind::SFREQ, 100.0);
            let chs = [
                ChInfo::new(long, 2, 107, 1.0),
                ChInfo::new("B", 2, 107, 1.0),
                ChInfo::new("C", 2, 107, 1.0),
            ];
            for ch in &chs {
                t.ch_info(ch);
            }
            for ch in &chs {
                t.extended(ch);
            }
            // An older list, replaced by the last one (MNE's tools append a new list when
            // they mark channels bad in place).
            t.start(block::MNE_BAD_CHANNELS)
                .names(kind::MNE_CH_NAME_LIST, &["B"])
                .end(block::MNE_BAD_CHANNELS);
            // MNE writes bad channels under their struct names (cut to 15 bytes).
            t.start(block::MNE_BAD_CHANNELS)
                .names(kind::MNE_CH_NAME_LIST, &[&long[..15], "C", "nope"])
                .end(block::MNE_BAD_CHANNELS);
        },
        |t| {
            let data: Vec<u8> = [1i32, 2, 3].iter().flat_map(|v| v.to_be_bytes()).collect();
            t.tag(kind::DATA_BUFFER, types::INT, &data);
        },
    );
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.bad_channels(), [ChannelId(0), ChannelId(2)]);
    assert!(
        rec.warnings().iter().any(|w| w.contains("nope")),
        "{:?}",
        rec.warnings()
    );
}

#[test]
fn only_the_combined_stim_channel_has_events() {
    let stim = |names: &[(&str, f32)]| {
        let bytes = file(
            |t| {
                t.int(kind::NCHAN, names.len() as i32)
                    .float(kind::SFREQ, 100.0);
                for &(name, cal) in names {
                    t.ch_info(&ChInfo::new(name, 3, 107, cal));
                }
            },
            |_| {},
        );
        Built::new(&bytes)
            .open()
            .channels()
            .iter()
            .map(|c| (c.kind, c.events, c.trigger_mask))
            .collect::<Vec<_>>()
    };
    use ChannelKind::*;
    let combined = (Stim, true, Some(0xffff));
    let line = (Stim, false, None);
    // STI101 wins over STI 014, which is then one more line it combines.
    assert_eq!(
        stim(&[("STI001", 1.0), ("STI 014", 1.0), ("STI101", 1.0)]),
        [line, line, combined]
    );
    assert_eq!(
        stim(&[("STI 001", 1.0), ("STI 014", 1.0)]),
        [line, combined]
    );
    // Without a combined stim channel (here STI 014 in volts, a misc channel), every stim
    // channel has events, unmasked.
    assert_eq!(
        stim(&[("DIN1", 1.0), ("STI 014", 0.5)]),
        [(Stim, true, None), (Misc, true, None)]
    );
}

#[test]
fn stim_lines_a_combined_channel_covers_have_no_changes() {
    use kymo_core::pyramid::Pyramid;
    // fif-split: STI 001 and STI 014, which combines it.
    let rec = open(TempFile::preset("fif-split").path());
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    let changes = pyramid.stim_changes(&rec);
    let names: Vec<&str> = changes
        .iter()
        .map(|s| rec.channel(s.channel).name.as_str())
        .collect();
    assert_eq!(names, ["STI 014"]);
    assert!(!changes[0].changes.is_empty());
}
