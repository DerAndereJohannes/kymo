//! The value under the pointer, from the tile cache alone: the UI thread never reads the
//! file.

use kymo_core::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, Tile, TileCache, TileData, TileKey, View, level_for,
    max_level,
};
use kymo_core::{ChannelId, Grid, Recording};

/// What a channel shows at one time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// The sample nearest to the time, in physical units.
    Value(f64),
    /// The envelope drawn there: the smallest and largest physical value of a bucket.
    Range { min: f64, max: f64 },
}

impl Reading {
    /// The reading measured from `baseline`, as the trace is drawn with DC removal.
    pub fn relative_to(self, baseline: f64) -> Reading {
        match self {
            Reading::Value(v) => Reading::Value(v - baseline),
            Reading::Range { min, max } => Reading::Range {
                min: min - baseline,
                max: max - baseline,
            },
        }
    }
}

/// What `channel` shows at `time` (seconds from the start) in `view`: the nearest sample
/// if the view draws samples, else the envelope of the bucket holding `time`. Read from
/// the view's tile or, as the trace area does, its nearest cached coarser tile. `None` in
/// gaps, for NaN, outside the recording, or when no tile is cached.
pub fn read(
    recording: &Recording,
    cache: &TileCache,
    view: &View,
    channel: ChannelId,
    time: f64,
) -> Option<Reading> {
    let info = recording.channels().get(channel.0 as usize)?;
    let grid = recording.grid(info.grid);
    let row = grid.channels().iter().position(|&c| c == channel)?;
    if !time.is_finite() || grid.is_empty() {
        return None;
    }
    let level = level_for(grid, view);
    let sample = if level == Level::RAW {
        nearest_sample(grid, time)?
    } else {
        let s = grid.sample_at(time);
        (s < grid.len() && time >= grid.time_of(0)).then_some(s)?
    };
    let block = ChannelBlock {
        grid: info.grid,
        index: (row / CHANNEL_BLOCK) as u32,
    };
    let top = max_level(grid.len());
    let mut key = TileKey {
        version: view.version,
        level,
        block,
        time: level.tile_of(sample),
    };
    let tile = loop {
        if let Some(tile) = cache.get(&key) {
            break tile;
        }
        key = key.parent().filter(|k| k.level <= top)?;
    };
    let item = (sample - key.sample_range().start) / key.level.bucket_len();
    let (gain, offset) = info.tile_scaling();
    let physical = |native: f64| native * gain + offset;
    match native(&tile, row % CHANNEL_BLOCK, item as usize)? {
        Native::Sample(v) => Some(Reading::Value(physical(v))),
        Native::Bucket(lo, hi) => {
            let (a, b) = (physical(lo), physical(hi));
            Some(Reading::Range {
                min: a.min(b),
                max: a.max(b),
            })
        }
    }
}

/// The sample drawn nearest to `time`, if `time` lies within the grid's samples.
fn nearest_sample(grid: &Grid, time: f64) -> Option<u64> {
    let last = grid.len() - 1;
    if time < grid.time_of(0) || time > grid.time_of(last) {
        return None;
    }
    let s = grid.sample_at(time).min(last);
    let next = (s + 1).min(last);
    let closer = (grid.time_of(next) - time).abs() < (time - grid.time_of(s)).abs();
    Some(if closer { next } else { s })
}

enum Native {
    Sample(f64),
    Bucket(f64, f64),
}

/// Item `item` of row `row` of a tile, as f64; `None` outside its valid ranges or for NaN.
fn native(tile: &Tile, row: usize, item: usize) -> Option<Native> {
    if row >= tile.channels() || item >= tile.len() {
        return None;
    }
    let i = u32::try_from(item).ok()?;
    if !tile.valid().iter().any(|r| r.contains(&i)) {
        return None;
    }
    let at = |index: usize| -> f64 {
        match tile.data() {
            TileData::I16(d) => f64::from(d[index]),
            TileData::I32(d) => f64::from(d[index]),
            TileData::F32(d) => f64::from(d[index]),
        }
    };
    let index = row * tile.len() + item;
    let value = if tile.key().level == Level::RAW {
        Native::Sample(at(index))
    } else {
        Native::Bucket(at(2 * index), at(2 * index + 1))
    };
    match value {
        Native::Sample(v) if v.is_nan() => None,
        Native::Bucket(a, b) if a.is_nan() || b.is_nan() => None,
        value => Some(value),
    }
}
