//! Tile contents.

use std::mem::size_of;
use std::ops::Range;

use super::TileKey;
use crate::{Sample, SampleType};

/// The values of a tile in the grid's native sample type, channel-major.
///
/// A channel with a [`trigger_mask`](crate::Channel::trigger_mask) holds its masked codes
/// instead of its raw values (see [`Channel::tile_scaling`](crate::Channel::tile_scaling)).
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

    pub fn as_slice(&self) -> TileSlice<'_> {
        match self {
            TileData::I16(d) => TileSlice::I16(d),
            TileData::I32(d) => TileSlice::I32(d),
            TileData::F32(d) => TileSlice::F32(d),
        }
    }
}

/// Borrowed tile values, laid out as in [`TileData`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TileSlice<'a> {
    I16(&'a [i16]),
    I32(&'a [i32]),
    F32(&'a [f32]),
}

impl TileSlice<'_> {
    pub fn len(&self) -> usize {
        match self {
            TileSlice::I16(d) => d.len(),
            TileSlice::I32(d) => d.len(),
            TileSlice::F32(d) => d.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copies `range` of the values.
    ///
    /// # Panics
    /// If `range` is out of bounds.
    pub fn to_data(&self, range: Range<usize>) -> TileData {
        match self {
            TileSlice::I16(d) => TileData::I16(d[range].into()),
            TileSlice::I32(d) => TileData::I32(d[range].into()),
            TileSlice::F32(d) => TileData::F32(d[range].into()),
        }
    }
}

/// Native sample types that can be stored in a [`TileData`].
pub(crate) trait TileSample: Sample {
    fn into_data(values: Box<[Self]>) -> TileData;
    /// The value with only the bits of `mask` kept, as a stim code.
    fn masked(self, mask: u32) -> Self;
}

impl TileSample for i16 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::I16(values)
    }

    fn masked(self, mask: u32) -> Self {
        // Reinterpreting the bits: the mask applies to the two's-complement value.
        self & mask as i16
    }
}

impl TileSample for i32 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::I32(values)
    }

    fn masked(self, mask: u32) -> Self {
        self & mask as i32
    }
}

impl TileSample for f32 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::F32(values)
    }

    /// Truncated toward zero and saturated as an integer first; NaN stays NaN.
    fn masked(self, mask: u32) -> Self {
        if self.is_nan() {
            self
        } else {
            (self as i32 & mask as i32) as f32
        }
    }
}

/// Masks the rows (`len` values each) of the channels in `channels` that have a
/// [`trigger_mask`](crate::Channel::trigger_mask).
pub(crate) fn mask_rows<T: TileSample>(
    recording: &crate::Recording,
    channels: &[crate::ChannelId],
    rows: &mut [T],
    len: usize,
) {
    if len == 0 {
        return;
    }
    for (&id, row) in channels.iter().zip(rows.chunks_exact_mut(len)) {
        if let Some(mask) = recording.channel(id).trigger_mask {
            for x in row {
                *x = x.masked(mask);
            }
        }
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
