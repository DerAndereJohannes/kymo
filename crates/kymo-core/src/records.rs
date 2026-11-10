//! The block reader of formats with data records (EDF, BDF, GDF): every record holds one
//! block of samples per channel, each channel in its own encoding.

use std::fs::File;
use std::ops::Range;

use memmap2::Mmap;

use crate::decode::Decoder;
use crate::mapped;
use crate::{BlockReader, ChannelId, GridId, ReadError, Sample, SampleType, SamplesMut};

/// Where one channel's samples sit inside a data record, and how they are stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RecordBlock {
    /// Byte offset from the start of the record.
    pub offset: u64,
    /// Samples per record.
    pub samples: u64,
    pub decoder: Decoder,
}

/// Reads samples straight from the memory-mapped file, record by record.
#[derive(Debug)]
pub(crate) struct RecordReader {
    pub map: Mmap,
    /// The mapped file, for reading ahead.
    pub file: File,
    /// Where the first record starts.
    pub header_bytes: u64,
    pub record_bytes: u64,
    /// Records the reader may use.
    pub n_records: u64,
    /// One per data channel, in [`ChannelId`] order. The channels of a grid share samples
    /// per record and native type.
    pub blocks: Vec<RecordBlock>,
    /// The grid of each data channel.
    pub channel_grid: Vec<GridId>,
    /// Discontinuous files (EDF+D/BDF+D): per grid, the first grid sample of each record.
    pub record_starts: Option<Vec<Vec<u64>>>,
}

impl BlockReader for RecordReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        let blocks = channels
            .iter()
            .map(|&channel| match self.channel_grid.get(channel.0 as usize) {
                Some(&g) if g == grid => Ok(self.blocks[channel.0 as usize]),
                _ => Err(ReadError::ChannelNotInGrid { channel, grid }),
            })
            .collect::<Result<Vec<RecordBlock>, _>>()?;
        let n = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(usize::MAX);
        let expected = blocks.len().saturating_mul(n);
        let native = blocks
            .first()
            .map_or(SampleType::I16, |b| b.decoder.native());
        match out {
            SamplesMut::I16(dst) if dst.len() == expected => {
                self.copy(grid, &blocks, range, n, dst)
            }
            SamplesMut::I32(dst) if dst.len() == expected => {
                self.copy(grid, &blocks, range, n, dst)
            }
            SamplesMut::F32(dst) if dst.len() == expected => {
                self.copy(grid, &blocks, range, n, dst)
            }
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type: native,
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        let Some(records) = self.records(grid, &range) else {
            return;
        };
        let start = self.header_bytes + records.start * self.record_bytes;
        let end = self.header_bytes + records.end * self.record_bytes;
        mapped::prefetch(&self.file, self.map.len() as u64, [start..end]);
    }
}

impl RecordReader {
    /// The records holding samples of `range` of `grid`, if the grid has channels.
    fn records(&self, grid: GridId, range: &Range<u64>) -> Option<Range<u64>> {
        let channel = self.channel_grid.iter().position(|&g| g == grid)?;
        let spr = self.blocks[channel].samples;
        let n = self.n_records;
        let records = match &self.record_starts {
            Some(starts) => {
                let starts = &starts[grid.0 as usize];
                let first = starts
                    .partition_point(|&s| s <= range.start)
                    .saturating_sub(1);
                first as u64..starts.partition_point(|&s| s < range.end) as u64
            }
            None if spr == 0 => return None,
            None => range.start / spr..range.end.div_ceil(spr),
        };
        Some(records.start.min(n)..records.end.min(n))
    }

    /// Copies `range` of every block into `dst` (channel-major, `n` samples per channel),
    /// record by record so the file is read in order.
    fn copy<T: Sample>(
        &self,
        grid: GridId,
        blocks: &[RecordBlock],
        range: Range<u64>,
        n: usize,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let Some(spr) = blocks.first().map(|b| b.samples) else {
            return Ok(());
        };
        let starts = self
            .record_starts
            .as_ref()
            .map(|s| s[grid.0 as usize].as_slice());
        let not_present = || ReadError::NotPresent {
            grid,
            range: range.clone(),
        };
        let mut record = match starts {
            Some(starts) => starts
                .partition_point(|&s| s <= range.start)
                .checked_sub(1)
                .ok_or_else(not_present)?,
            None => (range.start / spr) as usize,
        };
        let mut i = range.start;
        while i < range.end {
            let first = match starts {
                Some(starts) => *starts.get(record).ok_or_else(not_present)?,
                None => record as u64 * spr,
            };
            if record as u64 >= self.n_records || i < first || i - first >= spr {
                return Err(not_present());
            }
            let j = i - first;
            let take = (spr - j).min(range.end - i);
            let record_start = self.header_bytes + record as u64 * self.record_bytes;
            let at = (i - range.start) as usize;
            for (k, block) in blocks.iter().enumerate() {
                let size = block.decoder.encoding().size();
                let start = record_start + block.offset + j * size as u64;
                let bytes = (start as usize)
                    .checked_add(take as usize * size)
                    .and_then(|end| self.map.get(start as usize..end))
                    .ok_or_else(|| {
                        ReadError::Corrupt(format!("record {record} is outside the file"))
                    })?;
                let samples = &mut dst[k * n + at..][..take as usize];
                block.decoder.decode(bytes, size, samples)?;
            }
            i += take;
            record += 1;
        }
        Ok(())
    }
}
