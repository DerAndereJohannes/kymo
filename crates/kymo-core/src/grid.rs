//! Sample grids: groups of channels that share a time base, gaps and sample type.

use std::ops::Range;
use std::sync::Arc;

use crate::{ChannelId, ModelError, SampleType, TimeTable};

/// Index of a grid in [`Recording::grids`](crate::Recording::grids).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridId(pub u32);

/// Maps sample indices to time.
///
/// The sample index is the primary axis (the tile grid is fixed in sample-index space); time is
/// always derived through the time base, never assumed to be `i / rate`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Timebase {
    /// `t(i) = start + i / rate`, in seconds since the recording start.
    Regular { rate: f64, start: f64 },
    /// Regular pieces, each with its own rate and start (an XDF stream whose segments
    /// between breaks are dejittered separately): from a piece's first sample up to the next
    /// piece's, `t(i) = start + (i - first) / rate`. The first piece starts at sample 0, and
    /// each starts after the previous one's last sample in time, so time increases with the
    /// sample index. `rate` is the grid's overall rate, for choosing levels and showing it.
    Segmented { rate: f64, pieces: Vec<Piece> },
    /// Each slot's time from a table (an XDF stream without a nominal rate, or one that can
    /// drop samples): `t(i) = times.time(i)`, non-decreasing; from the grid's length on,
    /// `times.end()` plus whole periods of `rate`. A gap is slots of their own (NaN), whose
    /// times the reader chooses between its neighbors': as many as the gap's duration holds
    /// at the usual step, so that no bucket of a coarse level holds samples from both sides
    /// and is drawn across it. `rate` is the average rate, for showing
    /// it. With `hold`, a sample's value holds until the next sample (drawn as steps),
    /// otherwise lines join the samples.
    Irregular {
        rate: f64,
        hold: bool,
        times: Arc<TimeTable>,
    },
}

/// A piece of a [`Timebase::Segmented`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub first: u64,
    pub start: f64,
    pub rate: f64,
}

/// Samples evenly spaced in time, from [`Grid::regular_run`].
#[derive(Clone, Debug, PartialEq)]
pub struct RegularRun {
    /// The samples, which may reach past the grid's length.
    pub samples: Range<u64>,
    pub rate: f64,
    /// The time of `samples.start`.
    pub start: f64,
}

impl RegularRun {
    /// The time of `sample` on this run's line (also outside the run).
    pub fn time_of(&self, sample: u64) -> f64 {
        // u64 -> f64 is exact below 2^53 samples, far beyond any recording.
        if sample >= self.samples.start {
            self.start + (sample - self.samples.start) as f64 / self.rate
        } else {
            self.start - (self.samples.start - sample) as f64 / self.rate
        }
    }
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
    /// For an irregular time base, its density per tile at each level.
    density: Option<Arc<Density>>,
}

/// How densely an irregular grid's samples lie: per level-0 tile (block of [`TILE_LEN`]
/// slots), its present samples per second of present time; per tile of each level above,
/// the least of its blocks'. A block without present samples, or whose samples share one
/// time, has infinite density.
///
/// [`TILE_LEN`]: crate::tile::TILE_LEN
#[derive(Clone, Debug, PartialEq)]
struct Density {
    levels: Vec<Vec<f64>>,
}

impl Density {
    fn new(times: &TimeTable, segments: &[Range<u64>], len: u64) -> Density {
        use crate::tile::{Level, TILE_LEN, max_level};
        let blocks = len.div_ceil(TILE_LEN);
        let mut first = 0;
        let mut base = Vec::with_capacity(blocks as usize);
        for b in 0..blocks {
            let span = b * TILE_LEN..((b + 1) * TILE_LEN).min(len);
            first += segments[first..].partition_point(|s| s.end <= span.start);
            let (mut samples, mut seconds) = (0u64, 0.0);
            for s in segments[first..].iter().take_while(|s| s.start < span.end) {
                let part = s.start.max(span.start)..s.end.min(span.end);
                // A present run lasts from its first sample's time to the time of the slot
                // after it (a gap slot one usual step on, or the grid's end).
                samples += part.end - part.start;
                seconds += times.time(part.end) - times.time(part.start);
            }
            let density = samples as f64 / seconds;
            base.push(if samples > 0 && density.is_finite() {
                density
            } else {
                f64::INFINITY
            });
        }
        let mut levels = vec![base];
        for level in (1..=max_level(len).get()).filter_map(Level::new) {
            let below = &levels[levels.len() - 1];
            let ratio = if level.get() == 1 { 32 } else { 4 };
            let tiles = len.div_ceil(level.tile_samples()) as usize;
            let row = (0..tiles)
                .map(|t| {
                    below[(t * ratio).min(below.len())..((t + 1) * ratio).min(below.len())]
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min)
                })
                .collect();
            levels.push(row);
        }
        Density { levels }
    }
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
        match &timebase {
            &Timebase::Regular { rate, start } => {
                if !(rate.is_finite() && rate > 0.0) {
                    return Err(ModelError::InvalidRate(rate));
                }
                if !start.is_finite() {
                    return Err(ModelError::InvalidStart(start));
                }
            }
            Timebase::Segmented { rate, pieces } => {
                if !(rate.is_finite() && *rate > 0.0) {
                    return Err(ModelError::InvalidRate(*rate));
                }
                for (k, p) in pieces.iter().enumerate() {
                    if !(p.rate.is_finite() && p.rate > 0.0) {
                        return Err(ModelError::InvalidRate(p.rate));
                    }
                    if !p.start.is_finite() {
                        return Err(ModelError::InvalidStart(p.start));
                    }
                    let ordered = match k.checked_sub(1).map(|j| pieces[j]) {
                        None => p.first == 0,
                        Some(prev) => {
                            p.first > prev.first
                                && p.start
                                    > prev.start + (p.first - 1 - prev.first) as f64 / prev.rate
                        }
                    };
                    if !ordered {
                        return Err(ModelError::InvalidPiece(k));
                    }
                }
                if pieces.is_empty() {
                    return Err(ModelError::InvalidPiece(0));
                }
            }
            Timebase::Irregular { rate, times, .. } => {
                if !(rate.is_finite() && *rate > 0.0) {
                    return Err(ModelError::InvalidRate(*rate));
                }
                if times.len() != len {
                    return Err(ModelError::TimesLength {
                        times: times.len(),
                        len,
                    });
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

        let density = match &timebase {
            Timebase::Irregular { times, .. } => Some(Arc::new(Density::new(times, &merged, len))),
            _ => None,
        };
        Ok(Grid {
            timebase,
            len,
            segments: merged,
            sample_type,
            channels,
            density,
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

    /// The run of evenly spaced samples that holds `sample`: the whole grid for a regular
    /// time base (and beyond), the piece for a segmented one (the first piece also holds
    /// earlier samples, the last later ones); `None` for an irregular one.
    pub fn regular_run(&self, sample: u64) -> Option<RegularRun> {
        Some(match &self.timebase {
            &Timebase::Regular { rate, start } => RegularRun {
                samples: 0..u64::MAX,
                rate,
                start,
            },
            Timebase::Segmented { pieces, .. } => {
                let k = pieces.partition_point(|p| p.first <= sample).max(1) - 1;
                let p = pieces[k];
                let end = pieces.get(k + 1).map_or(u64::MAX, |n| n.first);
                RegularRun {
                    samples: p.first..end,
                    rate: p.rate,
                    start: p.start,
                }
            }
            Timebase::Irregular { .. } => return None,
        })
    }

    /// The table of an irregular time base.
    pub fn times(&self) -> Option<&TimeTable> {
        match &self.timebase {
            Timebase::Irregular { times, .. } => Some(times),
            _ => None,
        }
    }

    /// For an irregular time base, the least density (present samples per second) of the
    /// level-0 tiles in tile `time` of `level`; infinite where there are no samples or
    /// outside the grid. `None` for other time bases.
    pub fn least_density(&self, level: crate::tile::Level, time: u64) -> Option<f64> {
        let density = self.density.as_ref()?;
        Some(
            density
                .levels
                .get(usize::from(level.get()))
                .and_then(|row| row.get(usize::try_from(time).ok()?))
                .copied()
                .unwrap_or(f64::INFINITY),
        )
    }

    /// Whether a sample's value holds until the next sample (an irregular time base's
    /// `hold`), so it is drawn as steps rather than lines.
    pub fn holds_values(&self) -> bool {
        matches!(self.timebase, Timebase::Irregular { hold: true, .. })
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

    /// Sampling rate in Hz; for a segmented time base, the grid's overall rate, for an
    /// irregular one its average rate.
    pub fn rate(&self) -> f64 {
        match self.timebase {
            Timebase::Regular { rate, .. }
            | Timebase::Segmented { rate, .. }
            | Timebase::Irregular { rate, .. } => rate,
        }
    }

    /// Time of a sample in seconds since the recording start.
    pub fn time_of(&self, sample: u64) -> f64 {
        match self.timebase {
            // u64 -> f64 is exact below 2^53 samples, far beyond any recording.
            Timebase::Regular { rate, start } => start + sample as f64 / rate,
            Timebase::Segmented { .. } => match self.regular_run(sample) {
                Some(run) => run.time_of(sample),
                None => f64::NAN,
            },
            Timebase::Irregular {
                rate, ref times, ..
            } => match sample.checked_sub(times.len()) {
                None => times.time(sample),
                Some(past) => times.end() + past as f64 / rate,
            },
        }
    }

    /// The sample whose period contains time `t` (floor), clamped to `0..=len`. For an
    /// irregular time base, the last sample at or before `t`.
    ///
    /// Times computed by [`time_of`](Self::time_of) map back to their sample despite rounding
    /// (for an irregular time base, to the last sample of that time).
    pub fn sample_at(&self, t: f64) -> u64 {
        let x = match &self.timebase {
            Timebase::Irregular { times, .. } => return times.slot_at(t).min(self.len),
            &Timebase::Regular { rate, start } => (t - start) * rate,
            Timebase::Segmented { pieces, .. } => {
                // The last piece starting at or before `t`; its last sample before the next
                // piece at most (times in a gap belong to the samples before it).
                let k = pieces.partition_point(|p| p.start <= t).max(1) - 1;
                let p = pieces[k];
                let x = p.first as f64 + (t - p.start) * p.rate;
                match pieces.get(k + 1) {
                    Some(next) => x.min((next.first - 1) as f64),
                    None => x,
                }
            }
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
