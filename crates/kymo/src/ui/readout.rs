//! The value under the pointer, from the tile cache alone: the UI thread never reads the
//! file.

use std::ops::Range;

use kymo_core::tile::{
    CHANNEL_BLOCK, ChannelBlock, Level, SignalVersion, TILE_LEN, Tile, TileCache, TileData,
    TileKey, View, exact_cover, level_at, max_level,
};
use kymo_core::{ChannelId, ChannelKind, Grid, Quantity, Recording, Unit};

use crate::types;

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
    /// The reading times `factor`, e.g. to another unit.
    pub fn scaled(self, factor: f64) -> Reading {
        match self {
            Reading::Value(v) => Reading::Value(v * factor),
            Reading::Range { min, max } => Reading::Range {
                min: min * factor,
                max: max * factor,
            },
        }
    }

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
/// The unit the readings of a channel of type `kind` in `unit` are shown in, and the factor
/// from `unit` to it. As MNE shows them: fT for magnetic fields, fT/cm for field gradients,
/// mV for sEEG and µV for the other types whose own quantity is voltage
/// ([`types::start`]), whatever the file stores (teslas, volts, ...).
/// Channels of other types and units the type has no unit for keep their own unit, as its
/// symbol ("microvolts" as µV). Stim channels, whose values are codes, have none.
pub fn display_unit(kind: &ChannelKind, unit: &Unit) -> (String, f64) {
    if kind.is_stim() {
        return (String::new(), 1.0);
    }
    let own = (unit.symbol(), 1.0);
    let (Some(quantity), Some(to_si)) = (unit.quantity(), unit.to_si) else {
        return own;
    };
    let voltage_type = types::start(kind).quantity == Some(Quantity::Voltage);
    let target = match quantity {
        Quantity::MagneticField => ("fT", 1e-15),
        Quantity::FieldGradient => ("fT/cm", 1e-13),
        Quantity::Voltage if *kind == ChannelKind::SEEG => ("mV", 1e-3),
        Quantity::Voltage if voltage_type => ("\u{b5}V", 1e-6),
        _ => return own,
    };
    (target.0.to_owned(), to_si / target.1)
}

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
    let level = level_at(grid, view, grid.sample_at(time).min(grid.len() - 1));
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

/// The smallest and largest physical value of `channel` over `samples` of its grid, gaps
/// and NaN left out, exact whatever the zoom: from the tiles of
/// [`exact_cover`](kymo_core::tile::exact_cover) at `version`, never a coarser tile's
/// wider bucket.
#[derive(Clone, Debug, PartialEq)]
pub enum Extremes {
    /// Every tile needed is cached: the extremes, `None` if no sample has a value.
    Ready(Option<(f64, f64)>),
    /// The tiles still to compute (or that failed) first.
    Missing(Vec<TileKey>),
}

pub fn extremes(
    recording: &Recording,
    cache: &TileCache,
    version: SignalVersion,
    channel: ChannelId,
    samples: Range<u64>,
) -> Extremes {
    let Some(info) = recording.channels().get(channel.0 as usize) else {
        return Extremes::Ready(None);
    };
    let grid = recording.grid(info.grid);
    let Some(row) = grid.channels().iter().position(|&c| c == channel) else {
        return Extremes::Ready(None);
    };
    let samples = samples.start.min(grid.len())..samples.end.min(grid.len());
    let block = ChannelBlock {
        grid: info.grid,
        index: (row / CHANNEL_BLOCK) as u32,
    };
    let (gain, offset) = info.tile_scaling();
    let mut missing = Vec::new();
    let mut found: Option<(f64, f64)> = None;
    for (level, items) in exact_cover(samples, max_level(grid.len())) {
        for time in items.start / TILE_LEN..items.end.div_ceil(TILE_LEN) {
            let key = TileKey {
                version,
                level,
                block,
                time,
            };
            let Some(tile) = cache.get(&key) else {
                // Both edges of a level may lie in one tile.
                if !missing.contains(&key) {
                    missing.push(key);
                }
                continue;
            };
            let first = time * TILE_LEN;
            let from = (items.start.max(first) - first) as usize;
            let to = (items.end.min(first + TILE_LEN) - first) as usize;
            for item in from..to {
                let (a, b) = match native(&tile, row % CHANNEL_BLOCK, item) {
                    Some(Native::Sample(v)) => (v, v),
                    Some(Native::Bucket(lo, hi)) => (lo, hi),
                    None => continue,
                };
                let (a, b) = (a * gain + offset, b * gain + offset);
                let (lo, hi) = (a.min(b), a.max(b));
                found = Some(found.map_or((lo, hi), |(l, h)| (l.min(lo), h.max(hi))));
            }
        }
    }
    if missing.is_empty() {
        Extremes::Ready(found)
    } else {
        Extremes::Missing(missing)
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
