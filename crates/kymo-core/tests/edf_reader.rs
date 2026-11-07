// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use common::TempFile;
use kymo_core::edf::EdfError;
use kymo_core::{ChannelId, GridId, OpenError, ReadError, Recording, SamplesMut};
use kymo_testgen::{ChannelSpec, Format, Spec};
use proptest::prelude::*;

fn open(path: &Path) -> Recording {
    Recording::new(kymo_core::open(path).unwrap()).unwrap().0
}

/// Native samples as i64, whatever the sample type.
fn read_native(
    rec: &Recording,
    grid: GridId,
    channels: &[ChannelId],
    range: Range<u64>,
) -> Vec<i64> {
    let n = channels.len() * (range.end - range.start) as usize;
    if rec.grid(grid).sample_type() == kymo_core::SampleType::I16 {
        let mut out = vec![0i16; n];
        rec.read_native(grid, channels, range, SamplesMut::I16(&mut out))
            .unwrap();
        out.into_iter().map(i64::from).collect()
    } else {
        let mut out = vec![0i32; n];
        rec.read_native(grid, channels, range, SamplesMut::I32(&mut out))
            .unwrap();
        out.into_iter().map(i64::from).collect()
    }
}

/// The truth for a read: channel-major `Spec::digital` values.
fn truth(spec: &Spec, channels: &[ChannelId], range: Range<u64>) -> Vec<i64> {
    channels
        .iter()
        .flat_map(|c| {
            range
                .clone()
                .map(move |i| i64::from(spec.digital(c.0 as usize, i)))
        })
        .collect()
}

#[test]
fn reads_whole_channels_of_each_format() {
    for format in [Format::Edf, Format::EdfPlusC, Format::Bdf, Format::BdfPlusC] {
        let channels = vec![
            ChannelSpec::eeg("EEG Fz", 64, format),
            ChannelSpec::eeg("EEG Cz", 16, format),
            ChannelSpec::eeg("EEG Pz", 64, format),
        ];
        let spec = Spec::new(format, channels, 0.25, 11);
        let file = TempFile::written(&spec);
        let rec = open(file.path());
        assert_eq!(rec.format(), format.name());
        assert!(rec.warnings().is_empty());
        for (g, grid) in rec.grids().iter().enumerate() {
            let ids = grid.channels().to_vec();
            let got = read_native(&rec, GridId(g as u32), &ids, 0..grid.len());
            assert_eq!(
                got,
                truth(&spec, &ids, 0..grid.len()),
                "{format:?}, grid {g}"
            );
        }
    }
}

#[test]
fn int24_spikes_reach_both_limits() {
    // Spikes sit at the digital limits, so this checks the int24 sign extension at both ends.
    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let rec = open(file.path());
    let len = rec.grid(GridId(0)).len();
    let got = read_native(&rec, GridId(0), &[ChannelId(0)], 0..len);
    assert!(got.contains(&-8_388_608));
    assert!(got.contains(&8_388_607));
    assert_eq!(got, truth(&spec, &[ChannelId(0)], 0..len));
}

#[test]
fn physical_values_use_f64_scaling() {
    let format = Format::Edf;
    let channels = vec![
        ChannelSpec {
            label: "DC".into(),
            physical_min: 999_000.0,
            physical_max: 1_001_000.0,
            ..ChannelSpec::eeg("", 100, format)
        },
        ChannelSpec {
            label: "Flipped".into(),
            physical_min: 500.0,
            physical_max: -500.0,
            ..ChannelSpec::eeg("", 100, format)
        },
    ];
    let spec = Spec::new(format, channels, 1.0, 3);
    let file = TempFile::written(&spec);
    let rec = open(file.path());
    let ids = [ChannelId(0), ChannelId(1)];
    let mut out = vec![0.0f32; 2 * 300];
    rec.read_physical(GridId(0), &ids, 0..300, &mut out)
        .unwrap();
    for (k, &c) in ids.iter().enumerate() {
        for i in 0..300u64 {
            let expected = spec.physical(c.0 as usize, i) as f32;
            assert_eq!(
                out[k * 300 + i as usize],
                expected,
                "channel {c:?}, sample {i}"
            );
        }
    }
}

#[test]
fn truncated_file_reads_its_complete_records() {
    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let record = spec.record_bytes().unwrap();
    let full = spec.file_size().unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(file.path())
        .unwrap()
        .set_len(full - record / 3)
        .unwrap();

    let rec = open(file.path());
    assert_eq!(rec.warnings().len(), 1);
    assert!(rec.warnings()[0].contains("truncated"));
    let grid = rec.grid(GridId(0));
    assert_eq!(grid.len(), 59 * 512);
    // The last complete sample reads; one past it is out of range.
    let last = grid.len() - 1;
    assert_eq!(
        read_native(&rec, GridId(0), &[ChannelId(3)], last..last + 1),
        truth(&spec, &[ChannelId(3)], last..last + 1)
    );
    let mut out = [0i32; 1];
    let err = rec
        .read_native(
            GridId(0),
            &[ChannelId(3)],
            last + 1..last + 2,
            SamplesMut::I32(&mut out),
        )
        .unwrap_err();
    assert!(matches!(err, ReadError::OutOfRange { .. }));
}

#[test]
fn reads_concurrently_from_shared_recording() {
    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let rec = Arc::new(open(file.path()));
    let spec = Arc::new(spec);
    let threads: Vec<_> = (0..4u64)
        .map(|t| {
            let (rec, spec) = (Arc::clone(&rec), Arc::clone(&spec));
            std::thread::spawn(move || {
                for k in 0..50 {
                    let start = (t * 7919 + k * 613) % 30_000;
                    let range = start..start + 700;
                    let ids = [ChannelId((t % 4) as u32), ChannelId(((t + 1) % 4) as u32)];
                    assert_eq!(
                        read_native(&rec, GridId(0), &ids, range.clone()),
                        truth(&spec, &ids, range)
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

#[test]
fn open_reports_unreadable_and_unsupported_files() {
    let missing = std::env::temp_dir().join("kymo-reader-does-not-exist.edf");
    assert!(matches!(
        kymo_core::open(&missing),
        Err(OpenError::Io { .. })
    ));

    let text = TempFile::new("txt");
    std::fs::write(text.path(), "hello, not an EEG file").unwrap();
    assert!(matches!(
        kymo_core::open(text.path()),
        Err(OpenError::Unsupported { .. })
    ));

    let empty = TempFile::new("edf");
    std::fs::write(empty.path(), "").unwrap();
    assert!(matches!(
        kymo_core::open(empty.path()),
        Err(OpenError::Unsupported { .. })
    ));

    let short = TempFile::new("edf");
    std::fs::write(short.path(), "0       too short").unwrap();
    assert!(matches!(
        kymo_core::open(short.path()),
        Err(OpenError::Edf {
            source: EdfError::Truncated { .. },
            ..
        })
    ));
}

/// A random continuous file and random reads from it.
#[derive(Debug)]
struct Case {
    spec: Spec,
    reads: Vec<(usize, Vec<usize>, (u64, u64))>,
}

fn case() -> impl Strategy<Value = Case> {
    let format = prop_oneof![
        Just(Format::Edf),
        Just(Format::EdfPlusC),
        Just(Format::Bdf),
        Just(Format::BdfPlusC)
    ];
    let spr = proptest::sample::select(vec![1u32, 3, 7, 10, 32, 50]);
    (
        format,
        proptest::collection::vec(spr, 1..6),
        proptest::sample::select(vec![0.25, 0.5, 1.0, 2.0]),
        1u64..10,
        any::<u64>(),
        proptest::collection::vec(
            (
                any::<prop::sample::Index>(),
                proptest::collection::vec(any::<prop::sample::Index>(), 1..4),
                (any::<prop::sample::Index>(), any::<prop::sample::Index>()),
            ),
            1..6,
        ),
    )
        .prop_map(|(format, sprs, record_duration, n_records, seed, picks)| {
            let channels = sprs
                .iter()
                .enumerate()
                .map(|(i, &spr)| ChannelSpec::eeg(format!("EEG {i}"), spr, format))
                .collect();
            let mut spec = Spec::new(format, channels, record_duration, n_records);
            spec.seed = seed;
            // Resolve the picks against the grids (one per distinct samples per record).
            let mut grid_sprs: Vec<u32> = Vec::new();
            for &s in &sprs {
                if !grid_sprs.contains(&s) {
                    grid_sprs.push(s);
                }
            }
            let reads = picks
                .into_iter()
                .map(|(g, chans, (a, b))| {
                    let g = g.index(grid_sprs.len());
                    let members: Vec<usize> = (0..sprs.len())
                        .filter(|&c| sprs[c] == grid_sprs[g])
                        .collect();
                    let chans = chans
                        .iter()
                        .map(|c| members[c.index(members.len())])
                        .collect();
                    let len = n_records * u64::from(grid_sprs[g]);
                    let (a, b) = (
                        a.index(len as usize + 1) as u64,
                        b.index(len as usize + 1) as u64,
                    );
                    (g, chans, (a.min(b), a.max(b)))
                })
                .collect();
            Case { spec, reads }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let file = TempFile::written(&case.spec);
        let rec = open(file.path());
        for (g, chans, (start, end)) in &case.reads {
            let grid = GridId(*g as u32);
            let ids: Vec<ChannelId> = chans.iter().map(|&c| ChannelId(c as u32)).collect();
            let range = *start..*end;
            prop_assert_eq!(
                read_native(&rec, grid, &ids, range.clone()),
                truth(&case.spec, &ids, range.clone())
            );
            let mut physical = vec![0.0f32; ids.len() * (end - start) as usize];
            rec.read_physical(grid, &ids, range.clone(), &mut physical).unwrap();
            let expected: Vec<f32> = ids
                .iter()
                .flat_map(|c| range.clone().map(|i| case.spec.physical(c.0 as usize, i) as f32))
                .collect();
            prop_assert_eq!(physical, expected);
        }
    }
}
