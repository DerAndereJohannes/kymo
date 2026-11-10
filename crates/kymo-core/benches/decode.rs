//! Level-0 decode of a 20 s window over all 256 channels of a BDF: what the viewer reads
//! when zoomed in to raw samples. And the shared sample decoding of the readers, per
//! encoding and byte order, over a 20 s window of 64 sample-major channels.

// Benchmark code: panicking on a broken input is the point.
#![allow(clippy::unwrap_used)]

mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kymo_core::GridId;
use kymo_core::Sample;
use kymo_core::decode::{ByteOrder, Decoder, Encoding};
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

/// Decodes every channel of a sample-major buffer, as a reader of interleaved samples
/// (BrainVision's multiplexed layout, FIFF's buffers) does.
fn interleaved<T: Sample>(decoder: &Decoder, src: &[u8], channels: usize, out: &mut [T]) {
    let size = decoder.encoding().size();
    let n = out.len() / channels;
    for (c, row) in out.chunks_exact_mut(n).enumerate() {
        decoder
            .decode(&src[c * size..], channels * size, row)
            .unwrap();
    }
}

fn encodings(c: &mut Criterion) {
    const CHANNELS: usize = 64;
    const SAMPLES: usize = 20_000;
    let mut group = c.benchmark_group("decode 64 interleaved channels, 20 s at 1 kHz");
    let cases = [
        (Encoding::I16, ByteOrder::Little),
        (Encoding::I24, ByteOrder::Little),
        (Encoding::I32, ByteOrder::Little),
        (Encoding::F32, ByteOrder::Little),
        (Encoding::F64, ByteOrder::Little),
        (Encoding::I16, ByteOrder::Big),
        (Encoding::I32, ByteOrder::Big),
        (Encoding::F32, ByteOrder::Big),
    ];
    for (encoding, order) in cases {
        let size = encoding.size();
        // Bytes that make finite floats: a slow ramp in every byte but the exponent's.
        let src: Vec<u8> = (0..CHANNELS * SAMPLES * size)
            .map(|i| (i % 61) as u8 + 1)
            .collect();
        let decoder = Decoder::new(encoding, order).with_reference(12.5);
        group.throughput(Throughput::Bytes(src.len() as u64));
        let name = format!("{encoding:?} {order:?}").to_lowercase();
        let mut i16s = vec![0i16; CHANNELS * SAMPLES];
        let mut i32s = vec![0i32; CHANNELS * SAMPLES];
        let mut f32s = vec![0f32; CHANNELS * SAMPLES];
        group.bench_function(&name, |b| {
            b.iter(|| match decoder.native() {
                kymo_core::SampleType::I16 => interleaved(&decoder, &src, CHANNELS, &mut i16s),
                kymo_core::SampleType::I32 => interleaved(&decoder, &src, CHANNELS, &mut i32s),
                _ => interleaved(&decoder, &src, CHANNELS, &mut f32s),
            })
        });
    }
    group.finish();
}

criterion_group!(benches, window, encodings);
criterion_main!(benches);
