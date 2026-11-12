//! Which tiles to compute for a view and around it, most urgent first.

use std::collections::HashSet;
use std::ops::Range;

use super::{
    CHANNEL_BLOCK, Level, Tile, TileKey, View, cover, level_for, max_level, samples_in,
    tiles_for_view, tiles_over, view_blocks,
};
use crate::{Grid, Recording};

/// The way the view last moved in time; prefetching looks further ahead that way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Forward,
    Backward,
}

/// The tiles `view` needs, then those it may need next, most urgent first:
///
/// 1. the tiles on screen ([`tiles_for_view`]), nearest to the screen's center first;
/// 2. the margins at the same level, one screen width ahead in `direction` and half a
///    screen behind, nearest to the screen first;
/// 3. the level above over the screen and margins, for zooming out;
/// 4. the level below over the screen, from its center outwards, for zooming in.
///
/// The tiles after the screen's stop before their estimated memory ([`tile_bytes`]) would
/// pass `max_bytes`, so a smaller budget gives a prefix of a larger one's list. Every tile
/// lies inside the recording, and none is listed twice. Empty for an empty view.
pub fn prefetch(
    recording: &Recording,
    view: &View,
    direction: Direction,
    max_bytes: usize,
) -> Vec<TileKey> {
    let mut screen = tiles_for_view(recording, view);
    if screen.is_empty() {
        return screen;
    }
    let shown = view.time.clone();
    let width = shown.end - shown.start;
    let (before, after) = match direction {
        Direction::Forward => (width / 2.0, width),
        Direction::Backward => (width, width / 2.0),
    };
    let wide = shown.start - before..shown.end + after;
    let center = shown.start + width / 2.0;
    let span = |key: &TileKey| span_of(recording.grid(key.block.grid), key);
    // Ties between the two margins go to the one ahead.
    let behind = |key: &TileKey| {
        let s = span(key);
        match direction {
            Direction::Forward => s.end <= shown.start,
            Direction::Backward => s.start >= shown.end,
        }
    };

    let mut margins = Vec::new();
    let mut above = Vec::new();
    let mut below = Vec::new();
    for block in view_blocks(recording, &view.channels) {
        let grid = recording.grid(block.grid);
        if grid.times().is_some() {
            // Levels vary per tile: the cover over the margins, the tiles above it, and
            // those below the screen's.
            let top = max_level(grid.len());
            let wide_cover: Vec<TileKey> = cover(grid, block, view, &wide)
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            above.extend(
                wide_cover
                    .iter()
                    .filter_map(TileKey::parent)
                    .filter(|k| k.level <= top),
            );
            for key in wide_cover {
                if !margins.contains(&key) {
                    margins.push(key);
                }
            }
            let visible = samples_in(grid, &shown);
            below.extend(
                cover(grid, block, view, &shown)
                    .iter()
                    .flat_map(|(key, _)| key.children())
                    .filter(|k| {
                        let s = k.sample_range();
                        let end = visible.end.saturating_add(1).min(grid.len());
                        s.start < end && s.end > visible.start
                    }),
            );
            continue;
        }
        let level = level_for(grid, view);
        margins.extend(tiles_over(grid, block, level, view.version, &wide));
        if let Some(up) = level.up().filter(|&up| up <= max_level(grid.len())) {
            above.extend(tiles_over(grid, block, up, view.version, &wide));
        }
        if let Some(down) = level.down() {
            below.extend(tiles_over(grid, block, down, view.version, &shown));
        }
    }

    sort_by_distance(&mut screen, |k| (distance(&span(k), center..center), false));
    sort_by_distance(&mut margins, |k| {
        (distance(&span(k), shown.clone()), behind(k))
    });
    sort_by_distance(&mut above, |k| {
        (distance(&span(k), shown.clone()), behind(k))
    });
    sort_by_distance(&mut below, |k| (distance(&span(k), center..center), false));

    let mut seen: HashSet<TileKey> = screen.iter().copied().collect();
    let mut bytes = 0usize;
    for key in margins.into_iter().chain(above).chain(below) {
        if !seen.insert(key) {
            continue;
        }
        bytes = bytes.saturating_add(tile_bytes(recording, &key));
        if bytes > max_bytes {
            break;
        }
        screen.push(key);
    }
    screen
}

/// Memory a tile of `key` holds, as [`Tile::bytes`] counts it apart from its list of gaps;
/// 0 for a key outside the recording.
pub fn tile_bytes(recording: &Recording, key: &TileKey) -> usize {
    let Some(grid) = recording.grids().get(key.block.grid.0 as usize) else {
        return 0;
    };
    let first = key.block.index as usize * CHANNEL_BLOCK;
    let channels = grid
        .channels()
        .len()
        .saturating_sub(first)
        .min(CHANNEL_BLOCK);
    let span = key.sample_range();
    let samples = span.end.min(grid.len()).saturating_sub(span.start);
    if channels == 0 || samples == 0 {
        return 0;
    }
    let per_bucket = if key.level == Level::RAW { 1 } else { 2 };
    let items = samples.div_ceil(key.level.bucket_len()) as usize;
    let times = if grid.times().is_some() {
        (items + 1) * size_of::<f64>()
    } else {
        0
    };
    size_of::<Tile>() + channels * items * per_bucket * grid.sample_type().size() + times
}

/// Seconds covered by the tile `key` of `grid`, cut at the grid's end.
fn span_of(grid: &Grid, key: &TileKey) -> Range<f64> {
    let samples = key.sample_range();
    grid.time_of(samples.start)..grid.time_of(samples.end.min(grid.len()))
}

/// Seconds between two intervals; 0 if they touch or overlap.
fn distance(a: &Range<f64>, b: Range<f64>) -> f64 {
    (b.start - a.end).max(a.start - b.end).max(0.0)
}

/// Sorts by `(distance, tie-break)`, then by key, so the order never depends on the input's.
fn sort_by_distance(keys: &mut [TileKey], rank: impl Fn(&TileKey) -> (f64, bool)) {
    keys.sort_by(|a, b| {
        let (da, ta) = rank(a);
        let (db, tb) = rank(b);
        da.total_cmp(&db).then(ta.cmp(&tb)).then_with(|| a.cmp(b))
    });
}
