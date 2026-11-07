//! The recording: header, sample grids and the read path over a format reader.

use std::fmt;
use std::ops::Range;

use crate::{
    AnnotationData, Channel, ChannelId, Grid, GridId, ModelError, ReadError, Sample, SampleType,
    SamplesMut,
};

/// File metadata, as filled by a reader.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    /// Format name, e.g. `"EDF+D"` or `"BDF"`.
    pub format: &'static str,
    /// Wall-clock time of `t = 0`, without time zone (as stored in EDF).
    pub start_time: Option<jiff::civil::DateTime>,
    pub channels: Vec<Channel>,
    pub grids: Vec<Grid>,
    /// Format-specific fields (patient, equipment, ...) for an info view.
    pub extra: Vec<(String, String)>,
}

/// What a reader produces when it opens a file.
pub struct Opened {
    pub header: Header,
    /// Annotations stored in the file, read at index time.
    pub annotations: Vec<AnnotationData>,
    /// Problems the reader worked around, e.g. a truncated last record.
    pub warnings: Vec<String>,
    pub reader: Box<dyn BlockReader>,
}

impl fmt::Debug for Opened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("header", &self.header)
            .field("annotations", &self.annotations)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

/// The format-specific read path.
///
/// Byte ranges, record layout and the index stay behind this trait; above it only the
/// sample-space segments of each [`Grid`] are visible.
pub trait BlockReader: Send + Sync {
    /// Reads raw samples of `channels` (all in `grid`) over `range` into `out`, channel-major.
    ///
    /// [`Recording`] guarantees that the grid and channels exist, that `range` is non-empty and
    /// lies inside one segment of the grid, and that `out` holds `channels.len() * range.len()`
    /// samples of the grid's sample type.
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError>;
}

/// An open recording. Immutable, so it can be shared with tile workers as `Arc<Recording>`.
///
/// Mutable layers (annotations, display state) live beside it.
pub struct Recording {
    header: Header,
    warnings: Vec<String>,
    reader: Box<dyn BlockReader>,
}

impl fmt::Debug for Recording {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recording")
            .field("header", &self.header)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl Recording {
    /// Validates what a reader opened. Returns the recording and the file's annotations,
    /// which belong in an annotation store beside it.
    pub fn new(opened: Opened) -> Result<(Self, Vec<AnnotationData>), ModelError> {
        let Opened {
            header,
            annotations,
            warnings,
            reader,
        } = opened;
        validate_header(&header)?;
        validate_annotations(&annotations, header.channels.len())?;
        let recording = Recording {
            header,
            warnings,
            reader,
        };
        Ok((recording, annotations))
    }

    pub fn format(&self) -> &'static str {
        self.header.format
    }

    pub fn start_time(&self) -> Option<jiff::civil::DateTime> {
        self.header.start_time
    }

    pub fn extra(&self) -> &[(String, String)] {
        &self.header.extra
    }

    pub fn channels(&self) -> &[Channel] {
        &self.header.channels
    }

    /// # Panics
    /// If `id` is not a channel of this recording.
    pub fn channel(&self, id: ChannelId) -> &Channel {
        &self.header.channels[id.0 as usize]
    }

    pub fn grids(&self) -> &[Grid] {
        &self.header.grids
    }

    /// # Panics
    /// If `id` is not a grid of this recording.
    pub fn grid(&self, id: GridId) -> &Grid {
        &self.header.grids[id.0 as usize]
    }

    /// Seconds from the recording start to the end of the longest grid.
    pub fn duration(&self) -> f64 {
        self.header
            .grids
            .iter()
            .map(|g| g.time_of(g.len()))
            .fold(0.0, f64::max)
    }

    /// Highest sampling rate of all grids: the recording-level rate for the common case only.
    pub fn sfreq(&self) -> f64 {
        self.header.grids.iter().map(Grid::rate).fold(0.0, f64::max)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Reads native samples of `channels` (all in `grid`) over `range` into `out`, channel-major.
    ///
    /// Gap positions are set to zero; use [`Grid::present_in`] to tell them apart.
    pub fn read_native(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        let (g, len) = self.check(grid, channels, &range)?;
        check_buffer(
            channels.len().saturating_mul(len),
            g.sample_type(),
            out.len(),
            out.sample_type(),
        )?;
        match out {
            SamplesMut::I16(buf) => self.read_typed(grid, g, channels, range, len, buf),
            SamplesMut::I32(buf) => self.read_typed(grid, g, channels, range, len, buf),
            SamplesMut::F32(buf) => self.read_typed(grid, g, channels, range, len, buf),
        }
    }

    /// Reads samples in physical units (computed in f64, stored as f32), NaN in gaps.
    ///
    /// Layout as in [`read_native`](Self::read_native).
    pub fn read_physical(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: &mut [f32],
    ) -> Result<(), ReadError> {
        let (g, len) = self.check(grid, channels, &range)?;
        check_buffer(
            channels.len().saturating_mul(len),
            SampleType::F32,
            out.len(),
            SampleType::F32,
        )?;
        match g.sample_type() {
            SampleType::I16 => self.physical_typed::<i16>(grid, g, channels, range, len, out),
            SampleType::I32 => self.physical_typed::<i32>(grid, g, channels, range, len, out),
            SampleType::F32 => self.physical_typed::<f32>(grid, g, channels, range, len, out),
        }
    }

    /// Checks a read request; returns the grid and the range length.
    fn check(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: &Range<u64>,
    ) -> Result<(&Grid, usize), ReadError> {
        let g = self
            .header
            .grids
            .get(grid.0 as usize)
            .ok_or(ReadError::UnknownGrid(grid))?;
        for &channel in channels {
            match self.header.channels.get(channel.0 as usize) {
                Some(c) if c.grid == grid => {}
                _ => return Err(ReadError::ChannelNotInGrid { channel, grid }),
            }
        }
        let out_of_range = || ReadError::OutOfRange {
            range: range.clone(),
            len: g.len(),
        };
        if range.start > range.end || range.end > g.len() {
            return Err(out_of_range());
        }
        let len = usize::try_from(range.end - range.start).map_err(|_| out_of_range())?;
        Ok((g, len))
    }

    /// Reads a checked request, calling the reader only on present sub-ranges.
    fn read_typed<T: Sample>(
        &self,
        grid_id: GridId,
        grid: &Grid,
        channels: &[ChannelId],
        range: Range<u64>,
        len: usize,
        out: &mut [T],
    ) -> Result<(), ReadError> {
        if out.is_empty() {
            return Ok(());
        }
        let mut present = grid.present_in(range.clone());
        let first = present.next();
        if first.as_ref() == Some(&range) {
            return self
                .reader
                .read(grid_id, channels, range, T::as_samples_mut(out));
        }

        out.fill(T::default());
        let mut scratch = Vec::new();
        for part in first.into_iter().chain(present) {
            // Both fit in usize: they are bounded by `len`.
            let n = (part.end - part.start) as usize;
            let offset = (part.start - range.start) as usize;
            if let [_] = channels {
                let dst = &mut out[offset..offset + n];
                self.reader
                    .read(grid_id, channels, part, T::as_samples_mut(dst))?;
            } else {
                scratch.clear();
                scratch.resize(channels.len() * n, T::default());
                self.reader
                    .read(grid_id, channels, part, T::as_samples_mut(&mut scratch))?;
                for (k, row) in scratch.chunks_exact(n).enumerate() {
                    out[k * len + offset..][..n].copy_from_slice(row);
                }
            }
        }
        Ok(())
    }

    fn physical_typed<T: Sample>(
        &self,
        grid_id: GridId,
        grid: &Grid,
        channels: &[ChannelId],
        range: Range<u64>,
        len: usize,
        out: &mut [f32],
    ) -> Result<(), ReadError> {
        let mut native = vec![T::default(); out.len()];
        self.read_typed(grid_id, grid, channels, range.clone(), len, &mut native)?;
        out.fill(f32::NAN);
        for (k, &id) in channels.iter().enumerate() {
            let Channel { gain, offset, .. } = *self.channel(id);
            for part in grid.present_in(range.clone()) {
                let start = k * len + (part.start - range.start) as usize;
                let end = start + (part.end - part.start) as usize;
                for (dst, &src) in out[start..end].iter_mut().zip(&native[start..end]) {
                    *dst = (src.to_f64() * gain + offset) as f32;
                }
            }
        }
        Ok(())
    }
}

fn check_buffer(
    expected: usize,
    expected_type: SampleType,
    found: usize,
    found_type: SampleType,
) -> Result<(), ReadError> {
    if expected == found && expected_type == found_type {
        Ok(())
    } else {
        Err(ReadError::BufferMismatch {
            expected,
            expected_type,
            found,
            found_type,
        })
    }
}

fn validate_header(header: &Header) -> Result<(), ModelError> {
    let channels = &header.channels;
    if channels.is_empty() {
        return Err(ModelError::NoChannels);
    }
    for (i, c) in channels.iter().enumerate() {
        let id = ChannelId(i as u32);
        if c.grid.0 as usize >= header.grids.len() {
            return Err(ModelError::UnknownGrid {
                channel: id,
                grid: c.grid,
            });
        }
        if !(c.gain.is_finite() && c.offset.is_finite()) {
            return Err(ModelError::InvalidScaling {
                channel: id,
                gain: c.gain,
                offset: c.offset,
            });
        }
    }

    let mut listed = vec![false; channels.len()];
    for (g, grid) in header.grids.iter().enumerate() {
        let grid_id = GridId(g as u32);
        for &id in grid.channels() {
            let Some(channel) = channels.get(id.0 as usize) else {
                return Err(ModelError::UnknownChannel {
                    grid: grid_id,
                    channel: id,
                });
            };
            if channel.grid != grid_id {
                return Err(ModelError::GridMismatch {
                    channel: id,
                    expected: channel.grid,
                    found: grid_id,
                });
            }
            // A channel listed in its own grid twice is caught by `Grid::new`.
            listed[id.0 as usize] = true;
        }
    }
    if let Some(i) = listed.iter().position(|&l| !l) {
        return Err(ModelError::ChannelNotListed(ChannelId(i as u32)));
    }
    Ok(())
}

fn validate_annotations(
    annotations: &[AnnotationData],
    n_channels: usize,
) -> Result<(), ModelError> {
    for (index, a) in annotations.iter().enumerate() {
        let reason = if let Some(reason) = a.timing_error() {
            reason
        } else if let Some(c) = a.channels.iter().find(|c| c.0 as usize >= n_channels) {
            format!("channel {c:?} does not exist")
        } else {
            continue;
        };
        return Err(ModelError::InvalidAnnotation { index, reason });
    }
    Ok(())
}
