//! Irregular time bases: lookups, the tiles a view draws (levels chosen per tile), tile
//! times, queries and prefetching.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use kymo_core::pyramid::Pyramid;
use kymo_core::testing::{MemData, opened};
use kymo_core::tile::{
    self, Direction, Level, TILE_LEN, TileCache, TileKey, choose_level, compute_raw, level_at,
    max_level, placements_for_view, samples_in, tiles_for_view,
};
use kymo_core::{
    Channel, ChannelId, Grid, GridId, ModelError, Recording, SampleType, TimeTable, Timebase,
};
use proptest::prelude::*;

use crate::common::view;

/// Slot times, the end, and the present segments of an irregular grid.
#[derive(Clone, Debug)]
struct Times {
    times: Vec<f64>,
    end: f64,
    segments: Vec<Range<u64>>,
}

impl Times {
    fn time(&self, slot: u64) -> f64 {
        self.times.get(slot as usize).copied().unwrap_or(self.end)
    }

    fn len(&self) -> u64 {
        self.times.len() as u64
    }
}

/// Runs of samples at one step each (0 for equal times), some after a gap: a slot of its
/// own one step after the sample before, then a jump.
fn times() -> impl Strategy<Value = Times> {
    let run = (
        prop_oneof![1u64..50, 1u64..5_000, 1u64..200_000],
        proptest::sample::select(vec![1e-6, 1e-5, 1e-4, 1e-3, 0.004, 0.02, 1.0, 0.0]),
        proptest::option::weighted(0.3, 1.0f64..1000.0),
    );
    proptest::collection::vec(run, 1..8).prop_map(|runs| {
        let mut times = Vec::new();
        let mut segments: Vec<Range<u64>> = Vec::new();
        let mut t = 3.0;
        let mut step = 0.01;
        for (k, (n, s, gap)) in runs.into_iter().enumerate() {
            if let (Some(jump), true) = (gap, k > 0) {
                times.push(t + step);
                t += step + jump;
            }
            let first = times.len() as u64;
            for _ in 0..n {
                times.push(t);
                t += s;
            }
            step = if s > 0.0 { s } else { 0.01 };
            let range = first..times.len() as u64;
            match segments.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => segments.push(range),
            }
        }
        let end = times.last().unwrap() + step;
        Times {
            times,
            end,
            segments,
        }
    })
}

fn grid_of(t: &Times, channels: usize, hold: bool) -> Grid {
    let table = TimeTable::new(&t.times, t.end).unwrap();
    let rate = t.len() as f64 / (t.end - t.times[0]).max(1e-9);
    Grid::new(
        Timebase::Irregular {
            rate,
            hold,
            times: Arc::new(table),
        },
        t.len(),
        t.segments.clone(),
        SampleType::F32,
        (0..channels as u32).map(ChannelId).collect(),
    )
    .unwrap()
}

/// A recording of one irregular grid of `channels` float channels, sample `i` of channel
/// `c` holding `i + 1000 c`.
fn recording(t: &Times, channels: usize) -> Recording {
    let grid = grid_of(t, channels, true);
    let rows = (0..channels)
        .map(|c| (0..t.len()).map(|i| (i + 1000 * c as u64) as f32).collect())
        .collect();
    let chans = (0..channels)
        .map(|c| Channel::new(format!("ch{c}"), GridId(0)))
        .collect();
    Recording::new(opened(chans, vec![grid], vec![MemData::F32(rows)]))
        .unwrap()
        .0
}

/// The coarsest level level-0 tile `block` allows at `per_second` pixels per second:
/// from its present samples per second of present time (a run lasting from its first
/// sample's time to the next slot's), as `choose_level` decides for evenly spaced ones.
fn wanted(t: &Times, block: u64, per_second: f64) -> Level {
    let span = block * TILE_LEN..((block + 1) * TILE_LEN).min(t.len());
    let (mut samples, mut seconds) = (0u64, 0.0);
    for s in &t.segments {
        let part = s.start.max(span.start)..s.end.min(span.end);
        if part.start < part.end {
            samples += part.end - part.start;
            seconds += t.time(part.end) - t.time(part.start);
        }
    }
    let density = samples as f64 / seconds;
    let top = max_level(t.len());
    if samples == 0 || !density.is_finite() {
        return top;
    }
    choose_level(density / per_second).min(top)
}

/// The least level the level-0 tiles in `span` allow.
fn least_wanted(t: &Times, span: &Range<u64>, per_second: f64) -> Level {
    (span.start / TILE_LEN..span.end.div_ceil(TILE_LEN))
        .map(|b| wanted(t, b, per_second))
        .min()
        .unwrap()
}

#[test]
fn irregular_lookups() {
    // Slot 4 is a gap, one step after the sample before it.
    let t = Times {
        times: vec![1.0, 1.5, 1.5, 1.6, 1.7, 4.0, 4.25],
        end: 4.5,
        segments: vec![0..4, 5..7],
    };
    let g = grid_of(&t, 1, true);
    assert_eq!(g.time_of(1), 1.5);
    assert_eq!(g.time_of(7), 4.5);
    assert_eq!(g.time_of(9), 4.5 + 2.0 / g.rate());
    assert_eq!(g.sample_at(0.0), 0);
    assert_eq!(g.sample_at(1.5), 2);
    assert_eq!(g.sample_at(1.65), 3);
    // Times in a gap belong to its slot, as in a segmented time base.
    assert_eq!(g.sample_at(3.0), 4);
    assert_eq!(g.sample_at(4.3), 6);
    assert_eq!(g.sample_at(9.0), 7);
    assert!(g.regular_run(0).is_none());
    assert!(g.holds_values());
    assert!(!grid_of(&t, 1, false).holds_values());
    assert_eq!(samples_in(&g, &(1.55..4.1)), 2..6);

    let table = Arc::new(TimeTable::new(&t.times, t.end).unwrap());
    let irregular = |rate, len| {
        Grid::continuous(
            Timebase::Irregular {
                rate,
                hold: true,
                times: Arc::clone(&table),
            },
            len,
            SampleType::F32,
            vec![ChannelId(0)],
        )
    };
    assert_eq!(
        irregular(2.0, 5),
        Err(ModelError::TimesLength { times: 7, len: 5 })
    );
    assert_eq!(irregular(0.0, 7), Err(ModelError::InvalidRate(0.0)));
    assert!(irregular(2.0, 7).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Every slot maps back from its time (to the last slot of that time), and times
    /// between slots belong to the slot before.
    #[test]
    fn times_map_back_to_their_samples(t in times()) {
        let g = grid_of(&t, 1, true);
        for i in (0..t.len()).step_by(37).chain(t.len().saturating_sub(3)..t.len()) {
            let time = t.time(i);
            prop_assert_eq!(g.time_of(i), time);
            let last = t.times.partition_point(|&x| x <= time) as u64 - 1;
            prop_assert_eq!(g.sample_at(time), last);
            let next = t.time(i + 1);
            if next > time {
                prop_assert_eq!(g.sample_at((time + next) / 2.0), i);
            }
        }
    }

    /// What a view draws covers the samples it shows, in time order and without overlap.
    /// No level-0 tile is drawn coarser than its own density allows; each placement is a
    /// tile's span drawn at the coarsest level its level-0 tiles allow (and its buckets start
    /// on), and that tile's parent has a level-0 tile that doesn't allow the parent's level.
    /// `level_at` agrees.
    #[test]
    fn views_draw_tiles_as_coarse_as_their_density_allows(
        t in times(),
        at in 0.0f64..1.0,
        zoom in -14.0f64..1.0,
        width in prop_oneof![1u32..100, 100u32..4000],
    ) {
        let rec = recording(&t, 3);
        let grid = rec.grid(GridId(0));
        let first = t.times[0];
        // From the whole grid (and more) down to a 16384th of it.
        let seconds = (t.end - first).max(1e-3) * 2f64.powf(zoom);
        let start = first - seconds / 4.0 + at * (t.end - first);
        let v = view(start..start + seconds, width, vec![1]);
        let per_second = f64::from(width) / seconds;
        let placements = placements_for_view(&rec, &v);
        let tiles = tiles_for_view(&rec, &v);
        let shown = samples_in(grid, &v.time);
        if shown.is_empty() {
            prop_assert!(placements.is_empty());
            return Ok(());
        }
        let top = max_level(t.len());
        let mut covered = shown.start;
        for (k, (key, samples)) in placements.iter().enumerate() {
            prop_assert!(samples.start < samples.end && samples.end <= t.len());
            prop_assert!(key.sample_range().start <= samples.start);
            prop_assert!(samples.end <= key.sample_range().end);
            if k > 0 {
                prop_assert!(placements[k - 1].1.end <= samples.start, "in order");
            }
            prop_assert!(samples.start <= covered, "no hole before {key:?}");
            covered = covered.max(samples.end);
            // The placement is the span of a tile of some level, or several such spans
            // merged; check each.
            let mut s = samples.start;
            while s < samples.end {
                let level = (0..=key.level.get())
                    .rev()
                    .filter_map(Level::new)
                    .find(|l| {
                        s.is_multiple_of(l.tile_samples())
                            && (s + l.tile_samples()).min(t.len()) <= samples.end
                            && least_wanted(&t, &(s..(s + l.tile_samples()).min(t.len())), per_second) >= *l
                    });
                let Some(level) = level else {
                    prop_assert!(false, "{key:?} over {samples:?}: no tile at {s}");
                    unreachable!()
                };
                let span = s..(s + level.tile_samples()).min(t.len());
                let want = least_wanted(&t, &span, per_second);
                let mut aligned = want;
                while aligned.bucket_len() > level.tile_samples() {
                    aligned = aligned.down().unwrap();
                }
                prop_assert_eq!(key.level, aligned, "{:?} over {:?}", key, span);
                if level < top {
                    let up = level.up().unwrap();
                    let p = up.tile_of(s);
                    let parent = p * up.tile_samples()..((p + 1) * up.tile_samples()).min(t.len());
                    prop_assert!(least_wanted(&t, &parent, per_second) < up, "{:?}'s parent", span);
                }
                s = span.end;
            }
        }
        prop_assert!(covered >= shown.end, "the shown samples are covered");
        // The line from the last shown sample runs on to the next.
        if shown.end < t.len() {
            prop_assert!(covered > shown.end);
        }
        for s in [shown.start, (shown.start + shown.end) / 2, shown.end - 1] {
            let (key, _) = placements.iter().find(|(_, r)| r.contains(&s)).unwrap();
            prop_assert_eq!(level_at(grid, &v, s), key.level);
        }
        // Densities, against brute force.
        for b in shown.start / TILE_LEN..shown.end.div_ceil(TILE_LEN) {
            let span = b * TILE_LEN..((b + 1) * TILE_LEN).min(t.len());
            let (mut samples, mut seconds) = (0u64, 0.0);
            for s in &t.segments {
                let part = s.start.max(span.start)..s.end.min(span.end);
                if part.start < part.end {
                    samples += part.end - part.start;
                    seconds += t.time(part.end) - t.time(part.start);
                }
            }
            let want = samples as f64 / seconds;
            let want = if samples > 0 && want.is_finite() { want } else { f64::INFINITY };
            let got = grid.least_density(Level::RAW, b).unwrap();
            prop_assert!(got == want || ((got - want) / want).abs() < 1e-12, "block {}: {} {}", b, got, want);
        }

        // Tiles carry each item's start time and the last one's end.
        let pyramid = Pyramid::build(&rec, GridId(0)).unwrap();
        for key in tiles.iter().take(3) {
            prop_assert!(key.level <= top);
            let tile = if key.level == Level::RAW {
                compute_raw(&rec, *key).unwrap()
            } else {
                pyramid.tile(*key).unwrap()
            };
            let times = tile.times().unwrap();
            prop_assert_eq!(times.len(), tile.len() + 1);
            let bucket = key.level.bucket_len();
            let first = key.sample_range().start;
            for (i, &time) in times.iter().enumerate() {
                let slot = (first + i as u64 * bucket).min(t.len());
                prop_assert_eq!(time, t.time(slot), "{:?} item {}", key, i);
            }
        }

        // A query of an empty cache wants exactly these tiles; prefetching lists them
        // first (nearest the center first), then others inside the grid, none twice.
        let q = tile::query(&rec, &v, &TileCache::new(1 << 20));
        prop_assert_eq!(&q.missing, &tiles);
        let listed = tile::prefetch(&rec, &v, Direction::Forward, 64 << 20);
        let screen: HashSet<TileKey> = listed[..tiles.len()].iter().copied().collect();
        prop_assert_eq!(screen, tiles.iter().copied().collect::<HashSet<_>>());
        let mut seen = HashSet::new();
        for key in &listed {
            prop_assert!(seen.insert(*key), "{key:?} listed twice");
            prop_assert!(key.sample_range().start < t.len(), "{key:?} inside the grid");
            prop_assert!(key.level <= top);
        }
    }
}

/// A dense burst amid sparse samples: zoomed out, the sparse samples stay raw and the burst
/// is drawn at the top level, even where it shares a tile with them.
#[test]
fn levels_follow_the_local_density() {
    let block = TILE_LEN as usize;
    let n = 66 * block;
    // A level-0 tile of samples 0.05 s apart, then 63 tiles 1 µs apart (the rest of the
    // first level-1 tile and all of the second), then 2 tiles 0.05 s apart.
    let mut times = Vec::with_capacity(n);
    let mut t = 0.0;
    for i in 0..n {
        times.push(t);
        let dense = (block..64 * block).contains(&i);
        t += if dense { 1e-6 } else { 0.05 };
    }
    let end = t + 1.0;
    let rec = recording(
        &Times {
            times,
            end,
            segments: vec![0..n as u64],
        },
        1,
    );
    let v = view(0.0..end, 2000, vec![0]);
    let placements = placements_for_view(&rec, &v);
    let level_of = |s: u64| {
        placements
            .iter()
            .find(|(_, r)| r.contains(&s))
            .unwrap()
            .0
            .level
    };
    // About 6 sparse samples per pixel allow only raw ones; the burst, far under a pixel,
    // the top: its tile drawn over the burst's 63 level-0 tiles.
    let top = max_level(n as u64);
    assert_eq!(level_of(10), Level::RAW);
    assert_eq!(level_of(20 * TILE_LEN), top);
    assert_eq!(level_of(40 * TILE_LEN), top);
    assert_eq!(level_of(n as u64 - 10), Level::RAW);
    let burst = placements.iter().find(|(k, _)| k.level == top).unwrap();
    assert_eq!(burst.1, TILE_LEN..64 * TILE_LEN);
    assert_eq!(placements.len(), 4, "{placements:?}");
}

/// Where a coarse tile's level-0 tiles are dense on both sides of a sparse one, the coarse
/// tile is drawn on both sides, and wanted once; a view ending on a tile boundary also
/// draws the tile after it, which its last line runs to.
#[test]
fn a_coarse_tile_is_drawn_around_a_sparse_one() {
    let block = TILE_LEN as usize;
    let n = 96 * block;
    let mut times = Vec::with_capacity(n);
    let mut t = 0.0;
    for i in 0..n {
        times.push(t);
        t += if i / block == 32 { 0.05 } else { 1e-6 };
    }
    let end = t + 1e-6;
    let rec = recording(
        &Times {
            times: times.clone(),
            end,
            segments: vec![0..n as u64],
        },
        1,
    );
    let v = view(0.0..end, 2000, vec![0]);
    let top = max_level(n as u64);
    let coarse = TileKey {
        version: v.version,
        level: top,
        block: tile::ChannelBlock {
            grid: GridId(0),
            index: 0,
        },
        time: 0,
    };
    let raw = TileKey {
        level: Level::RAW,
        time: 32,
        ..coarse
    };
    let b = TILE_LEN;
    assert_eq!(
        placements_for_view(&rec, &v),
        vec![
            (coarse, 0..32 * b),
            (raw, 32 * b..33 * b),
            (coarse, 33 * b..96 * b)
        ]
    );
    assert_eq!(tiles_for_view(&rec, &v), vec![coarse, raw]);
    let q = tile::query(&rec, &v, &TileCache::new(1 << 20));
    assert_eq!(q.missing, vec![coarse, raw]);

    // Samples a second apart, zoomed in to raw ones: a view ending just after sample 4095
    // shows samples up to it, and draws the tile of sample 4096 too.
    let sparse: Vec<f64> = (0..3 * block).map(|i| i as f64).collect();
    let rec = recording(
        &Times {
            times: sparse,
            end: (3 * block) as f64,
            segments: vec![0..3 * b],
        },
        1,
    );
    let v = view(0.0..4095.5, 4000, vec![0]);
    assert_eq!(samples_in(rec.grid(GridId(0)), &v.time), 0..b);
    let placed: Vec<u64> = placements_for_view(&rec, &v)
        .iter()
        .map(|(k, _)| k.time)
        .collect();
    assert_eq!(placed, vec![0, 1]);
}
