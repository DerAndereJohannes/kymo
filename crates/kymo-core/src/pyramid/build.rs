//! Building a pyramid in one sequential pass.

use std::ops::Range;
use std::sync::Arc;

use super::{MemStorage, Pyramid, PyramidStorage};
use crate::stim::Scanner;
use crate::tile::{LEVEL_FACTOR, Level, TILE_LEN, TileSample, max_level};
use crate::{ChannelKind, GridId, ReadError, Recording, Sample, SampleType};

/// By default a step reads 10 s of samples or 16 MB, whichever is longer, but at most
/// 2^24 samples (all channels).
const CHUNK_SECONDS: f64 = 10.0;
const CHUNK_BYTES: u64 = 16 << 20;
const MAX_CHUNK_VALUES: u64 = 1 << 24;
/// Upper bound on the threads a build uses by default.
const MAX_THREADS: usize = 8;

/// Builds the pyramid of one grid in a single sequential pass over its samples, a chunk of
/// samples across all channels per [`step`](Self::step).
///
/// Each step reads one chunk (10 s or 16 MB by default, whichever is longer), stores every
/// level tile it completes and publishes its progress to the storage. Meanwhile a helper
/// thread fetches the next chunk from disk, and the chunk's channels are folded on several
/// threads. Gaps are never read. After a step fails, the builder must not be used further.
pub struct PyramidBuilder<'a> {
    recording: &'a Recording,
    grid: GridId,
    len: u64,
    chunk_len: u64,
    done: u64,
    /// The end of what was fetched ahead of the chunks so far.
    prefetched: u64,
    threads: usize,
    storage: Arc<dyn PyramidStorage>,
    state: State,
}

impl std::fmt::Debug for PyramidBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyramidBuilder")
            .field("grid", &self.grid)
            .field("len", &self.len)
            .field("chunk_len", &self.chunk_len)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

enum State {
    I16(Levels<i16>),
    I32(Levels<i32>),
    F32(Levels<f32>),
}

impl<'a> PyramidBuilder<'a> {
    /// A builder for `grid` that keeps the pyramid in memory.
    pub fn new(recording: &'a Recording, grid: GridId) -> Result<Self, ReadError> {
        let g = recording
            .grids()
            .get(grid.0 as usize)
            .ok_or(ReadError::UnknownGrid(grid))?;
        let (channels, len) = (g.channels().len(), g.len());
        let stim: Vec<(usize, Scanner)> = g
            .channels()
            .iter()
            .enumerate()
            .filter_map(|(row, &id)| {
                let channel = recording.channels().get(id.0 as usize)?;
                (channel.kind == ChannelKind::Stim)
                    .then(|| (row, Scanner::new(channel.trigger_mask)))
            })
            .collect();
        let state = match g.sample_type() {
            SampleType::I16 => State::I16(Levels::new(channels, len, stim)),
            SampleType::I32 => State::I32(Levels::new(channels, len, stim)),
            SampleType::F32 => State::F32(Levels::new(channels, len, stim)),
        };
        let seconds = (g.rate() * CHUNK_SECONDS).ceil() as u64;
        let instant_bytes = (channels * g.sample_type().size()).max(1) as u64;
        let chunk_len = seconds
            .max(CHUNK_BYTES / instant_bytes)
            .min(MAX_CHUNK_VALUES / channels.max(1) as u64)
            .max(1);
        Ok(PyramidBuilder {
            recording,
            grid,
            len,
            chunk_len,
            done: 0,
            prefetched: 0,
            threads: std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(MAX_THREADS),
            storage: Arc::new(MemStorage::new()),
            state,
        })
    }

    /// Writes into `storage` instead of memory. Call before the first step.
    pub fn with_storage(mut self, storage: Arc<dyn PyramidStorage>) -> Self {
        self.storage = storage;
        self
    }

    /// Reads `samples` samples (at least 1) per step.
    pub fn with_chunk_len(mut self, samples: u64) -> Self {
        self.chunk_len = samples.max(1);
        self
    }

    /// Reads and folds the channels of a step on up to `threads` threads (at least 1).
    /// The default is the number of CPUs, up to 8.
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// Samples of the grid.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Samples processed so far; everything is stored once this reaches [`len`](Self::len).
    pub fn done(&self) -> u64 {
        self.done
    }

    /// Processes the next chunk. Returns whether the build is complete.
    pub fn step(&mut self) -> Result<bool, ReadError> {
        if self.done < self.len {
            let range = self.done..self.done.saturating_add(self.chunk_len).min(self.len);
            // The next chunk, fetched from disk while this one is folded.
            let next = range.end.max(self.prefetched)
                ..range.end.saturating_add(self.chunk_len).min(self.len);
            let (rec, grid, storage) = (self.recording, self.grid, &*self.storage);
            let (r, threads, state) = (range.clone(), self.threads, &mut self.state);
            std::thread::scope(|scope| {
                if next.start < next.end {
                    let next = next.clone();
                    scope.spawn(move || rec.prefetch(grid, next));
                }
                match state {
                    State::I16(l) => l.step(rec, grid, r, storage, threads),
                    State::I32(l) => l.step(rec, grid, r, storage, threads),
                    State::F32(l) => l.step(rec, grid, r, storage, threads),
                }
            })?;
            self.prefetched = self.prefetched.max(next.end);
            self.done = range.end;
            self.storage.publish(self.done);
        }
        Ok(self.done >= self.len)
    }

    /// The pyramid being built: readable now, its tiles appear as the build passes them.
    pub fn pyramid(&self) -> Pyramid {
        let grid = self.recording.grid(self.grid).clone();
        Pyramid::new(self.grid, grid, Arc::clone(&self.storage))
    }

    /// Runs the remaining steps and returns the pyramid.
    pub fn finish(mut self) -> Result<Pyramid, ReadError> {
        while !self.step()? {}
        Ok(self.pyramid())
    }
}

/// The level tiles in progress, one per level from 1 to the top.
struct Levels<T> {
    channels: usize,
    len: u64,
    tiles: Vec<Partial<T>>,
    /// Sum of the current block so far, per channel.
    carry: Vec<f64>,
    /// The section being processed, channel-major.
    chunk: Vec<T>,
    /// Block sums of the section, channel-major.
    sums: Vec<f64>,
    /// The sums of one block across the channels, as stored.
    column: Vec<f64>,
    /// The stim channels' rows and their change scanners.
    stim: Vec<(usize, Scanner)>,
}

/// One level tile being filled: (min, max) pairs, channel-major, `TILE_LEN` per channel.
struct Partial<T> {
    level: Level,
    time: u64,
    pairs: Vec<T>,
}

const TILE: usize = TILE_LEN as usize;

impl<T: Fold> Levels<T> {
    fn new(channels: usize, len: u64, stim: Vec<(usize, Scanner)>) -> Self {
        let top = max_level(len);
        let mut tiles = Vec::new();
        let mut level = Level::RAW;
        while let Some(up) = level.up().filter(|_| level < top) {
            let mut pairs = Vec::new();
            pairs.resize(channels * TILE * 2, T::default());
            clear(&mut pairs);
            tiles.push(Partial {
                level: up,
                time: 0,
                pairs,
            });
            level = up;
        }
        Levels {
            channels,
            len,
            tiles,
            carry: vec![0.0; channels],
            chunk: Vec::new(),
            sums: Vec::new(),
            column: Vec::new(),
            stim,
        }
    }

    fn step(
        &mut self,
        recording: &Recording,
        grid: GridId,
        range: Range<u64>,
        storage: &dyn PyramidStorage,
        threads: usize,
    ) -> Result<(), ReadError> {
        let present: Vec<Range<u64>> = recording.grid(grid).present_in(range.clone()).collect();
        let level1 = self.tiles.first().map(|t| t.level.tile_samples());
        let mut start = range.start;
        while start < range.end {
            // A section lies inside one level-1 tile, so its channels can be folded
            // independently; tiles and sums are then stored in order.
            let end = level1.map_or(range.end, |l| {
                (start / l + 1).saturating_mul(l).min(range.end)
            });
            self.section(recording, grid, start..end, &present, threads)?;
            self.store_sums(start..end, storage);
            if level1.is_some_and(|l| end.is_multiple_of(l) || end == self.len) {
                self.finish_tile(storage);
            }
            start = end;
        }
        for (row, scanner) in &mut self.stim {
            let changes = scanner.take();
            if !changes.is_empty() {
                storage.put_changes(*row, &changes);
            }
        }
        Ok(())
    }

    /// Reads `section` (inside one level-1 tile) and folds its present samples into the
    /// level-1 buckets and the section's block sums, splitting the channels into up to
    /// `threads` groups that are read and folded in parallel.
    fn section(
        &mut self,
        recording: &Recording,
        grid: GridId,
        section: Range<u64>,
        present: &[Range<u64>],
        threads: usize,
    ) -> Result<(), ReadError> {
        let n = (section.end - section.start) as usize;
        let first_block = section.start / TILE_LEN;
        let blocks = ((section.end - 1) / TILE_LEN - first_block + 1) as usize;
        self.sums.clear();
        self.sums.resize(self.channels * blocks, 0.0);
        for (row, &carry) in self.sums.chunks_exact_mut(blocks).zip(&self.carry) {
            row[0] = carry;
        }
        let runs: Vec<Range<u64>> = present
            .iter()
            .map(|p| p.start.max(section.start)..p.end.min(section.end))
            .filter(|r| r.start < r.end)
            .collect();
        if runs.is_empty() || self.channels == 0 {
            return Ok(());
        }
        self.chunk.clear();
        self.chunk.resize(self.channels * n, T::default());

        let per = self.channels.div_ceil(threads.clamp(1, self.channels));
        let (bucket_len, first_bucket, mut pairs) = match self.tiles.first_mut() {
            Some(t) => (
                t.level.bucket_len(),
                t.time * TILE_LEN,
                t.pairs.chunks_mut(per * TILE * 2).map(Some).collect(),
            ),
            None => (1, 0, Vec::new()),
        };
        pairs.resize_with(self.channels.div_ceil(per), || None);

        let at = |i: u64| (i - section.start) as usize;
        let ids = recording.grid(grid).channels();
        let work = |ids: &[crate::ChannelId],
                    rows: &mut [T],
                    sums: &mut [f64],
                    mut pairs: Option<&mut [T]>|
         -> Result<(), ReadError> {
            recording.read_native(grid, ids, section.clone(), T::as_samples_mut(rows))?;
            for (c, row) in rows.chunks_exact(n).enumerate() {
                let sums = &mut sums[c * blocks..][..blocks];
                for run in &runs {
                    let mut i = run.start;
                    while i < run.end {
                        let block = i / TILE_LEN;
                        let end = ((block + 1) * TILE_LEN).min(run.end);
                        let b = (block - first_block) as usize;
                        sums[b] = T::sum(&row[at(i)..at(end)], sums[b]);
                        i = end;
                    }
                }
                let Some(pairs) = pairs.as_deref_mut() else {
                    continue;
                };
                // The sums are of the raw values, the envelopes of the codes. The chunk
                // stays raw for the stim scan: a masked f32 can lose bits.
                let masked: Vec<T>;
                let row = match recording.channel(ids[c]).trigger_mask {
                    Some(mask) => {
                        masked = row.iter().map(|x| x.masked(mask)).collect();
                        &masked[..]
                    }
                    None => row,
                };
                let pairs = &mut pairs[c * TILE * 2..][..TILE * 2];
                for run in &runs {
                    let mut i = run.start;
                    while i < run.end {
                        let k = i / bucket_len;
                        let end = ((k + 1) * bucket_len).min(run.end);
                        let slot = &mut pairs[2 * (k - first_bucket) as usize..][..2];
                        (slot[0], slot[1]) = T::min_max(&row[at(i)..at(end)], slot[0], slot[1]);
                        i = end;
                    }
                }
            }
            Ok(())
        };

        let mut jobs = ids
            .chunks(per)
            .zip(self.chunk.chunks_mut(per * n))
            .zip(self.sums.chunks_mut(per * blocks))
            .zip(pairs)
            .map(|(((ids, rows), sums), pairs)| (ids, rows, sums, pairs));
        let Some((ids, rows, sums, pairs)) = jobs.next() else {
            return Ok(());
        };
        let work = &work;
        std::thread::scope(|scope| -> Result<(), ReadError> {
            let others: Vec<_> = jobs
                .map(|(ids, rows, sums, pairs)| scope.spawn(move || work(ids, rows, sums, pairs)))
                .collect();
            let mut result = work(ids, rows, sums, pairs);
            for other in others {
                let other = other
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                result = result.and(other);
            }
            result
        })?;

        for (row, scanner) in &mut self.stim {
            let row = &self.chunk[*row * n..][..n];
            for run in &runs {
                let codes = row[at(run.start)..at(run.end)].iter().map(|&x| x.code());
                scanner.scan(run.start, codes);
            }
        }
        Ok(())
    }

    /// Stores the sums of the blocks that `section` completes and carries the last block's
    /// sums over if it continues past the section.
    fn store_sums(&mut self, section: Range<u64>, storage: &dyn PyramidStorage) {
        let first_block = section.start / TILE_LEN;
        let blocks = ((section.end - 1) / TILE_LEN - first_block + 1) as usize;
        let last_complete = section.end.is_multiple_of(TILE_LEN) || section.end == self.len;
        let complete = if last_complete { blocks } else { blocks - 1 };
        for b in 0..complete {
            self.column.clear();
            self.column
                .extend(self.sums.chunks_exact(blocks).map(|row| row[b]));
            storage.put_sums(first_block + b as u64, &self.column);
        }
        for (carry, row) in self.carry.iter_mut().zip(self.sums.chunks_exact(blocks)) {
            *carry = if last_complete { 0.0 } else { row[blocks - 1] };
        }
    }

    /// Stores the current level-1 tile, folds it into level 2, and so on up while each
    /// parent tile is complete.
    fn finish_tile(&mut self, storage: &dyn PyramidStorage) {
        let mut l = 0;
        loop {
            let (lower, upper) = self.tiles.split_at_mut(l + 1);
            let tile = &mut lower[l];
            let time = tile.time;
            let buckets = self.len.div_ceil(tile.level.bucket_len());
            let len = (buckets - time * TILE_LEN).min(TILE_LEN) as usize;

            let parent = upper.first_mut();
            let mut out = Vec::with_capacity(self.channels * len * 2);
            for c in 0..self.channels {
                let pairs = &tile.pairs[c * TILE * 2..][..len * 2];
                out.extend_from_slice(pairs);
            }
            if let Some(parent) = parent {
                let base = (time % LEVEL_FACTOR) as usize * (TILE / LEVEL_FACTOR as usize);
                for c in 0..self.channels {
                    let pairs = &tile.pairs[c * TILE * 2..][..len * 2];
                    let up = &mut parent.pairs[c * TILE * 2..][..TILE * 2];
                    for (j, pair) in pairs.as_chunks::<2>().0.iter().enumerate() {
                        let slot = &mut up[2 * (base + j / LEVEL_FACTOR as usize)..][..2];
                        if pair[0] < slot[0] {
                            slot[0] = pair[0];
                        }
                        if pair[1] > slot[1] {
                            slot[1] = pair[1];
                        }
                    }
                }
            }
            storage.put_tile(tile.level, time, T::into_data(out.into_boxed_slice()));
            clear(&mut tile.pairs);
            tile.time = time + 1;

            let last = time == (buckets - 1) / TILE_LEN;
            let parent_complete = time % LEVEL_FACTOR == LEVEL_FACTOR - 1 || last;
            if l + 1 == self.tiles.len() || !parent_complete {
                break;
            }
            l += 1;
        }
    }
}

/// The per-sample kernels of the build, one per sample type so that they vectorise.
trait Fold: TileSample {
    /// `acc` plus the sum of `xs`, which lie inside one block. Exact for integers; for
    /// floats the samples are added one by one, in order.
    fn sum(xs: &[Self], acc: f64) -> f64;
    /// `(min, max)` extended by `xs`. NaN never becomes a min or max.
    fn min_max(xs: &[Self], min: Self, max: Self) -> (Self, Self);
    /// The value as a stim code.
    fn code(self) -> i32;
}

impl Fold for i16 {
    fn code(self) -> i32 {
        i32::from(self)
    }

    fn sum(xs: &[i16], acc: f64) -> f64 {
        debug_assert!(xs.len() <= TILE);
        // At most 4096 · 2^15 = 2^27 in magnitude: exact in i32, then exact in f64.
        acc + f64::from(xs.iter().map(|&x| i32::from(x)).sum::<i32>())
    }

    fn min_max(xs: &[i16], min: i16, max: i16) -> (i16, i16) {
        let lo = xs.iter().copied().fold(min, Ord::min);
        let hi = xs.iter().copied().fold(max, Ord::max);
        (lo, hi)
    }
}

impl Fold for i32 {
    fn code(self) -> i32 {
        self
    }

    fn sum(xs: &[i32], acc: f64) -> f64 {
        debug_assert!(xs.len() <= TILE);
        // At most 4096 · 2^31 = 2^43 in magnitude: exact in i64 and in f64.
        acc + xs.iter().map(|&x| i64::from(x)).sum::<i64>() as f64
    }

    fn min_max(xs: &[i32], min: i32, max: i32) -> (i32, i32) {
        let lo = xs.iter().copied().fold(min, Ord::min);
        let hi = xs.iter().copied().fold(max, Ord::max);
        (lo, hi)
    }
}

impl Fold for f32 {
    /// Truncated toward zero and saturated; NaN is 0.
    fn code(self) -> i32 {
        self as i32
    }

    fn sum(xs: &[f32], acc: f64) -> f64 {
        xs.iter().fold(acc, |sum, &x| sum + f64::from(x))
    }

    fn min_max(xs: &[f32], min: f32, max: f32) -> (f32, f32) {
        // NaN compares false either way and never replaces the bound.
        xs.iter().fold((min, max), |(lo, hi), &x| {
            (if x < lo { x } else { lo }, if x > hi { x } else { hi })
        })
    }
}

/// Resets (min, max) pairs to the empty sentinel.
fn clear<T: Sample>(pairs: &mut [T]) {
    for pair in pairs.as_chunks_mut::<2>().0 {
        pair[0] = T::EMPTY_MIN;
        pair[1] = T::EMPTY_MAX;
    }
}
