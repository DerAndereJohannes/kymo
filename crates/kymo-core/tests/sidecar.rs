//! The pyramid sidecar: build, reopen, invalidation, corruption, and reads during a build.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{TempDir, TempFile, len_and_segments, mem_recording, sample_types};
use kymo_core::pyramid::{Pyramid, PyramidBuilder, Sidecar};
use kymo_core::tile::{ChannelBlock, Level, SignalVersion, TILE_LEN, TileKey};
use kymo_core::{GridId, Recording, SampleType};
use kymo_testgen::Spec;
use proptest::prelude::*;

/// Every key of every level of `grid` (one past the end too), and every block of sums.
fn keys(rec: &Recording, grid: GridId) -> Vec<TileKey> {
    let g = rec.grid(grid);
    let top = kymo_core::tile::max_level(g.len());
    let mut keys = Vec::new();
    let mut level = Level::RAW;
    while let Some(up) = level.up().filter(|_| level < top) {
        level = up;
        let tiles = g.len().div_ceil(level.bucket_len()).div_ceil(TILE_LEN);
        for index in 0..ChannelBlock::count(g) {
            for time in 0..=tiles {
                keys.push(TileKey {
                    version: SignalVersion::RAW,
                    level,
                    block: ChannelBlock { grid, index },
                    time,
                });
            }
        }
    }
    keys
}

/// `got` serves exactly what `want` serves, tiles and sums.
fn assert_same(rec: &Recording, grid: GridId, got: &Pyramid, want: &Pyramid) {
    assert_eq!(got.top(), want.top());
    for key in keys(rec, grid) {
        assert_eq!(got.tile(key), want.tile(key), "{key:?}");
    }
    let g = rec.grid(grid);
    for block in 0..=g.len().div_ceil(TILE_LEN) {
        for &c in g.channels() {
            assert_eq!(got.block_sum(c, block), want.block_sum(c, block));
        }
    }
}

fn open(file: &TempFile) -> Recording {
    Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0
}

fn generated(preset: &str) -> (TempFile, Recording) {
    let file = TempFile::written(&Spec::preset(preset).unwrap());
    let rec = open(&file);
    (file, rec)
}

fn built(dir: &TempDir, source: &Path, rec: &Recording) -> Sidecar {
    let sidecar = Sidecar::open_or_create(dir.path(), source, rec).unwrap();
    assert!(!sidecar.is_complete());
    assert!(sidecar.build(rec).unwrap());
    sidecar
}

#[test]
fn build_then_reopen_serves_identical_tiles() {
    for preset in ["edf+d-mixed", "bdf+c-small"] {
        let (file, rec) = generated(preset);
        let dir = TempDir::new();
        let sidecar = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
        assert!(!sidecar.is_complete());
        assert!(sidecar.path().to_string_lossy().ends_with(".tmp"));
        // Nothing is served before the build.
        let early = sidecar.pyramid(GridId(0)).unwrap();
        assert!(!early.is_complete());
        assert!(
            keys(&rec, GridId(0))
                .into_iter()
                .all(|k| early.tile(k).is_none())
        );

        assert!(sidecar.build(&rec).unwrap());
        assert!(sidecar.is_complete());
        let files = dir.files();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with(".kymo-pyramid"));
        assert_eq!(dir.path().join(&files[0]), sidecar.path());
        drop(sidecar);

        let reopened = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
        assert!(reopened.is_complete(), "{preset}: reopened sidecar rebuilt");
        for g in 0..rec.grids().len() {
            let id = GridId(g as u32);
            assert!(
                reopened.builder(&rec, id).is_none(),
                "complete grids need no builder"
            );
            let pyramid = reopened.pyramid(id).unwrap();
            assert!(pyramid.is_complete());
            let want = Pyramid::build(&rec, id).unwrap();
            assert_same(&rec, id, &pyramid, &want);
            let c = rec.grid(id).channels()[0];
            let range = 100..rec.grid(id).len() - 100;
            assert_eq!(
                pyramid.window_mean(&rec, c, range.clone()).unwrap(),
                want.window_mean(&rec, c, range).unwrap()
            );
        }
        assert!(reopened.pyramid(GridId(rec.grids().len() as u32)).is_none());
    }
}

#[test]
fn changed_source_invalidates_the_sidecar() {
    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let rec = open(&file);
    let dir = TempDir::new();
    built(&dir, file.path(), &rec);
    let reopen = |rec: &Recording| Sidecar::open_or_create(dir.path(), file.path(), rec).unwrap();
    assert!(reopen(&rec).is_complete());

    // A new modification time alone.
    let f = std::fs::File::options()
        .write(true)
        .open(file.path())
        .unwrap();
    let mtime = f.metadata().unwrap().modified().unwrap();
    f.set_modified(mtime + Duration::from_secs(1)).unwrap();
    assert!(!reopen(&rec).is_complete());
    drop(f);

    // Same size and modification time, different samples at the start. The source is only
    // changed while no recording maps it, which Windows enforces.
    built(&dir, file.path(), &rec);
    drop(rec);
    let mtime = std::fs::metadata(file.path()).unwrap().modified().unwrap();
    let mut other = spec.clone();
    other.seed += 1;
    kymo_testgen::write(&other, file.path()).unwrap();
    let f = std::fs::File::options()
        .write(true)
        .open(file.path())
        .unwrap();
    f.set_modified(mtime).unwrap();
    let rec = open(&file);
    assert!(!reopen(&rec).is_complete());

    // A different size.
    built(&dir, file.path(), &rec);
    drop(rec);
    let len = f.metadata().unwrap().len();
    f.set_len(len + 1).unwrap();
    let rec = open(&file);
    assert!(!reopen(&rec).is_complete());
}

#[test]
fn corrupt_or_truncated_sidecars_are_rebuilt() {
    let (file, rec) = generated("bdf+c-small");
    let dir = TempDir::new();
    let path = built(&dir, file.path(), &rec).path();
    let good = std::fs::read(&path).unwrap();
    let want = Pyramid::build(&rec, GridId(0)).unwrap();

    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("10 bytes", good[..10].to_vec()),
        ("half", good[..good.len() / 2].to_vec()),
        ("one byte short", good[..good.len() - 1].to_vec()),
        ("one byte long", [good.as_slice(), &[0]].concat()),
    ];
    for (name, at) in [
        ("magic", 0),
        ("version", 8),
        ("grid count", 12),
        ("key", 20),
        ("size", 50),
        ("complete flag", 56),
        ("grid length", 64),
        ("channel count", 72),
        ("sample type", 80),
        ("sums offset", 64 + 40),
        ("padding", 4000),
    ] {
        let mut bad = good.clone();
        bad[at] ^= 0x5A;
        cases.push((name, bad));
    }
    for (name, bytes) in cases {
        std::fs::write(&path, &bytes).unwrap();
        let sidecar = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
        assert!(!sidecar.is_complete(), "{name}: accepted");
        assert!(sidecar.build(&rec).unwrap(), "{name}: rebuild");
        drop(sidecar);
        let reopened = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
        assert!(reopened.is_complete(), "{name}: rebuilt sidecar not reused");
        assert_same(
            &rec,
            GridId(0),
            &reopened.pyramid(GridId(0)).unwrap(),
            &want,
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Any change to the header is caught; a change to the pyramid data is not (no
    /// checksums); a change to the stim trailer is caught if it breaks its structure. None
    /// makes reading panic.
    #[test]
    fn flipped_bytes_never_panic(at in any::<proptest::sample::Index>(), mask in 1u8..=255) {
        let (file, rec) = generated("bdf+c-small");
        let dir = TempDir::new();
        let path = built(&dir, file.path(), &rec).path();
        let mut bytes = std::fs::read(&path).unwrap();
        let trailer = bytes
            .windows(8)
            .rposition(|w| w == b"KYMOSTIM")
            .unwrap();
        let i = at.index(bytes.len());
        bytes[i] ^= mask;
        std::fs::write(&path, &bytes).unwrap();
        let sidecar = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
        if i < trailer {
            prop_assert_eq!(sidecar.is_complete(), i >= 4096, "byte {}", i);
        }
        let pyramid = sidecar.pyramid(GridId(0)).unwrap();
        for key in keys(&rec, GridId(0)) {
            let _ = pyramid.tile(key);
        }
        let _ = pyramid.stim_changes(&rec);
    }

    /// While a build is throttled step by step, the sidecar and the in-memory storage serve
    /// exactly the tiles and sums that end at or before the watermark, with their final
    /// values.
    #[test]
    fn throttled_build_serves_only_published_tiles(
        sample_type in sample_types(),
        n in 1..=2usize,
        (len, segments) in len_and_segments(400_000),
        chunk in 1000..=60_000u64,
        seed in any::<u64>(),
    ) {
        let (rec, _) = mem_recording(sample_type, n, len, segments, seed);
        let want = Pyramid::build(&rec, GridId(0)).unwrap();
        let source = TempFile::new("bin");
        std::fs::write(source.path(), seed.to_le_bytes()).unwrap();
        let dir = TempDir::new();
        let sidecar = Sidecar::open_or_create(dir.path(), source.path(), &rec).unwrap();
        let reader = sidecar.pyramid(GridId(0)).unwrap();
        let mut in_sidecar = sidecar.builder(&rec, GridId(0)).unwrap().with_chunk_len(chunk);
        let mut in_memory = PyramidBuilder::new(&rec, GridId(0)).unwrap().with_chunk_len(chunk);
        let memory_reader = in_memory.pyramid();
        let keys = keys(&rec, GridId(0));
        loop {
            let done = in_sidecar.done();
            prop_assert_eq!(in_memory.done(), done);
            for pyramid in [&reader, &memory_reader] {
                prop_assert_eq!(pyramid.done(), done);
                for &key in &keys {
                    let got = pyramid.tile(key);
                    let end = key.sample_range().end.min(len);
                    let exists = want.tile(key).is_some();
                    prop_assert_eq!(got.is_some(), exists && end <= done, "{:?} at {}", key, done);
                    if got.is_some() {
                        prop_assert_eq!(got, want.tile(key));
                    }
                }
                for block in 0..len.div_ceil(TILE_LEN) {
                    let end = ((block + 1) * TILE_LEN).min(len);
                    let c = rec.grid(GridId(0)).channels()[0];
                    let got = pyramid.block_sum(c, block);
                    prop_assert_eq!(got.is_some(), end <= done);
                    if got.is_some() {
                        prop_assert_eq!(got, want.block_sum(c, block));
                    }
                }
            }
            let finished = in_sidecar.step().unwrap();
            prop_assert_eq!(in_memory.step().unwrap(), finished);
            if finished && in_sidecar.done() == done {
                break;
            }
        }
        prop_assert!(reader.is_complete());
        prop_assert!(sidecar.finish().unwrap());
    }
}

/// Readers on other threads see only final tiles while the build runs.
#[test]
fn readers_during_a_build_see_only_final_tiles() {
    let len = 2_000_000;
    let (rec, _) = mem_recording(SampleType::I32, 3, len, vec![0..len / 3, len / 2..len], 9);
    let want = Pyramid::build(&rec, GridId(0)).unwrap();
    let source = TempFile::new("bin");
    std::fs::write(source.path(), b"source").unwrap();
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), source.path(), &rec).unwrap();
    let keys = keys(&rec, GridId(0));
    let building = AtomicBool::new(true);
    std::thread::scope(|s| {
        for r in 0..3 {
            let (sidecar, keys, want, building) = (&sidecar, &keys, &want, &building);
            s.spawn(move || {
                let pyramid = sidecar.pyramid(GridId(0)).unwrap();
                while building.load(Ordering::Acquire) {
                    for key in keys.iter().skip(r) {
                        if let Some(tile) = pyramid.tile(*key) {
                            assert_eq!(Some(tile), want.tile(*key));
                        }
                    }
                }
            });
        }
        let mut builder = sidecar
            .builder(&rec, GridId(0))
            .unwrap()
            .with_chunk_len(5000);
        while !builder.step().unwrap() {}
        building.store(false, Ordering::Release);
    });
    assert!(sidecar.finish().unwrap());
    assert_same(&rec, GridId(0), &sidecar.pyramid(GridId(0)).unwrap(), &want);
}

#[test]
fn one_builder_per_grid() {
    let (file, rec) = generated("edf+d-mixed");
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
    let first = sidecar.builder(&rec, GridId(0)).unwrap();
    assert!(sidecar.builder(&rec, GridId(0)).is_none());
    let past = GridId(rec.grids().len() as u32);
    assert!(sidecar.builder(&rec, past).is_none());
    // `build` skips the grid handed out and cannot finish without it.
    assert!(!sidecar.build(&rec).unwrap());
    first.finish().unwrap();
    assert!(sidecar.finish().unwrap());

    // A recording whose grid differs gets no builder.
    let (other_file, other) = generated("bdf+c-small");
    let fresh = Sidecar::open_or_create(dir.path(), other_file.path(), &rec).unwrap();
    assert!(fresh.builder(&other, GridId(0)).is_none());
}

#[test]
fn unfinished_sidecars_leave_no_file() {
    let (file, rec) = generated("bdf+c-small");
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), file.path(), &rec).unwrap();
    let mut builder = sidecar
        .builder(&rec, GridId(0))
        .unwrap()
        .with_chunk_len(1000);
    builder.step().unwrap();
    assert_eq!(dir.files().len(), 1);
    drop(builder);
    drop(sidecar);
    assert!(dir.files().is_empty(), "{:?}", dir.files());
}

#[test]
fn missing_source_is_an_error() {
    let (_file, rec) = generated("bdf+c-small");
    let dir = TempDir::new();
    let missing = dir.path().join("missing.bdf");
    assert!(Sidecar::open_or_create(dir.path(), &missing, &rec).is_err());
}

#[test]
fn default_dir_is_in_the_user_cache() {
    if let Some(dir) = Sidecar::default_dir() {
        assert!(dir.ends_with("kymo"));
    }
}
