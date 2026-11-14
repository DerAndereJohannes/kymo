//! BVRF recordings (Brain Products' BrainVision Recording Format 1.0): a JSON header
//! (`.bvrh`) describing multiplexed binary data (`.bvrd`) and a tab-separated marker file
//! (`.bvrm`). The impedance file (`.bvri`) isn't read. With several participants in a
//! recording, each is a stream (see [`Header::streams`](crate::Header::streams)).

mod header;
mod markers;
mod participants;
mod reader;

use std::path::PathBuf;

use crate::ModelError;

pub use reader::{BvrfReader, open};

/// The header is malformed, or describes data kymo doesn't read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BvrfError {
    #[error("the header is not valid JSON: {0}")]
    Json(String),
    #[error("the header has no {0}")]
    Missing(&'static str),
    #[error("invalid {field}: {value}")]
    Invalid { field: &'static str, value: String },
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("channel {index} ({name:?}): {reason}")]
    InvalidChannel {
        index: usize,
        name: String,
        reason: String,
    },
    #[error("data file {} not found", .0.display())]
    NoDataFile(PathBuf),
    #[error(transparent)]
    Model(#[from] ModelError),
}
