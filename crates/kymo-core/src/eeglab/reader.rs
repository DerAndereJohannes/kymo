//! The EEGLAB block reader over a read-only map of the `.fdt` or of the `.set` itself.

use std::fs::File;
use std::ops::Range;
use std::path::Path;

use memmap2::Mmap;

use super::EeglabError;
use super::dataset::{self, Data};
use crate::decode::{ByteOrder, Decoder, Encoding, frames, strided};
use crate::mapped::{self, map_read_only};
use crate::open::companion;
use crate::{
    BlockReader, ChannelId, ChannelKind, Grid, GridId, Header, OpenError, Opened, ReadError,
    Sample, SamplesMut, Timebase,
};

/// Reads samples straight from the mapped data: MATLAB's column-major nbchan × pnts array,
/// so one frame holds a sample of every channel.
#[derive(Debug)]
pub struct EeglabReader {
    map: Mmap,
    /// The mapped file, for reading ahead.
    file: File,
    /// Where the samples start in the map.
    start: usize,
    /// One per channel: float64 data needs each channel's own reference.
    decoders: Vec<Decoder>,
    /// Samples per channel.
    samples: u64,
}

/// Opens an EEGLAB dataset from its `.set` file. The samples are memory-mapped, never
/// loaded, whether they are in a `.fdt` beside it or in the `.set`.
///
/// A `.fdt` the dataset names but that doesn't exist is looked for under the `.set`'s own
/// name too (a renamed dataset), with a warning.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| OpenError::Io { path, source }
    };
    let error = |source| OpenError::Eeglab {
        path: path.to_owned(),
        source,
    };
    let mut warnings = Vec::new();
    let set_file = File::open(path).map_err(io(path))?;
    let set_map = map_read_only(&set_file).map_err(io(path))?;
    // What the samples need from the dataset, so its borrow of the map ends here.
    let parsed = dataset::read(&set_map, &mut warnings).map_err(error)?;
    let place = match &parsed.data {
        Data::Embedded(array) => Place::Set(array.encoding, array.order(), array.data.start),
        Data::File(name) => Place::Fdt(name.clone()),
    };
    let (rate, pnts, channels, annotations, extra) = (
        parsed.rate,
        parsed.samples,
        parsed.channels,
        parsed.annotations,
        parsed.extra,
    );
    let n = channels.len();

    let (encoding, order, start, samples, data_path) = match place {
        Place::Set(encoding, order, start) => (encoding, order, start, pnts, None),
        Place::Fdt(name) => {
            let dir = path.parent().unwrap_or(Path::new(""));
            let base = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let found = companion(dir, &base, &name, "fdt", "data", &mut warnings)
                .ok_or_else(|| error(EeglabError::NoDataFile(dir.join(&name))))?;
            let len = std::fs::metadata(&found).map_err(io(&found))?.len();
            let samples = fdt_samples(len, n, pnts, &mut warnings);
            (Encoding::F32, ByteOrder::Little, 0, samples, Some(found))
        }
    };
    let (map, file) = match &data_path {
        Some(data_path) => {
            let file = File::open(data_path).map_err(io(data_path))?;
            (map_read_only(&file).map_err(io(data_path))?, file)
        }
        None => (set_map, set_file),
    };

    let size = encoding.size();
    let mut channels = channels;
    let decoders = channels
        .iter_mut()
        .enumerate()
        .map(|(c, channel)| {
            let decoder = Decoder::new(encoding, order);
            if channel.kind == ChannelKind::STIM || samples == 0 {
                return decoder;
            }
            let first = map.get(start + c * size..).unwrap_or_default();
            let values = strided(first, n * size, samples as usize);
            let decoder = decoder.with_reference(decoder.reference_among(values));
            channel.offset += decoder.reference() * channel.gain;
            decoder
        })
        .collect();

    let grid = Grid::continuous(
        Timebase::Regular { rate, start: 0.0 },
        samples,
        encoding.native(),
        (0..n as u32).map(ChannelId).collect(),
    )
    .map_err(|e| error(e.into()))?;
    let mut sources = vec![path.to_owned()];
    sources.extend(data_path);
    Ok(Opened {
        header: Header {
            format: "EEGLAB",
            start_time: None,
            channels,
            grids: vec![grid],
            extra,
            bad_channels: Vec::new(),
            streams: Vec::new(),
            utc_offset: None,
        },
        annotations,
        warnings,
        sources,
        reader: Box::new(EeglabReader {
            map,
            file,
            start,
            decoders,
            samples,
        }),
    })
}

/// Where the samples are.
enum Place {
    /// In the `.set`, stored as `Encoding` from this byte offset.
    Set(Encoding, ByteOrder, usize),
    /// In this `.fdt`, as the dataset names it.
    Fdt(String),
}

/// Samples per channel in a `.fdt` of `len` bytes for `pnts` samples of `channels`
/// float32 values: its whole frames, up to `pnts`.
fn fdt_samples(len: u64, channels: usize, pnts: u64, warnings: &mut Vec<String>) -> u64 {
    let frame = channels as u64 * 4;
    let whole = len / frame;
    let needed = pnts.saturating_mul(frame);
    if whole < pnts {
        warnings.push(format!(
            "the data file holds {whole} samples per channel, fewer than the dataset's \
             {pnts}"
        ));
        whole
    } else {
        if len > needed {
            warnings.push(format!(
                "the data file has {} bytes after the dataset's {pnts} samples, which are \
                 ignored",
                len - needed
            ));
        }
        pnts
    }
}

impl BlockReader for EeglabReader {
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
        if let Some(&channel) = channels
            .iter()
            .find(|c| c.0 as usize >= self.decoders.len())
        {
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
                expected_type: self.native(),
                found: out.len(),
                found_type: out.sample_type(),
            }),
        }
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        if grid != GridId(0) {
            return;
        }
        let frame = self.frame() as u64;
        let start = self.start as u64;
        mapped::prefetch(
            &self.file,
            self.map.len() as u64,
            [start + range.start * frame..start + range.end * frame],
        );
    }
}

impl EeglabReader {
    fn native(&self) -> crate::SampleType {
        self.decoders
            .first()
            .map_or(crate::SampleType::F32, Decoder::native)
    }

    fn frame(&self) -> usize {
        let size = self.decoders.first().map_or(4, |d| d.encoding().size());
        self.decoders.len() * size
    }

    /// Decodes `range` of `channels` into `dst`, channel-major.
    fn copy<T: Sample>(
        &self,
        channels: &[ChannelId],
        range: Range<u64>,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let size = self.frame() / self.decoders.len().max(1);
        let columns: Vec<(usize, Decoder)> = channels
            .iter()
            .map(|c| (c.0 as usize * size, self.decoders[c.0 as usize]))
            .collect();
        let data = self.map.get(self.start..).unwrap_or_default();
        frames(data, self.frame(), &columns, range, dst)
    }
}
