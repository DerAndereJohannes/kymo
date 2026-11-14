//! The BVRF block reader over a read-only map of the data file.

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::participants::Participants;
use super::{BvrfError, header, markers};
use crate::decode::{ByteOrder, Decoder, frames};
use crate::mapped::{self, map_read_only};
use crate::{
    BlockReader, ChannelId, ChannelKind, Grid, GridId, Header, OpenError, Opened, ReadError,
    Sample, SampleType, SamplesMut, Timebase,
};

/// Reads samples straight from the memory-mapped data file, which is multiplexed: one frame
/// holds a sample of every channel (section 3 of the specification).
#[derive(Debug)]
pub struct BvrfReader {
    map: Mmap,
    /// The mapped file, for reading ahead.
    file: File,
    /// One per channel: float64 data needs each channel's own reference.
    decoders: Vec<Decoder>,
    /// Each channel's byte offset within a frame.
    columns: Vec<usize>,
    /// Bytes per frame.
    frame: usize,
    /// Samples per channel.
    samples: u64,
}

/// The UTF-8 byte-order mark, which the specification puts before every text file.
const BOM: &[u8] = b"\xef\xbb\xbf";

/// The file beside `header` with `extension`, in lower or upper case, if it exists.
fn sibling(header: &Path, extension: &str) -> Option<PathBuf> {
    [extension.to_owned(), extension.to_ascii_uppercase()]
        .iter()
        .map(|e| header.with_extension(e))
        .find(|p| p.is_file())
}

/// UTF-8 text without its byte-order mark; text that isn't valid UTF-8 is repaired with
/// U+FFFD and a warning.
fn utf8(bytes: &[u8], what: &str, warnings: &mut Vec<String>) -> String {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => {
            warnings.push(format!(
                "the {what} is not valid UTF-8; invalid bytes read as \u{fffd}"
            ));
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// Opens a BVRF recording from its header file (`.bvrh`). The data file (`.bvrd` beside
/// it) is memory-mapped, never loaded; the marker file (`.bvrm`) is read once.
///
/// The header names no files, so the others are the header's name with their extensions. A
/// missing marker file only means no annotations, with a warning; a missing data file is an
/// error. JSON that isn't a BVRF header is [`OpenError::Unsupported`].
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| OpenError::Io { path, source }
    };
    let error = |source| OpenError::Bvrf {
        path: path.to_owned(),
        source,
    };
    let mut warnings = Vec::new();
    let text = utf8(
        &std::fs::read(path).map_err(io(path))?,
        "header",
        &mut warnings,
    );
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| error(BvrfError::Json(e.to_string())))?;
    if !header::is_bvrf(&json) {
        return Err(OpenError::Unsupported {
            path: path.to_owned(),
            reason: "a JSON file, but not a BVRF header (it has no EEGModality section)".into(),
        });
    }
    let parsed = header::parse(&json, &mut warnings).map_err(error)?;
    let names = parsed.channels.iter().map(|c| c.name.clone()).collect();
    let participants = Participants::new(
        &parsed.participants,
        &parsed.participant,
        names,
        &mut warnings,
    );
    let mut channels = participants.arrange(parsed.channels);
    let n = channels.len();

    let data_path = sibling(path, "bvrd")
        .ok_or_else(|| error(BvrfError::NoDataFile(path.with_extension("bvrd"))))?;
    let file = File::open(&data_path).map_err(io(&data_path))?;
    let map = map_read_only(&file).map_err(io(&data_path))?;
    let size = parsed.encoding.size();
    let frame = n * size;
    let samples = map.len() as u64 / frame as u64;
    let rest = map.len() % frame;
    if rest != 0 {
        warnings.push(format!(
            "the data file ends in {rest} bytes of an incomplete sample, which are ignored"
        ));
    }

    let columns: Vec<usize> = participants.order.iter().map(|&c| c * size).collect();
    let decoders = channels
        .iter_mut()
        .zip(&columns)
        .map(|(channel, &column)| {
            let decoder = Decoder::new(parsed.encoding, ByteOrder::Little);
            if !decoder.uses_reference() || channel.kind == ChannelKind::Stim || samples == 0 {
                return decoder;
            }
            let first = map.get(column..).unwrap_or_default();
            let decoder = decoder.with_reference(decoder.reference_from(first));
            channel.offset += decoder.reference() * channel.gain;
            decoder
        })
        .collect();

    let mut extra = parsed.extra;
    let markers = match sibling(path, "bvrm") {
        Some(marker_path) => {
            let bytes = std::fs::read(&marker_path).map_err(io(&marker_path))?;
            let text = utf8(&bytes, "marker file", &mut warnings);
            markers::parse(&text, parsed.rate, &participants, &mut warnings)
        }
        None => {
            warnings.push(format!(
                "marker file {:?} not found, so there are no annotations",
                path.with_extension("bvrm").file_name().unwrap_or_default()
            ));
            markers::Markers::default()
        }
    };
    extra.extend(markers.utc_offset.map(|o| ("UTC offset".to_owned(), o)));
    let streams = participants.streams(&markers.annotations);

    let grid = Grid::continuous(
        Timebase::Regular {
            rate: parsed.rate,
            start: 0.0,
        },
        samples,
        parsed.encoding.native(),
        (0..n as u32).map(ChannelId).collect(),
    )
    .map_err(|e| error(e.into()))?;
    Ok(Opened {
        header: Header {
            format: "BVRF",
            start_time: markers.start,
            channels,
            grids: vec![grid],
            extra,
            bad_channels: Vec::new(),
            streams,
        },
        annotations: markers.annotations,
        warnings,
        sources: vec![path.to_owned(), data_path],
        reader: Box::new(BvrfReader {
            map,
            file,
            decoders,
            columns,
            frame,
            samples,
        }),
    })
}

impl BlockReader for BvrfReader {
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
        let frame = self.frame as u64;
        mapped::prefetch(
            &self.file,
            self.map.len() as u64,
            [range.start * frame..range.end * frame],
        );
    }
}

impl BvrfReader {
    fn native(&self) -> SampleType {
        self.decoders
            .first()
            .map_or(SampleType::F32, Decoder::native)
    }

    /// Decodes `range` of `channels` into `dst`, channel-major.
    fn copy<T: Sample>(
        &self,
        channels: &[ChannelId],
        range: Range<u64>,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let columns: Vec<(usize, Decoder)> = channels
            .iter()
            .map(|c| (self.columns[c.0 as usize], self.decoders[c.0 as usize]))
            .collect();
        frames(&self.map, self.frame, &columns, range, dst)
    }
}
