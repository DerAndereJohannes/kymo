//! Tile contents.

use std::mem::size_of;
use std::ops::Range;

use super::TileKey;
use crate::SampleType;
use crate::sample::NativeSample;

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

/// Masks the rows (`len` values each) of the channels in `channels` that have a
/// [`trigger_mask`](crate::Channel::trigger_mask).
pub(crate) fn mask_rows<T: NativeSample>(
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
    times: Option<Box<[f64]>>,
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
            times: None,
        }
    }

    /// The tile with its items' times if `grid` has an irregular time base.
    pub(crate) fn with_times(mut self, grid: &crate::Grid) -> Tile {
        if let Some(table) = grid.times() {
            let first = self.key.time * super::TILE_LEN;
            self.times = Some(
                (first..=first + u64::from(self.len))
                    .map(|k| table.bucket_time(self.key.level, k))
                    .collect(),
            );
        }
        self
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

    /// For a grid with an irregular time base, where each item starts (seconds since the
    /// recording start), and after them where the last one ends: `len + 1` times, the last
    /// the next item's start (or the grid's end). A bucket starts at its first slot's time.
    pub fn times(&self) -> Option<&[f64]> {
        self.times.as_deref()
    }

    /// Memory held by the tile, for the cache budget.
    pub fn bytes(&self) -> usize {
        size_of::<Tile>()
            + self.data.len() * self.data.sample_type().size()
            + self.valid.capacity() * size_of::<Range<u32>>()
            + self
                .times
                .as_ref()
                .map_or(0, |t| t.len() * size_of::<f64>())
    }
}
