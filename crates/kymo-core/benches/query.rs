//! The phase-1 query criterion: the whole 8 h 256-channel BDF in one 1920 px view, all
//! channels, from its pyramid sidecar. Needs `KYMO_BENCH_BDF`; builds the sidecar once
//! (into `KYMO_BENCH_SIDECARS`, or Cargo's target directory).

// Benchmark code: panicking on a broken input is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use kymo_core::pyramid::{Pyramid, Sidecar};
use kymo_core::tile::{Level, SignalVersion, TileCache, View, compute_raw, query, tiles_for_view};
use kymo_core::{ChannelId, GridId, Recording};

/// Computes or reads every tile `view` wants and puts it into `cache`.
fn fetch(rec: &Recording, pyramids: &[Pyramid], view: &View, cache: &TileCache) {
    for key in tiles_for_view(rec, view) {
        let tile = if key.level == Level::RAW {
            compute_raw(rec, key).unwrap()
        } else {
            pyramids[key.block.grid.0 as usize].tile(key).unwrap()
        };
        cache.insert(Arc::new(tile));
    }
}

fn full_view(c: &mut Criterion) {
    let Some(path) = common::large() else {
        return;
    };
    let rec = common::open(&path);
    let dir = std::env::var_os("KYMO_BENCH_SIDECARS")
        .map_or_else(|| common::dir().join("sidecars"), PathBuf::from);
    let sidecar = Sidecar::open_or_create(&dir, &path, &rec).unwrap();
    if !sidecar.is_complete() {
        eprintln!("building the pyramid sidecar once, in {}", dir.display());
        let start = Instant::now();
        sidecar.build(&rec).unwrap();
        eprintln!(
            "built in {}",
            common::seconds(start.elapsed().as_secs_f64())
        );
    }
    let pyramids: Vec<Pyramid> = (0..rec.grids().len() as u32)
        .map(|g| sidecar.pyramid(GridId(g)).unwrap())
        .collect();
    let view = View {
        version: SignalVersion::RAW,
        time: 0.0..rec.duration(),
        width_px: 1920,
        channels: (0..rec.channels().len() as u32).map(ChannelId).collect(),
    };
    let keys = tiles_for_view(&rec, &view);
    let cache = TileCache::default();
    fetch(&rec, &pyramids, &view, &cache);
    eprintln!(
        "full view: {} tiles at level {}, {:.1} MB",
        keys.len(),
        keys.first().map_or(0, |k| k.level.get()),
        cache.bytes() as f64 / 1e6
    );

    let mut group = c.benchmark_group("query bdf-256ch-8h, full recording, 1920 px");
    group.bench_function("tiles cached", |b| b.iter(|| query(&rec, &view, &cache)));
    group.bench_function("tiles read from the sidecar", |b| {
        b.iter(|| {
            let cache = TileCache::default();
            fetch(&rec, &pyramids, &view, &cache);
            query(&rec, &view, &cache)
        })
    });
    group.finish();
}

criterion_group!(benches, full_view);
criterion_main!(benches);
