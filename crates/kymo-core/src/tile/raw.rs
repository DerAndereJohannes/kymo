//! Level-0 tiles: raw samples decoded from the recording.

use std::ops::Range;

use super::{ChannelBlock, Level, SignalVersion, Tile, TileData, TileKey};
use crate::{ChannelId, GridId, ReadError, Recording, Sample, SampleType, TileError};

/// Decodes the level-0 tile `key` from the recording: native samples, zero in gaps.
///
/// The last tile of a grid is cut at the grid's end.
pub fn compute_raw(recording: &Recording, key: TileKey) -> Result<Tile, TileError> {
    if key.level != Level::RAW {
        return Err(TileError::NotRaw(key));
    }
    if key.version != SignalVersion::RAW {
        return Err(TileError::UnknownVersion(key));
    }
    let grid = recording
        .grids()
        .get(key.block.grid.0 as usize)
        .filter(|g| key.block.index < ChannelBlock::count(g))
        .ok_or(TileError::UnknownBlock(key))?;
    let span = key.sample_range();
    if span.start >= grid.len() {
        return Err(TileError::PastEnd {
            key,
            len: grid.len(),
        });
    }
    let range = span.start..span.end.min(grid.len());
    let channels = key.block.channels(grid);
    let id = key.block.grid;
    let data = match grid.sample_type() {
        SampleType::I16 => TileData::I16(read(recording, id, channels, range.clone())?),
        SampleType::I32 => TileData::I32(read(recording, id, channels, range.clone())?),
        SampleType::F32 => TileData::F32(read(recording, id, channels, range.clone())?),
    };
    // Offsets within one tile fit in u32: a tile spans TILE_LEN samples.
    let valid = grid
        .present_in(range.clone())
        .map(|p| (p.start - range.start) as u32..(p.end - range.start) as u32)
        .collect();
    let len = (range.end - range.start) as u32;
    Ok(Tile::new(key, channels.len() as u32, len, data, valid))
}

fn read<T: Sample>(
    recording: &Recording,
    grid: GridId,
    channels: &[ChannelId],
    range: Range<u64>,
) -> Result<Box<[T]>, ReadError> {
    let mut buf = vec![T::default(); channels.len() * (range.end - range.start) as usize];
    recording.read_native(grid, channels, range, T::as_samples_mut(&mut buf))?;
    Ok(buf.into_boxed_slice())
}
