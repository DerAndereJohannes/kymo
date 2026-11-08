use std::collections::BTreeSet;

use kymo_core::GridId;
use kymo_core::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, SignalVersion, TILE_LEN, TileKey, choose_level,
    drawn_samples, level_for, max_level, samples_in, tiles_for_view,
};
use proptest::prelude::*;

use crate::common::{grids, shape_recording as recording, view, view_of};

fn level(l: u8) -> Level {
    Level::new(l).unwrap()
}

#[test]
fn level_sizes() {
    let buckets: Vec<u64> = (0..5).map(|l| level(l).bucket_len()).collect();
    assert_eq!(buckets, [1, 32, 128, 512, 2048]);
    assert_eq!(level(0).tile_samples(), TILE_LEN);
    assert_eq!(level(1).tile_samples(), 131_072);
    assert_eq!(Level::MAX.tile_samples(), 1 << 63);
    assert_eq!(Level::new(Level::MAX.get() + 1), None);
    assert_eq!(Level::MAX.up(), None);
    assert_eq!(Level::RAW.down(), None);
    assert_eq!(level(3).tile_of(3 * level(3).tile_samples() + 5), 3);
}

#[test]
fn choose_level_examples() {
    let cases = [
        (0.01, 0),
        (1.0, 0),
        (31.9, 0),
        (32.0, 1),
        (127.9, 1),
        (128.0, 2),
        (600.0, 3),
        (f64::NAN, 0),
        (-5.0, 0),
    ];
    for (spp, expected) in cases {
        assert_eq!(
            choose_level(spp),
            level(expected),
            "{spp} samples per pixel"
        );
    }
    assert_eq!(choose_level(f64::INFINITY), Level::MAX);
}

#[test]
fn max_level_examples() {
    let cases = [(0, 0), (4096, 0), (4097, 1), (131_072, 1), (131_073, 2)];
    for (len, expected) in cases {
        assert_eq!(max_level(len), level(expected), "{len} samples");
    }
    // 8 h at 2048 Hz.
    let len = 8 * 3600 * 2048;
    let top = max_level(len);
    assert!(top.tile_samples() >= len && top.down().unwrap().tile_samples() < len);
}

#[test]
fn channel_blocks_split_grids_into_32s() {
    let rec = recording(&[(256.0, 0.0, 1000, 70)]);
    let grid = rec.grid(GridId(0));
    assert_eq!(ChannelBlock::count(grid), 3);
    let last = ChannelBlock {
        grid: GridId(0),
        index: 2,
    };
    assert_eq!(last.channels(grid), &grid.channels()[64..70]);
}

#[test]
fn view_picks_blocks_of_visible_channels_per_grid() {
    // Grid 0: channels 0..70 at 256 Hz. Grid 1: channels 70..73 at 64 Hz.
    let rec = recording(&[(256.0, 0.0, 256 * 600, 70), (64.0, 0.0, 64 * 600, 3)]);
    let tiles = tiles_for_view(&rec, &view(0.0..20.0, 1920, vec![40, 71, 3, 45]));
    let blocks: BTreeSet<(u32, u32)> = tiles
        .iter()
        .map(|t| (t.block.grid.0, t.block.index))
        .collect();
    assert_eq!(blocks, BTreeSet::from([(0, 0), (0, 1), (1, 0)]));
    // 20 s at 256 Hz on 1920 px is 2.7 samples per pixel: raw tiles of 4096 samples.
    let first: Vec<&TileKey> = tiles.iter().filter(|t| t.block.grid == GridId(0)).collect();
    assert!(first.iter().all(|t| t.level == Level::RAW));
    assert_eq!(first.iter().filter(|t| t.block.index == 0).count(), 2);
}

#[test]
fn whole_recording_uses_few_coarse_tiles() {
    // 8 h at 2048 Hz, 256 channels, on 1920 px.
    let len = 8 * 3600 * 2048;
    let rec = recording(&[(2048.0, 0.0, len, 256)]);
    let all: Vec<u32> = (0..256).collect();
    let v = view(0.0..8.0 * 3600.0, 1920, all);
    let tiles = tiles_for_view(&rec, &v);
    let level = level_for(rec.grid(GridId(0)), &v);
    // 30,720 samples per pixel: level 5 has buckets of 8,192 samples, level 6 of 32,768.
    assert_eq!(level, Level::new(5).unwrap());
    let spp = len as f64 / 1920.0;
    assert!(level.bucket_len() as f64 <= spp);
    assert!(level.up().unwrap().bucket_len() as f64 > spp);
    // 8 channel blocks of 2 tiles (4096 buckets of 8,192 samples each).
    assert_eq!(tiles.len(), 8 * 2);
}

#[test]
fn level_0_views_need_the_sample_after_the_last_visible_one() {
    // 100 Hz: the view ends inside the last sample of tile 0 (sample 4095).
    let rec = recording(&[(100.0, 0.0, 100_000, 1)]);
    let end = 40.955;
    let tiles = tiles_for_view(&rec, &view(30.0..end, 800, vec![0]));
    assert_eq!(tiles.iter().map(|t| t.time).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(
        drawn_samples(rec.grid(GridId(0)), Level::RAW, &(30.0..end)),
        3000..4097
    );
    // Not past the grid's end.
    let grid = rec.grid(GridId(0));
    assert_eq!(
        drawn_samples(grid, Level::RAW, &(999.0..1000.0)),
        99_900..100_000
    );
    assert_eq!(
        drawn_samples(grid, Level::RAW, &(2000.0..3000.0)),
        100_000..100_000
    );
    // Buckets reach to the next one by themselves.
    assert_eq!(drawn_samples(grid, level(1), &(30.0..end)), 3000..4096);
}

#[test]
fn empty_views_need_no_tiles() {
    let rec = recording(&[(100.0, 0.0, 10_000, 4)]);
    let channels = vec![0, 1];
    for v in [
        view(5.0..5.0, 800, channels.clone()),
        view(6.0..5.0, 800, channels.clone()),
        view(f64::NAN..5.0, 800, channels.clone()),
        view(0.0..5.0, 0, channels.clone()),
        view(0.0..5.0, 800, vec![]),
        view(200.0..300.0, 800, channels.clone()),
        view(-300.0..-200.0, 800, channels.clone()),
        view(0.0..5.0, 800, vec![99]),
    ] {
        assert_eq!(tiles_for_view(&rec, &v), vec![], "{v:?}");
    }
}

#[test]
fn samples_in_includes_partly_visible_samples() {
    let rec = recording(&[(100.0, 0.5, 1000, 1)]);
    let grid = rec.grid(GridId(0));
    // Sample i spans [0.5 + i/100, 0.5 + (i+1)/100).
    assert_eq!(samples_in(grid, &(0.505..0.52)), 0..2);
    assert_eq!(samples_in(grid, &(0.51..0.52)), 1..2);
    assert_eq!(samples_in(grid, &(0.51..0.5201)), 1..3);
    assert_eq!(samples_in(grid, &(-10.0..0.5)), 0..0);
    assert_eq!(samples_in(grid, &(0.0..100.0)), 0..1000);
}

// ---------- properties ----------

fn spp() -> impl Strategy<Value = f64> {
    (0.0f64..40.0).prop_map(|e| 2f64.powf(e))
}

proptest! {
    #[test]
    fn chosen_level_is_the_coarsest_within_a_pixel(spp in spp()) {
        let level = choose_level(spp);
        if level != Level::RAW {
            prop_assert!(level.bucket_len() as f64 <= spp);
        }
        if let Some(up) = level.up() {
            prop_assert!(up.bucket_len() as f64 > spp);
        }
    }

    #[test]
    fn max_level_is_the_lowest_single_tile(len in 0u64..(1 << 62)) {
        let top = max_level(len);
        prop_assert!(top.tile_samples() >= len);
        if let Some(down) = top.down() {
            prop_assert!(down.tile_samples() < len);
        }
    }

    #[test]
    fn parents_and_children_nest(l in 0u8..Level::MAX.get(), time in 0u64..1_000_000) {
        // Keep the parent's samples within u64.
        let parent_tiles = (1u64 << 63) / level(l + 1).tile_samples();
        let ratio = level(l + 1).tile_samples() / level(l).tile_samples();
        let time = time % (parent_tiles * ratio);
        let key = TileKey {
            version: SignalVersion(3),
            level: level(l),
            block: ChannelBlock { grid: GridId(1), index: 2 },
            time,
        };
        let parent = key.parent().unwrap();
        let (outer, inner) = (parent.sample_range(), key.sample_range());
        prop_assert!(outer.start <= inner.start && inner.end <= outer.end);
        prop_assert_eq!((parent.version, parent.block), (key.version, key.block));

        let children = parent.children();
        prop_assert_eq!(children.len(), if l == 0 { 32 } else { 4 });
        prop_assert!(children.contains(&key));
        // The children tile the parent exactly, in order.
        prop_assert_eq!(children[0].sample_range().start, outer.start);
        prop_assert_eq!(children.last().unwrap().sample_range().end, outer.end);
        for pair in children.windows(2) {
            prop_assert_eq!(pair[0].sample_range().end, pair[1].sample_range().start);
        }
        for child in &children {
            prop_assert_eq!(child.parent(), Some(parent));
        }
    }

    #[test]
    fn tiles_cover_the_view_exactly(
        (g, v) in grids().prop_flat_map(|g| {
            let views = view_of(&g);
            (Just(g), views)
        }),
    ) {
        let rec = recording(&g);
        let tiles = tiles_for_view(&rec, &v);

        // Exactly the blocks of the visible channels that have visible samples.
        let mut expected_blocks = BTreeSet::new();
        for &id in &v.channels {
            let g = rec.channel(id).grid;
            let grid = rec.grid(g);
            if samples_in(grid, &v.time).is_empty() {
                continue;
            }
            let position = grid.channels().iter().position(|&c| c == id).unwrap();
            expected_blocks.insert(ChannelBlock { grid: g, index: (position / CHANNEL_BLOCK) as u32 });
        }
        let blocks: BTreeSet<ChannelBlock> = tiles.iter().map(|t| t.block).collect();
        prop_assert_eq!(&blocks, &expected_blocks);

        for block in blocks {
            let grid = rec.grid(block.grid);
            let level = level_for(grid, &v);
            // The visible samples and, at level 0, the one after them.
            let mut samples = samples_in(grid, &v.time);
            if level == Level::RAW {
                samples.end = (samples.end + 1).min(grid.len());
            }
            let mine: Vec<&TileKey> = tiles.iter().filter(|t| t.block == block).collect();
            prop_assert!(mine.iter().all(|t| t.level == level && t.version == v.version));
            // Contiguous, in order, starting at the first sample drawn and ending at the last.
            for pair in mine.windows(2) {
                prop_assert_eq!(pair[0].time + 1, pair[1].time);
            }
            let (first, last) = (mine[0].sample_range(), mine[mine.len() - 1].sample_range());
            prop_assert!(first.contains(&samples.start));
            prop_assert!(last.contains(&(samples.end - 1)));
            prop_assert!(first.start < grid.len());
        }
    }

    #[test]
    fn tile_of_a_sample_does_not_depend_on_the_view_start(
        g in grids(),
        t0 in 0.0f64..1000.0,
        shift in -500.0f64..500.0,
        length in 0.01f64..5000.0,
        width in 100u32..4000,
    ) {
        let rec = recording(&g);
        let all: Vec<u32> = (0..rec.channels().len() as u32).collect();
        let a = view(t0..t0 + length, width, all.clone());
        let b = view(t0 + shift..t0 + shift + length, width, all);
        let (ta, tb) = (tiles_for_view(&rec, &a), tiles_for_view(&rec, &b));
        for (gi, grid) in rec.grids().iter().enumerate() {
            let (sa, sb) = (samples_in(grid, &a.time), samples_in(grid, &b.time));
            let both = sa.start.max(sb.start)..sa.end.min(sb.end);
            if both.is_empty() {
                continue;
            }
            // Check the overlap's ends and middle.
            for s in [both.start, both.start + (both.end - both.start) / 2, both.end.saturating_sub(1)] {
                if !both.contains(&s) {
                    continue;
                }
                let find = |tiles: &[TileKey]| {
                    tiles
                        .iter()
                        .find(|t| t.block.grid == GridId(gi as u32) && t.block.index == 0
                            && t.sample_range().contains(&s))
                        .copied()
                };
                prop_assert_eq!(find(&ta), find(&tb), "sample {} of grid {}", s, gi);
            }
        }
    }
}
