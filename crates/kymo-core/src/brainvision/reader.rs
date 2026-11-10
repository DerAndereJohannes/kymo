//! The BrainVision block reader over a read-only map of the data file.

use std::fs::File;
use std::ops::Range;
use std::path::Path;

use memmap2::Mmap;

use super::header::{self, Orientation};
use super::text::{Ini, decode};
use super::{BrainVisionError, markers};
use crate::decode::{ByteOrder, Decoder, frames};
use crate::mapped::{self, map_read_only};
use crate::open::companion;
use crate::{
    BlockReader, ChannelId, Grid, GridId, Header, OpenError, Opened, ReadError, Sample, SamplesMut,
    Timebase,
};

/// Reads samples straight from the memory-mapped data file.
#[derive(Debug)]
pub struct BrainVisionReader {
    map: Mmap,
    /// The mapped file, for reading ahead.
    file: File,
    orientation: Orientation,
    decoder: Decoder,
    channels: usize,
    /// Samples per channel.
    samples: u64,
}

/// Opens a BrainVision recording from its header file (`.vhdr`). The data file is
/// memory-mapped, never loaded; the marker file is read once.
///
/// A data or marker file the header names but that doesn't exist is looked for under the
/// header's own name too (a renamed recording), with a warning. A missing marker file only
/// means no annotations, with a warning; a missing data file is an error.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| OpenError::Io { path, source }
    };
    let error = |source| OpenError::BrainVision {
        path: path.to_owned(),
        source,
    };
    let mut warnings = Vec::new();
    let text = decode(
        &std::fs::read(path).map_err(io(path))?,
        "header",
        &mut warnings,
    );
    let base = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = header::parse(&Ini::parse(&text), &base, &mut warnings).map_err(error)?;
    let dir = path.parent().unwrap_or(Path::new(""));

    let data_path = companion(dir, &base, &parsed.data_file, "eeg", "data", &mut warnings)
        .ok_or_else(|| error(BrainVisionError::NoDataFile(dir.join(&parsed.data_file))))?;
    let file = File::open(&data_path).map_err(io(&data_path))?;
    let map = map_read_only(&file).map_err(io(&data_path))?;
    let n = parsed.channels.len();
    let samples = samples(
        map.len() as u64,
        n,
        parsed.encoding.size(),
        parsed.orientation,
        parsed.data_points,
        &mut warnings,
    )
    .map_err(error)?;

    let annotations_and_start = match &parsed.marker_file {
        Some(name) => match companion(dir, &base, name, "vmrk", "marker", &mut warnings) {
            Some(marker_path) => {
                let bytes = std::fs::read(&marker_path).map_err(io(&marker_path))?;
                let text = decode(&bytes, "marker file", &mut warnings);
                let m = markers::parse(&Ini::parse(&text), parsed.rate, n, &mut warnings);
                (m.annotations, m.start)
            }
            None => {
                warnings.push(format!(
                    "marker file {name:?} not found, so there are no annotations"
                ));
                (Vec::new(), None)
            }
        },
        None => (Vec::new(), None),
    };
    let (annotations, start_time) = annotations_and_start;

    let decoder = Decoder::new(parsed.encoding, ByteOrder::Little);
    let grid = Grid::continuous(
        Timebase::Regular {
            rate: parsed.rate,
            start: 0.0,
        },
        samples,
        decoder.native(),
        (0..n as u32).map(ChannelId).collect(),
    )
    .map_err(|e| error(e.into()))?;
    Ok(Opened {
        header: Header {
            format: "BrainVision",
            start_time,
            channels: parsed.channels,
            grids: vec![grid],
            extra: parsed.extra,
        },
        annotations,
        warnings,
        sources: vec![path.to_owned(), data_path],
        reader: Box::new(BrainVisionReader {
            map,
            file,
            orientation: parsed.orientation,
            decoder,
            channels: n,
            samples,
        }),
    })
}

/// Samples per channel in a data file of `len` bytes.
///
/// Multiplexed files hold whole frames (one sample of every channel); bytes after the last
/// whole frame are ignored. Vectorized files need `DataPoints` to find each channel's start;
/// without it, the length is inferred from the file size.
fn samples(
    len: u64,
    channels: usize,
    size: usize,
    orientation: Orientation,
    data_points: Option<u64>,
    warnings: &mut Vec<String>,
) -> Result<u64, BrainVisionError> {
    let frame = (channels * size) as u64;
    let whole = len / frame;
    let rest = len % frame;
    match (orientation, data_points) {
        (Orientation::Multiplexed, points) => {
            if rest != 0 {
                warnings.push(format!(
                    "the data file ends in {rest} bytes of an incomplete sample, which are \
                     ignored"
                ));
            }
            match points {
                Some(points) if points < whole => {
                    warnings.push(format!(
                        "the data file holds {whole} samples, more than the header's \
                         DataPoints ({points}); the rest are ignored"
                    ));
                    Ok(points)
                }
                Some(points) if points > whole => {
                    warnings.push(format!(
                        "the data file holds {whole} samples, fewer than the header's \
                         DataPoints ({points})"
                    ));
                    Ok(whole)
                }
                _ => Ok(whole),
            }
        }
        (Orientation::Vectorized, Some(points)) => {
            let needed = points
                .checked_mul(frame)
                .ok_or(BrainVisionError::Truncated {
                    needed: u64::MAX,
                    found: len,
                })?;
            if needed > len {
                return Err(BrainVisionError::Truncated { needed, found: len });
            }
            if needed < len {
                warnings.push(format!(
                    "the data file has {} bytes after the {points} samples of DataPoints, \
                     which are ignored",
                    len - needed
                ));
            }
            Ok(points)
        }
        (Orientation::Vectorized, None) => {
            warnings.push(format!(
                "the header has no DataPoints, so the {whole} samples per channel are \
                 inferred from the data file's size"
            ));
            if rest != 0 {
                warnings.push(format!(
                    "the data file's size is not a whole number of samples per channel; {rest} \
                     bytes at its end are ignored"
                ));
            }
            Ok(whole)
        }
    }
}

impl BlockReader for BrainVisionReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        if grid != GridId(0) {
            return Err(ReadError::UnknownGrid(grid));
        }
        if let Some(&channel) = channels.iter().find(|c| c.0 as usize >= self.channels) {
            return Err(ReadError::ChannelNotInGrid { channel, grid });
        }
        if range.end > self.samples || range.start > range.end {
            return Err(ReadError::OutOfRange {
                range,
                len: self.samples,
            });
        }
        let n = (range.end - range.start) as usize;
        let expected = channels.len().saturating_mul(n);
        match out {
            SamplesMut::I16(dst) if dst.len() == expected => self.copy(channels, range, dst),
            SamplesMut::I32(dst) if dst.len() == expected => self.copy(channels, range, dst),
            SamplesMut::F32(dst) if dst.len() == expected => self.copy(channels, range, dst),
            out => Err(ReadError::BufferMismatch {
                expected,
                expected_type: self.decoder.native(),
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        if grid != GridId(0) {
            return;
        }
        let size = self.decoder.encoding().size() as u64;
        let len = self.map.len() as u64;
        match self.orientation {
            Orientation::Multiplexed => {
                let frame = self.channels as u64 * size;
                mapped::prefetch(&self.file, len, [range.start * frame..range.end * frame]);
            }
            Orientation::Vectorized => {
                let ranges = (0..self.channels as u64).map(|c| {
                    let start = (c * self.samples + range.start) * size;
                    start..start + (range.end - range.start) * size
                });
                mapped::prefetch(&self.file, len, ranges);
            }
        }
    }
}

impl BrainVisionReader {
    /// Decodes `range` of `channels` into `dst`, channel-major.
    fn copy<T: Sample>(
        &self,
        channels: &[ChannelId],
        range: Range<u64>,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let n = (range.end - range.start) as usize;
        if n == 0 {
            return Ok(());
        }
        let size = self.decoder.encoding().size();
        let outside = || ReadError::Corrupt("samples beyond the end of the data file".into());
        match self.orientation {
            Orientation::Multiplexed => {
                let columns: Vec<(usize, Decoder)> = channels
                    .iter()
                    .map(|c| (c.0 as usize * size, self.decoder))
                    .collect();
                frames(&self.map, self.channels * size, &columns, range, dst)?;
            }
            Orientation::Vectorized => {
                for (k, channel) in channels.iter().enumerate() {
                    let start =
                        (channel.0 as usize * self.samples as usize + range.start as usize) * size;
                    let bytes = self.map.get(start..start + n * size).ok_or_else(outside)?;
                    self.decoder.decode(bytes, size, &mut dst[k * n..][..n])?;
                }
            }
        }
        Ok(())
    }
}
