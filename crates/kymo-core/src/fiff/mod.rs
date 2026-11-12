//! FIFF (Elekta/MEGIN Neuromag and MNE-Python's `.fif`): a sequence of big-endian tags,
//! nested into blocks. A raw recording is a measurement block holding the measurement info
//! (channels, rate, date) and a raw data block of data buffers, each a run of whole frames
//! (one sample of every channel), with skips where the acquisition paused.
//!
//! What kymo reads, and where it deliberately differs from MNE's `read_raw_fif`:
//!
//! - `t = 0` is the first sample (`first_samp`), so the start time is
//!   `meas_date + first_samp / sfreq`, on the recording's clock: UTC, or local time where
//!   the file stores its UTC offset.
//! - Skips inside the data are gaps; MNE fills them with zeros and adds a `BAD_ACQ_SKIP`
//!   annotation. A skip before the first buffer moves `first_samp`, as in MNE.
//! - Stim channels whose calibration (range × cal) isn't 1 are misc channels with their
//!   calibrated values: they are 5 V trigger lines or analog inputs, not codes.
//! - Of the stim channels, only the combined one (STI101, else STI 014), if there is one,
//!   has events, its codes masked to the low 16 bits (MNE's `find_events` needs
//!   `uint_cast=True` for that).
//! - Any part of a split recording opens the whole recording, from its first part; MNE
//!   reads from the given part on. A jump in the first sample between parts is a gap.
//! - Annotation onsets count from the first sample; their channel lists become the
//!   annotations' channels. Bad channels seed the viewer's.
//! - MaxShield (Internal Active Shielding) recordings open with a warning, where MNE needs
//!   `allow_maxshield`.
//! - Projectors and CTF compensation are ignored, as `read_raw_fif` leaves them unapplied.

mod annotations;
mod info;
mod reader;
mod tag;

use crate::ModelError;

pub use reader::open;

/// The file is malformed, or holds something kymo doesn't read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum FiffError {
    #[error("not a FIFF file: it doesn't start with a file id tag")]
    NotFiff,
    #[error("the file has no measurement block")]
    NoMeasurement,
    #[error("the file has no measurement info")]
    NoInfo,
    #[error(
        "the file holds no continuous (raw) data{0}; kymo reads raw recordings, not epochs, \
         evoked responses or other FIFF files"
    )]
    NoRawData(&'static str),
    #[error("the file has no channels")]
    NoChannels,
    #[error("invalid {field}: {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("channel {index}: {reason}")]
    InvalidChannel { index: usize, reason: String },
    #[error("split part {file} doesn't continue the recording: {reason}")]
    SplitMismatch { file: String, reason: String },
    #[error("data buffer at byte {position}: {reason}")]
    InvalidBuffer { position: u64, reason: String },
    #[error(transparent)]
    Model(#[from] ModelError),
}
