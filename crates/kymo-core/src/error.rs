//! Error types of the recording model.

use std::ops::Range;

use crate::{ChannelId, GridId, SampleType};

/// A header, grid or annotation violates the recording model's invariants.
///
/// Readers are expected to clean up what a file allows (and warn about it); this error means
/// the reader produced something the model cannot represent.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("sampling rate {0} is not a positive finite number")]
    InvalidRate(f64),
    #[error("time base start {0} is not finite")]
    InvalidStart(f64),
    #[error("segment {0:?} is empty")]
    EmptySegment(Range<u64>),
    #[error("segment {segment:?} ends past the grid length {len}")]
    SegmentPastEnd { segment: Range<u64>, len: u64 },
    #[error("segments {first:?} and {second:?} are unsorted or overlap")]
    UnsortedSegments {
        first: Range<u64>,
        second: Range<u64>,
    },
    #[error("grid has no channels")]
    EmptyGrid,
    #[error("channel {0:?} is listed more than once")]
    DuplicateChannel(ChannelId),
    #[error("recording has no channels")]
    NoChannels,
    #[error("channel {channel:?} refers to grid {grid:?}, which does not exist")]
    UnknownGrid { channel: ChannelId, grid: GridId },
    #[error("grid {grid:?} lists channel {channel:?}, which does not exist")]
    UnknownChannel { grid: GridId, channel: ChannelId },
    #[error("channel {channel:?} refers to grid {expected:?} but is listed in grid {found:?}")]
    GridMismatch {
        channel: ChannelId,
        expected: GridId,
        found: GridId,
    },
    #[error("channel {0:?} is not listed in its grid")]
    ChannelNotListed(ChannelId),
    #[error("channel {channel:?} has a non-finite gain or offset ({gain}, {offset})")]
    InvalidScaling {
        channel: ChannelId,
        gain: f64,
        offset: f64,
    },
    #[error("annotation {index}: {reason}")]
    InvalidAnnotation { index: usize, reason: String },
}

/// A sample read failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadError {
    #[error("grid {0:?} does not exist")]
    UnknownGrid(GridId),
    #[error("channel {channel:?} is not in grid {grid:?}")]
    ChannelNotInGrid { channel: ChannelId, grid: GridId },
    #[error("sample range {range:?} is outside the grid (length {len})")]
    OutOfRange { range: Range<u64>, len: u64 },
    #[error(
        "output buffer holds {found} {found_type:?} samples, expected {expected} {expected_type:?}"
    )]
    BufferMismatch {
        expected: usize,
        expected_type: SampleType,
        found: usize,
        found_type: SampleType,
    },
    #[error("samples {range:?} of grid {grid:?} are not present in the file")]
    NotPresent { grid: GridId, range: Range<u64> },
    #[error("corrupt data: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
