//! Core library of the Kymo viewer: recording model, readers, min/max pyramid,
//! tile cache, fixed-grid tile query and the tile service. No GUI dependencies.
//!
//! See `docs/design.md` for the architecture.

mod annotation;
pub mod annotation_file;
pub mod brainvision;
pub mod bvrf;
mod channel;
pub mod decode;
pub mod edf;
pub mod eeglab;
mod error;
pub mod fiff;
pub mod gdf;
mod grid;
mod mapped;
mod open;
pub mod pyramid;
mod recording;
mod records;
mod sample;
pub mod service;
pub mod stim;
pub mod tile;
mod timetable;
pub mod xdf;

#[cfg(feature = "testing")]
pub mod testing;

pub use annotation::{
    Annotation, AnnotationData, AnnotationId, AnnotationOp, AnnotationStore, Origin,
};
pub use channel::{Channel, ChannelId, ChannelKind, Quantity, Unit};
pub use error::{AnnotationError, ModelError, OpenError, ReadError, TileError};
pub use grid::{Grid, GridId, Piece, RegularRun, Timebase};
pub use open::open;
pub use recording::{BlockReader, Header, Opened, Recording, Stream, StreamContent};
pub use sample::{Sample, SampleType, SamplesMut};
pub use timetable::{TimeTable, TimeTableError, TimeTableWriter};

/// Locks `mutex`, poisoned or not: every critical section in this crate leaves its state
/// consistent before anything could panic.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
