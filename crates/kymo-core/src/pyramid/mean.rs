//! Window means for DC removal, from the block sums.

use std::ops::Range;

use super::Pyramid;
use crate::tile::TILE_LEN;
use crate::{ChannelId, Grid, GridId, ReadError, Recording, Sample, SampleType};

impl Pyramid {
    /// Mean of `channel` over the present samples of `range`, in physical units, or `None`
    /// if no sample in `range` is present.
    ///
    /// Whole blocks inside `range` come from the block sums; the partial blocks at the two
    /// edges (and any block whose sum is not stored) are decoded from `recording`, which must
    /// be the recording the pyramid was built from.
    pub fn window_mean(
        &self,
        recording: &Recording,
        channel: ChannelId,
        range: Range<u64>,
    ) -> Result<Option<f64>, ReadError> {
        let grid = &self.grid;
        let Some(row) = grid.channels().iter().position(|&c| c == channel) else {
            return Err(ReadError::ChannelNotInGrid {
                channel,
                grid: self.id,
            });
        };
        if range.start > range.end || range.end > grid.len() {
            return Err(ReadError::OutOfRange {
                range,
                len: grid.len(),
            });
        }
        let count: u64 = grid
            .present_in(range.clone())
            .map(|p| p.end - p.start)
            .sum();
        if count == 0 {
            return Ok(None);
        }

        // Blocks `first..last` lie entirely inside the range.
        let first = range.start.div_ceil(TILE_LEN);
        let last = range.end / TILE_LEN;
        let mut sum = 0.0;
        if first >= last {
            sum += raw_sum(recording, self.id, grid, channel, range)?;
        } else {
            sum += raw_sum(
                recording,
                self.id,
                grid,
                channel,
                range.start..first * TILE_LEN,
            )?;
            for block in first..last {
                match self.row_sum(block, row) {
                    Some(s) => sum += s,
                    None => {
                        let samples = block * TILE_LEN..(block + 1) * TILE_LEN;
                        sum += raw_sum(recording, self.id, grid, channel, samples)?;
                    }
                }
            }
            sum += raw_sum(
                recording,
                self.id,
                grid,
                channel,
                last * TILE_LEN..range.end,
            )?;
        }
        let c = recording.channel(channel);
        Ok(Some(sum / count as f64 * c.gain + c.offset))
    }

    /// How many samples [`window_mean`](Self::window_mean) over `range` would decode now:
    /// the partial blocks at its edges, and every whole block the build hasn't reached.
    /// Counts sample positions, gaps included; `range` is cut to the grid.
    pub fn window_mean_decodes(&self, range: Range<u64>) -> u64 {
        let len = self.grid.len();
        let range = range.start.min(len)..range.end.min(len);
        if range.start >= range.end {
            return 0;
        }
        let first = range.start.div_ceil(TILE_LEN);
        let last = range.end / TILE_LEN;
        if first >= last {
            return range.end - range.start;
        }
        // Whole blocks inside the range end before the grid does, so block `b` is built
        // once the watermark reaches `(b + 1) · TILE_LEN`.
        let built = (self.done() / TILE_LEN).clamp(first, last);
        (first * TILE_LEN - range.start) + (range.end - last * TILE_LEN) + (last - built) * TILE_LEN
    }
}

/// Sum of the native values of `channel` over the present samples of `range`.
fn raw_sum(
    recording: &Recording,
    id: GridId,
    grid: &Grid,
    channel: ChannelId,
    range: Range<u64>,
) -> Result<f64, ReadError> {
    match grid.sample_type() {
        SampleType::I16 => typed_sum::<i16>(recording, id, grid, channel, range),
        SampleType::I32 => typed_sum::<i32>(recording, id, grid, channel, range),
        SampleType::F32 => typed_sum::<f32>(recording, id, grid, channel, range),
    }
}

fn typed_sum<T: Sample>(
    recording: &Recording,
    id: GridId,
    grid: &Grid,
    channel: ChannelId,
    range: Range<u64>,
) -> Result<f64, ReadError> {
    let mut sum = 0.0;
    let mut buf = Vec::new();
    for part in grid.present_in(range) {
        // Edges and single blocks: at most TILE_LEN samples.
        buf.clear();
        buf.resize((part.end - part.start) as usize, T::default());
        recording.read_native(id, &[channel], part, T::as_samples_mut(&mut buf))?;
        for &x in &buf {
            sum += x.to_f64();
        }
    }
    Ok(sum)
}
