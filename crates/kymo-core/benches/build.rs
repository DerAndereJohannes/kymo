//! Pyramid build throughput against a plain sequential read and a decode-only pass over
//! the same file. All runs after the first find the file in the page cache; for builds from
//! disk, see the `pyramid` example.

// Benchmark code: panicking on a broken input is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kymo_core::pyramid::{Pyramid, PyramidBuilder, Sidecar};
use kymo_core::{GridId, Recording, SampleType, SamplesMut};

/// Reads the whole file in 8 MB pieces.
fn read_file(path: &Path) -> u64 {
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = vec![0u8; 8 << 20];
    let mut sum = 0u64;
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            return sum;
        }
        sum += u64::from(buf[n - 1]);
    }
}

/// Decodes every sample of the first grid in 10 s chunks, without folding it.
fn decode(rec: &Recording) {
    let grid = rec.grid(GridId(0));
    let channels = grid.channels();
    let chunk = (grid.rate() * 10.0) as u64;
    let (mut i16s, mut i32s) = (Vec::new(), Vec::new());
    let mut start = 0;
    while start < grid.len() {
        let end = (start + chunk).min(grid.len());
        let n = channels.len() * (end - start) as usize;
        let out = match grid.sample_type() {
            SampleType::I16 => {
                i16s.resize(n, 0);
                SamplesMut::I16(&mut i16s)
            }
            _ => {
                i32s.resize(n, 0);
                SamplesMut::I32(&mut i32s)
            }
        };
        rec.read_native(GridId(0), channels, start..end, out)
            .unwrap();
        start = end;
    }
}

fn build(c: &mut Criterion) {
    for (preset, seconds) in [("edf-64ch-1h", 600), ("bdf-256ch-8h", 120)] {
        let path = common::generated(preset, seconds);
        let rec = common::open(&path);
        let bytes = std::fs::metadata(&path).unwrap().len();
        let mut group = c.benchmark_group(format!("build {preset} cut to {seconds} s"));
        group.throughput(Throughput::Bytes(bytes));
        group.sample_size(10);
        group.bench_function("plain file read", |b| b.iter(|| read_file(&path)));
        group.bench_function("decode only", |b| b.iter(|| decode(&rec)));
        group.bench_function("pyramid in memory, 1 thread", |b| {
            b.iter(|| {
                let builder = PyramidBuilder::new(&rec, GridId(0)).unwrap();
                builder.with_threads(1).finish().unwrap()
            })
        });
        group.bench_function("pyramid in memory", |b| {
            b.iter(|| Pyramid::build(&rec, GridId(0)).unwrap())
        });
        group.bench_function("pyramid sidecar", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for i in 0..iters {
                    let dir = common::dir().join(format!("build-{}-{i}", std::process::id()));
                    let start = Instant::now();
                    let sidecar = Sidecar::open_or_create(&dir, &path, &rec).unwrap();
                    assert!(sidecar.build(&rec).unwrap());
                    total += start.elapsed();
                    drop(sidecar);
                    std::fs::remove_dir_all(&dir).unwrap();
                }
                total
            })
        });
        group.finish();
    }
}

criterion_group!(benches, build);
criterion_main!(benches);
