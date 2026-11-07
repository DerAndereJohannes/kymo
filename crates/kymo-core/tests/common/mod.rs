//! Shared test helpers and proptest strategies.

// Each test crate uses only some of these.
#![allow(dead_code)]

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use kymo_core::testing::{MemData, opened};
use kymo_core::tile::TileData;
use kymo_core::{Channel, ChannelId, Grid, GridId, Opened, Recording, SampleType, Timebase};
use proptest::prelude::*;

/// A grid length up to `max_len` and random gap structure: sorted, disjoint, non-touching
/// segments (possibly none, possibly covering everything).
pub fn len_and_segments(max_len: u64) -> impl Strategy<Value = (u64, Vec<Range<u64>>)> {
    (0..=max_len)
        .prop_flat_map(|len| (Just(len), proptest::collection::btree_set(0..=len, 0..12)))
        .prop_map(|(len, cuts)| {
            let cuts: Vec<u64> = cuts.into_iter().collect();
            let segments = cuts.chunks_exact(2).map(|c| c[0]..c[1]).collect();
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
