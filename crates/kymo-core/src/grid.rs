//! Sample grids: groups of channels that share a time base, gaps and sample type.

use std::ops::Range;

use crate::{ChannelId, ModelError, SampleType};

/// Index of a grid in [`Recording::grids`](crate::Recording::grids).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridId(pub u32);

/// Maps sample indices to time.
///
/// The sample index is the primary axis (the tile grid is fixed in sample-index space); time is
/// always derived through the time base, never assumed to be `i / rate`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Timebase {
    /// `t(i) = start + i / rate`, in seconds since the recording start.
    Regular { rate: f64, start: f64 },
}

/// Channels that share a time base, gap structure and sample type.
///
/// EDF has one grid per distinct number of samples per record; XDF will have one per stream.
/// Tiles never mix grids, so every tile has one rate and one sample grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    timebase: Timebase,
    len: u64,
    segments: Vec<Range<u64>>,
    sample_type: SampleType,
    channels: Vec<ChannelId>,
}

impl Grid {
    /// Creates a grid of `len` sample slots, of which `segments` hold data (the rest are gaps).
    ///
    /// Segments must be non-empty, sorted, non-overlapping and end at or before `len`.
    /// Touching segments are merged.
    pub fn new(
        timebase: Timebase,
        len: u64,
        segments: Vec<Range<u64>>,
        sample_type: SampleType,
        channels: Vec<ChannelId>,
    ) -> Result<Self, ModelError> {
        match timebase {
            Timebase::Regular { rate, start } => {
                if !(rate.is_finite() && rate > 0.0) {
                    return Err(ModelError::InvalidRate(rate));
                }
                if !start.is_finite() {
                    return Err(ModelError::InvalidStart(start));
                }
            }
        }

        let mut merged: Vec<Range<u64>> = Vec::with_capacity(segments.len());
        for segment in segments {
            if segment.start >= segment.end {
                return Err(ModelError::EmptySegment(segment));
            }
            if segment.end > len {
                return Err(ModelError::SegmentPastEnd { segment, len });
            }
            match merged.last_mut() {
                Some(last) if segment.start == last.end => last.end = segment.end,
                Some(last) if segment.start < last.end => {
                    return Err(ModelError::UnsortedSegments {
                        first: last.clone(),
                        second: segment,
                    });
                }
                _ => merged.push(segment),
            }
        }

        if channels.is_empty() {
            return Err(ModelError::EmptyGrid);
        }
        let mut sorted = channels.clone();
        sorted.sort_unstable();
        if let Some(pair) = sorted.windows(2).find(|w| w[0] == w[1]) {
            return Err(ModelError::DuplicateChannel(pair[0]));
        }

        Ok(Grid {
            timebase,
            len,
            segments: merged,
            sample_type,
            channels,
        })
    }

    /// A grid whose samples `0..len` are all present.
    pub fn continuous(
        timebase: Timebase,
        len: u64,
        sample_type: SampleType,
        channels: Vec<ChannelId>,
    ) -> Result<Self, ModelError> {
        let segments = if len > 0 { vec![0..len] } else { vec![] };
        Grid::new(timebase, len, segments, sample_type, channels)
    }

    pub fn timebase(&self) -> &Timebase {
        &self.timebase
    }

    /// Number of sample slots, gaps included.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sample ranges that hold data: sorted, disjoint, non-empty and not touching.
    pub fn segments(&self) -> &[Range<u64>] {
        &self.segments
    }

    /// The parts of `range` that hold data, in order.
    pub fn present_in(&self, range: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
        let first = self.segments.partition_point(|s| s.end <= range.start);
        self.segments[first..]
            .iter()
            .take_while(move |s| s.start < range.end)
            .map(move |s| s.start.max(range.start)..s.end.min(range.end))
            .filter(|r| r.start < r.end)
    }

    pub fn sample_type(&self) -> SampleType {
        self.sample_type
    }

    /// Channels of this grid. Their order is the row order of reads and tiles.
    pub fn channels(&self) -> &[ChannelId] {
        &self.channels
    }

    /// Nominal sampling rate in Hz.
    pub fn rate(&self) -> f64 {
        match self.timebase {
            Timebase::Regular { rate, .. } => rate,
        }
    }

    /// Time of a sample in seconds since the recording start.
    pub fn time_of(&self, sample: u64) -> f64 {
        match self.timebase {
            // u64 -> f64 is exact below 2^53 samples, far beyond any recording.
            Timebase::Regular { rate, start } => start + sample as f64 / rate,
        }
    }

    /// The sample whose period contains time `t` (floor), clamped to `0..=len`.
    ///
    /// Times computed by [`time_of`](Self::time_of) map back to their sample despite rounding.
    pub fn sample_at(&self, t: f64) -> u64 {
        let x = match self.timebase {
            Timebase::Regular { rate, start } => (t - start) * rate,
        };
        if x.is_nan() || x <= 0.0 {
            return 0;
        }
        let nearest = x.round();
        // Absorb the rounding error of `time_of` (a few ulps of x) so round trips are exact.
        let tolerance = 1e-6_f64.max(x * 8.0 * f64::EPSILON);
        let sample = if (x - nearest).abs() <= tolerance {
            nearest
        } else {
            x.floor()
        };
        // Float-to-int casts saturate, so huge times clamp to u64::MAX before the min.
        (sample as u64).min(self.len)
    }
}
