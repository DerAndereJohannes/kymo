//! Opening EDF/BDF files: the header, the index pass, and the shared record reader over a
//! read-only memory map.

use std::fs::File;
use std::path::Path;

use super::index::index;
use super::{EdfError, parse_header};
use crate::decode::{ByteOrder, Decoder, Encoding};
use crate::mapped::map_read_only;
use crate::records::{RecordBlock, RecordReader};
use crate::{OpenError, Opened};

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
    let layout = parsed.layout;
    let encoding = match layout.bytes_per_sample {
        3 => Encoding::I24,
        _ => Encoding::I16,
    };
    let decoder = Decoder::new(encoding, ByteOrder::Little);
    let blocks = layout
        .channels
        .iter()
        .map(|b| RecordBlock {
            offset: b.offset,
            samples: b.samples,
            decoder,
        })
        .collect();
    Ok(Opened {
        header: parsed.header,
        annotations,
        warnings: parsed.warnings,
        sources: vec![path.to_owned()],
        reader: Box::new(RecordReader {
            map,
            file,
            header_bytes: layout.header_bytes,
            record_bytes: layout.record_bytes,
            n_records: layout.n_records,
            blocks,
            channel_grid,
            record_starts,
        }),
    })
}
