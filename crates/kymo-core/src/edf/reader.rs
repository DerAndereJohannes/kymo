//! The EDF/BDF block reader over a read-only memory map.

use std::fs::File;
use std::ops::Range;
use std::path::Path;

use memmap2::Mmap;

use super::{Block, EdfError, RecordLayout, parse_header};
use crate::{BlockReader, ChannelId, GridId, OpenError, Opened, ReadError, SampleType, SamplesMut};

/// Reads samples straight from the memory-mapped file.
#[derive(Debug)]
pub struct EdfReader {
    map: Mmap,
    layout: RecordLayout,
    /// The grid of each data channel.
    channel_grid: Vec<GridId>,
}

/// Opens an EDF, EDF+C, BDF or BDF+C file. The file is memory-mapped, never loaded.
///
/// EDF+D/BDF+D files are not supported yet (their gaps come from the annotation records),
/// and EDF+/BDF+ annotations are not read yet.
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
    let parsed = parse_header(&map, map.len() as u64).map_err(edf)?;
    if parsed.layout.discontinuous {
        return Err(OpenError::Unsupported {
            path: path.to_owned(),
            reason: format!(
                "{} (discontinuous) files are not supported yet",
                parsed.header.format
            ),
        });
    }
    let channel_grid = parsed.header.channels.iter().map(|c| c.grid).collect();
    Ok(Opened {
        header: parsed.header,
        annotations: Vec::new(),
        warnings: parsed.warnings,
        reader: Box::new(EdfReader {
            map,
            layout: parsed.layout,
            channel_grid,
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
        let (expected_type, bdf) = match self.layout.bytes_per_sample {
            3 => (SampleType::I32, true),
            _ => (SampleType::I16, false),
        };
        match out {
            SamplesMut::I16(dst) if !bdf && dst.len() == expected => {
                self.copy(&blocks, range, n, dst, |b| i16::from_le_bytes([b[0], b[1]]))
            }
            SamplesMut::I32(dst) if bdf && dst.len() == expected => {
                // Little-endian int24, sign-extended by the arithmetic shift.
                self.copy(&blocks, range, n, dst, |b| {
                    i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8
                })
            }
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type,
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }
}

impl EdfReader {
    /// Copies `range` of every block into `dst` (channel-major, `n` samples per channel),
    /// record by record so the file is read in order.
    fn copy<T>(
        &self,
        blocks: &[Block],
        range: Range<u64>,
        n: usize,
        dst: &mut [T],
        decode: impl Fn(&[u8]) -> T,
    ) -> Result<(), ReadError> {
        let Some(spr) = blocks.first().map(|b| b.samples) else {
            return Ok(());
        };
        let layout = &self.layout;
        let bps = layout.bytes_per_sample;
        if range.end > layout.n_records * spr {
            return Err(ReadError::OutOfRange {
                range,
                len: layout.n_records * spr,
            });
        }
        let mut i = range.start;
        while i < range.end {
            let (record, j) = (i / spr, i % spr);
            let take = (spr - j).min(range.end - i);
            let record_start = layout.header_bytes + record * layout.record_bytes;
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
                for (d, s) in samples.iter_mut().zip(bytes.chunks_exact(bps)) {
                    *d = decode(s);
                }
            }
            i += take;
        }
        Ok(())
    }
}
