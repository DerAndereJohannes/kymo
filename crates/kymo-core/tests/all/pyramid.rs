//! The min/max pyramid and block sums, checked bucket by bucket against brute force.

use std::ops::Range;

use crate::common::{TempFile, is_present, len_and_segments, mem_recording, sample_types, values};
use kymo_core::pyramid::{Pyramid, PyramidBuilder};
use kymo_core::testing::{MemData, brute_minmax, opened};
use kymo_core::tile::{ChannelBlock, Level, SignalVersion, TILE_LEN, TileKey, max_level};
use kymo_core::{Channel, ChannelId, Grid, GridId, Recording, SampleType, SamplesMut, Timebase};
use kymo_testgen::Spec;
use proptest::prelude::*;

fn key(grid: GridId, level: Level, block: u32, time: u64) -> TileKey {
    TileKey {
        version: SignalVersion::RAW,
        level,
        block: ChannelBlock { grid, index: block },
        time,
    }
}

/// Sum of the native values of `channel` over the present samples of `range`, in order.
fn brute_sum(rec: &Recording, channel: ChannelId, range: Range<u64>) -> f64 {
    let grid = rec.grid(rec.channel(channel).grid);
    let n = (range.end - range.start) as usize;
    let native: Vec<f64> = match grid.sample_type() {
        SampleType::I16 => {
            let mut buf = vec![0i16; n];
            let out = SamplesMut::I16(&mut buf);
            rec.read_native(rec.channel(channel).grid, &[channel], range.clone(), out)
                .unwrap();
            buf.into_iter().map(f64::from).collect()
        }
        SampleType::I32 => {
            let mut buf = vec![0i32; n];
            let out = SamplesMut::I32(&mut buf);
            rec.read_native(rec.channel(channel).grid, &[channel], range.clone(), out)
                .unwrap();
            buf.into_iter().map(f64::from).collect()
        }
        _ => {
            let mut buf = vec![0f32; n];
            let out = SamplesMut::F32(&mut buf);
            rec.read_native(rec.channel(channel).grid, &[channel], range.clone(), out)
                .unwrap();
            buf.into_iter().map(f64::from).collect()
        }
    };
    let mut sum = 0.0;
    for (i, x) in range.zip(native) {
        if is_present(grid.segments(), i) {
            sum += x;
        }
    }
    sum
}

/// Checks every bucket of every level and every block sum of `pyramid` against brute force
/// over the recording, and that keys outside the pyramid give no tile.
fn check_pyramid(rec: &Recording, id: GridId, pyramid: &Pyramid) -> Result<(), TestCaseError> {
    let grid = rec.grid(id);
    let len = grid.len();
    let ids = grid.channels();
    prop_assert_eq!(pyramid.grid(), id);
    prop_assert_eq!(pyramid.top(), max_level(len));

    let mut level = Level::RAW;
    while let Some(up) = level.up().filter(|_| level < pyramid.top()) {
        level = up;
        let bucket = level.bucket_len();
        let buckets = len.div_ceil(bucket);
        let tiles = buckets.div_ceil(TILE_LEN);
        for block in 0..ChannelBlock::count(grid) {
            let rows = ChannelBlock {
                grid: id,
                index: block,
            }
            .channels(grid);
            for time in 0..tiles {
                let tile = pyramid.tile(key(id, level, block, time));
                prop_assert!(tile.is_some(), "level {:?}, tile {} missing", level, time);
                let tile = tile.unwrap();
                let first = time * TILE_LEN;
                let n = (buckets - first).min(TILE_LEN) as usize;
                prop_assert_eq!(tile.len(), n);
                prop_assert_eq!(tile.channels(), rows.len());
                prop_assert_eq!(tile.data().sample_type(), grid.sample_type());
                prop_assert_eq!(tile.data().len(), rows.len() * n * 2);
                let pairs = values(tile.data());
                let mut valid = Vec::new();
                for (r, &channel) in rows.iter().enumerate() {
                    for j in 0..n {
                        let k = first + j as u64;
                        let samples = k * bucket..((k + 1) * bucket).min(len);
                        let (min, max) = (pairs[2 * (r * n + j)], pairs[2 * (r * n + j) + 1]);
                        let present = samples.clone().any(|i| is_present(grid.segments(), i));
                        match brute_minmax(rec, channel, samples.clone()).unwrap() {
                            Some(want) => prop_assert_eq!(
                                (min, max),
                                want,
                                "level {:?}, channel {:?}, samples {:?}",
                                level,
                                channel,
                                samples
                            ),
                            None => prop_assert!(
                                min > max,
                                "level {:?}, channel {:?}, samples {:?}: empty bucket \
                                 holds ({}, {})",
                                level,
                                channel,
                                samples,
                                min,
                                max
                            ),
                        }
                        if r == 0 {
                            valid.push(present);
                        }
                    }
                }
                let mut flags = vec![false; n];
                for v in tile.valid() {
                    for f in &mut flags[v.start as usize..v.end as usize] {
                        *f = true;
                    }
                }
                prop_assert_eq!(flags, valid, "level {:?}, tile {}: valid", level, time);
            }
            prop_assert!(pyramid.tile(key(id, level, block, tiles)).is_none());
        }
        prop_assert!(
            pyramid
                .tile(key(id, level, ChannelBlock::count(grid), 0))
                .is_none()
        );
    }
    prop_assert!(pyramid.tile(key(id, Level::RAW, 0, 0)).is_none());
    if let Some(above) = pyramid.top().up() {
        prop_assert!(pyramid.tile(key(id, above, 0, 0)).is_none());
    }

    for block in 0..len.div_ceil(TILE_LEN) {
        let samples = block * TILE_LEN..((block + 1) * TILE_LEN).min(len);
        for &channel in ids {
            let want = brute_sum(rec, channel, samples.clone());
            prop_assert_eq!(
                pyramid.block_sum(channel, block),
                Some(want),
                "block {}",
                block
            );
        }
    }
    prop_assert_eq!(pyramid.block_sum(ids[0], len.div_ceil(TILE_LEN)), None);
    Ok(())
}

/// Few channels and long grids (several levels), or a second, partial channel block.
fn shapes() -> impl Strategy<Value = (usize, (u64, Vec<Range<u64>>))> {
    prop_oneof![
        (1..=3usize, len_and_segments(5000)),
        (1..=2usize, len_and_segments(700_000)),
        (33..=34usize, len_and_segments(150_000)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The phase-1 correctness criterion: every bucket at every level equals brute force
    /// over its samples, empty buckets are flagged, and block sums are exact, whatever the
    /// chunk size and thread count of the build.
    #[test]
    fn pyramid_matches_brute_force(
        sample_type in sample_types(),
        (n, (len, segments)) in shapes(),
        chunk in prop_oneof![1..=100u64, 1..=300_000u64],
        threads in 1..=5usize,
        seed in any::<u64>(),
    ) {
        // Chunks of a few samples on short grids; long ones in at most 1000 steps, which
        // still split buckets at every level.
        let chunk = chunk.max(len / 1000);
        let (rec, _) = mem_recording(sample_type, n, len, segments, seed);
        let mut builder = PyramidBuilder::new(&rec, GridId(0))
            .unwrap()
            .with_chunk_len(chunk)
            .with_threads(threads);
        prop_assert_eq!(builder.len(), len);
        // Count steps on small builds only.
        if len <= 5000 {
            let mut steps = 0;
            while !builder.step().unwrap() {
                steps += 1;
                prop_assert_eq!(builder.done(), (steps * chunk).min(len));
            }
            prop_assert_eq!(builder.done(), len);
        }
        let pyramid = builder.finish().unwrap();
        check_pyramid(&rec, GridId(0), &pyramid)?;
    }

    /// A single-sample spike at either extreme of the type is the max (or min) of its
    /// bucket at every level.
    #[test]
    fn spikes_survive_every_level(
        sample_type in sample_types(),
        len in 1..600_000u64,
        at in any::<proptest::sample::Index>(),
        up in any::<bool>(),
    ) {
        let i = at.index(len as usize);
        let (spike, data) = match sample_type {
            SampleType::I16 => {
                let mut row = vec![0i16; len as usize];
                row[i] = if up { i16::MAX } else { i16::MIN };
                (f64::from(row[i]), MemData::I16(vec![row]))
            }
            SampleType::I32 => {
                let mut row = vec![0i32; len as usize];
                row[i] = if up { i32::MAX } else { i32::MIN };
                (f64::from(row[i]), MemData::I32(vec![row]))
            }
            _ => {
                let mut row = vec![0f32; len as usize];
                row[i] = if up { f32::MAX } else { f32::MIN };
                (f64::from(row[i]), MemData::F32(vec![row]))
            }
        };
        let rec = Recording::new(kymo_core::testing::single_grid(1000.0, data)).unwrap().0;
        let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
        let mut level = Level::RAW;
        while let Some(up_level) = level.up().filter(|_| level < pyramid.top()) {
            level = up_level;
            let k = i as u64 / level.bucket_len();
            let tile = pyramid.tile(key(GridId(0), level, 0, k / TILE_LEN)).unwrap();
            let j = (k % TILE_LEN) as usize;
            let pair = &values(tile.data())[2 * j..2 * j + 2];
            prop_assert_eq!(pair[usize::from(up)], spike, "level {:?}", level);
        }
    }
}

#[test]
fn short_grids_have_no_levels_but_sums() {
    let (rec, _) = mem_recording(SampleType::I16, 2, TILE_LEN, vec![0..10], 3);
    let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
    assert_eq!(pyramid.top(), Level::RAW);
    assert!(
        pyramid
            .tile(key(GridId(0), Level::new(1).unwrap(), 0, 0))
            .is_none()
    );
    check_pyramid(&rec, GridId(0), &pyramid).unwrap();
}

#[test]
fn unknown_grid_is_an_error() {
    let (rec, _) = mem_recording(SampleType::I16, 1, 10, vec![0..10], 0);
    assert!(Pyramid::build(&rec, GridId(1)).is_err());
}

/// Two grids of different types: each pyramid answers for its own grid only.
#[test]
fn pyramids_of_two_grids() {
    let channels = vec![Channel::new("a", GridId(0)), Channel::new("b", GridId(1))];
    let timebase = |rate| Timebase::Regular { rate, start: 0.0 };
    let len = 200_000;
    let grids = vec![
        Grid::continuous(timebase(1000.0), len, SampleType::I16, vec![ChannelId(0)]).unwrap(),
        Grid::new(
            timebase(250.0),
            len / 4,
            vec![100..20_000, 30_000..len / 4],
            SampleType::F32,
            vec![ChannelId(1)],
        )
        .unwrap(),
    ];
    let data = vec![
        MemData::I16(vec![(0..len).map(|i| (i % 1000) as i16 - 500).collect()]),
        MemData::F32(vec![(0..len / 4).map(|i| (i as f32).sin()).collect()]),
    ];
    let rec = Recording::new(opened(channels, grids, data)).unwrap().0;
    for g in [GridId(0), GridId(1)] {
        let pyramid = Pyramid::build(&rec, g).unwrap();
        check_pyramid(&rec, g, &pyramid).unwrap();
        let other = GridId(1 - g.0);
        assert!(
            pyramid
                .tile(key(other, Level::new(1).unwrap(), 0, 0))
                .is_none()
        );
        assert_eq!(pyramid.block_sum(ChannelId(other.0), 0), None);
    }
}

/// Through the real readers: generated EDF+D (mixed rates, gaps, huge offset, negative
/// gain), BDF+C and BrainVision files (both layouts; int16, int32 and float32).
#[test]
fn pyramids_of_generated_files() {
    for preset in [
        "edf+d-mixed",
        "bdf+c-small",
        "bv-int16-multiplexed",
        "bv-int32-vectorized",
        "bv-float32-multiplexed",
    ] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let rec = Recording::new(kymo_core::open(file.path()).unwrap())
            .unwrap()
            .0;
        for g in 0..rec.grids().len() {
            let id = GridId(g as u32);
            let pyramid = Pyramid::build(&rec, id).unwrap();
            assert!(pyramid.top() >= Level::new(1).unwrap(), "{preset}: levels");
            check_pyramid(&rec, id, &pyramid).unwrap();
        }
    }
}
