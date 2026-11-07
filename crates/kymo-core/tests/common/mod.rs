//! Shared test helpers and proptest strategies.

// Each test crate uses only some of these.
#![allow(dead_code)]

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use kymo_core::testing::{MemData, opened};
use kymo_core::tile::{SignalVersion, TileData, View};
use kymo_core::{
    BlockReader, Channel, ChannelId, Grid, GridId, Header, Opened, ReadError, Recording,
    SampleType, SamplesMut, Timebase,
};
use proptest::prelude::*;

/// A grid length up to `max_len` and random gap structure: sorted, disjoint, non-touching
/// segments (possibly none, possibly covering everything).
pub fn len_and_segments(max_len: u64) -> impl Strategy<Value = (u64, Vec<Range<u64>>)> {
    (0..=max_len)
        .prop_flat_map(|len| (Just(len), proptest::collection::btree_set(0..=len, 0..12)))
        .prop_map(|(len, cuts)| {
            let cuts: Vec<u64> = cuts.into_iter().collect();
            let segments = cuts.as_chunks::<2>().0.iter().map(|c| c[0]..c[1]).collect();
            (len, segments)
        })
}

/// A range inside `0..=len`, possibly empty.
pub fn sub_range(len: u64) -> impl Strategy<Value = Range<u64>> {
    (0..=len, 0..=len).prop_map(|(a, b)| a.min(b)..a.max(b))
}

/// Whether sample `i` lies in one of `segments`.
pub fn is_present(segments: &[Range<u64>], i: u64) -> bool {
    segments.iter().any(|s| s.contains(&i))
}

/// A file in the temp directory, deleted on drop.
pub struct TempFile(PathBuf);

impl TempFile {
    pub fn new(extension: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("kymo-test-{}-{n}.{extension}", std::process::id());
        TempFile(std::env::temp_dir().join(name))
    }

    pub fn written(spec: &kymo_testgen::Spec) -> Self {
        let file = TempFile::new(spec.format.extension());
        kymo_testgen::write(spec, &file.0).unwrap();
        file
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A directory in the temp directory, deleted with its contents on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let file = TempFile::new("dir");
        std::fs::create_dir_all(file.path()).unwrap();
        let path = file.path().to_owned();
        std::mem::forget(file);
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Names of the files in the directory, sorted.
    pub fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Deterministic sample values: hashed, with the type's extremes mixed in.
pub fn value(seed: u64, c: usize, i: u64) -> i64 {
    let mut z = seed ^ (c as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ i.wrapping_mul(0xBF58_476D);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    match z % 64 {
        0 => i64::MIN,
        1 => i64::MAX,
        _ => z as i64,
    }
}

pub fn mem_data(sample_type: SampleType, n: usize, len: u64, seed: u64) -> MemData {
    fn rows<T>(n: usize, len: u64, seed: u64, f: impl Fn(i64) -> T) -> Vec<Vec<T>> {
        (0..n)
            .map(|c| (0..len).map(|i| f(value(seed, c, i))).collect())
            .collect()
    }
    match sample_type {
        SampleType::I16 => MemData::I16(rows(n, len, seed, clamp_i16)),
        SampleType::I32 => MemData::I32(rows(n, len, seed, clamp_i32)),
        _ => MemData::F32(rows(n, len, seed, |v| clamp_i32(v) as f32 * 0.37)),
    }
}

fn clamp_i16(v: i64) -> i16 {
    v.clamp(i16::MIN.into(), i16::MAX.into()) as i16
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i32::MIN.into(), i32::MAX.into()) as i32
}

/// A one-grid recording at 100 Hz with `n` channels and the given gaps, plus its data.
pub fn mem_recording(
    sample_type: SampleType,
    n: usize,
    len: u64,
    segments: Vec<Range<u64>>,
    seed: u64,
) -> (Recording, MemData) {
    let (opened, data) = mem_opened(sample_type, n, len, segments, seed);
    (Recording::new(opened).unwrap().0, data)
}

/// As [`mem_recording`], before validation, so tests can change the header.
pub fn mem_opened(
    sample_type: SampleType,
    n: usize,
    len: u64,
    segments: Vec<Range<u64>>,
    seed: u64,
) -> (Opened, MemData) {
    let ids: Vec<ChannelId> = (0..n as u32).map(ChannelId).collect();
    let channels = ids
        .iter()
        .map(|id| Channel::new(format!("ch{}", id.0), GridId(0)))
        .collect();
    let timebase = Timebase::Regular {
        rate: 100.0,
        start: 0.0,
    };
    let grid = Grid::new(timebase, len, segments, sample_type, ids).unwrap();
    let data = mem_data(sample_type, n, len, seed);
    (opened(channels, vec![grid], vec![data.clone()]), data)
}

pub fn values(data: &TileData) -> Vec<f64> {
    match data {
        TileData::I16(d) => d.iter().map(|&x| f64::from(x)).collect(),
        TileData::I32(d) => d.iter().map(|&x| f64::from(x)).collect(),
        TileData::F32(d) => d.iter().map(|&x| f64::from(x)).collect(),
    }
}

pub fn sample_types() -> impl Strategy<Value = SampleType> {
    prop_oneof![
        Just(SampleType::I16),
        Just(SampleType::I32),
        Just(SampleType::F32)
    ]
}

/// Tile math needs grid shapes only, never samples.
pub struct NoReader;

impl BlockReader for NoReader {
    fn read(
        &self,
        _: GridId,
        _: &[ChannelId],
        _: Range<u64>,
        _: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        unreachable!("tile math never reads samples")
    }
}

/// A recording of grids given as (rate, start, len, channel count), without data.
pub fn shape_recording(grids: &[(f64, f64, u64, usize)]) -> Recording {
    let mut channels = Vec::new();
    let mut built = Vec::new();
    for (g, &(rate, start, len, n)) in grids.iter().enumerate() {
        let ids: Vec<ChannelId> = (channels.len()..channels.len() + n)
            .map(|i| ChannelId(i as u32))
            .collect();
        for id in &ids {
            channels.push(Channel::new(format!("ch{}", id.0), GridId(g as u32)));
        }
        let timebase = Timebase::Regular { rate, start };
        built.push(Grid::continuous(timebase, len, SampleType::I16, ids).unwrap());
    }
    let opened = Opened {
        header: Header {
            format: "TEST",
            start_time: None,
            channels,
            grids: built,
            extra: vec![],
        },
        annotations: vec![],
        warnings: vec![],
        reader: Box::new(NoReader),
    };
    Recording::new(opened).unwrap().0
}

pub fn view(time: Range<f64>, width_px: u32, channels: Vec<u32>) -> View {
    View {
        version: SignalVersion(0),
        time,
        width_px,
        channels: channels.into_iter().map(ChannelId).collect(),
    }
}

/// 1 to 3 grids of different rates, starts, lengths up to 10^9 and up to 80 channels.
pub fn grids() -> impl Strategy<Value = Vec<(f64, f64, u64, usize)>> {
    let grid = (
        proptest::sample::select(vec![100.0, 256.0, 512.0, 2048.0, 1000.0 / 3.0]),
        proptest::sample::select(vec![0.0, 0.25]),
        prop_oneof![0u64..10_000, 0u64..1_000_000_000],
        1usize..80,
    );
    proptest::collection::vec(grid, 1..4)
}

/// A view somewhere around the recording of `grids`, at any zoom, with random channels.
pub fn view_of(grids: &[(f64, f64, u64, usize)]) -> impl Strategy<Value = View> + use<> {
    let duration = grids
        .iter()
        .map(|&(rate, start, len, _)| start + len as f64 / rate)
        .fold(0.0, f64::max);
    let n: u32 = grids.iter().map(|g| g.3 as u32).sum();
    (
        -10.0..duration + 10.0,
        (-10.0f64..18.0).prop_map(|e| 2f64.powf(e)),
        1u32..4000,
        proptest::collection::vec(0..n, 0..10),
    )
        .prop_map(|(t0, length, width, channels)| view(t0..t0 + length, width, channels))
}

/// What a [`ProbeReader`] logged about one read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadLog {
    /// Name of the reading thread.
    pub thread: String,
    pub grid: GridId,
    pub first: ChannelId,
    pub range: Range<u64>,
}

/// Shared state of a [`ProbeReader`]: its log, and its hold on tile workers' reads.
#[derive(Default)]
pub struct Probe {
    log: std::sync::Mutex<Vec<ReadLog>>,
    hold: std::sync::Mutex<Hold>,
    changed: std::sync::Condvar,
    /// Microseconds every build read sleeps.
    build_delay_us: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct Hold {
    on: bool,
    waiting: usize,
}

/// Tile workers' threads are named `kymo-tile-N`; the build runs on `kymo-build` and on
/// unnamed helpers. Test threads have names, so their reads count as neither.
fn is_worker(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.starts_with("kymo-tile"))
}

fn is_build(name: Option<&str>) -> bool {
    name.is_none_or(|n| n == "kymo-build")
}

impl Probe {
    /// Reads by tile workers wait from now on, until [`release`](Self::release).
    pub fn hold(&self) {
        self.hold.lock().unwrap().on = true;
    }

    pub fn release(&self) {
        self.hold.lock().unwrap().on = false;
        self.changed.notify_all();
    }

    /// Waits until `n` worker reads are held.
    pub fn wait_held(&self, n: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut hold = self.hold.lock().unwrap();
        while hold.waiting < n {
            assert!(std::time::Instant::now() < deadline, "{n} reads never held");
            hold = self
                .changed
                .wait_timeout(hold, std::time::Duration::from_millis(10))
                .unwrap()
                .0;
        }
    }

    /// Slows every read of the pyramid build down by `delay`.
    pub fn slow_build(&self, delay: std::time::Duration) {
        self.build_delay_us
            .store(delay.as_micros() as u64, Ordering::Relaxed);
    }

    /// The reads tile workers made, in the order they made them (after any hold).
    pub fn worker_reads(&self) -> Vec<ReadLog> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| is_worker(Some(&r.thread)))
            .cloned()
            .collect()
    }

    /// Number of reads the pyramid build made.
    pub fn build_reads(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.thread.is_empty() || r.thread == "kymo-build")
            .count()
    }
}

/// A reader that passes everything to another one and lets a [`Probe`] watch and steer.
pub struct ProbeReader {
    inner: Box<dyn BlockReader>,
    probe: std::sync::Arc<Probe>,
}

impl BlockReader for ProbeReader {
    fn read(
        &self,
        grid: GridId,
        channels: &[ChannelId],
        range: Range<u64>,
        out: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        let thread = std::thread::current();
        let name = thread.name();
        if is_worker(name) {
            let mut hold = self.probe.hold.lock().unwrap();
            hold.waiting += 1;
            self.probe.changed.notify_all();
            while hold.on {
                hold = self.probe.changed.wait(hold).unwrap();
            }
            hold.waiting -= 1;
        } else if is_build(name) {
            let us = self.probe.build_delay_us.load(Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_micros(us));
        }
        self.probe.log.lock().unwrap().push(ReadLog {
            thread: name.unwrap_or_default().to_owned(),
            grid,
            first: channels[0],
            range: range.clone(),
        });
        self.inner.read(grid, channels, range, out)
    }

    fn prefetch(&self, grid: GridId, range: Range<u64>) {
        self.inner.prefetch(grid, range);
    }
}

/// `opened` with its reader wrapped in a [`ProbeReader`].
pub fn probed(mut opened: Opened) -> (Opened, std::sync::Arc<Probe>) {
    let probe = std::sync::Arc::new(Probe::default());
    let inner = std::mem::replace(&mut opened.reader, Box::new(NoReader));
    opened.reader = Box::new(ProbeReader {
        inner,
        probe: std::sync::Arc::clone(&probe),
    });
    (opened, probe)
}

/// Waits until `done` holds, polling; panics after 20 s.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
