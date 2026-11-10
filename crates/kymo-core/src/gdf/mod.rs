//! GDF 1.x and 2.x (the General Data Format for biomedical signals): a header with a fixed
//! part, a part per channel and (GDF 2) tag-length-value fields, then data records as in
//! EDF but with a sample type per channel, then an event table.

mod events;
mod header;
mod reader;
mod units;

use crate::ModelError;

pub use reader::open;

/// The header is malformed, or describes data kymo doesn't read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum GdfError {
    #[error("not a GDF 1 or 2 file (version field {0:?})")]
    NotGdf(String),
    #[error("header truncated: it needs {needed} bytes, the file has {found}")]
    Truncated { needed: u64, found: u64 },
    #[error("invalid {field}: {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("channel {index} ({label:?}): {reason}")]
    InvalidChannel {
        index: usize,
        label: String,
        reason: String,
    },
    #[error("the file has no channels with samples in its data records")]
    NoChannels,
    #[error(transparent)]
    Model(#[from] ModelError),
}
