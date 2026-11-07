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
pub(crate) use data::TileSample;
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
/// Neighbouring buckets per pixel: 1 to 4 at levels ≥ 1, up to 31 raw samples at level 0.
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

/// The level a view uses for a grid.
pub fn level_for(grid: &Grid, view: &View) -> Level {
    let seconds = view.time.end - view.time.start;
    let samples_per_px = seconds * grid.rate() / f64::from(view.width_px.max(1));
    choose_level(samples_per_px).min(max_level(grid.len()))
}

/// The tiles a view needs: for every grid with visible channels, each channel block holding
/// one of them, times each tile of the chosen level that holds one of its
/// [`drawn_samples`]. Sorted by grid, block and time. Empty for an empty view.
pub fn tiles_for_view(recording: &Recording, view: &View) -> Vec<TileKey> {
    let mut tiles = Vec::new();
    // `is_empty` is also true for NaN bounds.
    if view.time.is_empty() || view.width_px == 0 {
        return tiles;
    }
    for block in view_blocks(recording, &view.channels) {
        let grid = recording.grid(block.grid);
        let level = level_for(grid, view);
        tiles.extend(tiles_over(grid, block, level, view.version, &view.time));
    }
    tiles
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
