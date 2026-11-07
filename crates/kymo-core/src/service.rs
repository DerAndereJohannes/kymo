//! The tile service: computes the tiles a view asks for on a few worker threads, while the
//! pyramid builds in the background.
//!
//! The UI thread submits keys and reads the cache; it never waits for a tile. Workers
//! decode level-0 tiles from the recording and read higher levels from the pyramid. A
//! pyramid tile the build hasn't reached stays queued in its place until the build's
//! watermark passes it.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::pyramid::{MemStorage, Pyramid, PyramidBuilder, PyramidStorage, Sidecar, SidecarError};
use crate::tile::{DEFAULT_BUDGET, Level, SignalVersion, Tile, TileCache, TileKey, compute_raw};
use crate::{GridId, Recording, TileError};

/// Where the pyramid is kept.
#[derive(Clone, Debug)]
pub enum PyramidStore {
    /// A sidecar file in `dir` for the recording read from `source`, reused when complete
    /// (see [`Sidecar`]).
    Sidecar { dir: PathBuf, source: PathBuf },
    /// In memory, about a twelfth of the recording's samples: for tests and recordings
    /// without a file.
    Memory,
}

/// How a [`TileService`] runs.
#[derive(Clone, Debug)]
pub struct ServiceOptions {
    /// Worker threads computing tiles (at least 1).
    pub workers: usize,
    /// Byte budget of the tile cache.
    pub cache_budget: usize,
    pub pyramids: PyramidStore,
    /// Threads per build step; `None` for the builder's default.
    pub build_threads: Option<usize>,
    /// Samples per build step; `None` for the builder's default.
    pub build_chunk_len: Option<u64>,
}

impl ServiceOptions {
    /// Defaults: one worker per CPU up to 4, the default cache budget, and the builder's
    /// own step size and threads.
    pub fn new(pyramids: PyramidStore) -> Self {
        ServiceOptions {
            workers: std::thread::available_parallelism().map_or(1, |n| n.get().min(4)),
            cache_budget: DEFAULT_BUDGET,
            pyramids,
            build_threads: None,
            build_chunk_len: None,
        }
    }
}

/// A tile service could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServiceError {
    #[error(transparent)]
    Sidecar(#[from] SidecarError),
    #[error("cannot start a thread: {0}")]
    Thread(#[source] io::Error),
}

/// How far the pyramid build has got.
#[derive(Clone, Debug)]
pub struct BuildProgress {
    /// Samples built so far, summed over all grids.
    pub done: u64,
    /// Samples of all grids.
    pub total: u64,
    /// Whether every grid is built (and a sidecar is in place for the next open).
    pub complete: bool,
    /// Why the build stopped early; pyramid tiles past the watermark then fail.
    pub error: Option<Arc<SidecarError>>,
}

impl BuildProgress {
    /// Share of the samples built, from 0 to 1 (1 for an empty recording).
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

/// Owns a recording's tile cache and pyramid, the pyramid's background build, and the
/// workers that fill the cache.
///
/// Dropping it stops the workers after their current tile and the build after its current
/// step, and joins every thread. An unfinished sidecar is deleted then.
pub struct TileService {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for TileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileService")
            .field("threads", &self.threads.len())
            .field("progress", &self.progress())
            .finish_non_exhaustive()
    }
}

struct Shared {
    recording: Arc<Recording>,
    cache: TileCache,
    /// One per grid.
    pyramids: Vec<Pyramid>,
    sidecar: Option<Sidecar>,
    queue: Mutex<Queue>,
    /// Signalled when keys are queued, the build progresses, or the service stops.
    work: Condvar,
    build: Mutex<BuildEnd>,
    stop: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Queue {
    generation: u64,
    /// Keys to compute, most urgent first.
    pending: Vec<TileKey>,
    /// The keys in `pending`.
    queued: HashSet<TileKey>,
    /// Keys a worker is computing.
    running: HashSet<TileKey>,
    failed: HashMap<TileKey, Arc<TileError>>,
    /// The pyramids won't grow any more, so a pyramid key is ready or never will be.
    build_ended: bool,
    shutdown: bool,
}

#[derive(Default)]
struct BuildEnd {
    complete: bool,
    error: Option<Arc<SidecarError>>,
}

impl TileService {
    /// Starts the workers and the pyramid build for `recording`. `notify` runs on a worker
    /// or build thread after each tile is cached or fails, and after each build step; a
    /// viewer passes its repaint request.
    pub fn new(
        recording: Arc<Recording>,
        options: ServiceOptions,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<TileService, ServiceError> {
        let grids = (0..recording.grids().len()).map(|g| GridId(g as u32));
        let (sidecar, pyramids, storages) = match &options.pyramids {
            PyramidStore::Sidecar { dir, source } => {
                let sidecar = Sidecar::open_or_create(dir, source, &recording)?;
                let pyramids = grids.filter_map(|g| sidecar.pyramid(g)).collect::<Vec<_>>();
                (Some(sidecar), pyramids, None)
            }
            PyramidStore::Memory => {
                let mut pyramids = Vec::new();
                let mut storages = Vec::new();
                for g in grids {
                    let storage: Arc<dyn PyramidStorage> = Arc::new(MemStorage::new());
                    let grid = recording.grid(g).clone();
                    pyramids.push(Pyramid::new(g, grid, Arc::clone(&storage)));
                    storages.push(storage);
                }
                (None, pyramids, Some(storages))
            }
        };
        let shared = Arc::new(Shared {
            cache: TileCache::new(options.cache_budget),
            recording,
            pyramids,
            sidecar,
            queue: Mutex::default(),
            work: Condvar::new(),
            build: Mutex::default(),
            stop: AtomicBool::new(false),
            notify: Box::new(notify),
        });

        // Dropped on an error below, which stops the threads started so far.
        let mut service = TileService {
            shared,
            threads: Vec::new(),
        };
        let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name)
                .spawn(f)
                .map_err(ServiceError::Thread)
        };
        let shared = Arc::clone(&service.shared);
        let build = BuildOptions {
            storages,
            threads: options.build_threads,
            chunk_len: options.build_chunk_len,
        };
        service.threads.push(spawn(
            "kymo-build".into(),
            Box::new(move || shared.run_build(build)),
        )?);
        for i in 0..options.workers.max(1) {
            let shared = Arc::clone(&service.shared);
            service.threads.push(spawn(
                format!("kymo-tile-{i}"),
                Box::new(move || shared.run_worker()),
            )?);
        }
        Ok(service)
    }

    /// Queues `keys` for the workers, most urgent first.
    ///
    /// A newer `generation` than the last replaces the queue, so queued keys it doesn't
    /// contain are dropped; the same generation adds its keys behind the queued ones; an
    /// older one is ignored. Keys already cached, being computed, queued or failed are
    /// skipped, so no key is ever computed twice at once.
    pub fn submit(&self, generation: u64, keys: impl IntoIterator<Item = TileKey>) {
        let shared = &*self.shared;
        let mut queue = shared.lock_queue();
        if generation < queue.generation {
            return;
        }
        if generation > queue.generation {
            queue.generation = generation;
            queue.pending.clear();
            queue.queued.clear();
        }
        for key in keys {
            let Queue {
                queued,
                running,
                failed,
                pending,
                ..
            } = &mut *queue;
            if queued.contains(&key)
                || running.contains(&key)
                || failed.contains_key(&key)
                || shared.cache.contains(&key)
            {
                continue;
            }
            queued.insert(key);
            pending.push(key);
        }
        drop(queue);
        shared.work.notify_all();
    }

    pub fn recording(&self) -> &Arc<Recording> {
        &self.shared.recording
    }

    /// The cache the workers fill; [`crate::tile::query`] reads it.
    pub fn cache(&self) -> &TileCache {
        &self.shared.cache
    }

    /// The pyramid of `grid`, readable while it builds.
    pub fn pyramid(&self, grid: GridId) -> Option<&Pyramid> {
        self.shared.pyramids.get(grid.0 as usize)
    }

    pub fn progress(&self) -> BuildProgress {
        let shared = &*self.shared;
        let build = lock(&shared.build);
        BuildProgress {
            done: shared.pyramids.iter().map(Pyramid::done).sum(),
            total: shared.recording.grids().iter().map(|g| g.len()).sum(),
            complete: build.complete,
            error: build.error.clone(),
        }
    }

    /// Keys that could not be computed, sorted; they are never tried again.
    pub fn failures(&self) -> Vec<(TileKey, Arc<TileError>)> {
        let queue = self.shared.lock_queue();
        let mut failures: Vec<_> = queue
            .failed
            .iter()
            .map(|(key, error)| (*key, Arc::clone(error)))
            .collect();
        failures.sort_by_key(|(key, _)| *key);
        failures
    }
}

impl Drop for TileService {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.lock_queue().shutdown = true;
        self.shared.work.notify_all();
        for thread in self.threads.drain(..) {
            // A thread that panicked has nothing left to clean up.
            let _ = thread.join();
        }
    }
}

struct BuildOptions {
    /// Per grid, for an in-memory pyramid.
    storages: Option<Vec<Arc<dyn PyramidStorage>>>,
    threads: Option<usize>,
    chunk_len: Option<u64>,
}

impl Shared {
    fn lock_queue(&self) -> MutexGuard<'_, Queue> {
        lock(&self.queue)
    }

    fn run_worker(&self) {
        loop {
            let key = {
                let mut queue = self.lock_queue();
                loop {
                    if queue.shutdown {
                        return;
                    }
                    let ended = queue.build_ended;
                    if let Some(i) = queue.pending.iter().position(|k| self.ready(k, ended)) {
                        let key = queue.pending.remove(i);
                        queue.queued.remove(&key);
                        queue.running.insert(key);
                        break key;
                    }
                    queue = self
                        .work
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            let failure = match self.compute(key) {
                Ok(tile) => {
                    // Cached before it stops running, so no submit in between queues it again.
                    self.cache.insert(Arc::new(tile));
                    None
                }
                Err(error) => Some(Arc::new(error)),
            };
            let mut queue = self.lock_queue();
            queue.running.remove(&key);
            if let Some(error) = failure {
                queue.failed.insert(key, error);
            }
            drop(queue);
            (self.notify)();
        }
    }

    /// Whether a worker can compute `key` now: level-0 tiles always, pyramid tiles once the
    /// build has passed them or has ended. Keys outside the recording are ready, to fail.
    fn ready(&self, key: &TileKey, build_ended: bool) -> bool {
        if key.level == Level::RAW || build_ended {
            return true;
        }
        let g = key.block.grid.0 as usize;
        match (self.recording.grids().get(g), self.pyramids.get(g)) {
            (Some(grid), Some(pyramid)) => key.sample_range().end.min(grid.len()) <= pyramid.done(),
            _ => true,
        }
    }

    fn compute(&self, key: TileKey) -> Result<Tile, TileError> {
        if key.level == Level::RAW {
            return compute_raw(&self.recording, key);
        }
        if key.version != SignalVersion::RAW {
            return Err(TileError::UnknownVersion(key));
        }
        self.pyramids
            .get(key.block.grid.0 as usize)
            .and_then(|p| p.tile(key))
            .ok_or(TileError::NotInPyramid(key))
    }

    fn run_build(&self, options: BuildOptions) {
        let result = self.build(options);
        let stopped = self.stop.load(Ordering::Acquire);
        {
            let mut end = lock(&self.build);
            end.complete = result.is_ok() && !stopped;
            end.error = result.err().map(Arc::new);
        }
        self.lock_queue().build_ended = true;
        self.wake();
    }

    /// Steps every grid's builder in turn, always the one furthest behind in time, so
    /// grids of different rates fill in together. Returns early, unfinished, on stop.
    fn build(&self, options: BuildOptions) -> Result<(), SidecarError> {
        let recording = &*self.recording;
        let mut builders = Vec::new();
        for (g, grid) in recording.grids().iter().enumerate() {
            let id = GridId(g as u32);
            let builder = match (&options.storages, &self.sidecar) {
                (Some(storages), _) => {
                    Some(PyramidBuilder::new(recording, id)?.with_storage(Arc::clone(&storages[g])))
                }
                (None, Some(sidecar)) => sidecar.builder(recording, id),
                (None, None) => None,
            };
            let Some(mut builder) = builder else {
                continue;
            };
            if let Some(threads) = options.threads {
                builder = builder.with_threads(threads);
            }
            if let Some(len) = options.chunk_len {
                builder = builder.with_chunk_len(len);
            }
            builders.push((grid.rate(), builder));
        }
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let behind = builders
                .iter_mut()
                .filter(|(_, b)| b.done() < b.len())
                .min_by(|(ra, a), (rb, b)| {
                    (a.done() as f64 / ra).total_cmp(&(b.done() as f64 / rb))
                });
            let Some((_, builder)) = behind else {
                break;
            };
            builder.step()?;
            self.wake();
        }
        drop(builders);
        if let Some(sidecar) = &self.sidecar {
            sidecar.finish()?;
        }
        Ok(())
    }

    /// Lets waiting workers recheck the queue, then tells the viewer.
    fn wake(&self) {
        // Taking the lock orders this after any worker's readiness check, so a worker
        // that just found nothing ready is already waiting and gets the signal.
        drop(self.lock_queue());
        self.work.notify_all();
        (self.notify)();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every critical section leaves the state consistent before anything could panic.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
