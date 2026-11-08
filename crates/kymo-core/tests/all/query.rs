//! The fixed-grid query: exact tiles, coarser fallbacks and loading regions.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use crate::common::{grids, shape_recording, values, view, view_of};
use kymo_core::pyramid::Pyramid;
use kymo_core::testing::{MemData, single_grid, tile};
use kymo_core::tile::{
    ChannelBlock, Level, Loading, Query, SignalVersion, TILE_LEN, Tile, TileCache, TileData,
    TileKey, max_level, query, tiles_for_view,
};
use kymo_core::{GridId, Recording};
use proptest::prelude::*;

/// A placeholder tile for `key`: the query never looks inside tiles.
fn dummy(key: TileKey) -> Arc<Tile> {
    let n = if key.level == Level::RAW { 1 } else { 2 };
    Arc::new(tile(key, 1, 1, TileData::I16(vec![0; n].into()), vec![]))
}

fn big_cache() -> TileCache {
    TileCache::new(1 << 30)
}

fn clipped(rec: &Recording, key: TileKey) -> Range<u64> {
    let span = key.sample_range();
    span.start..span.end.min(rec.grid(key.block.grid).len())
}

/// The nearest cached tile for `key` (itself or an ancestor up to the grid's top).
fn expected(rec: &Recording, cached: &HashSet<TileKey>, key: TileKey) -> Option<TileKey> {
    let top = max_level(rec.grid(key.block.grid).len());
    let mut k = Some(key);
    while let Some(candidate) = k.filter(|k| k.level <= top) {
        if cached.contains(&candidate) {
            return Some(candidate);
        }
        k = candidate.parent();
    }
    None
}

/// Checks a query against brute force over the wanted tiles and the cached keys.
fn check(
    rec: &Recording,
    wanted: &[TileKey],
    cached: &HashSet<TileKey>,
    q: &Query,
) -> Result<(), TestCaseError> {
    let missing: Vec<TileKey> = wanted
        .iter()
        .copied()
        .filter(|k| !cached.contains(k))
        .collect();
    prop_assert_eq!(&q.missing, &missing);

    // Every drawn range lies inside its tile's span and is drawn from a cached tile.
    for p in &q.draw {
        let span = p.tile.key().sample_range();
        prop_assert!(span.start <= p.samples.start && p.samples.end <= span.end);
        prop_assert!(p.samples.start < p.samples.end);
        prop_assert!(cached.contains(&p.tile.key()));
    }

    // Each wanted tile is covered by exactly the expected tile, or is loading.
    let mut wanted_len: BTreeMap<ChannelBlock, u64> = BTreeMap::new();
    for &key in wanted {
        let samples = clipped(rec, key);
        *wanted_len.entry(key.block).or_default() += samples.end - samples.start;
        let covering: Vec<_> = q
            .draw
            .iter()
            .filter(|p| {
                p.tile.key().block == key.block
                    && p.samples.start <= samples.start
                    && samples.end <= p.samples.end
            })
            .collect();
        let loading = q.loading.iter().any(|l| {
            l.block == key.block && l.samples.start <= samples.start && samples.end <= l.samples.end
        });
        match expected(rec, cached, key) {
            Some(want) => {
                prop_assert_eq!(covering.len(), 1, "{:?}", key);
                prop_assert_eq!(covering[0].tile.key(), want);
                prop_assert_eq!(covering[0].exact, want == key);
                prop_assert!(!loading);
            }
            None => {
                prop_assert!(covering.is_empty(), "{:?}", key);
                prop_assert!(loading, "{:?} neither drawn nor loading", key);
            }
        }
    }

    // Drawn and loading ranges add up to the wanted samples: together with full coverage
    // above, that makes them a partition.
    let mut covered: BTreeMap<ChannelBlock, u64> = BTreeMap::new();
    for p in &q.draw {
        *covered.entry(p.tile.key().block).or_default() += p.samples.end - p.samples.start;
    }
    for l in &q.loading {
        *covered.entry(l.block).or_default() += l.samples.end - l.samples.start;
    }
    prop_assert_eq!(covered, wanted_len);
    Ok(())
}

/// Cheap deterministic coin per key.
fn coin(seed: u64, key: &TileKey, threshold: u8) -> bool {
    let mut z = seed
        ^ key.time.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(key.level.get()) << 56
        ^ u64::from(key.block.index) << 40
        ^ u64::from(key.block.grid.0) << 32;
    z = (z ^ (z >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 29;
    (z % 4) < u64::from(threshold)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever the cache holds, every wanted tile is drawn from itself or its nearest cached
    /// ancestor, or is loading, and nothing is drawn twice.
    #[test]
    fn query_draws_nearest_cached_or_loading(
        (g, v) in grids().prop_flat_map(|g| {
            let views = view_of(&g);
            (Just(g), views)
        }),
        seed in any::<u64>(),
        threshold in 0u8..=4,
    ) {
        let rec = shape_recording(&g);
        let wanted = tiles_for_view(&rec, &v);
        let cache = big_cache();
        let mut cached = HashSet::new();
        for &key in &wanted {
            let mut k = Some(key);
            while let Some(candidate) = k {
                if coin(seed, &candidate, threshold) && cached.insert(candidate) {
                    cache.insert(dummy(candidate));
                }
                k = candidate.parent();
            }
        }
        let q = query(&rec, &v, &cache);
        check(&rec, &wanted, &cached, &q)?;
    }
}

fn raw_key(time: u64) -> TileKey {
    TileKey {
        version: SignalVersion::RAW,
        level: Level::RAW,
        block: ChannelBlock {
            grid: GridId(0),
            index: 0,
        },
        time,
    }
}

/// One grid of 4 channels at 100 Hz, 1000 s; a 60 s view at 6000 px wants level-0 tiles 0
/// and 1 (samples 0..6000).
fn setup() -> (Recording, kymo_core::tile::View) {
    let rec = shape_recording(&[(100.0, 0.0, 100_000, 4)]);
    let v = view(0.0..60.0, 6000, vec![0, 1]);
    assert_eq!(tiles_for_view(&rec, &v), vec![raw_key(0), raw_key(1)]);
    (rec, v)
}

#[test]
fn exact_tiles_are_drawn() {
    let (rec, v) = setup();
    let cache = big_cache();
    cache.insert(dummy(raw_key(0)));
    cache.insert(dummy(raw_key(1)));
    let q = query(&rec, &v, &cache);
    assert!(q.missing.is_empty() && q.loading.is_empty());
    let drawn: Vec<_> = q
        .draw
        .iter()
        .map(|p| (p.tile.key(), p.samples.clone(), p.exact))
        .collect();
    assert_eq!(
        drawn,
        vec![
            (raw_key(0), 0..TILE_LEN, true),
            (raw_key(1), TILE_LEN..2 * TILE_LEN, true)
        ]
    );
}

#[test]
fn one_coarser_tile_stands_in_for_both() {
    let (rec, v) = setup();
    let cache = big_cache();
    let parent = raw_key(0).parent().unwrap();
    assert_eq!(raw_key(1).parent(), Some(parent));
    cache.insert(dummy(parent));
    let q = query(&rec, &v, &cache);
    assert_eq!(q.missing, vec![raw_key(0), raw_key(1)]);
    assert!(q.loading.is_empty());
    assert_eq!(q.draw.len(), 1);
    assert_eq!(q.draw[0].tile.key(), parent);
    assert_eq!(q.draw[0].samples, 0..2 * TILE_LEN);
    assert!(!q.draw[0].exact);
}

#[test]
fn exact_tile_wins_over_coarser_and_the_rest_falls_back() {
    let (rec, v) = setup();
    let cache = big_cache();
    let parent = raw_key(0).parent().unwrap();
    cache.insert(dummy(parent));
    cache.insert(dummy(raw_key(1)));
    let q = query(&rec, &v, &cache);
    assert_eq!(q.missing, vec![raw_key(0)]);
    let drawn: Vec<_> = q
        .draw
        .iter()
        .map(|p| (p.tile.key(), p.samples.clone()))
        .collect();
    assert_eq!(
        drawn,
        vec![(parent, 0..TILE_LEN), (raw_key(1), TILE_LEN..2 * TILE_LEN)]
    );
}

#[test]
fn empty_cache_is_all_loading() {
    let (rec, v) = setup();
    let q = query(&rec, &v, &big_cache());
    assert!(q.draw.is_empty());
    assert_eq!(q.missing, vec![raw_key(0), raw_key(1)]);
    assert_eq!(
        q.loading,
        vec![Loading {
            block: raw_key(0).block,
            samples: 0..2 * TILE_LEN
        }]
    );
}

#[test]
fn other_versions_and_levels_above_the_top_never_stand_in() {
    let (rec, v) = setup();
    let cache = big_cache();
    let mut other = raw_key(0).parent().unwrap();
    other.version = SignalVersion(7);
    cache.insert(dummy(other));
    // The grid's top is level 2 (100 000 samples); level 3 is above it.
    let mut above = raw_key(0);
    while above.level <= max_level(100_000) {
        above = above.parent().unwrap();
    }
    cache.insert(dummy(above));
    let q = query(&rec, &v, &cache);
    assert!(q.draw.is_empty());
    assert_eq!(q.loading.len(), 1);
}

#[test]
fn the_last_tile_is_cut_at_the_grid_end() {
    let rec = shape_recording(&[(100.0, 0.0, 5000, 1)]);
    let v = view(0.0..60.0, 6000, vec![0]);
    let q = query(&rec, &v, &big_cache());
    assert_eq!(q.loading[0].samples, 0..5000);
}

proptest! {
    /// A spike stays visible when only coarser tiles are ready: the fallback tile drawn over
    /// the spike's sample holds it as its bucket's extreme.
    #[test]
    fn coarser_fallback_still_shows_the_spike(
        len in 200_000u64..2_000_000,
        at in any::<proptest::sample::Index>(),
        up in any::<bool>(),
        zoom in 0.001f64..0.2,
        width in 200u32..4000,
        extra_level in any::<proptest::sample::Index>(),
    ) {
        let i = at.index(len as usize) as u64;
        let spike = if up { i32::MAX } else { i32::MIN };
        let mut row = vec![0i32; len as usize];
        row[i as usize] = spike;
        let rate = 1000.0;
        let rec = Recording::new(single_grid(rate, MemData::I32(vec![row]))).unwrap().0;
        let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
        let top = pyramid.top();

        // Cache the top level and one random level in between; never the wanted level.
        let duration = len as f64 / rate;
        let length = duration * zoom;
        let t = i as f64 / rate;
        let v = view(t - length / 2.0..t + length / 2.0, width, vec![0]);
        let wanted = tiles_for_view(&rec, &v);
        let wanted_level = wanted[0].level;
        prop_assume!(wanted_level < top);
        let extra = Level::new(
            wanted_level.get() + 1 + extra_level.index(usize::from(top.get() - wanted_level.get())) as u8,
        )
        .unwrap();
        let cache = big_cache();
        for level in [top, extra] {
            let tiles = len.div_ceil(level.tile_samples());
            for time in 0..tiles {
                let key = TileKey { level, time, ..wanted[0] };
                cache.insert(Arc::new(pyramid.tile(key).unwrap()));
            }
        }

        let q = query(&rec, &v, &cache);
        prop_assert!(q.loading.is_empty());
        let placed = q.draw.iter().find(|p| p.samples.contains(&i));
        prop_assert!(placed.is_some(), "nothing drawn over the spike");
        let placed = placed.unwrap();
        prop_assert!(!placed.exact);
        let key = placed.tile.key();
        prop_assert!(key.level == extra || key.level == top);
        let bucket = (i / key.level.bucket_len() - key.time * TILE_LEN) as usize;
        let pair = &values(placed.tile.data())[2 * bucket..2 * bucket + 2];
        prop_assert_eq!(pair[usize::from(up)], f64::from(spike));
    }
}
