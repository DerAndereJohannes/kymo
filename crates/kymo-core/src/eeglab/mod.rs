//! EEGLAB datasets: a MATLAB file (`.set`) holding the `EEG` struct, with the samples in it
//! or in a float32 file beside it (`.fdt`).
//!
//! MATLAB's v6 format (level 5 MAT-files, uncompressed) is read, which EEGLAB writes by
//! default; v7.3 (HDF5) files and compressed variables (`-v7`) are refused, and so are
//! epoched datasets.

mod dataset;
mod mat;
mod reader;

use std::path::PathBuf;

use crate::ModelError;

pub use reader::{EeglabReader, open};

/// How to make a file kymo reads: EEGLAB's option to save in MATLAB's v6 format.
pub(crate) const SAVE_AS_V6: &str = "save it again from EEGLAB with \"Save as Matlab v6.5\" \
                                     turned on in File > Preferences (option_saveversion6)";

/// The dataset is malformed, or holds data kymo doesn't read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum EeglabError {
    #[error("malformed MAT-file: {0}")]
    Mat(String),
    #[error(
        "the dataset is saved compressed (MATLAB's -v7), which kymo doesn't read; {}",
        SAVE_AS_V6
    )]
    Compressed,
    #[error("not an EEGLAB dataset: the MAT-file holds no EEG struct or EEG fields")]
    NotEeglab,
    #[error("the MAT-file holds an ALLEEG array of datasets; save a single dataset")]
    AllEeg,
    #[error("the dataset has no {0} field")]
    Missing(&'static str),
    #[error("invalid {field}: {value}")]
    Invalid { field: &'static str, value: String },
    #[error("epoched datasets ({0} trials) are not supported; only continuous data is read")]
    Epoched(u64),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("data file {} not found", .0.display())]
    NoDataFile(PathBuf),
    #[error(transparent)]
    Model(#[from] ModelError),
}
