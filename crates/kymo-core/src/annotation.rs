//! Annotation records.

use crate::ChannelId;

/// Identifies an annotation for the whole session, across edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

/// Where an annotation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Origin {
    /// Read from the file (e.g. EDF+ TAL).
    File,
    /// Added in the viewer.
    Viewer,
}

/// The content of an annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationData {
    /// Seconds since the recording start.
    pub onset: f64,
    /// Seconds, `>= 0`. Zero marks an event.
    pub duration: f64,
    pub description: String,
    /// Channels the annotation applies to; empty means all channels.
    pub channels: Vec<ChannelId>,
    pub origin: Origin,
}

/// An annotation with its session id.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: AnnotationId,
    pub data: AnnotationData,
}
