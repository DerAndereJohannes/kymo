//! Building a pyramid in one sequential pass.

use std::ops::Range;

use super::{MemStorage, Pyramid, PyramidStorage};
use crate::tile::{LEVEL_FACTOR, Level, TILE_LEN, TileSample, max_level};
use crate::{GridId, ReadError, Recording, Sample, SampleType};

/// Seconds of samples read per step by default.
const CHUNK_SECONDS: f64 = 10.0;
/// Upper bound on the samples (all channels) read per step by default.
const MAX_CHUNK_VALUES: u64 = 1 << 24;

/// Builds the pyramid of one grid in a single sequential pass over its samples, a chunk of
/// samples across all channels per [`step`](Self::step).
///
/// Each step reads one chunk (about 10 s by default) and stores every level tile it
/// completes. Gaps are never read. After a step fails, the builder must not be used further.
pub struct PyramidBuilder<'a> {
    recording: &'a Recording,
    grid: GridId,
    len: u64,
    chunk_len: u64,
    done: u64,
    storage: Box<dyn PyramidStorage>,
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
        let state = match g.sample_type() {
            SampleType::I16 => State::I16(Levels::new(channels, len)),
            SampleType::I32 => State::I32(Levels::new(channels, len)),
            SampleType::F32 => State::F32(Levels::new(channels, len)),
        };
        let seconds = (g.rate() * CHUNK_SECONDS).ceil() as u64;
        let chunk_len = seconds.min(MAX_CHUNK_VALUES / channels as u64).max(1);
        Ok(PyramidBuilder {
            recording,
            grid,
            len,
            chunk_len,
            done: 0,
            storage: Box::new(MemStorage::new()),
            state,
        })
    }

    /// Writes into `storage` instead of memory. Call before the first step.
    pub fn with_storage(mut self, storage: Box<dyn PyramidStorage>) -> Self {
        self.storage = storage;
        self
    }

    /// Reads `samples` samples (at least 1) per step.
    pub fn with_chunk_len(mut self, samples: u64) -> Self {
        self.chunk_len = samples.max(1);
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
            let (rec, grid, storage) = (self.recording, self.grid, self.storage.as_mut());
            match &mut self.state {
                State::I16(l) => l.step(rec, grid, range.clone(), storage)?,
                State::I32(l) => l.step(rec, grid, range.clone(), storage)?,
                State::F32(l) => l.step(rec, grid, range.clone(), storage)?,
            }
            self.done = range.end;
        }
        Ok(self.done >= self.len)
    }

    /// Runs the remaining steps and returns the pyramid.
    pub fn finish(mut self) -> Result<Pyramid, ReadError> {
        while !self.step()? {}
        let grid = self.recording.grid(self.grid).clone();
        Ok(Pyramid::new(self.grid, grid, self.storage))
    }
}

/// The level tiles in progress, one per level from 1 to the top.
struct Levels<T> {
    channels: usize,
    len: u64,
    tiles: Vec<Partial<T>>,
    /// Sums of the current block, per channel.
    sums: Vec<f64>,
    /// The chunk being processed, channel-major.
    chunk: Vec<T>,
}

/// One level tile being filled: (min, max) pairs, channel-major, `TILE_LEN` per channel.
struct Partial<T> {
    level: Level,
    time: u64,
    pairs: Vec<T>,
}

const TILE: usize = TILE_LEN as usize;

impl<T: TileSample> Levels<T> {
    fn new(channels: usize, len: u64) -> Self {
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
            sums: vec![0.0; channels],
            chunk: Vec::new(),
        }
    }

    fn step(
        &mut self,
        recording: &Recording,
        grid: GridId,
        range: Range<u64>,
        storage: &mut dyn PyramidStorage,
    ) -> Result<(), ReadError> {
        let g = recording.grid(grid);
        let n = (range.end - range.start) as usize;
        let present: Vec<Range<u64>> = g.present_in(range.clone()).collect();
        if !present.is_empty() {
            self.chunk.clear();
            self.chunk.resize(self.channels * n, T::default());
            let out = T::as_samples_mut(&mut self.chunk);
            recording.read_native(grid, g.channels(), range.clone(), out)?;
        }
        let level1 = Level::RAW.up().map_or(u64::MAX, Level::tile_samples);
        // Pieces end at block boundaries; level-1 tiles end at block boundaries too.
        let mut start = range.start;
        while start < range.end {
            let block = start / TILE_LEN;
            let end = ((block + 1) * TILE_LEN).min(range.end);
            for p in &present {
                let run = p.start.max(start)..p.end.min(end);
                if run.start < run.end {
                    self.fold(range.start, n, run);
                }
            }
            if end % TILE_LEN == 0 || end == self.len {
                storage.put_sums(block, &self.sums);
                self.sums.fill(0.0);
            }
            if !self.tiles.is_empty() && (end % level1 == 0 || end == self.len) {
                self.finish_tile(storage);
            }
            start = end;
        }
        Ok(())
    }

    /// Adds the present samples `run` (inside one block) to the sums and level-1 buckets.
    fn fold(&mut self, chunk_start: u64, n: usize, run: Range<u64>) {
        let offset = (run.start - chunk_start) as usize..(run.end - chunk_start) as usize;
        for c in 0..self.channels {
            let row = &self.chunk[c * n..][..n];
            let mut sum = self.sums[c];
            for &x in &row[offset.clone()] {
                sum += x.to_f64();
            }
            self.sums[c] = sum;

            let Some(tile) = self.tiles.first_mut() else {
                continue;
            };
            let bucket_len = tile.level.bucket_len();
            let first_bucket = tile.time * TILE_LEN;
            let pairs = &mut tile.pairs[c * TILE * 2..][..TILE * 2];
            let mut i = run.start;
            while i < run.end {
                let k = i / bucket_len;
                let end = ((k + 1) * bucket_len).min(run.end);
                let j = (k - first_bucket) as usize;
                let slot = &mut pairs[2 * j..][..2];
                for &x in &row[(i - chunk_start) as usize..(end - chunk_start) as usize] {
                    // NaN compares false either way and never lands in a bucket.
                    if x < slot[0] {
                        slot[0] = x;
                    }
                    if x > slot[1] {
                        slot[1] = x;
                    }
                }
                i = end;
            }
        }
    }

    /// Stores the current level-1 tile, folds it into level 2, and so on up while each
    /// parent tile is complete.
    fn finish_tile(&mut self, storage: &mut dyn PyramidStorage) {
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
                    for (j, pair) in pairs.chunks_exact(2).enumerate() {
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

/// Resets (min, max) pairs to the empty sentinel.
fn clear<T: Sample>(pairs: &mut [T]) {
    for pair in pairs.chunks_exact_mut(2) {
        pair[0] = T::EMPTY_MIN;
        pair[1] = T::EMPTY_MAX;
    }
}
