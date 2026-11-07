//! Window means for DC removal, against brute force over the present samples.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::ops::Range;
use std::sync::Arc;

use common::{TempFile, is_present, len_and_segments, mem_opened, sample_types};
use kymo_core::pyramid::{MemStorage, Pyramid, PyramidBuilder, PyramidStorage};
use kymo_core::stim::Change;
use kymo_core::testing::{MemData, single_grid};
use kymo_core::tile::{Level, TILE_LEN, TileData};
use kymo_core::{ChannelId, GridId, ReadError, Recording};
use kymo_testgen::Spec;
use proptest::prelude::*;

/// Storage that loses the sums of odd blocks, as a partly built sidecar would.
struct OddSumsLost(MemStorage);

impl PyramidStorage for OddSumsLost {
    fn put_tile(&self, level: Level, time: u64, data: TileData) {
        self.0.put_tile(level, time, data);
    }

    fn put_sums(&self, block: u64, sums: &[f64]) {
        if block.is_multiple_of(2) {
            self.0.put_sums(block, sums);
        }
    }

    fn publish(&self, done: u64) {
        self.0.publish(done);
    }

    fn done(&self) -> u64 {
        self.0.done()
    }

    fn tile_rows(
        &self,
        level: Level,
        time: u64,
        rows: Range<usize>,
        len: usize,
    ) -> Option<TileData> {
        self.0.tile_rows(level, time, rows, len)
    }

    fn sum(&self, block: u64, row: usize) -> Option<f64> {
        self.0.sum(block, row)
    }

    fn put_changes(&self, row: usize, changes: &[Change]) {
        self.0.put_changes(row, changes);
    }

    fn changes(&self, row: usize) -> Vec<Change> {
        self.0.changes(row)
    }
}

/// Brute-force physical mean over the present samples, and a scale for the tolerance.
fn brute_mean(
    physical: impl Fn(u64) -> f64,
    segments: &[Range<u64>],
    range: Range<u64>,
) -> (Option<f64>, f64) {
    let (mut sum, mut n, mut scale) = (0.0, 0u64, 0.0f64);
    for i in range.filter(|&i| is_present(segments, i)) {
        let x = physical(i);
        sum += x;
        n += 1;
        scale = scale.max(x.abs());
    }
    ((n > 0).then(|| sum / n as f64), scale)
}

fn assert_close(got: Option<f64>, want: Option<f64>, scale: f64) -> Result<(), TestCaseError> {
    match (got, want) {
        (Some(a), Some(b)) => prop_assert!(
            (a - b).abs() <= 1e-10 * scale.max(f64::MIN_POSITIVE),
            "mean {} instead of {} (scale {})",
            a,
            b,
            scale
        ),
        _ => prop_assert_eq!(got, want),
    }
    Ok(())
}

/// A window end in `0..=len`: anywhere, or on or next to a block boundary.
fn endpoint(len: u64) -> impl Strategy<Value = u64> {
    (0..=len, 0u8..4).prop_map(move |(x, snap)| {
        let block = x / TILE_LEN * TILE_LEN;
        match snap {
            0 => x,
            1 => block,
            2 => (block + 1).min(len),
            _ => block.saturating_sub(1),
        }
    })
}

fn window(len: u64) -> impl Strategy<Value = Range<u64>> {
    (endpoint(len), endpoint(len)).prop_map(|(a, b)| a.min(b)..a.max(b))
}

fn gains() -> impl Strategy<Value = f64> {
    prop_oneof![Just(1.0), 1e-8..1.0f64, -1e-2..-1e-6f64, -10.0..-0.5f64,]
}

fn offsets() -> impl Strategy<Value = f64> {
    prop_oneof![Just(0.0), -1e6..1e6f64, Just(1e6), Just(-2.5e7),]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn window_mean_matches_brute_force(
        sample_type in sample_types(),
        n in 1..=2usize,
        ((len, segments), windows) in len_and_segments(40_000).prop_flat_map(|(len, s)| {
            (Just((len, s)), proptest::collection::vec(window(len), 1..8))
        }),
        gain in gains(),
        offset in offsets(),
        chunk in 1..=20_000u64,
        threads in 1..=3usize,
        lose_sums in any::<bool>(),
        seed in any::<u64>(),
    ) {
        let (mut opened, data) = mem_opened(sample_type, n, len, segments.clone(), seed);
        for c in &mut opened.header.channels {
            c.gain = gain;
            c.offset = offset;
        }
        let rec = Recording::new(opened).unwrap().0;
        let mut builder = PyramidBuilder::new(&rec, GridId(0))
            .unwrap()
            .with_chunk_len(chunk)
            .with_threads(threads);
        if lose_sums {
            builder = builder.with_storage(Arc::new(OddSumsLost(MemStorage::new())));
        }
        let pyramid = builder.finish().unwrap();
        for range in windows {
            for c in 0..n {
                let physical = |i: u64| data.get(c, i as usize) * gain + offset;
                let (want, scale) = brute_mean(physical, &segments, range.clone());
                let got = pyramid
                    .window_mean(&rec, ChannelId(c as u32), range.clone())
                    .unwrap();
                assert_close(got, want, scale)?;
            }
        }
    }
}

#[test]
fn huge_offset_keeps_precision() {
    // 100 · 1e-3 + 1e6: a float32 pipeline would lose the 0.1 entirely.
    let len = 3 * TILE_LEN + 17;
    let mut opened = single_grid(256.0, MemData::I16(vec![vec![100; len as usize]]));
    opened.header.channels[0].gain = 1e-3;
    opened.header.channels[0].offset = 1e6;
    let rec = Recording::new(opened).unwrap().0;
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    let mean = pyramid
        .window_mean(&rec, ChannelId(0), 5..len)
        .unwrap()
        .unwrap();
    assert!((mean - 1_000_000.1).abs() < 1e-6, "{mean}");
}

#[test]
fn empty_and_gap_windows_have_no_mean() {
    let (opened, _) = mem_opened(kymo_core::SampleType::I32, 1, 10_000, vec![0..100], 1);
    let rec = Recording::new(opened).unwrap().0;
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    let mean = |r| pyramid.window_mean(&rec, ChannelId(0), r).unwrap();
    assert_eq!(mean(50..50), None);
    assert_eq!(mean(100..10_000), None);
    assert!(mean(99..10_000).is_some());
}

#[test]
fn bad_requests_are_errors() {
    let (opened, _) = mem_opened(kymo_core::SampleType::I16, 2, 100, vec![0..100], 1);
    let rec = Recording::new(opened).unwrap().0;
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    assert!(matches!(
        pyramid.window_mean(&rec, ChannelId(2), 0..10),
        Err(ReadError::ChannelNotInGrid { .. })
    ));
    assert!(matches!(
        pyramid.window_mean(&rec, ChannelId(0), 0..101),
        Err(ReadError::OutOfRange { .. })
    ));
}

/// Through the real reader: the EDF+D preset has gaps, a huge offset and a negative gain.
#[test]
fn window_means_of_a_generated_edf_plus_d_file() {
    let spec = Spec::preset("edf+d-mixed").unwrap();
    let file = TempFile::written(&spec);
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    for (g, grid) in rec.grids().iter().enumerate() {
        let pyramid = Pyramid::build(&rec, GridId(g as u32)).unwrap();
        let len = grid.len();
        let windows = [
            0..len,
            len / 3..2 * len / 3,
            5..TILE_LEN + 5,
            TILE_LEN..4 * TILE_LEN,
            len - 1000..len,
        ];
        for &id in grid.channels() {
            let c = id.0 as usize;
            for w in &windows {
                let (want, scale) =
                    brute_mean(|i| spec.physical(c, i), &spec.segments(c), w.clone());
                let got = pyramid.window_mean(&rec, id, w.clone()).unwrap();
                assert_close(got, want, scale).unwrap();
            }
        }
    }
}
