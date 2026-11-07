//! The tile service: delivery, generations, priority, no duplicate work, the background
//! build, and shutdown.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{Probe, TempDir, TempFile, mem_data, probed, wait_until};
use kymo_core::pyramid::{Pyramid, Sidecar};
use kymo_core::service::{PyramidStore, ServiceOptions, TileService};
use kymo_core::testing::opened;
use kymo_core::tile::{
    ChannelBlock, Level, SignalVersion, TILE_LEN, Tile, TileKey, compute_raw, max_level,
};
use kymo_core::{
    Channel, ChannelId, Grid, GridId, Opened, Recording, SampleType, TileError, Timebase,
};
use kymo_testgen::Spec;

/// Two grids: 40 int32 channels at 100 Hz with a gap (two channel blocks, levels 0 to 2),
/// and 3 int16 channels at 25 Hz (levels 0 and 1).
fn two_grids() -> Opened {
    let ids = |range: std::ops::Range<u32>| range.map(ChannelId).collect::<Vec<_>>();
    let rate = |rate| Timebase::Regular { rate, start: 0.0 };
    let grids = vec![
        Grid::new(
            rate(100.0),
            200_000,
            vec![0..70_000, 90_000..200_000],
            SampleType::I32,
            ids(0..40),
        )
        .unwrap(),
        Grid::continuous(rate(25.0), 50_000, SampleType::I16, ids(40..43)).unwrap(),
    ];
    let channels = (0..43)
        .map(|c| Channel::new(format!("ch{c}"), GridId(u32::from(c >= 40))))
        .collect();
    let data = vec![
        mem_data(SampleType::I32, 40, 200_000, 1),
        mem_data(SampleType::I16, 3, 50_000, 2),
    ];
    opened(channels, grids, data)
}

/// One grid of 4 int16 channels at 1000 Hz, 100 raw tiles long.
fn one_grid() -> Opened {
    let len = 100 * TILE_LEN;
    let grid = Grid::continuous(
        Timebase::Regular {
            rate: 1000.0,
            start: 0.0,
        },
        len,
        SampleType::I16,
        (0..4).map(ChannelId).collect(),
    )
    .unwrap();
    let channels = (0..4)
        .map(|c| Channel::new(format!("ch{c}"), GridId(0)))
        .collect();
    opened(
        channels,
        vec![grid],
        vec![mem_data(SampleType::I16, 4, len, 3)],
    )
}

fn key(grid: u32, block: u32, level: u8, time: u64) -> TileKey {
    TileKey {
        version: SignalVersion::RAW,
        level: Level::new(level).unwrap(),
        block: ChannelBlock {
            grid: GridId(grid),
            index: block,
        },
        time,
    }
}

/// Every tile of every level of `rec`.
fn all_keys(rec: &Recording) -> Vec<TileKey> {
    let mut keys = Vec::new();
    for (g, grid) in rec.grids().iter().enumerate() {
        for level in 0..=max_level(grid.len()).get() {
            let tiles = grid
                .len()
                .div_ceil(Level::new(level).unwrap().tile_samples());
            for block in 0..ChannelBlock::count(grid) {
                keys.extend((0..tiles).map(|t| key(g as u32, block, level, t)));
            }
        }
    }
    keys
}

/// The tiles of `keys`, computed directly: decoded from the recording, or from a pyramid
/// built in memory.
fn expected(rec: &Recording, keys: &[TileKey]) -> HashMap<TileKey, Tile> {
    let pyramids: Vec<Pyramid> = (0..rec.grids().len())
        .map(|g| Pyramid::build(rec, GridId(g as u32)).unwrap())
        .collect();
    keys.iter()
        .map(|&k| {
            let tile = if k.level == Level::RAW {
                compute_raw(rec, k).unwrap()
            } else {
                pyramids[k.block.grid.0 as usize].tile(k).unwrap()
            };
            (k, tile)
        })
        .collect()
}

struct Running {
    service: TileService,
    probe: Arc<Probe>,
    notified: Arc<AtomicUsize>,
}

impl Running {
    fn has(&self, key: &TileKey) -> bool {
        self.service.cache().contains(key)
    }

    /// Waits until every key is cached.
    fn wait_for(&self, keys: &[TileKey]) {
        wait_until("tiles", || keys.iter().all(|k| self.has(k)));
    }

    /// Worker reads of the raw tiles of grid 0, block 0 (gap-free grids only: one read per
    /// tile), as tile times.
    fn raw_reads(&self) -> Vec<u64> {
        self.probe
            .worker_reads()
            .iter()
            .map(|r| r.range.start / TILE_LEN)
            .collect()
    }

    /// Drops the service, checking that every thread is gone: they all hold the notify
    /// callback, which goes with the last of them.
    fn stop(self) {
        drop(self.service);
        assert_eq!(
            Arc::strong_count(&self.notified),
            1,
            "a thread is still running"
        );
    }
}

fn start(opened: Opened, configure: impl FnOnce(&mut ServiceOptions, &Probe)) -> Running {
    let (opened, probe) = probed(opened);
    let rec = Arc::new(Recording::new(opened).unwrap().0);
    let mut options = ServiceOptions::new(PyramidStore::Memory);
    options.cache_budget = 1 << 30;
    configure(&mut options, &probe);
    let notified = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&notified);
    let service = TileService::new(rec, options, move || {
        counter.fetch_add(1, Ordering::Relaxed);
    })
    .unwrap();
    Running {
        service,
        probe,
        notified,
    }
}

/// A build that takes a while: one-tile steps on one thread, each read slowed by `delay`.
fn slow(options: &mut ServiceOptions, probe: &Probe, delay: Duration) {
    options.build_chunk_len = Some(TILE_LEN);
    options.build_threads = 1;
    probe.slow_build(delay);
}

fn one_worker(options: &mut ServiceOptions) {
    options.workers = 1;
}

fn delivers_every_key(slow_build: bool) {
    let rec = Recording::new(two_grids()).unwrap().0;
    let keys = all_keys(&rec);
    let want = expected(&rec, &keys);
    let bad = [
        key(0, 0, 0, 200_000 / TILE_LEN + 1), // past the end
        key(1, 0, 3, 0),                      // above the top level
        key(0, 2, 1, 0),                      // no such block
        TileKey {
            version: SignalVersion(7),
            ..key(0, 0, 1, 0)
        },
    ];

    let run = start(two_grids(), |options, probe| {
        if slow_build {
            slow(options, probe, Duration::from_millis(2));
        }
    });
    run.service.submit(1, keys.iter().chain(&bad).copied());
    if slow_build {
        assert!(!run.service.progress().complete, "the build is not slow");
    }
    run.wait_for(&keys);
    for k in &keys {
        assert_eq!(*run.service.cache().get(k).unwrap(), want[k], "{k:?}");
    }
    wait_until("failures", || run.service.failures().len() == bad.len());
    let failed: Vec<TileKey> = run.service.failures().iter().map(|f| f.0).collect();
    let mut bad = bad.to_vec();
    bad.sort();
    assert_eq!(failed, bad);
    wait_until("build", || run.service.progress().complete);
    let progress = run.service.progress();
    assert_eq!((progress.done, progress.total), (250_000, 250_000));
    assert!(run.notified.load(Ordering::Relaxed) >= keys.len() + bad.len());
    run.stop();
}

#[test]
fn delivers_every_key_as_computed_directly() {
    delivers_every_key(false);
}

#[test]
fn delivers_every_key_during_a_slow_build() {
    delivers_every_key(true);
}

#[test]
fn pyramid_tiles_wait_for_the_build_without_blocking_raw_ones() {
    let run = start(two_grids(), |options, probe| {
        slow(options, probe, Duration::from_millis(5));
        one_worker(options);
    });
    // The last level-1 tile of grid 0 is built last; the raw tile behind it is ready now.
    let late = key(0, 0, 1, 1);
    let raw = key(0, 0, 0, 3);
    run.service.submit(1, [late, raw]);
    run.wait_for(&[raw]);
    assert!(!run.has(&late));
    assert!(run.service.pyramid(GridId(0)).unwrap().done() < 200_000);
    run.wait_for(&[late]);
    assert!(run.service.failures().is_empty());
    run.stop();
}

fn newer_generation_drops_queued_keys(slow_build: bool) {
    let run = start(one_grid(), |options, probe| {
        one_worker(options);
        if slow_build {
            slow(options, probe, Duration::from_millis(2));
        }
    });
    let k = |t| key(0, 0, 0, t);
    run.probe.hold();
    run.service.submit(1, [k(0), k(1), k(2), k(3)]);
    run.probe.wait_held(1); // k(0) is being computed
    run.service.submit(2, [k(4), k(2), k(5)]);
    run.probe.release();
    run.wait_for(&[k(0), k(4), k(2), k(5)]);
    // Give dropped keys time to show up if they were (wrongly) still queued.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(run.raw_reads(), [0, 4, 2, 5]);
    assert!(!run.has(&k(1)) && !run.has(&k(3)));
    run.stop();
}

#[test]
fn a_newer_generation_drops_queued_keys() {
    newer_generation_drops_queued_keys(false);
}

#[test]
fn a_newer_generation_drops_queued_keys_during_a_slow_build() {
    newer_generation_drops_queued_keys(true);
}

fn keeps_priority_order(slow_build: bool) {
    let run = start(one_grid(), |options, probe| {
        one_worker(options);
        if slow_build {
            slow(options, probe, Duration::from_millis(2));
        }
    });
    let k = |t| key(0, 0, 0, t);
    run.probe.hold();
    run.service.submit(1, [10, 3, 7, 1, 9].map(k));
    run.probe.wait_held(1);
    // The same generation adds behind the queue; an older one is ignored.
    run.service.submit(1, [k(5), k(7)]);
    run.service.submit(0, [k(6)]);
    run.probe.release();
    run.wait_for(&[10, 3, 7, 1, 9, 5].map(k));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(run.raw_reads(), [10, 3, 7, 1, 9, 5]);
    run.stop();
}

#[test]
fn one_worker_keeps_priority_order() {
    keeps_priority_order(false);
}

#[test]
fn one_worker_keeps_priority_order_during_a_slow_build() {
    keeps_priority_order(true);
}

fn computes_each_key_once(slow_build: bool) {
    let run = start(one_grid(), |options, probe| {
        options.workers = 4;
        if slow_build {
            slow(options, probe, Duration::from_millis(1));
        }
    });
    let k = |t| key(0, 0, 0, t);

    // Resubmitted while it is computed.
    run.probe.hold();
    run.service.submit(1, [k(0)]);
    run.probe.wait_held(1);
    run.service.submit(2, [k(0)]);
    run.service.submit(3, [k(0)]);
    run.probe.release();
    run.wait_for(&[k(0)]);
    run.service.submit(4, [k(0)]); // cached by now

    // Many threads submitting overlapping sets in racing generations.
    let generation = AtomicUsize::new(5);
    std::thread::scope(|scope| {
        for seed in 0..4u64 {
            let (service, generation) = (&run.service, &generation);
            scope.spawn(move || {
                for round in 0..200u64 {
                    let start = (seed * 7 + round * 13) % 90;
                    let g = generation.fetch_add(1, Ordering::Relaxed) as u64;
                    service.submit(g, (start..start + 10).map(k));
                }
            });
        }
    });
    run.service.submit(u64::MAX, (0..100).map(k));
    run.wait_for(&(0..100).map(k).collect::<Vec<_>>());

    let mut reads: Vec<u64> = run.raw_reads();
    reads.sort();
    assert_eq!(
        reads,
        (0..100).collect::<Vec<_>>(),
        "each key read exactly once"
    );
    run.stop();
}

#[test]
fn never_computes_a_key_twice() {
    computes_each_key_once(false);
}

#[test]
fn never_computes_a_key_twice_during_a_slow_build() {
    computes_each_key_once(true);
}

/// A generated EDF+D file (10 min, up to 512 Hz) with a sidecar directory.
fn generated() -> (TempFile, TempDir) {
    let file = TempFile::written(&Spec::preset("edf+d-mixed").unwrap());
    (file, TempDir::new())
}

fn start_on_file(
    file: &TempFile,
    dir: &TempDir,
    configure: impl FnOnce(&mut ServiceOptions, &Probe),
) -> Running {
    start(kymo_core::open(file.path()).unwrap(), |options, probe| {
        options.pyramids = PyramidStore::Sidecar {
            dir: dir.path().to_owned(),
            source: file.path().to_owned(),
        };
        configure(options, probe);
    })
}

#[test]
fn shutdown_right_away_leaves_no_files() {
    let (file, dir) = generated();
    let run = start_on_file(&file, &dir, |_, _| {});
    run.stop();
    assert_eq!(dir.files(), Vec::<String>::new());
}

#[test]
fn shutdown_during_the_build_leaves_no_files() {
    let (file, dir) = generated();
    let run = start_on_file(&file, &dir, |options, probe| {
        slow(options, probe, Duration::from_millis(5));
    });
    wait_until("build started", || run.service.progress().done > 0);
    assert!(!run.service.progress().complete);
    assert_eq!(dir.files().len(), 1, "the temporary sidecar");
    run.stop();
    assert_eq!(dir.files(), Vec::<String>::new());
}

#[test]
fn shutdown_waits_for_a_tile_in_flight() {
    let (file, dir) = generated();
    let run = start_on_file(&file, &dir, |options, probe| {
        slow(options, probe, Duration::from_millis(5));
    });
    run.probe.hold();
    run.service.submit(1, [key(0, 0, 0, 0)]);
    run.probe.wait_held(1);
    let probe = Arc::clone(&run.probe);
    let stopping = std::thread::spawn(move || run.stop());
    std::thread::sleep(Duration::from_millis(50));
    assert!(!stopping.is_finished());
    probe.release();
    stopping.join().unwrap();
    assert_eq!(dir.files(), Vec::<String>::new());
}

#[test]
fn shutdown_at_any_point_leaves_no_temporary_files() {
    let (file, dir) = generated();
    for ms in [0, 1, 2, 4, 8, 16, 32] {
        let run = start_on_file(&file, &dir, |options, probe| {
            slow(options, probe, Duration::from_micros(300));
        });
        let rec = Arc::clone(run.service.recording());
        run.service.submit(
            1,
            kymo_core::tile::prefetch(
                &rec,
                &common::view(0.0..60.0, 1000, (0..7).collect()),
                Default::default(),
                usize::MAX,
            ),
        );
        std::thread::sleep(Duration::from_millis(ms));
        let complete = run.service.progress().complete;
        run.stop();
        let files = dir.files();
        if complete {
            assert_eq!(files.len(), 1);
            assert!(files[0].ends_with(".kymo-pyramid"), "{files:?}");
            std::fs::remove_file(dir.path().join(&files[0])).unwrap();
        } else {
            assert_eq!(files, Vec::<String>::new(), "after {ms} ms");
        }
    }
}

#[test]
fn a_finished_build_stays_for_the_next_open() {
    let (file, dir) = generated();
    let run = start_on_file(&file, &dir, |_, _| {});
    wait_until("build", || run.service.progress().complete);
    run.stop();
    let files = dir.files();
    assert_eq!(files.len(), 1);

    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    assert!(
        Sidecar::open_or_create(dir.path(), file.path(), &rec)
            .unwrap()
            .is_complete()
    );

    // A second service reads the pyramid without building it again.
    let run = start_on_file(&file, &dir, |_, _| {});
    wait_until("build", || run.service.progress().complete);
    let keys: Vec<TileKey> = all_keys(&rec)
        .into_iter()
        .filter(|k| k.level > Level::RAW)
        .collect();
    let want = expected(&rec, &keys);
    run.service.submit(1, keys.iter().copied());
    run.wait_for(&keys);
    for k in &keys {
        assert_eq!(*run.service.cache().get(k).unwrap(), want[k], "{k:?}");
    }
    assert_eq!(run.probe.build_reads(), 0);
    run.stop();
    assert_eq!(dir.files(), files);
}

#[test]
fn failures_name_the_key() {
    let run = start(one_grid(), |_, _| {});
    let past = key(0, 0, 0, 100);
    run.service.submit(1, [past]);
    wait_until("failure", || !run.service.failures().is_empty());
    let failures = run.service.failures();
    assert!(matches!(*failures[0].1, TileError::PastEnd { key, .. } if key == past));
    // Never tried again.
    run.service.submit(2, [past]);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(run.probe.worker_reads().len(), 0);
    run.stop();
}

// Window means for DC removal.

/// `two_grids` with a gain and an offset on every channel.
fn scaled_two_grids() -> Opened {
    let mut opened = two_grids();
    for (c, channel) in opened.header.channels.iter_mut().enumerate() {
        channel.gain = 1e-3 * (c as f64 + 1.0) * if c % 2 == 0 { 1.0 } else { -1.0 };
        channel.offset = 2.5e5 * c as f64;
    }
    opened
}

/// The mean of `channel` of `scaled_two_grids` over the present samples of `range`.
fn brute_mean(channel: u32, range: Range<u64>) -> Option<f64> {
    let rec = Recording::new(scaled_two_grids()).unwrap().0;
    let info = rec.channel(ChannelId(channel));
    let grid = rec.grid(info.grid);
    let row = grid
        .channels()
        .iter()
        .position(|&c| c == ChannelId(channel))
        .unwrap();
    let data = match info.grid.0 {
        0 => mem_data(SampleType::I32, 40, 200_000, 1),
        _ => mem_data(SampleType::I16, 3, 50_000, 2),
    };
    let (mut sum, mut n) = (0.0, 0u64);
    for part in grid.present_in(range) {
        for i in part {
            sum += data.get(row, i as usize) * info.gain + info.offset;
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn means_match_brute_force(slow_build: bool) {
    let windows = [
        (3, 0..200_000),        // the whole grid, gap included
        (5, 65_000..95_000),    // across the gap
        (0, 4096..8192),        // one whole block
        (41, 1234..1300),       // inside one block
        (42, 10..50_000),       // the other grid
        (39, 199_990..200_000), // the end
    ];
    let run = start(scaled_two_grids(), |options, probe| {
        if slow_build {
            slow(options, probe, Duration::from_millis(2));
        }
    });
    run.service
        .request_means(windows.iter().map(|(c, r)| (ChannelId(*c), r.clone())));
    wait_until("means", || {
        windows.iter().all(|(c, r)| {
            run.service
                .mean(ChannelId(*c))
                .is_some_and(|m| m.range == *r)
        })
    });
    for (c, range) in windows {
        let got = run.service.mean(ChannelId(c)).unwrap().value;
        let want = brute_mean(c, range.clone()).unwrap();
        assert!(
            close(got, want),
            "channel {c} {range:?}: {got} instead of {want}"
        );
    }
    run.stop();
}

#[test]
fn means_match_brute_force_after_the_build() {
    means_match_brute_force(false);
}

#[test]
fn means_match_brute_force_during_a_slow_build() {
    means_match_brute_force(true);
}

#[test]
fn a_window_in_a_gap_keeps_the_last_mean() {
    let run = start(scaled_two_grids(), |options, _| one_worker(options));
    wait_until("build", || run.service.progress().complete);
    run.service.request_means([(ChannelId(7), 0..1000)]);
    wait_until("mean", || run.service.mean(ChannelId(7)).is_some());
    // One worker takes them in order, so once channel 8 has its mean, 7 had its turn.
    run.service
        .request_means([(ChannelId(7), 70_000..90_000), (ChannelId(8), 0..1000)]);
    wait_until("later mean", || run.service.mean(ChannelId(8)).is_some());
    let mean = run.service.mean(ChannelId(7)).unwrap();
    assert_eq!(mean.range, 0..1000);
    assert!(close(mean.value, brute_mean(7, 0..1000).unwrap()));
    assert_eq!(brute_mean(7, 70_000..90_000), None);
    run.stop();
}

#[test]
fn a_newer_request_replaces_the_older_one() {
    let run = start(scaled_two_grids(), |options, _| one_worker(options));
    wait_until("build", || run.service.progress().complete);
    run.probe.hold();
    run.service.request_means([(ChannelId(0), 100..200)]);
    run.probe.wait_held(1);
    run.service
        .request_means([(ChannelId(1), 100..200), (ChannelId(2), 100..200)]);
    run.service.request_means([(ChannelId(3), 100..200)]);
    run.probe.release();
    wait_until("newest mean", || run.service.mean(ChannelId(3)).is_some());
    // The running one finishes; the replaced ones never start.
    assert!(run.service.mean(ChannelId(0)).is_some());
    let read: Vec<u32> = run.probe.worker_reads().iter().map(|r| r.first.0).collect();
    assert_eq!(read, [0, 3]);
    assert_eq!(run.service.mean(ChannelId(1)), None);
    assert_eq!(run.service.mean(ChannelId(2)), None);
    // Asking again for a computed window reads nothing.
    run.service.request_means([(ChannelId(3), 100..200)]);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(run.probe.worker_reads().len(), 2);
    run.stop();
}

#[test]
fn means_beyond_the_decode_limit_wait_for_the_build() {
    let limit = 3 * TILE_LEN;
    let run = start(one_grid(), |options, probe| {
        slow(options, probe, Duration::from_millis(3));
        one_worker(options);
        options.mean_decode_limit = limit;
    });
    let long = 0..60 * TILE_LEN + 100;
    run.service.request_means([
        (ChannelId(0), long.clone()),
        (ChannelId(1), 10..2 * TILE_LEN),
    ]);
    // The short window is ready at once; the long one waits.
    wait_until("short mean", || run.service.mean(ChannelId(1)).is_some());
    assert_eq!(run.service.mean(ChannelId(0)), None);
    assert!(run.service.pyramid(GridId(0)).unwrap().done() < long.end);
    wait_until("long mean", || run.service.mean(ChannelId(0)).is_some());
    let decoded: u64 = run
        .probe
        .worker_reads()
        .iter()
        .filter(|r| r.first == ChannelId(0))
        .map(|r| r.range.end - r.range.start)
        .sum();
    assert!(decoded <= limit, "decoded {decoded} samples");
    let data = mem_data(SampleType::I16, 4, 100 * TILE_LEN, 3);
    let want =
        long.clone().map(|i| data.get(0, i as usize)).sum::<f64>() / (long.end - long.start) as f64;
    assert!(close(run.service.mean(ChannelId(0)).unwrap().value, want));
    run.stop();
}
