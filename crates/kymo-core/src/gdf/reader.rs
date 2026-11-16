//! Opening GDF files: the header, the event table, and the shared record reader over a
//! read-only memory map.

use std::fs::File;
use std::path::Path;

use super::{events, header};
use crate::mapped::map_read_only;
use crate::records::RecordReader;
use crate::{ChannelKind, Header, OpenError, Opened};

/// Opens a GDF 1 or 2 file. The file is memory-mapped, never loaded; only the header and
/// the event table after the data records are read at open, plus each channel's first
/// finite sample where it serves as the reference of 64-bit, unsigned 32-bit and float
/// samples.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let io = |source| OpenError::Io {
        path: path.to_owned(),
        source,
    };
    let error = |source| OpenError::Gdf {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(io)?;
    let map = map_read_only(&file).map_err(io)?;
    let mut warnings = Vec::new();
    let mut parsed = header::parse(&map, &mut warnings).map_err(error)?;

    // References from the first finite samples, record by record.
    if parsed.n_records > 0 {
        for (channel, block) in parsed.channels.iter_mut().zip(&mut parsed.blocks) {
            if channel.kind == ChannelKind::Stim || block.samples == 0 {
                continue;
            }
            let size = block.decoder.encoding().size() as u64;
            let values = (0..parsed.n_records)
                .flat_map(|r| (0..block.samples).map(move |i| (r, i)))
                .map_while(|(r, i)| {
                    let at = parsed.header_bytes + r * parsed.record_bytes + block.offset;
                    map.get(usize::try_from(at + i * size).ok()?..)
                });
            block.decoder = block
                .decoder
                .with_reference(block.decoder.reference_among(values));
            channel.offset += block.decoder.reference() * channel.gain;
        }
    }

    let fallback_rate = parsed.grids.iter().map(|g| g.rate()).fold(0.0, f64::max);
    let annotations = match parsed.event_table {
        Some(at) => events::read(
            map.get(at as usize..).unwrap_or_default(),
            parsed.version,
            fallback_rate,
            &parsed.file_channels,
            &mut warnings,
        ),
        None => Vec::new(),
    };

    let channel_grid = parsed.channels.iter().map(|c| c.grid).collect();
    Ok(Opened {
        header: Header {
            format: "GDF",
            start_time: parsed.start_time,
            channels: parsed.channels,
            grids: parsed.grids,
            extra: parsed.extra,
            bad_channels: Vec::new(),
            streams: Vec::new(),
            utc_offset: None,
        },
        annotations,
        warnings,
        sources: vec![path.to_owned()],
        reader: Box::new(RecordReader {
            map,
            file,
            header_bytes: parsed.header_bytes,
            record_bytes: parsed.record_bytes,
            n_records: parsed.n_records,
            blocks: parsed.blocks,
            channel_grid,
            record_starts: None,
        }),
    })
}
