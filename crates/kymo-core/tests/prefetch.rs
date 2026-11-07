//! The prefetch list: the screen first, then margins and neighbouring levels, within a budget.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::{BTreeMap, HashSet};

use common::{grids, shape_recording, view, view_of};
use kymo_core::tile::{
    CHANNEL_BLOCK, ChannelBlock, Direction, Level, TileKey, View, level_for, max_level, prefetch,
    samples_in, tile_bytes, tiles_for_view,
};
use kymo_core::{GridId, Recording};
use proptest::prelude::*;

/// The tiles of `block` at `level` over `time`, worked out from samples directly.
fn tiles(
    rec: &Recording,
    block: ChannelBlock,
    level: Level,
    view: &View,
    time: std::ops::Range<f64>,
) -> Vec<TileKey> {
    let grid = rec.grid(block.grid);
    let mut samples = samples_in(grid, &time);
    if samples.is_empty() {
        return Vec::new();
    }
    // At level 0 the line runs on to the next sample.
    if level == Level::RAW {
        samples.end = (samples.end + 1).min(grid.len());
    }
    (level.tile_of(samples.start)..=level.tile_of(samples.end - 1))
        .map(|time| TileKey {
            version: view.version,
            level,
            block,
            time,
        })
        .collect()
}

/// Everything an unlimited prefetch should list, by the definition in its docs.
fn expected(rec: &Recording, view: &View, direction: Direction) -> HashSet<TileKey> {
    let screen = tiles_for_view(rec, view);
    let width = view.time.end - view.time.start;
    let wide = match direction {
        Direction::Forward => view.time.start - width / 2.0..view.time.end + width,
        Direction::Backward => view.time.start - width..view.time.end + width / 2.0,
    };
    if screen.is_empty() {
        return HashSet::new();
    }
    // Every block of the view's channels, also one whose grid has nothing on screen.
    let levels: BTreeMap<ChannelBlock, Level> = view
        .channels
        .iter()
        .map(|&id| {
            let grid_id = rec.channel(id).grid;
            let grid = rec.grid(grid_id);
            let row = grid.channels().iter().position(|&c| c == id).unwrap();
            let block = ChannelBlock {
                grid: grid_id,
                index: (row / CHANNEL_BLOCK) as u32,
            };
            (block, level_for(grid, view))
        })
        .collect();
    let mut all: HashSet<TileKey> = screen.iter().copied().collect();
    for (&block, &level) in &levels {
        let top = max_level(rec.grid(block.grid).len());
        all.extend(tiles(rec, block, level, view, wide.clone()));
        if let Some(up) = level.up().filter(|&up| up <= top) {
            all.extend(tiles(rec, block, up, view, wide.clone()));
        }
        if let Some(down) = level.down() {
            all.extend(tiles(rec, block, down, view, view.time.clone()));
        }
    }
    all
}

fn directions() -> impl Strategy<Value = Direction> {
    prop_oneof![Just(Direction::Forward), Just(Direction::Backward)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn covers_screen_margins_and_neighbouring_levels(
        (shape, view) in grids().prop_flat_map(|g| (Just(g.clone()), view_of(&g))),
        direction in directions(),
    ) {
        let rec = shape_recording(&shape);
        let list = prefetch(&rec, &view, direction, usize::MAX);
        let set: HashSet<TileKey> = list.iter().copied().collect();
        prop_assert_eq!(set.len(), list.len(), "duplicates");
        prop_assert_eq!(&set, &expected(&rec, &view, direction));

        // The screen first.
        let screen: HashSet<TileKey> = tiles_for_view(&rec, &view).into_iter().collect();
        let head: HashSet<TileKey> = list[..screen.len()].iter().copied().collect();
        prop_assert_eq!(head, screen);

        // Inside the recording.
        for key in &list {
            let grid = rec.grid(key.block.grid);
            prop_assert!(key.block.index < ChannelBlock::count(grid));
            prop_assert!(key.level <= max_level(grid.len()));
            prop_assert!(key.sample_range().start < grid.len(), "{:?}", key);
            prop_assert_eq!(key.version, view.version);
        }
    }

    #[test]
    fn a_budget_keeps_the_screen_and_cuts_the_rest(
        (shape, view) in grids().prop_flat_map(|g| (Just(g.clone()), view_of(&g))),
        direction in directions(),
        budget in prop_oneof![0usize..1 << 20, 0usize..1 << 30],
    ) {
        let rec = shape_recording(&shape);
        let full = prefetch(&rec, &view, direction, usize::MAX);
        let cut = prefetch(&rec, &view, direction, budget);
        let screen = tiles_for_view(&rec, &view).len();
        prop_assert!(cut.len() >= screen);
        prop_assert_eq!(&full[..cut.len()], &cut[..]);
        let extra: usize = cut[screen..].iter().map(|k| tile_bytes(&rec, k)).sum();
        prop_assert!(extra <= budget);
        if let Some(next) = full.get(cut.len()) {
            prop_assert!(extra + tile_bytes(&rec, next) > budget, "stopped early");
        }
    }
}

fn raw(time: u64) -> TileKey {
    TileKey {
        version: Default::default(),
        level: Level::RAW,
        block: ChannelBlock {
            grid: kymo_core::GridId(0),
            index: 0,
        },
        time,
    }
}

/// 1 channel at 1000 Hz for 10^7 samples; 100-140 s on 2000 px is level 0 (20 samples/px),
/// tiles 24 to 34 (4096 samples each).
fn example(direction: Direction) -> (Recording, Vec<TileKey>) {
    let rec = shape_recording(&[(1000.0, 0.0, 10_000_000, 1)]);
    let list = prefetch(
        &rec,
        &view(100.0..140.0, 2000, vec![0]),
        direction,
        usize::MAX,
    );
    (rec, list)
}

#[test]
fn orders_by_distance_from_the_screen() {
    let (_, list) = example(Direction::Forward);
    // Screen: from the tile holding the centre (120 s, tile 29) outwards.
    assert_eq!(list[0], raw(29));
    let screen: HashSet<TileKey> = (24..=34).map(raw).collect();
    assert_eq!(list[..11].iter().copied().collect::<HashSet<_>>(), screen);
    // Margins 80-100 s behind (tiles 19-23) and 140-180 s ahead (35-43), nearest first:
    // tile 23 ends 1.7 s before the screen, tile 35 starts 3.4 s after it.
    let margins: Vec<u64> = list[11..25].iter().map(|k| k.time).collect();
    assert_eq!(
        margins,
        [23, 35, 22, 36, 21, 37, 20, 38, 19, 39, 40, 41, 42, 43]
    );
    // Then level 1 over 80-180 s; level 0 has nothing below.
    assert_eq!(list.len(), 27);
    assert!(list[25..].iter().all(|k| k.level == Level::new(1).unwrap()));
    assert_eq!(list[25].time, 0);
    assert_eq!(list[26].time, 1);
}

#[test]
fn looks_further_back_when_moving_backward() {
    let (_, list) = example(Direction::Backward);
    // Margins 60-100 s (tiles 14-23) and 140-160 s (35-39).
    let margins: HashSet<u64> = list[11..]
        .iter()
        .filter(|k| k.level == Level::RAW)
        .map(|k| k.time)
        .collect();
    assert_eq!(margins, (14..=23).chain(35..=39).collect());
}

#[test]
fn a_budget_counts_tile_bytes() {
    let (rec, full) = example(Direction::Forward);
    let one = tile_bytes(&rec, &full[11]);
    // One int16 channel of 4096 samples, plus the tile itself.
    assert!((4096 * 2..4096 * 2 + 256).contains(&one), "{one}");
    let cut = prefetch(
        &rec,
        &view(100.0..140.0, 2000, vec![0]),
        Direction::Forward,
        3 * one,
    );
    assert_eq!(cut[..], full[..14]);
    let none = prefetch(
        &rec,
        &view(100.0..140.0, 2000, vec![0]),
        Direction::Forward,
        0,
    );
    assert_eq!(none[..], full[..11]);
}

#[test]
fn an_empty_view_needs_nothing() {
    let rec = shape_recording(&[(1000.0, 0.0, 10_000, 1)]);
    assert!(
        prefetch(
            &rec,
            &view(5.0..5.0, 2000, vec![0]),
            Direction::Forward,
            usize::MAX
        )
        .is_empty()
    );
    assert!(
        prefetch(
            &rec,
            &view(0.0..1.0, 2000, vec![]),
            Direction::Forward,
            usize::MAX
        )
        .is_empty()
    );
}

#[test]
fn ties_go_to_the_margin_ahead() {
    // The screen spans exactly tiles 24 to 34 at level 1, so tiles 23 and 35 both touch it.
    let rec = shape_recording(&[(1000.0, 0.0, 10_000_000, 1)]);
    let tile = 4096.0 * 32.0 / 1000.0;
    let shown = view(24.0 * tile..35.0 * tile, 12_000, vec![0]);
    assert_eq!(
        level_for(rec.grid(GridId(0)), &shown),
        Level::new(1).unwrap()
    );
    let forward = prefetch(&rec, &shown, Direction::Forward, usize::MAX);
    assert_eq!((forward[11].time, forward[12].time), (35, 23));
    let backward = prefetch(&rec, &shown, Direction::Backward, usize::MAX);
    assert_eq!((backward[11].time, backward[12].time), (23, 35));
}
