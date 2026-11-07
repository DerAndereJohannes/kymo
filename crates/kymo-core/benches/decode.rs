//! Level-0 decode of a 20 s window over all 256 channels of a BDF: what the viewer reads
//! when zoomed in to raw samples.

// Benchmark code: panicking on a broken input is the point.
#![allow(clippy::unwrap_used)]

mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kymo_core::GridId;
use kymo_core::tile::{
    ChannelBlock, Level, SignalVersion, TILE_LEN, TileKey, compute_raw, samples_in,
};

fn window(c: &mut Criterion) {
    let path = common::generated("bdf-256ch-8h", 60);
    let rec = common::open(&path);
    let grid = rec.grid(GridId(0));
    let samples = samples_in(grid, &(20.0..40.0));
    let keys: Vec<TileKey> = (0..ChannelBlock::count(grid))
        .flat_map(|index| {
            let tiles = samples.start / TILE_LEN..samples.end.div_ceil(TILE_LEN);
            tiles.map(move |time| TileKey {
                version: SignalVersion::RAW,
                level: Level::RAW,
                block: ChannelBlock {
                    grid: GridId(0),
                    index,
                },
                time,
            })
        })
        .collect();
    let channels = grid.channels();
    let n = (samples.end - samples.start) as usize;

    let mut group = c.benchmark_group("decode bdf 256 channels, 20 s window");
    group.throughput(Throughput::Bytes((channels.len() * n * 3) as u64));
    group.bench_function("level-0 tiles", |b| {
        b.iter(|| {
            keys.iter()
                .map(|&key| compute_raw(&rec, key).unwrap())
                .collect::<Vec<_>>()
        })
    });
    let mut out = vec![0f32; channels.len() * n];
    group.bench_function("physical values", |b| {
        b.iter(|| {
            rec.read_physical(GridId(0), channels, samples.clone(), &mut out)
                .unwrap()
        })
    });
    group.finish();
}

criterion_group!(benches, window);
criterion_main!(benches);
