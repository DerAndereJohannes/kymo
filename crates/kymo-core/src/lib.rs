//! Core library of the Kymo viewer: recording model, readers, min/max pyramid,
//! tile cache and fixed-grid tile query. No GUI dependencies.
//!
//! See `docs/design.md` for the architecture.

mod annotation;
mod channel;
mod error;
mod grid;
mod recording;
mod sample;

#[cfg(feature = "testing")]
pub mod testing;

pub use annotation::{Annotation, AnnotationData, AnnotationId, Origin};
pub use channel::{Channel, ChannelId, ChannelKind, Unit};
pub use error::{ModelError, ReadError};
pub use grid::{Grid, GridId, Timebase};
pub use recording::{BlockReader, Header, Opened, Recording};
pub use sample::{Sample, SampleType, SamplesMut};

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
