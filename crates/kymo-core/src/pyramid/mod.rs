//! The min/max pyramid of one grid, and the block sums for DC removal.
//!
//! Level `L ≥ 1` holds the minimum and maximum of every bucket of `32 · 4^(L-1)` samples,
//! in the grid's native type, over present samples only: gaps never count, and a bucket that
//! lies entirely in a gap keeps the empty sentinel (`min > max`, see [`Sample::EMPTY_MIN`]).
//! Levels are stored as tiles of the fixed grid ([`crate::tile`]), all channels of the grid
//! together, so one read serves a whole screen of channels.
//!
//! Beside the levels sits one f64 sum of native values per channel and block of
//! [`TILE_LEN`] samples (a level-0 tile span), over the block's present samples. A NaN
//! sample makes its block's sum NaN; NaN samples never change a minimum or maximum.
//!
//! [`Sample::EMPTY_MIN`]: crate::Sample::EMPTY_MIN

mod build;

use std::collections::HashMap;
use std::ops::Range;

pub use build::PyramidBuilder;

use crate::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, SignalVersion, TILE_LEN, Tile, TileData, TileKey,
    TileSlice, max_level,
};
use crate::{ChannelId, Grid, GridId, ReadError, Recording};

/// Where a pyramid is kept. The builder writes into it; [`Pyramid`] reads from it.
///
/// A level tile holds all channels of the grid in grid order, channel-major, with the
/// tile's (min, max) pairs interleaved as in [`TileData`]. The last tile of a level is cut
/// at the grid's end.
pub trait PyramidStorage: Send + Sync {
    /// Stores tile `time` of `level`.
    fn put_tile(&mut self, level: Level, time: u64, data: TileData);
    /// Stores the sums of block `block`, one per channel in grid order.
    fn put_sums(&mut self, block: u64, sums: &[f64]);
    /// Tile `time` of `level`, if stored.
    fn tile(&self, level: Level, time: u64) -> Option<TileSlice<'_>>;
    /// The sums of block `block`, if stored.
    fn sums(&self, block: u64) -> Option<&[f64]>;
}

/// Keeps a pyramid in memory.
#[derive(Debug, Default)]
pub struct MemStorage {
    tiles: HashMap<(Level, u64), TileData>,
    sums: Vec<Option<Box<[f64]>>>,
}

impl MemStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PyramidStorage for MemStorage {
    fn put_tile(&mut self, level: Level, time: u64, data: TileData) {
        self.tiles.insert((level, time), data);
    }

    fn put_sums(&mut self, block: u64, sums: &[f64]) {
        let block = block as usize;
        if self.sums.len() <= block {
            self.sums.resize(block + 1, None);
        }
        self.sums[block] = Some(sums.into());
    }

    fn tile(&self, level: Level, time: u64) -> Option<TileSlice<'_>> {
        self.tiles.get(&(level, time)).map(TileData::as_slice)
    }

    fn sums(&self, block: u64) -> Option<&[f64]> {
        self.sums.get(block as usize)?.as_deref()
    }
}

/// The pyramid of one grid of a recording.
pub struct Pyramid {
    id: GridId,
    grid: Grid,
    top: Level,
    storage: Box<dyn PyramidStorage>,
}

impl std::fmt::Debug for Pyramid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pyramid")
            .field("id", &self.id)
            .field("top", &self.top)
            .finish_non_exhaustive()
    }
}

impl Pyramid {
    /// Builds the pyramid of `grid` in memory, in one pass.
    pub fn build(recording: &Recording, grid: GridId) -> Result<Pyramid, ReadError> {
        PyramidBuilder::new(recording, grid)?.finish()
    }

    pub(crate) fn new(id: GridId, grid: Grid, storage: Box<dyn PyramidStorage>) -> Pyramid {
        let top = max_level(grid.len());
        Pyramid {
            id,
            grid,
            top,
            storage,
        }
    }

    pub fn grid(&self) -> GridId {
        self.id
    }

    /// The highest level: one tile covers the whole grid. [`Level::RAW`] for grids of at most
    /// [`TILE_LEN`] samples, which have no pyramid levels.
    pub fn top(&self) -> Level {
        self.top
    }

    /// The tile `key` (level 1 to [`top`](Self::top), raw signal version, this grid), or
    /// `None` if the key is outside the pyramid or the tile is not stored.
    pub fn tile(&self, key: TileKey) -> Option<Tile> {
        let level = key.level;
        if key.version != SignalVersion::RAW
            || key.block.grid != self.id
            || level == Level::RAW
            || level > self.top
            || key.block.index >= ChannelBlock::count(&self.grid)
        {
            return None;
        }
        let buckets = self.grid.len().div_ceil(level.bucket_len());
        let first = key.time.checked_mul(TILE_LEN)?;
        if first >= buckets {
            return None;
        }
        let len = (buckets - first).min(TILE_LEN) as usize;
        let values = self.storage.tile(level, key.time)?;
        let rows = key.block.index as usize * CHANNEL_BLOCK;
        let channels = key.block.channels(&self.grid).len();
        if values.len() != self.grid.channels().len() * len * 2 {
            return None;
        }
        let data = values.to_data(rows * len * 2..(rows + channels) * len * 2);
        let valid = self.valid(level, first, len as u64);
        Some(Tile::new(key, channels as u32, len as u32, data, valid))
    }

    /// The buckets `first..first + len` of `level` that hold at least one present sample,
    /// relative to `first`.
    fn valid(&self, level: Level, first: u64, len: u64) -> Vec<Range<u32>> {
        let bucket = level.bucket_len();
        let samples = first * bucket..((first + len) * bucket).min(self.grid.len());
        let mut valid: Vec<Range<u32>> = Vec::new();
        for p in self.grid.present_in(samples) {
            // Offsets within one tile fit in u32.
            let a = (p.start / bucket - first) as u32;
            let b = (p.end.div_ceil(bucket) - first) as u32;
            match valid.last_mut() {
                Some(last) if last.end >= a => last.end = last.end.max(b),
                _ => valid.push(a..b),
            }
        }
        valid
    }

    /// Sum of the native values of `channel` over the present samples of block `block`
    /// (samples `block · TILE_LEN ..`), or `None` if the channel is not in this grid or the
    /// block is not stored.
    pub fn block_sum(&self, channel: ChannelId, block: u64) -> Option<f64> {
        let row = self.grid.channels().iter().position(|&c| c == channel)?;
        self.storage.sums(block)?.get(row).copied()
    }
}
