//! Test support: an in-memory [`BlockReader`], the brute-force min/max reference and
//! hand-made tiles. Enabled by the `testing` feature.
//!
//! The reader checks the [`BlockReader`] contract strictly (for example, it fails on any
//! request that touches a gap), so tests also catch callers that break it.

use std::collections::HashMap;
use std::ops::Range;

use crate::tile::{Level, Tile, TileData, TileKey};
use crate::{
    BlockReader, Channel, ChannelId, Grid, GridId, Header, Opened, ReadError, Recording, Sample,
    SampleType, SamplesMut, Timebase,
};

/// Samples of one grid, one row per channel in [`Grid::channels`] order.
///
/// Every row has [`Grid::len`] samples; values in gaps are ignored.
#[derive(Clone, Debug, PartialEq)]
pub enum MemData {
    I16(Vec<Vec<i16>>),
    I32(Vec<Vec<i32>>),
    F32(Vec<Vec<f32>>),
}

impl MemData {
    pub fn sample_type(&self) -> SampleType {
        match self {
            MemData::I16(_) => SampleType::I16,
            MemData::I32(_) => SampleType::I32,
            MemData::F32(_) => SampleType::F32,
        }
    }

    /// Number of rows (channels).
    pub fn rows(&self) -> usize {
        match self {
            MemData::I16(rows) => rows.len(),
            MemData::I32(rows) => rows.len(),
            MemData::F32(rows) => rows.len(),
        }
    }

    /// Sample `i` of row `row`, as f64.
    pub fn get(&self, row: usize, i: usize) -> f64 {
        match self {
            MemData::I16(rows) => f64::from(rows[row][i]),
            MemData::I32(rows) => f64::from(rows[row][i]),
            MemData::F32(rows) => f64::from(rows[row][i]),
        }
    }

    fn row_lens(&self) -> Vec<usize> {
        match self {
            MemData::I16(rows) => rows.iter().map(Vec::len).collect(),
            MemData::I32(rows) => rows.iter().map(Vec::len).collect(),
            MemData::F32(rows) => rows.iter().map(Vec::len).collect(),
        }
    }
}

/// A [`BlockReader`] over samples held in memory.
#[derive(Debug)]
pub struct MemReader {
    grids: Vec<(Vec<Range<u64>>, MemData)>,
    rows: HashMap<ChannelId, (GridId, usize)>,
}

impl MemReader {
    /// `data[g]` holds the samples of `grids[g]`.
    ///
    /// # Panics
    /// If the data does not match the grids in count, sample type, rows or row length.
    pub fn new(grids: &[Grid], data: Vec<MemData>) -> Self {
        assert_eq!(grids.len(), data.len(), "one MemData per grid");
        let mut rows = HashMap::new();
        for (g, (grid, d)) in grids.iter().zip(&data).enumerate() {
            assert_eq!(grid.sample_type(), d.sample_type(), "grid {g}: sample type");
            assert_eq!(
                grid.channels().len(),
                d.rows(),
                "grid {g}: one row per channel"
            );
            for (row, len) in d.row_lens().into_iter().enumerate() {
                assert_eq!(len as u64, grid.len(), "grid {g}, row {row}: length");
            }
            for (row, &channel) in grid.channels().iter().enumerate() {
                rows.insert(channel, (GridId(g as u32), row));
            }
        }
        let grids = grids
            .iter()
            .map(|g| g.segments().to_vec())
            .zip(data)
            .collect();
        MemReader { grids, rows }
    }
}

impl BlockReader for MemReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        let (segments, data) = self
            .grids
            .get(grid.0 as usize)
            .ok_or(ReadError::UnknownGrid(grid))?;
        let inside_segment = segments
            .iter()
            .any(|s| s.start <= range.start && range.end <= s.end);
        if range.is_empty() || !inside_segment {
            return Err(ReadError::NotPresent { grid, range });
        }
        let rows = channels
            .iter()
            .map(|&channel| match self.rows.get(&channel) {
                Some(&(g, row)) if g == grid => Ok(row),
                _ => Err(ReadError::ChannelNotInGrid { channel, grid }),
            })
            .collect::<Result<Vec<_>, _>>()?;

        let n = (range.end - range.start) as usize;
        let expected = rows.len() * n;
        let mismatch = |found: usize, found_type| ReadError::BufferMismatch {
            expected,
            expected_type: data.sample_type(),
            found,
            found_type,
        };
        let samples = range.start as usize..range.end as usize;
        match (data, out) {
            (MemData::I16(d), SamplesMut::I16(out)) if out.len() == expected => {
                copy_rows(d, &rows, samples, out)
            }
            (MemData::I32(d), SamplesMut::I32(out)) if out.len() == expected => {
                copy_rows(d, &rows, samples, out)
            }
            (MemData::F32(d), SamplesMut::F32(out)) if out.len() == expected => {
                copy_rows(d, &rows, samples, out)
            }
            (_, out) => return Err(mismatch(out.len(), out.sample_type())),
        }
        Ok(())
    }
}

fn copy_rows<T: Copy>(data: &[Vec<T>], rows: &[usize], samples: Range<usize>, out: &mut [T]) {
    let n = samples.len();
    for (dst, &row) in out.chunks_exact_mut(n).zip(rows) {
        dst.copy_from_slice(&data[row][samples.clone()]);
    }
}

/// The smallest and largest native value of `channel` over the present samples in `range`,
/// by brute force: the reference that tiles and the pyramid are checked against. For a
/// channel with a [`trigger_mask`](crate::Channel::trigger_mask), of the masked codes (the
/// two's-complement bits of the native integer, or of a float truncated toward zero).
///
/// NaN samples are skipped. `None` if no sample in `range` is present (or all are NaN).
///
/// # Panics
/// If `channel` is not in the recording.
pub fn brute_minmax(
    recording: &Recording,
    channel: ChannelId,
    range: Range<u64>,
) -> Result<Option<(f64, f64)>, ReadError> {
    let grid_id = recording.channel(channel).grid;
    let grid = recording.grid(grid_id);
    let values = match grid.sample_type() {
        SampleType::I16 => native::<i16>(recording, grid_id, channel, range.clone())?,
        SampleType::I32 => native::<i32>(recording, grid_id, channel, range.clone())?,
        SampleType::F32 => native::<f32>(recording, grid_id, channel, range.clone())?,
    };
    let mask = recording.channel(channel).trigger_mask;
    let code = |x: f64| match mask {
        Some(m) if !x.is_nan() => match grid.sample_type() {
            SampleType::I16 => f64::from(x as i16 & m as i16),
            _ => f64::from(x as i32 & m as i32),
        },
        _ => x,
    };
    let mut minmax: Option<(f64, f64)> = None;
    for (i, x) in range.zip(values.into_iter().map(code)) {
        // Checked per sample against the segments, independent of `Grid::present_in`.
        if x.is_nan() || !grid.segments().iter().any(|s| s.contains(&i)) {
            continue;
        }
        minmax = Some(match minmax {
            None => (x, x),
            Some((lo, hi)) => (lo.min(x), hi.max(x)),
        });
    }
    Ok(minmax)
}

fn native<T: Sample>(
    recording: &Recording,
    grid: GridId,
    channel: ChannelId,
    range: Range<u64>,
) -> Result<Vec<f64>, ReadError> {
    let len = usize::try_from(range.end.saturating_sub(range.start)).map_err(|_| {
        ReadError::OutOfRange {
            range: range.clone(),
            len: recording.grid(grid).len(),
        }
    })?;
    let mut buf = vec![T::default(); len];
    recording.read_native(grid, &[channel], range, T::as_samples_mut(&mut buf))?;
    Ok(buf.into_iter().map(Sample::to_f64).collect())
}

/// A tile with the given contents, for filling caches by hand: `channels` rows of `len`
/// samples (level 0) or (min, max) pairs (levels ≥ 1), laid out as in [`TileData`].
///
/// # Panics
/// If `data` does not hold `channels · len` values (twice that above level 0), or `valid`
/// is not sorted, disjoint and inside `0..len`.
pub fn tile(
    key: TileKey,
    channels: usize,
    len: usize,
    data: TileData,
    valid: Vec<Range<u32>>,
) -> Tile {
    let per_bucket = if key.level == Level::RAW { 1 } else { 2 };
    assert_eq!(data.len(), channels * len * per_bucket, "tile data length");
    let mut end = 0;
    for v in &valid {
        assert!(
            end <= v.start && v.start < v.end,
            "valid ranges sorted and disjoint"
        );
        end = v.end;
    }
    assert!(end as usize <= len, "valid ranges inside the tile");
    Tile::new(key, channels as u32, len as u32, data, valid)
}

/// Wraps channels, grids and their data as a reader's [`Opened`] (format `"MEM"`).
///
/// # Panics
/// As [`MemReader::new`].
pub fn opened(channels: Vec<Channel>, grids: Vec<Grid>, data: Vec<MemData>) -> Opened {
    let reader = MemReader::new(&grids, data);
    Opened {
        header: Header {
            format: "MEM",
            start_time: None,
            channels,
            grids,
            extra: Vec::new(),
        },
        annotations: Vec::new(),
        warnings: Vec::new(),
        sources: Vec::new(),
        reader: Box::new(reader),
    }
}

/// A recording with one gap-free grid at `rate`, channels named `ch0`, `ch1`, ...
///
/// # Panics
/// If `rate` is invalid, `data` has no rows or its rows differ in length.
pub fn single_grid(rate: f64, data: MemData) -> Opened {
    let len = data.row_lens().first().copied().unwrap_or(0) as u64;
    let ids: Vec<ChannelId> = (0..data.rows() as u32).map(ChannelId).collect();
    let channels = ids
        .iter()
        .map(|id| Channel::new(format!("ch{}", id.0), GridId(0)))
        .collect();
    let grid = Grid::continuous(
        Timebase::Regular { rate, start: 0.0 },
        len,
        data.sample_type(),
        ids,
    )
    .expect("valid single grid");
    opened(channels, vec![grid], vec![data])
}
