//! Core library of the Kymo viewer: recording model, readers, min/max pyramid,
//! tile cache and fixed-grid tile query. No GUI dependencies.
//!
//! See `docs/design.md` for the architecture.

mod annotation;
mod channel;
pub mod edf;
mod error;
mod grid;
mod recording;
mod sample;

#[cfg(feature = "testing")]
pub mod testing;

pub use annotation::{
    Annotation, AnnotationData, AnnotationId, AnnotationOp, AnnotationStore, Origin,
};
pub use channel::{Channel, ChannelId, ChannelKind, Unit};
pub use error::{AnnotationError, ModelError, OpenError, ReadError};
pub use grid::{Grid, GridId, Timebase};
pub use recording::{BlockReader, Header, Opened, Recording};
pub use sample::{Sample, SampleType, SamplesMut};

/// Opens a recording file, choosing the reader from the file's first bytes.
///
/// Only EDF and BDF (with their + variants) are supported for now.
pub fn open(path: &std::path::Path) -> Result<Opened, OpenError> {
    use std::io::Read;

    let mut magic = [0u8; 8];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut magic))
        .map_err(|source| OpenError::Io {
            path: path.to_owned(),
            source,
        })?;
    match magic[..read].first() {
        // EDF's version field is "0", BDF's starts with 0xFF ("\xffBIOSEMI").
        Some(b'0' | 0xff) => edf::open(path),
        _ => Err(OpenError::Unsupported {
            path: path.to_owned(),
            reason: "not a supported file format (only EDF and BDF are supported)".into(),
        }),
    }
}

/// Version of this crate, as set in `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(version(), "0.1.0");
    }
}
