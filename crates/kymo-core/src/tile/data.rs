//! Tile contents.

use std::mem::size_of;
use std::ops::Range;

use super::TileKey;
use crate::SampleType;

/// The values of a tile in the grid's native sample type, channel-major.
///
/// Level 0 holds raw samples: `data[c * len + i]`. Levels ≥ 1 hold a (min, max) pair per
/// bucket, interleaved: `data[2 * (c * len + i)]` is the minimum, the next value the maximum.
#[derive(Clone, Debug, PartialEq)]
pub enum TileData {
    I16(Box<[i16]>),
    I32(Box<[i32]>),
    F32(Box<[f32]>),
}

impl TileData {
    pub fn sample_type(&self) -> SampleType {
        match self {
            TileData::I16(_) => SampleType::I16,
            TileData::I32(_) => SampleType::I32,
            TileData::F32(_) => SampleType::F32,
        }
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        match self {
            TileData::I16(d) => d.len(),
            TileData::I32(d) => d.len(),
            TileData::F32(d) => d.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One tile: the values of a channel block over the tile's time span, at one level.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    key: TileKey,
    channels: u32,
    len: u32,
    data: TileData,
    valid: Vec<Range<u32>>,
}

impl Tile {
    pub(crate) fn new(
        key: TileKey,
        channels: u32,
        len: u32,
        data: TileData,
        valid: Vec<Range<u32>>,
    ) -> Tile {
        Tile {
            key,
            channels,
            len,
            data,
            valid,
        }
    }

    pub fn key(&self) -> TileKey {
        self.key
    }

    /// Number of channels: those of the key's channel block.
    pub fn channels(&self) -> usize {
        self.channels as usize
    }

    /// Samples (level 0) or buckets (levels ≥ 1) per channel: [`TILE_LEN`](super::TILE_LEN),
    /// or fewer in the last tile of a grid.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn data(&self) -> &TileData {
        &self.data
    }

    /// Where the tile has data, relative to its first sample or bucket: the present samples
    /// (level 0), or the buckets holding at least one present sample (levels ≥ 1). Sorted,
    /// disjoint and non-touching; empty if the tile lies entirely in a gap. Values outside
    /// these ranges are meaningless (zero at level 0).
    pub fn valid(&self) -> &[Range<u32>] {
        &self.valid
    }

    /// Memory held by the tile, for the cache budget.
    pub fn bytes(&self) -> usize {
        size_of::<Tile>()
            + self.data.len() * self.data.sample_type().size()
            + self.valid.capacity() * size_of::<Range<u32>>()
    }
}
