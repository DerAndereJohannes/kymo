//! FIFF raw recordings: samples against the truth function for every buffer type, the
//! presets, and files put together tag by tag for each rule of the tag walk, the
//! measurement info and the data buffers.

use std::ops::Range;
use std::path::PathBuf;

use crate::common::{TempDir, TempFile};
use kymo_core::fiff::FiffError;
use kymo_core::{ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType};
use kymo_testgen::fiff::{ChInfo, Tags, block, kind, types};
use kymo_testgen::{ChannelSpec, FiffType, Format, GapSpec, Spec};
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
    Recording::new(kymo_core::open(path).unwrap()).unwrap().0
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
        ),
        any::<u64>(),
        proptest::collection::vec(any::<(prop::sample::Index, prop::sample::Index)>(), 1..4),
    )
        .prop_map(
            |(format, kinds, spr, records, duration, (skip, extra, varying, gap), seed, reads)| {
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
    /// for any range, with skips as gaps, an initial skip moving the first sample, and
    /// buffers of varying length.
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let spec = &case.spec;
        let file = TempFile::written(spec);
        prop_assert_eq!(std::fs::metadata(file.path()).unwrap().len(), spec.file_size().unwrap());
        let rec = open(file.path());
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
    ] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len(),
            spec.file_size().unwrap(),
            "{preset}"
        );
        let rec = open(file.path());
        assert!(rec.warnings().is_empty(), "{preset}: {:?}", rec.warnings());
        assert_eq!(rec.format(), "FIFF");
        assert_eq!(rec.sources(), [file.path().to_owned()]);
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
