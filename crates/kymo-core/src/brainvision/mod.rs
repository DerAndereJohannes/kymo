//! BrainVision recordings (Brain Products' Core Data Format, versions 1.0 and 2.0): a text
//! header (`.vhdr`) describing a binary data file (`.eeg`) and a text marker file (`.vmrk`).

mod header;
mod markers;
mod reader;
mod text;

use std::path::PathBuf;

use crate::ModelError;

pub use reader::{BrainVisionReader, open};

/// The header is malformed, or describes data kymo doesn't read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BrainVisionError {
    #[error("the header's [{section}] section has no {key}")]
    Missing {
        section: &'static str,
        key: &'static str,
    },
    #[error("invalid {key}: {value:?}")]
    Invalid { key: &'static str, value: String },
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("the header has no line for channel {0} (Ch{0}=…)")]
    MissingChannel(usize),
    #[error("channel {index} ({name:?}): {reason}")]
    InvalidChannel {
        index: usize,
        name: String,
        reason: String,
    },
    #[error("data file {} not found", .0.display())]
    NoDataFile(PathBuf),
    #[error("the data file is truncated: DataPoints needs {needed} bytes, the file has {found}")]
    Truncated { needed: u64, found: u64 },
    #[error(transparent)]
    Model(#[from] ModelError),
}
