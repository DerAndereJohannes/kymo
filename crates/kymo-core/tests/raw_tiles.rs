//! Level-0 tiles and the brute-force min/max reference.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::ops::Range;

use common::{
    TempFile, is_present, len_and_segments, mem_recording as recording, sample_types, sub_range,
    values,
};
use kymo_core::testing::{MemData, brute_minmax};
use kymo_core::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, SignalVersion, TILE_LEN, Tile, TileKey, compute_raw,
};
use kymo_core::{ChannelId, GridId, Recording, SampleType, TileError};
use kymo_testgen::Spec;
use proptest::prelude::*;

/// Runs of present samples in `start..start + len`, relative to `start`.
fn expected_valid(segments: &[Range<u64>], start: u64, len: u64) -> Vec<Range<u32>> {
    let mut runs: Vec<Range<u32>> = Vec::new();
    for j in 0..len {
        if !is_present(segments, start + j) {
            continue;
        }
        let j = j as u32;
        match runs.last_mut() {
            Some(r) if r.end == j => r.end += 1,
            _ => runs.push(j..j + 1),
        }
    }
    runs
}

fn raw_key(grid: u32, block: u32, time: u64) -> TileKey {
    TileKey {
        version: SignalVersion::RAW,
        level: Level::RAW,
        block: ChannelBlock {
            grid: GridId(grid),
            index: block,
        },
        time,
    }
}

/// Checks one tile against the truth `sample(c, i)` for row `c` of the block.
fn check_tile(
    tile: &Tile,
    key: TileKey,
    sample_type: SampleType,
    n_channels: usize,
    grid_len: u64,
    segments: &[Range<u64>],
    sample: impl Fn(usize, u64) -> f64,
) -> Result<(), TestCaseError> {
    let start = key.time * TILE_LEN;
    let len = TILE_LEN.min(grid_len - start);
    let first = key.block.index as usize * CHANNEL_BLOCK;
    let channels = CHANNEL_BLOCK.min(n_channels - first);
    prop_assert_eq!(tile.key(), key);
    prop_assert_eq!(tile.channels(), channels);
    prop_assert_eq!(tile.len() as u64, len);
    prop_assert_eq!(tile.data().sample_type(), sample_type);
    prop_assert_eq!(tile.data().len() as u64, channels as u64 * len);
    let valid = expected_valid(segments, start, len);
    prop_assert_eq!(tile.valid(), valid.as_slice());
    prop_assert!(tile.bytes() >= tile.data().len() * sample_type.size());

    let got = values(tile.data());
    for k in 0..channels {
        for j in 0..len {
            let i = start + j;
            let want = if is_present(segments, i) {
                sample(first + k, i)
            } else {
                0.0
            };
            let x = got[k * len as usize + j as usize];
            prop_assert_eq!(x, want, "row {}, sample {}", k, i);
        }
    }
    Ok(())
}

/// Few channels, or enough for a second, partial channel block.
fn channel_counts() -> impl Strategy<Value = usize> {
    prop_oneof![1..=4usize, 31..=70usize]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Every tile of a grid holds the right samples, zero in gaps, and marks exactly the
    /// present samples as valid; the tile just past the end and the block just past the
    /// last are errors.
    #[test]
    fn raw_tiles_match_the_data(
        sample_type in sample_types(),
        n in channel_counts(),
        (len, segments) in len_and_segments(3 * TILE_LEN + 100),
        seed in any::<u64>(),
    ) {
        let (rec, data) = recording(sample_type, n, len, segments.clone(), seed);
        let grid = rec.grid(GridId(0));
        let blocks = ChannelBlock::count(grid);
        let tiles = len.div_ceil(TILE_LEN);
        for block in 0..blocks {
            for time in 0..tiles {
                let key = raw_key(0, block, time);
                let tile = compute_raw(&rec, key).unwrap();
                check_tile(&tile, key, sample_type, n, len, &segments, |c, i| {
                    data.get(c, i as usize)
                })?;
            }
            let past = compute_raw(&rec, raw_key(0, block, tiles));
            let past_end = matches!(past, Err(TileError::PastEnd { len: l, .. }) if l == len);
            prop_assert!(past_end, "tile past the end: {:?}", past);
        }
        let beyond = compute_raw(&rec, raw_key(0, blocks, 0));
        prop_assert!(matches!(beyond, Err(TileError::UnknownBlock(_))));
    }

    /// The reference agrees with a fold over the data's present samples.
    #[test]
    fn brute_minmax_is_min_and_max_of_present_samples(
        sample_type in sample_types(),
        n in 1..=3usize,
        ((len, segments), range) in len_and_segments(2000)
            .prop_flat_map(|(len, s)| (Just((len, s)), sub_range(len))),
        channel in 0..3usize,
        seed in any::<u64>(),
    ) {
        let channel = channel % n;
        let (rec, data) = recording(sample_type, n, len, segments.clone(), seed);
        let present: Vec<f64> = range
            .clone()
            .filter(|&i| is_present(&segments, i))
            .map(|i| data.get(channel, i as usize))
            .collect();
        let want = (!present.is_empty()).then(|| {
            let lo = present.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = present.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (lo, hi)
        });
        prop_assert_eq!(brute_minmax(&rec, ChannelId(channel as u32), range).unwrap(), want);
    }
}

#[test]
fn brute_minmax_skips_nan() {
    let data = MemData::F32(vec![vec![f32::NAN, 2.0, -1.0, f32::NAN]]);
    let rec = Recording::new(kymo_core::testing::single_grid(1.0, data))
        .unwrap()
        .0;
    assert_eq!(
        brute_minmax(&rec, ChannelId(0), 0..4).unwrap(),
        Some((-1.0, 2.0))
    );
    assert_eq!(brute_minmax(&rec, ChannelId(0), 3..4).unwrap(), None);
    assert!(brute_minmax(&rec, ChannelId(0), 3..5).is_err());
}

#[test]
fn exact_tile_lengths() {
    // A grid of exactly one tile, then one sample more: the second tile has one sample.
    for (len, tiles) in [(TILE_LEN, 1), (TILE_LEN + 1, 2)] {
        let (rec, _) = recording(SampleType::I16, 1, len, vec![0..len], 7);
        let last = compute_raw(&rec, raw_key(0, 0, tiles - 1)).unwrap();
        assert_eq!(last.len() as u64, len - (tiles - 1) * TILE_LEN);
        assert_eq!(last.valid(), [0..last.len() as u32].as_slice());
        assert!(compute_raw(&rec, raw_key(0, 0, tiles)).is_err());
    }
}

#[test]
fn tile_in_a_gap_has_no_valid_samples() {
    let len = 3 * TILE_LEN;
    let segments = vec![0..10, 2 * TILE_LEN + 5..len];
    let (rec, _) = recording(SampleType::I32, 2, len, segments, 1);
    let tile = compute_raw(&rec, raw_key(0, 0, 1)).unwrap();
    assert!(tile.valid().is_empty());
    assert!(values(tile.data()).iter().all(|&x| x == 0.0));
    let tile = compute_raw(&rec, raw_key(0, 0, 2)).unwrap();
    assert_eq!(tile.valid(), [5..TILE_LEN as u32].as_slice());
}

#[test]
fn only_raw_level_0_tiles_are_decoded() {
    let (rec, _) = recording(SampleType::I16, 1, 100, vec![0..100], 0);
    let mut key = raw_key(0, 0, 0);
    key.level = Level::new(1).unwrap();
    assert!(matches!(compute_raw(&rec, key), Err(TileError::NotRaw(_))));
    let mut key = raw_key(0, 0, 0);
    key.version = SignalVersion(1);
    assert!(matches!(
        compute_raw(&rec, key),
        Err(TileError::UnknownVersion(_))
    ));
    assert!(matches!(
        compute_raw(&rec, raw_key(1, 0, 0)),
        Err(TileError::UnknownBlock(_))
    ));
}

/// Through the real reader: every level-0 tile of a generated EDF+D file with mixed rates
/// and gaps matches the generator's truth.
#[test]
fn raw_tiles_of_a_generated_edf_plus_d_file() {
    let spec = Spec::preset("edf+d-mixed").unwrap();
    let file = TempFile::written(&spec);
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    for (g, grid) in rec.grids().iter().enumerate() {
        let ids = grid.channels();
        let first = ids[0].0 as usize;
        let segments = spec.segments(first);
        assert!(segments.len() > 1, "the preset has gaps");
        for time in 0..grid.len().div_ceil(TILE_LEN) {
            let key = raw_key(g as u32, 0, time);
            let tile = compute_raw(&rec, key).unwrap();
            // Rows of the block are this grid's channels, in grid order.
            check_tile(
                &tile,
                key,
                grid.sample_type(),
                ids.len(),
                grid.len(),
                &segments,
                |k, i| f64::from(spec.digital(ids[k].0 as usize, i)),
            )
            .unwrap();
        }
    }
}
