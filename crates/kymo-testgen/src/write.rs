//! Streaming writer: header, then data records filled in parallel batches.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::header;
use crate::signal::ChannelSignal;
use crate::{Error, Spec, grid_start, onset};

/// Target size of one batch of records filled in parallel, in bytes.
const BATCH_BYTES: usize = 64 << 20;

/// Byte layout of a record and the per-record annotation bytes.
pub(crate) struct Layout {
    pub(crate) record_bytes: usize,
    pub(crate) annotation_spr: Option<u32>,
    /// Record onsets (EDF+/BDF+ only), as in [`Spec::record_onset`].
    onsets: Vec<f64>,
    /// The annotation signal's bytes per record, unpadded (EDF+/BDF+ only).
    tals: Vec<Vec<u8>>,
}

impl Layout {
    pub(crate) fn new(spec: &Spec) -> Result<Self, Error> {
        if !spec.format.is_edf() {
            return Err(Error::Invalid(format!(
                "{} has no data records",
                spec.format.name()
            )));
        }
        spec.validate()?;
        let bps = spec.format.bytes_per_sample();
        let data_bytes: usize = spec
            .channels
            .iter()
            .map(|c| c.samples_per_record as usize * bps)
            .sum();
        if !spec.format.is_plus() {
            return Ok(Layout {
                record_bytes: data_bytes,
                annotation_spr: None,
                onsets: Vec::new(),
                tals: Vec::new(),
            });
        }
        if spec.n_records == 0 && !spec.annotations.is_empty() {
            return Err(Error::Invalid(
                "annotations need at least one record".into(),
            ));
        }

        // Same arithmetic and summation order as Spec::record_onset, so values match exactly.
        let mut onsets = Vec::with_capacity(spec.n_records as usize);
        let (mut gap_sum, mut next_gap) = (0.0, 0);
        for r in 0..spec.n_records {
            while let Some(g) = spec.gaps.get(next_gap).filter(|g| g.before_record <= r) {
                gap_sum += g.seconds;
                next_gap += 1;
            }
            onsets.push(onset(spec.first_onset, r, spec.record_duration, gap_sum));
        }

        let mut tals: Vec<Vec<u8>> = onsets
            .iter()
            .map(|&o| {
                let mut bytes = Vec::new();
                header::timekeeping_tal(o, &mut bytes);
                bytes
            })
            .collect();
        for a in &spec.annotations {
            let record = onsets.partition_point(|&o| o <= a.onset).saturating_sub(1);
            header::tal(a, &mut tals[record]);
        }
        let longest = tals.iter().map(Vec::len).max().unwrap_or(0);
        let annotation_spr = longest.div_ceil(bps).max(1);
        let annotation_spr = u32::try_from(annotation_spr)
            .ok()
            .filter(|&n| n <= 99_999_999)
            .ok_or_else(|| Error::Invalid("too many annotations in one record".into()))?;
        Ok(Layout {
            record_bytes: data_bytes + annotation_spr as usize * bps,
            annotation_spr: Some(annotation_spr),
            onsets,
            tals,
        })
    }

    fn record_start(&self, spec: &Spec, channel: usize, record: u64) -> u64 {
        let spr = spec.channels[channel].samples_per_record;
        if spec.format.is_discontinuous() {
            let elapsed = self.onsets[record as usize] - self.onsets[0];
            grid_start(elapsed, spr, spec.record_duration)
        } else {
            record * u64::from(spr)
        }
    }
}

/// Writes `spec` to a new file at `path` (overwriting it). BrainVision writes its data and
/// marker files beside it too (see [`Format::files`](crate::Format::files)).
pub fn write(spec: &Spec, path: impl AsRef<Path>) -> Result<(), Error> {
    if spec.format.is_brainvision() {
        return crate::brainvision::write(spec, path.as_ref());
    }
    if spec.format.is_eeglab() {
        return crate::eeglab::write(spec, path.as_ref());
    }
    if spec.format.is_gdf() {
        return crate::gdf::write(spec, path.as_ref());
    }
    if spec.format.is_fiff() {
        return crate::fiff::write(spec, path.as_ref());
    }
    let file = File::create(path)?;
    write_to(spec, BufWriter::with_capacity(1 << 20, file))
}

/// Writes `spec` to `out`. EDF/BDF only: BrainVision and EEGLAB need [`write()`], as they
/// write several files, and so does GDF.
pub fn write_to<W: Write>(spec: &Spec, mut out: W) -> Result<(), Error> {
    if !spec.format.is_edf() {
        return Err(Error::Invalid(format!(
            "{} is written by write() with a path",
            spec.format.name()
        )));
    }
    let layout = Layout::new(spec)?;
    out.write_all(&header::header(spec, layout.annotation_spr))?;

    let signals: Vec<(ChannelSignal, Vec<f64>)> = (0..spec.channels.len())
        .map(|c| {
            let s = ChannelSignal::new(spec, c);
            let table = (0..s.sine_period).map(|k| s.sine(k)).collect();
            (s, table)
        })
        .collect();

    let record_bytes = layout.record_bytes;
    let n_records = spec.n_records as usize;
    let batch_records = (BATCH_BYTES / record_bytes.max(1)).clamp(1, n_records.max(1));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut buf = vec![0u8; batch_records * record_bytes];

    for batch_start in (0..n_records).step_by(batch_records) {
        let count = batch_records.min(n_records - batch_start);
        let batch = &mut buf[..count * record_bytes];
        let per_thread = count.div_ceil(threads);
        std::thread::scope(|scope| {
            for (t, part) in batch.chunks_mut(per_thread * record_bytes).enumerate() {
                let (layout, signals) = (&layout, &signals);
                scope.spawn(move || {
                    for (k, record) in part.chunks_mut(record_bytes).enumerate() {
                        let r = (batch_start + t * per_thread + k) as u64;
                        fill_record(spec, layout, signals, r, record);
                    }
                });
            }
        });
        out.write_all(batch)?;
    }
    out.flush()?;
    Ok(())
}

fn fill_record(
    spec: &Spec,
    layout: &Layout,
    signals: &[(ChannelSignal, Vec<f64>)],
    r: u64,
    record: &mut [u8],
) {
    let bdf = spec.format.is_bdf();
    let bps = spec.format.bytes_per_sample();
    let mut pos = 0;
    for (c, (signal, table)) in signals.iter().enumerate() {
        let spr = spec.channels[c].samples_per_record as usize;
        let start = layout.record_start(spec, c, r);
        let mut phase = (start % signal.sine_period) as usize;
        let bytes = &mut record[pos..pos + spr * bps];
        for (j, sample) in bytes.chunks_exact_mut(bps).enumerate() {
            let v = signal.value(start + j as u64, table[phase]);
            phase += 1;
            if phase == table.len() {
                phase = 0;
            }
            let le = v.to_le_bytes();
            if bdf {
                sample.copy_from_slice(&le[..3]);
            } else {
                sample.copy_from_slice(&(v as i16).to_le_bytes());
            }
        }
        pos += spr * bps;
    }
    if let Some(tal) = layout.tals.get(r as usize) {
        let annotation = &mut record[pos..];
        annotation[..tal.len()].copy_from_slice(tal);
        annotation[tal.len()..].fill(0);
    }
}
