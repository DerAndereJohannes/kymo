//! The index pass over EDF+/BDF+ records: record onsets, annotations, and the grids'
//! segments that follow from them.

use super::tal::parse_block;
use super::{EdfError, RecordLayout};
use crate::{AnnotationData, Grid, Header, Origin, Timebase};

/// At most this many TAL problems are reported one by one.
const MAX_TAL_WARNINGS: usize = 10;

/// What the index pass found.
pub(crate) struct Index {
    pub grids: Vec<Grid>,
    /// Per grid, the first grid sample of each record (EDF+D/BDF+D only).
    pub record_starts: Option<Vec<Vec<u64>>>,
    pub annotations: Vec<AnnotationData>,
}

/// Reads every record's annotation signals (EDF+/BDF+ only) and rebuilds the header's
/// grids from the record onsets.
pub(crate) fn index(
    data: &[u8],
    header: &Header,
    layout: &RecordLayout,
    warnings: &mut Vec<String>,
) -> Result<Index, EdfError> {
    let bps = layout.bytes_per_sample as u64;
    let mut onsets = Vec::with_capacity(layout.n_records as usize);
    let mut annotations = Vec::new();
    let mut tal_problems = Vec::new();

    for record in 0..layout.n_records {
        let record_start = layout.header_bytes + record * layout.record_bytes;
        let mut onset = None;
        for (a, block) in layout.annotations.iter().enumerate() {
            let start = (record_start + block.offset) as usize;
            let bytes = data
                .get(start..start + (block.samples * bps) as usize)
                .ok_or_else(|| record_error(record, "lies outside the file"))?;
            let (tals, problems) = parse_block(bytes);
            tal_problems.extend(
                problems
                    .into_iter()
                    .map(|p| format!("record {record}: {p}")),
            );
            for (t, tal) in tals.into_iter().enumerate() {
                // The first TAL of the first annotation signal keeps the record's time.
                if a == 0 && t == 0 {
                    onset = Some(tal.onset);
                }
                for text in tal.texts.into_iter().filter(|t| !t.is_empty()) {
                    annotations.push(AnnotationData {
                        onset: tal.onset,
                        duration: tal.duration.unwrap_or(0.0),
                        description: text,
                        channels: Vec::new(),
                        origin: Origin::File,
                        stream: None,
                    });
                }
            }
        }
        onsets.push(onset);
    }

    if tal_problems.len() > MAX_TAL_WARNINGS {
        let more = tal_problems.len() - MAX_TAL_WARNINGS;
        tal_problems.truncate(MAX_TAL_WARNINGS);
        tal_problems.push(format!("{more} more malformed TALs were skipped"));
    }
    warnings.extend(tal_problems);

    let onsets = record_onsets(onsets, layout, warnings)?;
    let first = onsets.first().copied().unwrap_or(0.0);

    let mut grids = Vec::with_capacity(header.grids.len());
    let mut all_starts = Vec::with_capacity(header.grids.len());
    for grid in &header.grids {
        // All channels of a grid share samples per record; a grid is never empty.
        let spr = layout.channels[grid.channels()[0].0 as usize].samples;
        let timebase = Timebase::Regular {
            rate: grid.rate(),
            start: first,
        };
        if !layout.discontinuous {
            grids.push(Grid::continuous(
                timebase,
                grid.len(),
                grid.sample_type(),
                grid.channels().to_vec(),
            )?);
            continue;
        }
        let starts = record_starts(&onsets, spr, layout.record_duration)?;
        let mut segments: Vec<std::ops::Range<u64>> = Vec::new();
        for &s in &starts {
            match segments.last_mut() {
                Some(last) if last.end == s => last.end = s + spr,
                _ => segments.push(s..s + spr),
            }
        }
        let len = segments.last().map_or(0, |s| s.end);
        grids.push(Grid::new(
            timebase,
            len,
            segments,
            grid.sample_type(),
            grid.channels().to_vec(),
        )?);
        all_starts.push(starts);
    }

    Ok(Index {
        grids,
        record_starts: layout.discontinuous.then_some(all_starts),
        annotations,
    })
}

fn record_error(record: u64, reason: &str) -> EdfError {
    EdfError::InvalidRecord {
        record,
        reason: reason.into(),
    }
}

/// Checks the time-keeping onsets: EDF+D needs one in every record, in order and without
/// overlap; for EDF+C they should be contiguous, which is only warned about.
fn record_onsets(
    onsets: Vec<Option<f64>>,
    layout: &RecordLayout,
    warnings: &mut Vec<String>,
) -> Result<Vec<f64>, EdfError> {
    let duration = layout.record_duration;
    let mut result = Vec::with_capacity(onsets.len());
    let mut warned = false;
    for (r, onset) in onsets.into_iter().enumerate() {
        let r = r as u64;
        let previous = result.last().copied();
        let onset = match (onset, previous) {
            (Some(onset), _) => onset,
            (None, _) if layout.discontinuous => {
                return Err(record_error(r, "has no time-keeping annotation"));
            }
            // EDF+C: a missing time stamp is implied by contiguity.
            (None, Some(p)) => p + duration,
            (None, None) => 0.0,
        };
        if let Some(p) = previous {
            // Up to a microsecond of slack for rounding in the written onsets.
            if onset < p + duration - 1e-6 {
                if layout.discontinuous {
                    return Err(record_error(
                        r,
                        &format!(
                            "starts at {onset} s, before the previous record ends ({} s)",
                            p + duration
                        ),
                    ));
                }
            } else if !layout.discontinuous && onset > p + duration + 1e-6 && !warned {
                warnings.push(format!(
                    "record {r} starts at {onset} s, not right after the previous one ({} s), \
                     but the file is marked continuous; reading it without gaps",
                    p + duration
                ));
                warned = true;
            }
        }
        result.push(onset);
    }
    Ok(result)
}

/// The first grid sample of each record: `round((onset(r) - onset(0)) * spr / duration)`.
fn record_starts(onsets: &[f64], spr: u64, duration: f64) -> Result<Vec<u64>, EdfError> {
    let first = onsets.first().copied().unwrap_or(0.0);
    let mut starts: Vec<u64> = Vec::with_capacity(onsets.len());
    for (r, &onset) in onsets.iter().enumerate() {
        let start = ((onset - first) * spr as f64 / duration).round();
        if start.is_nan() || start < 0.0 || start >= 2f64.powi(62) {
            return Err(record_error(r as u64, "has an onset out of range"));
        }
        let start = start as u64;
        if starts.last().is_some_and(|&p| start < p + spr) {
            return Err(record_error(
                r as u64,
                "overlaps the previous record after rounding to samples",
            ));
        }
        starts.push(start);
    }
    Ok(starts)
}
