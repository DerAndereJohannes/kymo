//! XDF (Lab Streaming Layer's LabRecorder, `.xdf`): one file of chunks, each a stream's
//! header, a run of its samples (each with an optional timestamp), a clock offset, or a
//! footer. Streams have their own rates (0 for irregular ones) and value formats.
//!
//! Streams are read in the modes MNELAB (mnextend's `read_raw_xdf`) gives them by default,
//! with these differences:
//!
//! - No resampling: every stream shown as channels keeps its own grid. `t = 0` is the
//!   earliest first timestamp of these streams.
//! - Irregular numeric streams with several channels that are not of an event type
//!   (Marker(s), Event(s), Trigger(s), Stim) are channels on their own timestamps, where
//!   mnextend makes annotations of them; until kymo draws irregular time bases they are
//!   left out, with a warning.
//! - Channels of type "trigger" (or "triggers") are stim channels, as are those of type
//!   "stim" and every channel of a regular stream of an event type; mnextend makes
//!   "trigger" channels misc.
//! - A label two streams share is prefixed by its stream's name, or, where streams share
//!   the name, its type, or else its id ("ACC X", "GYRO X"); labels left empty are named
//!   as unlabeled channels are (`"{stream name}_{n}"`).
//! - Regular streams are placed at their nominal rate from their first timestamp; clock
//!   offsets and the timestamps' jitter are not applied yet.
//! - Annotation samples without a timestamp take the previous one's plus a sample period
//!   (as in pyxdf); those before a stream's first timestamp are left out.

mod reader;
mod scan;
mod xml;

use crate::ModelError;

pub use reader::open;

/// The file is malformed, or holds nothing kymo shows.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum XdfError {
    #[error("not an XDF file: it doesn't start with \"XDF:\"")]
    NotXdf,
    #[error("the file has no streams with channels kymo shows{0}")]
    NoChannels(String),
    #[error(transparent)]
    Model(#[from] ModelError),
}
