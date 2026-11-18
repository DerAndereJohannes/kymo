//! The fixed tile grid: levels, tile keys, and which tiles a view needs.
//!
//! Bucket boundaries are fixed in sample-index space: bucket `k` of level `L` always covers
//! the same samples of a grid, whatever the view. Panning is a pure translation, and a tile
//! computed once stays valid.

mod cache;
mod data;
mod prefetch;
mod query;
mod raw;

use std::ops::Range;

pub use cache::{DEFAULT_BUDGET, TileCache};
pub(crate) use data::mask_rows;
pub use data::{Tile, TileData, TileSlice};
pub use prefetch::{Direction, prefetch, tile_bytes};
pub use query::{Loading, Placed, Query, query};
pub use raw::compute_raw;

use crate::{ChannelId, Grid, GridId, Recording};

/// Buckets (level ≥ 1) or samples (level 0) per channel in one tile.
pub const TILE_LEN: u64 = 4096;
/// Channels per tile.
pub const CHANNEL_BLOCK: usize = 32;
/// Samples per bucket at level 1.
pub const LEVEL1_BUCKET: u64 = 32;
/// Each level above 1 merges this many buckets of the level below.
pub const LEVEL_FACTOR: u64 = 4;

/// A pyramid level. Level 0 is raw samples; level `L ≥ 1` holds min/max pairs of buckets of
/// `32 · 4^(L-1)` samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Level(u8);

impl Level {
    pub const RAW: Level = Level(0);
    /// The highest level whose tiles fit in u64 sample indices (`4096 · 32 · 4^23 = 2^63`).
    pub const MAX: Level = Level(24);

    /// `None` above [`Level::MAX`].
    pub fn new(level: u8) -> Option<Level> {
        (level <= Self::MAX.0).then_some(Level(level))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Samples per bucket: 1, 32, 128, 512, ...
    pub fn bucket_len(self) -> u64 {
        match self.0 {
            0 => 1,
            l => LEVEL1_BUCKET << (2 * (u32::from(l) - 1)),
        }
    }

    /// Samples per tile and channel.
    pub fn tile_samples(self) -> u64 {
        TILE_LEN * self.bucket_len()
    }

    /// The tile (time index) holding `sample` at this level.
    pub fn tile_of(self, sample: u64) -> u64 {
        sample / self.tile_samples()
    }

    /// The next coarser level, `None` at [`Level::MAX`].
    pub fn up(self) -> Option<Level> {
        Level::new(self.0 + 1)
    }

    /// The next finer level, `None` at level 0.
    pub fn down(self) -> Option<Level> {
        self.0.checked_sub(1).map(Level)
    }
}

/// The coarsest level whose bucket spans at most `samples_per_px` samples, so no bucket is
/// wider than a pixel and every peak lands in its own pixel. Below 32 samples per pixel
/// (and for invalid input) that is level 0, raw samples.
///
/// Neighboring buckets per pixel: 1 to 4 at levels ≥ 1, up to 31 raw samples at level 0.
pub fn choose_level(samples_per_px: f64) -> Level {
    let mut level = Level::RAW;
    while let Some(up) = level.up() {
        if (up.bucket_len() as f64) > samples_per_px || samples_per_px.is_nan() {
            break;
        }
        level = up;
    }
    level
}

/// The lowest level at which one tile covers all `len` samples of a grid: the top of its
/// pyramid. Level 0 for grids of at most [`TILE_LEN`] samples.
pub fn max_level(len: u64) -> Level {
    let mut level = Level::RAW;
    while level.tile_samples() < len {
        match level.up() {
            Some(up) => level = up,
            None => break,
        }
    }
    level
}

/// The buckets that make up `samples` exactly, each sample in one of them: whole buckets of
/// the coarsest level up to `top` that fits, and at either edge finer ones, down to samples
/// at level 0. Each piece is a level and a range of its buckets (samples at level 0), at
/// most two per level, so their extremes are the samples' extremes from a few tiles,
/// however long `samples` is.
pub fn exact_cover(samples: Range<u64>, top: Level) -> Vec<(Level, Range<u64>)> {
    let mut out = Vec::new();
    if samples.is_empty() {
        return out;
    }
    let mut level = top;
    while level > Level::RAW && level.bucket_len() > samples.end - samples.start {
        level = level.down().unwrap_or(Level::RAW);
    }
    // What is left to cover: the whole at first, then the edges, each of which ends or
    // starts on a coarser bucket's boundary, so at most two parts at a time.
    let mut parts = vec![samples];
    loop {
        let b = level.bucket_len();
        let mut next = Vec::with_capacity(2);
        for part in parts {
            let (first, end) = (part.start.div_ceil(b), part.end / b);
            if first < end {
                out.push((level, first..end));
                if part.start < first * b {
                    next.push(part.start..first * b);
                }
                if end * b < part.end {
                    next.push(end * b..part.end);
                }
            } else {
                next.push(part);
            }
        }
        parts = next;
        match level.down() {
            Some(down) if !parts.is_empty() => level = down,
            _ => break,
        }
    }
    out
}

/// Identifies a derived signal (filters, references, projections); 0 is the raw recording.
/// Tiles of different versions never mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalVersion(pub u32);

impl SignalVersion {
    /// The recording as stored.
    pub const RAW: SignalVersion = SignalVersion(0);
}

/// Up to [`CHANNEL_BLOCK`] channels of one grid: `grid.channels()[32·index ..][..32]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelBlock {
    pub grid: GridId,
    pub index: u32,
}

impl ChannelBlock {
    /// Number of channel blocks of a grid.
    pub fn count(grid: &Grid) -> u32 {
        grid.channels().len().div_ceil(CHANNEL_BLOCK) as u32
    }

    /// The channels of this block (fewer than 32 in a grid's last block).
    ///
    /// # Panics
    /// If the block is not in `grid` (index past its last block).
    pub fn channels<'a>(&self, grid: &'a Grid) -> &'a [ChannelId] {
        let start = self.index as usize * CHANNEL_BLOCK;
        let end = (start + CHANNEL_BLOCK).min(grid.channels().len());
        &grid.channels()[start..end]
    }
}

/// One tile: a channel block over one fixed time span at one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub version: SignalVersion,
    pub level: Level,
    pub block: ChannelBlock,
    /// Index of the tile's time span: samples `time · tile_samples ..` of the grid.
    pub time: u64,
}

impl TileKey {
    /// The samples the tile spans; may extend past the end of the grid (saturating at
    /// `u64::MAX`, far beyond any grid).
    pub fn sample_range(&self) -> Range<u64> {
        let n = self.level.tile_samples();
        self.time.saturating_mul(n)..self.time.saturating_add(1).saturating_mul(n)
    }

    /// The tile one level up that contains this one; `None` at [`Level::MAX`].
    pub fn parent(&self) -> Option<TileKey> {
        let up = self.level.up()?;
        let ratio = up.tile_samples() / self.level.tile_samples();
        Some(TileKey {
            level: up,
            time: self.time / ratio,
            ..*self
        })
    }

    /// The tiles one level down that make up this one, in time order (32 below level 1,
    /// otherwise 4); empty at level 0.
    pub fn children(&self) -> Vec<TileKey> {
        let Some(down) = self.level.down() else {
            return Vec::new();
        };
        let ratio = self.level.tile_samples() / down.tile_samples();
        (self.time * ratio..(self.time + 1) * ratio)
            .map(|time| TileKey {
                level: down,
                time,
                ..*self
            })
            .collect()
    }
}

/// What is on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub version: SignalVersion,
    /// Seconds since the recording start.
    pub time: Range<f64>,
    /// Width of the trace area in physical pixels.
    pub width_px: u32,
    /// The visible channels, in any order and from any grids.
    pub channels: Vec<ChannelId>,
}

/// The samples of `grid` that intersect `time`, clamped to the grid.
pub fn samples_in(grid: &Grid, time: &Range<f64>) -> Range<u64> {
    let start = grid.sample_at(time.start);
    let mut end = grid.sample_at(time.end);
    // The sample whose period contains t1 is visible unless t1 is exactly its start.
    if end < grid.len() && grid.time_of(end) < time.end {
        end += 1;
    }
    start..end.max(start)
}

/// The samples of `grid` that a view over `time` draws at `level`: those that intersect
/// `time`, and at level 0 also the sample after the last of them, which the line to the
/// right edge runs to. Empty if no sample intersects `time`.
pub fn drawn_samples(grid: &Grid, level: Level, time: &Range<f64>) -> Range<u64> {
    let mut samples = samples_in(grid, time);
    if level == Level::RAW && !samples.is_empty() {
        samples.end = (samples.end + 1).min(grid.len());
    }
    samples
}

/// The level a view uses for a grid with evenly spaced samples (for an irregular one, the
/// level its average rate gives; [`level_at`] has the level it uses at a sample).
pub fn level_for(grid: &Grid, view: &View) -> Level {
    let seconds = view.time.end - view.time.start;
    let samples_per_px = seconds * grid.rate() / f64::from(view.width_px.max(1));
    choose_level(samples_per_px).min(max_level(grid.len()))
}

/// The level a view uses for `grid` at `sample`: [`level_for`], or for an irregular time
/// base the level its cover ([`placements_for_view`]) draws there.
pub fn level_at(grid: &Grid, view: &View, sample: u64) -> Level {
    if grid.times().is_none() {
        return level_for(grid, view);
    }
    let per_second = pixels_per_second(view);
    let mut level = max_level(grid.len());
    loop {
        let want = wanted(grid, level, level.tile_of(sample), per_second);
        match level.down() {
            Some(down) if want < level => level = down,
            _ => return aligned(level, want.max(level)),
        }
    }
}

/// Pixels per second of a view; 0 for a view without width or time.
fn pixels_per_second(view: &View) -> f64 {
    let seconds = view.time.end - view.time.start;
    let p = f64::from(view.width_px) / seconds;
    if p.is_finite() { p } else { 0.0 }
}

/// The coarsest level every level-0 tile in tile `time` of `level` allows at `per_second`
/// pixels per second, as [`choose_level`] decides from its own density (so no bucket of
/// its samples spans more than a pixel on average), at most the top of the pyramid.
fn wanted(grid: &Grid, level: Level, time: u64, per_second: f64) -> Level {
    let density = grid.least_density(level, time).unwrap_or(f64::INFINITY);
    let samples_per_px = density / per_second;
    let samples_per_px = if samples_per_px.is_nan() {
        f64::INFINITY
    } else {
        samples_per_px
    };
    choose_level(samples_per_px).min(max_level(grid.len()))
}

/// The coarsest level up to `want` whose buckets the tiles of `level` start on, so that its
/// tile can be drawn over the span of one of them.
fn aligned(level: Level, want: Level) -> Level {
    let mut m = want;
    while m > level && m.bucket_len() > level.tile_samples() {
        m = m.down().unwrap_or(Level::RAW);
    }
    m
}

/// The tiles of `block` of a grid with an irregular time base that a view at `per_second`
/// pixels per second draws over `samples` (and the sample after them, which a line to the
/// right edge runs to), each with the samples it is drawn over, in time order. From the top
/// of the pyramid down, a tile whose level-0 tiles all allow its level or a coarser one is
/// drawn over its span at the coarsest level they all allow (a coarser tile drawn over part
/// of its span); otherwise its children are visited. So no level-0 tile is drawn coarser
/// than its own density allows, and dense ones stay coarse beside sparse ones.
fn irregular_cover(
    grid: &Grid,
    block: ChannelBlock,
    version: SignalVersion,
    samples: Range<u64>,
    per_second: f64,
) -> Vec<(TileKey, Range<u64>)> {
    struct Visit<'a> {
        grid: &'a Grid,
        drawn: Range<u64>,
        per_second: f64,
        block: ChannelBlock,
        version: SignalVersion,
        out: Vec<(TileKey, Range<u64>)>,
    }
    impl Visit<'_> {
        fn visit(&mut self, level: Level, time: u64) {
            let n = level.tile_samples();
            let span = time.saturating_mul(n)
                ..time
                    .saturating_add(1)
                    .saturating_mul(n)
                    .min(self.grid.len());
            if span.start >= self.drawn.end || span.end <= self.drawn.start {
                return;
            }
            let want = wanted(self.grid, level, time, self.per_second);
            match level.down() {
                Some(down) if want < level => {
                    let ratio = n / down.tile_samples();
                    for child in time * ratio..(time + 1) * ratio {
                        self.visit(down, child);
                    }
                }
                _ => {
                    let m = aligned(level, want.max(level));
                    let key = TileKey {
                        version: self.version,
                        level: m,
                        block: self.block,
                        time: m.tile_of(span.start),
                    };
                    match self.out.last_mut() {
                        Some((k, s)) if *k == key && s.end == span.start => s.end = span.end,
                        _ => self.out.push((key, span)),
                    }
                }
            }
        }
    }
    if samples.is_empty() {
        return Vec::new();
    }
    let mut v = Visit {
        grid,
        drawn: samples.start..(samples.end + 1).min(grid.len()),
        per_second,
        block,
        version,
        out: Vec::new(),
    };
    let top = max_level(grid.len());
    for time in top.tile_of(v.drawn.start)..=top.tile_of(v.drawn.end - 1) {
        v.visit(top, time);
    }
    v.out
}

/// The tiles of `block` a view draws over `time`, in time order, each with the samples it
/// is drawn over: those of its level over their spans, or for an irregular time base those
/// of its cover.
fn cover(
    grid: &Grid,
    block: ChannelBlock,
    view: &View,
    time: &Range<f64>,
) -> Vec<(TileKey, Range<u64>)> {
    if grid.times().is_some() {
        let samples = samples_in(grid, time);
        irregular_cover(grid, block, view.version, samples, pixels_per_second(view))
    } else {
        let level = level_for(grid, view);
        tiles_over(grid, block, level, view.version, time)
            .map(|key| {
                let span = key.sample_range();
                (key, span.start..span.end.min(grid.len()))
            })
            .collect()
    }
}

/// What a view draws: for every grid with visible channels, each channel block holding one
/// of them, times each tile of the chosen level that holds one of its [`drawn_samples`],
/// with the samples it is drawn over (its span, cut at the grid's end). For a grid with an
/// irregular time base the level is chosen per level-0 tile from its density, and a coarser
/// tile may be drawn over part of its span (see [`level_at`]). Sorted by grid, block and
/// samples, which never overlap within a block. Empty for an empty view.
pub fn placements_for_view(recording: &Recording, view: &View) -> Vec<(TileKey, Range<u64>)> {
    let mut placements = Vec::new();
    // `is_empty` is also true for NaN bounds.
    if view.time.is_empty() || view.width_px == 0 {
        return placements;
    }
    for block in view_blocks(recording, &view.channels) {
        let grid = recording.grid(block.grid);
        placements.extend(cover(grid, block, view, &view.time));
    }
    placements
}

/// The tiles a view needs: those of [`placements_for_view`], each once, in their order.
pub fn tiles_for_view(recording: &Recording, view: &View) -> Vec<TileKey> {
    let mut seen = std::collections::HashSet::new();
    placements_for_view(recording, view)
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| seen.insert(*key))
        .collect()
}

/// The channel blocks holding `channels`, sorted and without duplicates; unknown channels
/// are ignored.
fn view_blocks(recording: &Recording, channels: &[ChannelId]) -> Vec<ChannelBlock> {
    let mut blocks: Vec<ChannelBlock> = channels
        .iter()
        .filter_map(|&id| {
            let channel = recording.channels().get(id.0 as usize)?;
            let grid = recording.grid(channel.grid);
            let position = grid.channels().iter().position(|&c| c == id)?;
            Some(ChannelBlock {
                grid: channel.grid,
                index: (position / CHANNEL_BLOCK) as u32,
            })
        })
        .collect();
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

/// The tiles of `block` at `level` holding the [`drawn_samples`] over `time`, in time order;
/// none outside the grid.
fn tiles_over(
    grid: &Grid,
    block: ChannelBlock,
    level: Level,
    version: SignalVersion,
    time: &Range<f64>,
) -> impl Iterator<Item = TileKey> + use<> {
    let samples = drawn_samples(grid, level, time);
    let times = (!samples.is_empty())
        .then(|| level.tile_of(samples.start)..=level.tile_of(samples.end - 1));
    times.into_iter().flatten().map(move |time| TileKey {
        version,
        level,
        block,
        time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarser_tiles_are_drawn_only_where_their_buckets_start() {
        let l = |l| Level::new(l).unwrap();
        // A level-0 tile's span holds whole buckets up to level 4 (2048 samples).
        assert_eq!(aligned(l(0), l(9)), l(4));
        assert_eq!(aligned(l(0), l(3)), l(3));
        // A level-L tile's up to level L + 6, whose bucket is the tile's span.
        assert_eq!(aligned(l(1), l(7)), l(7));
        assert_eq!(aligned(l(1), l(9)), l(7));
        assert_eq!(aligned(l(2), l(2)), l(2));
    }
}
