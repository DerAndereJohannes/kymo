//! The EDF/BDF block reader over a read-only memory map.

use std::fs::File;
use std::ops::Range;
use std::path::Path;

use memmap2::Mmap;

use super::index::index;
use super::{Block, EdfError, RecordLayout, parse_header};
use crate::decode::{ByteOrder, Decoder, Encoding};
use crate::{BlockReader, ChannelId, GridId, OpenError, Opened, ReadError, Sample, SamplesMut};

/// Reads samples straight from the memory-mapped file.
#[derive(Debug)]
pub struct EdfReader {
    map: Mmap,
    /// The mapped file, for reading ahead.
    file: File,
    layout: RecordLayout,
    /// The grid of each data channel.
    channel_grid: Vec<GridId>,
    /// EDF+D/BDF+D: per grid, the first grid sample of each record.
    record_starts: Option<Vec<Vec<u64>>>,
}

/// Opens an EDF, EDF+ or BDF/BDF+ file. The file is memory-mapped, never loaded.
///
/// For EDF+/BDF+ this reads the annotation signal of every record once: the time-keeping
/// TALs give the record onsets (a sub-second start, and the gaps of EDF+D/BDF+D), the other
/// TALs the annotations.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |source| OpenError::Io {
        path: path.to_owned(),
        source,
    };
    let edf = |source| OpenError::Edf {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    if len < 256 {
        return Err(edf(EdfError::Truncated {
            needed: 256,
            found: len,
        }));
    }
    let map = map_read_only(&file).map_err(io)?;
    let mut parsed = parse_header(&map, map.len() as u64).map_err(edf)?;
    let (mut annotations, mut record_starts) = (Vec::new(), None);
    if !parsed.layout.annotations.is_empty() {
        let found =
            index(&map, &parsed.header, &parsed.layout, &mut parsed.warnings).map_err(edf)?;
        parsed.header.grids = found.grids;
        annotations = found.annotations;
        record_starts = found.record_starts;
    } else if parsed.layout.discontinuous {
        return Err(edf(EdfError::InvalidRecord {
            record: 0,
            reason: "has no annotation signal, which EDF+D needs for its record times".into(),
        }));
    }
    let channel_grid = parsed.header.channels.iter().map(|c| c.grid).collect();
    Ok(Opened {
        header: parsed.header,
        annotations,
        warnings: parsed.warnings,
        sources: vec![path.to_owned()],
        reader: Box::new(EdfReader {
            map,
            file,
            layout: parsed.layout,
            channel_grid,
            record_starts,
        }),
    })
}

#[allow(unsafe_code)]
fn map_read_only(file: &File) -> std::io::Result<Mmap> {
    // SAFETY: the mapping is read-only and kymo never writes to source files. Memory mapping
    // is only unsound if another process modifies or truncates the file while it is mapped;
    // like every viewer that maps its input, we accept that (design: source files are
    // memory-mapped read-only and never modified). All reads go through bounds-checked slices.
    unsafe { Mmap::map(file) }
}

impl BlockReader for EdfReader {
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
                Some(&g) if g == grid => Ok(self.layout.channels[channel.0 as usize]),
                _ => Err(ReadError::ChannelNotInGrid { channel, grid }),
            })
            .collect::<Result<Vec<Block>, _>>()?;
        let n = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(usize::MAX);
        let expected = blocks.len().saturating_mul(n);
        let encoding = match self.layout.bytes_per_sample {
            3 => Encoding::I24,
            _ => Encoding::I16,
        };
        let decoder = Decoder::new(encoding, ByteOrder::Little);
        match out {
            SamplesMut::I16(dst) if dst.len() == expected => {
                self.copy(grid, &blocks, range, n, dst, &decoder)
            }
            SamplesMut::I32(dst) if dst.len() == expected => {
                self.copy(grid, &blocks, range, n, dst, &decoder)
            }
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type: decoder.native(),
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        let Some(records) = self.records(grid, &range) else {
            return;
        };
        let layout = &self.layout;
        let start = layout.header_bytes + records.start * layout.record_bytes;
        let end =
            (layout.header_bytes + records.end * layout.record_bytes).min(self.map.len() as u64);
        // Plain reads fill the page cache much faster than page faults on the mapping
        // (on macOS about 6 against 2 GB/s, even with madvise). Errors only cost speed.
        let mut buf = vec![0; (end.saturating_sub(start) as usize).min(PREFETCH_PIECE)];
        let mut at = start;
        while at < end {
            let piece = &mut buf[..(end - at).min(PREFETCH_PIECE as u64) as usize];
            match read_at(&self.file, piece, at) {
                Ok(0) | Err(_) => return,
                Ok(read) => at += read as u64,
            }
        }
    }
}

/// Bytes read per call when prefetching.
const PREFETCH_PIECE: usize = 8 << 20;

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(_: &File, _: &mut [u8], _: u64) -> std::io::Result<usize> {
    Ok(0)
}

impl EdfReader {
    /// The records holding samples of `range` of `grid`, if the grid has channels.
    fn records(&self, grid: GridId, range: &Range<u64>) -> Option<Range<u64>> {
        let channel = self.channel_grid.iter().position(|&g| g == grid)?;
        let spr = self.layout.channels[channel].samples;
        let n = self.layout.n_records;
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
        blocks: &[Block],
        range: Range<u64>,
        n: usize,
        dst: &mut [T],
        decoder: &Decoder,
    ) -> Result<(), ReadError> {
        let Some(spr) = blocks.first().map(|b| b.samples) else {
            return Ok(());
        };
        let layout = &self.layout;
        let bps = layout.bytes_per_sample;
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
            if record as u64 >= layout.n_records || i < first || i - first >= spr {
                return Err(not_present());
            }
            let j = i - first;
            let take = (spr - j).min(range.end - i);
            let record_start = layout.header_bytes + record as u64 * layout.record_bytes;
            let at = (i - range.start) as usize;
            for (k, block) in blocks.iter().enumerate() {
                let start = record_start + block.offset + j * bps as u64;
                let bytes = (start as usize)
                    .checked_add(take as usize * bps)
                    .and_then(|end| self.map.get(start as usize..end))
                    .ok_or_else(|| {
                        ReadError::Corrupt(format!("record {record} is outside the file"))
                    })?;
                let samples = &mut dst[k * n + at..][..take as usize];
                decoder.decode(bytes, bps, samples)?;
            }
            i += take;
            record += 1;
        }
        Ok(())
    }
}
