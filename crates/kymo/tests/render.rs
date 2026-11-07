//! The trace renderer on a real GPU: frames rendered off-screen, every pixel column checked
//! against an f64 reference computed from the raw samples.
//!
//! Without a GPU adapter the tests pass with a message; set `KYMO_REQUIRE_GPU` to make
//! them fail instead (CI does on Linux, with Mesa's software Vulkan driver).

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::OnceLock;

use common::{Fixture, HATCH, Rng, Truth, channel, f32_rows, grid, i16_rows, i32_rows, view, walk};
use eframe::egui_wgpu::wgpu;
use kymo::render::{EDGE_TOLERANCE, Frame, Scene, TraceRenderer};
use kymo_core::tile::{
    Level, Query, TILE_LEN, TileCache, TileKey, level_for, max_level, query, tiles_for_view,
};
use kymo_core::{ChannelId, SampleType};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// The test GPU, created once, with the limits eframe asks for.
fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    let gpu = GPU
        .get_or_init(|| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
            if wgpu::Backends::from_env().is_none() {
                desc.backends = wgpu::Backends::PRIMARY;
            }
            let instance = wgpu::Instance::new(desc);
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .ok()?;
            eprintln!("GPU tests on {:?}", adapter.get_info());
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("kymo test"),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }))
                .ok()?;
            Some(Gpu { device, queue })
        })
        .as_ref();
    if gpu.is_none() {
        assert!(
            std::env::var_os("KYMO_REQUIRE_GPU").is_none_or(|v| v.is_empty()),
            "no GPU adapter, and KYMO_REQUIRE_GPU is set"
        );
        eprintln!("skipped: no GPU adapter");
    }
    gpu
}

impl Gpu {
    fn renderer(&self) -> TraceRenderer {
        TraceRenderer::new(&self.device, wgpu::TextureFormat::Rgba8Unorm, 1).unwrap()
    }

    /// Renders `frame` into a cleared, transparent image.
    fn render(&self, renderer: &mut TraceRenderer, frame: &Frame) -> Image {
        let [width, height] = frame.size;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kymo test target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        renderer.prepare(&self.device, &self.queue, frame);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kymo test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.paint(&mut pass);
        }
        let row_bytes = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kymo test readback"),
            size: u64::from(row_bytes * height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let bytes = buffer.slice(..).get_mapped_range().unwrap();
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            let row = &bytes[y * row_bytes as usize..][..width as usize * 4];
            pixels.extend(row.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]));
        }
        Image {
            width,
            height,
            pixels,
        }
    }
}

#[derive(PartialEq)]
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

impl Image {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y * self.width + x) as usize]
    }

    /// Trace pixels are white; hatch pixels have no red.
    fn trace(&self, x: u32, y: u32) -> bool {
        self.at(x, y)[0] > 0
    }

    fn hatch(&self, x: u32, y: u32) -> bool {
        let p = self.at(x, y);
        p[0] == 0 && p[1] > 0
    }

    /// The trace rows of column `x`, if they form one run.
    fn run(&self, x: u32) -> Result<Option<(i64, i64)>, String> {
        let lit: Vec<i64> = (0..self.height)
            .filter(|&y| self.trace(x, y))
            .map(i64::from)
            .collect();
        match (lit.first(), lit.last()) {
            (Some(&a), Some(&b)) if b - a + 1 == lit.len() as i64 => Ok(Some((a, b))),
            (Some(_), _) => Err(format!("column {x}: rows {lit:?} not contiguous")),
            _ => Ok(None),
        }
    }
}

/// What the reference allows in a column: the line through the samples (level 0), or the
/// samples plus whole buckets on either side (levels ≥ 1, fallbacks), as wide as the
/// widest drawn near the column (`widths[x]` pixels).
#[derive(Clone, Debug)]
enum Shape {
    Line,
    Buckets { widths: Vec<f64> },
}

impl Shape {
    /// The widest bucket anywhere.
    fn widest(&self) -> f64 {
        match self {
            Shape::Line => 0.0,
            Shape::Buckets { widths } => widths.iter().copied().fold(0.0, f64::max),
        }
    }
}

/// Pixels a value or position may be off from the f64 reference: f32 rounding.
const EPS: f64 = 2.0 * EDGE_TOLERANCE as f64;

/// The rows of `range` in an image `height` rows tall, its ends moved inwards by `shrink`
/// (outwards if negative).
fn rows(range: Option<(f64, f64)>, shrink: f64, height: u32) -> Option<(i64, i64)> {
    let (lo, hi) = range?;
    let top = ((lo + shrink).floor() as i64).max(0);
    let bottom = ((hi - shrink).floor() as i64).min(i64::from(height) - 1);
    (top <= bottom).then_some((top, bottom))
}

/// Checks every column: the trace rows form one run that holds every row the reference
/// must light and stays within the rows it may light.
fn check_columns(
    image: &Image,
    truth: &Truth,
    shape: &Shape,
    columns: &[std::ops::Range<u32>],
) -> Result<(), String> {
    for x in columns.iter().flat_map(Clone::clone) {
        let c = f64::from(x);
        let (inner, outer) = match shape {
            Shape::Line => (
                truth.line(c + EPS, c + 1.0 - EPS),
                truth.line(c - EPS, c + 1.0 + EPS),
            ),
            Shape::Buckets { widths } => {
                let width = widths[x as usize];
                (
                    truth.samples(c + EPS, c + 1.0 - EPS),
                    truth.samples(c - width - EPS, c + 1.0 + width + EPS),
                )
            }
        };
        let must = rows(inner, EPS, image.height);
        let may = rows(outer, -EPS, image.height);
        let lit = image.run(x)?;
        let ok = match (lit, must, may) {
            (None, None, _) => true,
            (Some((a, b)), must, Some((lo, hi))) => {
                lo <= a && b <= hi && must.is_none_or(|(m0, m1)| a <= m0 && m1 <= b)
            }
            _ => false,
        };
        if !ok {
            return Err(format!(
                "column {x}: lit {lit:?}, must light {must:?}, may light {may:?} \
                 (inner {inner:?}, outer {outer:?})"
            ));
        }
    }
    // Where the data runs on without a gap, neighbouring columns connect: their runs share
    // a row.
    for range in columns {
        for x in range.start..range.end.saturating_sub(1) {
            let c = f64::from(x);
            let slack = match shape {
                Shape::Line => 0.0,
                Shape::Buckets { widths } => widths[x as usize].max(widths[x as usize + 1]) + EPS,
            };
            if !truth.continuous(c - slack, c + 2.0 + slack) {
                continue;
            }
            if let (Some((a0, a1)), Some((b0, b1))) = (image.run(x)?, image.run(x + 1)?)
                && a0.max(b0) > a1.min(b1)
            {
                return Err(format!(
                    "columns {x} and {} don't connect: rows {a0}..={a1} and {b0}..={b1}",
                    x + 1
                ));
            }
        }
    }
    Ok(())
}

/// The reference shape for what `query` draws: lines if everything is level 0, otherwise
/// buckets as wide as the widest drawn.
fn shape(fx: &Fixture, scene: &Scene, query: &Query) -> Shape {
    if query.draw.iter().all(|p| p.tile.key().level == Level::RAW) {
        return Shape::Line;
    }
    let per_px = f64::from(scene.size[0]) / (scene.time.end - scene.time.start);
    let x = |t: f64| (t - scene.time.start) * per_px;
    // Each tile's samples on screen, and its bucket width.
    let placed: Vec<(f64, f64, f64)> = query
        .draw
        .iter()
        .map(|p| {
            let key = p.tile.key();
            let grid = fx.recording.grid(key.block.grid);
            let width = key.level.bucket_len() as f64 / grid.rate() * per_px;
            (
                x(grid.time_of(p.samples.start)),
                x(grid.time_of(p.samples.end)),
                width,
            )
        })
        .collect();
    // The widest bucket of the tiles whose samples come within a pixel of the column.
    let widths = (0..scene.size[0])
        .map(|c| {
            let c = f64::from(c);
            placed
                .iter()
                .filter(|&&(a, b, _)| a <= c + 2.0 && c - 1.0 <= b)
                .map(|&(_, _, w)| w)
                .fold(0.0, f64::max)
        })
        .collect();
    Shape::Buckets { widths }
}

/// Renders `scene` from the tiles in `cache` and checks every column.
fn check(gpu: &Gpu, fx: &Fixture, scene: &Scene, cache: &TileCache) -> Image {
    check_some(gpu, fx, scene, cache, &[0..scene.size[0]])
}

/// Renders `scene` from the tiles in `cache` and checks the columns in `columns`.
fn check_some(
    gpu: &Gpu,
    fx: &Fixture,
    scene: &Scene,
    cache: &TileCache,
    columns: &[std::ops::Range<u32>],
) -> Image {
    let query = query(&fx.recording, &view(scene), cache);
    let frame = Frame::new(&fx.recording, scene, &query);
    let image = gpu.render(&mut gpu.renderer(), &frame);
    let shape = shape(fx, scene, &query);
    let margin = match shape {
        Shape::Line => 1.0,
        Shape::Buckets { .. } => shape.widest() + 2.0,
    };
    let truth = Truth::new(fx, scene, margin);
    if let Some(dir) = std::env::var_os("KYMO_DUMP") {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut ppm = format!("P6 {} {} 255\n", image.width, image.height).into_bytes();
        ppm.extend(image.pixels.iter().flat_map(|p| [p[0], p[1], p[2]]));
        std::fs::write(std::path::Path::new(&dir).join(format!("{n:03}.ppm")), ppm).unwrap();
    }
    if let Err(e) = check_columns(&image, &truth, &shape, columns) {
        panic!(
            "{:?} over {:?} at {:?} ({shape:?}): {e}",
            scene.rows[0].channel,
            scene.time,
            query
                .draw
                .iter()
                .map(|p| (p.tile.key().level, p.samples.clone()))
                .collect::<Vec<_>>()
        );
    }
    image
}

/// The tiles the view of `scene` wants, all cached.
fn exact(fx: &Fixture, scene: &Scene) -> TileCache {
    fx.cache(tiles_for_view(&fx.recording, &view(scene)))
}

const SIZE: [u32; 2] = [333, 120];

/// Two grids at different rates: int16 at 1000 Hz, and BDF-like int32 at 250 Hz with a huge
/// offset, one channel with a negative gain.
fn mixed() -> Fixture {
    let mut rng = Rng::new(7);
    let fast = 200_000;
    let slow = fast / 4;
    let data = vec![
        i16_rows(vec![
            walk(&mut rng, fast as usize, 300.0, -30_000.0, 30_000.0),
            walk(&mut rng, fast as usize, 20.0, -2_000.0, 2_000.0),
        ]),
        i32_rows(vec![
            walk(&mut rng, slow as usize, 2_000.0, -8_000_000.0, 8_000_000.0),
            walk(&mut rng, slow as usize, 50.0, -5_000.0, 5_000.0),
        ]),
    ];
    Fixture::new(
        vec![
            channel("fast", 0, 0.5, 0.0),
            channel("fast small", 0, 0.1, -3.0),
            channel("bdf", 1, 0.031_25, 1.0e6),
            channel("flipped", 1, -0.031_25, -2.5e5),
        ],
        vec![
            grid(1000.0, fast, vec![0..fast], SampleType::I16, 0..2),
            grid(250.0, slow, vec![0..slow], SampleType::I32, 2..4),
        ],
        data,
    )
}

/// Spans from the shortest window up to the whole recording, at several starts, including
/// windows around tile boundaries.
fn views(duration: f64, rate: f64) -> Vec<std::ops::Range<f64>> {
    let mut views = Vec::new();
    let mut span = 10.0 / rate;
    let mut rng = Rng::new(span.to_bits());
    loop {
        let span_now = span.min(duration);
        let boundary = TILE_LEN as f64 / rate;
        for start in [
            0.0,
            boundary - span_now / 2.0,
            // Ending inside a tile's last sample.
            boundary - 0.5 / rate - span_now,
            boundary * 3.0 - span_now / 3.0,
            rng.uniform() * (duration - span_now),
            duration - span_now,
        ] {
            let start = start.clamp(0.0, duration - span_now);
            views.push(start..start + span_now);
        }
        if span >= duration {
            return views;
        }
        span *= 7.3;
    }
}

#[test]
fn columns_match_the_samples_at_every_zoom() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    for id in 0..4 {
        let id = ChannelId(id);
        let rate = fx.grid_of(id).rate();
        for time in views(fx.recording.duration(), rate) {
            let scene = fx.scene(id, time, SIZE);
            check(gpu, &fx, &scene, &exact(&fx, &scene));
        }
    }
}

#[test]
fn every_level_is_drawn() {
    // The views of the test above reach level 0 and the top of the pyramid.
    let fx = mixed();
    let mut levels = std::collections::BTreeSet::new();
    for time in views(fx.recording.duration(), 1000.0) {
        let scene = fx.scene(ChannelId(0), time, SIZE);
        levels.insert(level_for(fx.grid_of(ChannelId(0)), &view(&scene)).get());
    }
    let top = max_level(fx.grid_of(ChannelId(0)).len()).get();
    assert!(top >= 2);
    assert_eq!(levels, (0..=top).collect());
}

#[test]
fn a_single_sample_spike_shows_at_every_zoom() {
    let Some(gpu) = gpu() else { return };
    let n = 1 << 20;
    let spike = 700_001;
    let mut flat = vec![0.0; n];
    flat[spike] = 1000.0;
    let fx = Fixture::new(
        vec![channel("spike", 0, 1.0, 0.0)],
        vec![grid(
            1000.0,
            n as u64,
            vec![0..n as u64],
            SampleType::I16,
            0..1,
        )],
        vec![i16_rows(vec![flat])],
    );
    let id = ChannelId(0);
    let at = spike as f64 / 1000.0;
    let mut span = 0.01;
    while span < fx.recording.duration() * 2.0 {
        let span_now = span.min(fx.recording.duration());
        let start = (at - span_now * 0.37).clamp(0.0, fx.recording.duration() - span_now);
        let scene = fx.scene(id, start..start + span_now, SIZE);
        let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
        // A column next to the spike's reaches the spike's row.
        let row = &scene.rows[0];
        let x = ((at - start) / span_now * f64::from(SIZE[0])) as u32;
        let top = (row.center - (1000.0 - row.baseline) * row.px_per_unit).floor() as u32;
        let reaches = (x.saturating_sub(1)..=(x + 1).min(SIZE[0] - 1)).any(|x| image.trace(x, top));
        assert!(
            reaches,
            "span {span_now}: spike at column {x} doesn't reach row {top}"
        );
        span *= 3.0;
    }
}

#[test]
fn huge_offsets_keep_sub_pixel_precision() {
    let Some(gpu) = gpu() else { return };
    // 24-bit BDF values around a 1e6 µV offset, drawn tall: 0.25 µV per pixel. In plain
    // f32 the offset alone would be off by a sizeable fraction of a pixel.
    let mut rng = Rng::new(3);
    let n = 50_000u64;
    let fx = Fixture::new(
        vec![channel("bdf", 0, 0.031_25, 1.0e6)],
        vec![grid(512.0, n, vec![0..n], SampleType::I32, 0..1)],
        vec![i32_rows(vec![walk(
            &mut rng,
            n as usize,
            5.0,
            7_999_000.0,
            8_000_000.0,
        )])],
    );
    let id = ChannelId(0);
    let (lo, hi) = fx.extent(id);
    assert!(lo > 1.24e6, "{lo}");
    for time in views(fx.recording.duration(), 512.0) {
        let mut scene = fx.scene(id, time, SIZE);
        scene.rows[0].px_per_unit = 4.0;
        scene.rows[0].baseline = (lo + hi) / 2.0;
        check(gpu, &fx, &scene, &exact(&fx, &scene));
    }
}

#[test]
fn extreme_values_stay_off_screen() {
    let Some(gpu) = gpu() else { return };
    // Values near the top of the int32 range around the baseline, and now and then the
    // bottom of the range: far below the screen, never wrapped around onto it.
    let n = 20_000u64;
    let top = f64::from(i32::MAX);
    let mut rng = Rng::new(17);
    let mut values = walk(&mut rng, n as usize, 20.0, top - 2000.0, top - 1.0);
    for i in (500..n as usize).step_by(1000) {
        values[i] = f64::from(i32::MIN);
    }
    let fx = Fixture::new(
        vec![channel("extreme", 0, 1.0, 0.0)],
        vec![grid(100.0, n, vec![0..n], SampleType::I32, 0..1)],
        vec![i32_rows(vec![values])],
    );
    for time in views(fx.recording.duration(), 100.0) {
        let mut scene = fx.scene(ChannelId(0), time, SIZE);
        scene.rows[0].baseline = top - 1000.0;
        scene.rows[0].px_per_unit = 0.05;
        check(gpu, &fx, &scene, &exact(&fx, &scene));
    }
}

#[test]
fn gaps_and_nans_stay_unlit() {
    let Some(gpu) = gpu() else { return };
    // An EDF+D-like grid with gaps, and float samples with NaN runs.
    let n = 120_000u64;
    // One gap starts where tile 3 starts (sample 12,288), so does a NaN in tile 4.
    let segments = vec![
        0..10_000,
        10_001..10_002,
        10_050..12_288,
        12_300..60_000,
        61_000..n,
    ];
    let mut rng = Rng::new(11);
    let mut floats = walk(&mut rng, n as usize, 0.5, -100.0, 100.0);
    for i in (20_000..20_500).chain([30_000, 30_002, 16_384]) {
        floats[i] = f64::NAN;
    }
    let ints = walk(&mut rng, n as usize, 100.0, -20_000.0, 20_000.0);
    let fx = Fixture::new(
        vec![channel("ints", 0, 1.0, 0.0), channel("floats", 1, 1.0, 0.0)],
        vec![
            grid(500.0, n, segments.clone(), SampleType::I16, 0..1),
            grid(500.0, n, vec![0..n], SampleType::F32, 1..2),
        ],
        vec![i16_rows(vec![ints]), f32_rows(vec![floats])],
    );
    for id in [ChannelId(0), ChannelId(1)] {
        let mut times = views(fx.recording.duration(), 500.0);
        // Zoomed in on the gaps, the isolated sample and the NaNs.
        for at in [
            10_001.0, 10_050.0, 12_288.0, 16_384.0, 60_000.0, 20_000.0, 30_001.0,
        ] {
            times.push((at - 15.0) / 500.0..(at + 15.0) / 500.0);
        }
        for time in times {
            let scene = fx.scene(id, time, SIZE);
            check(gpu, &fx, &scene, &exact(&fx, &scene));
        }
    }
    // The long gap is empty when it fills the screen.
    let scene = fx.scene(ChannelId(0), 60_100.0 / 500.0..60_900.0 / 500.0, SIZE);
    let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
    assert!(image.pixels.iter().all(|p| p[0] == 0));
}

#[test]
fn steps_at_tile_boundaries_connect() {
    let Some(gpu) = gpu() else { return };
    // The signal jumps where tiles of levels 1 and 2 meet. Zoomed out, the jump falls
    // between two samples 1/40 pixel apart; whichever column that is must show it, joining
    // the columns on either side.
    let n = 2_000_000u64;
    let boundaries = [131_072u64, 262_144, 524_288, 1_048_576];
    let values = (0..n)
        .map(|i| {
            if boundaries.iter().filter(|&&b| i >= b).count() % 2 == 1 {
                1000.0
            } else {
                0.0
            }
        })
        .collect();
    let fx = Fixture::new(
        vec![channel("steps", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..1)],
        vec![i16_rows(vec![values])],
    );
    let id = ChannelId(0);
    for samples_per_px in [40.0, 170.0] {
        let span = f64::from(SIZE[0]) * samples_per_px / 1000.0;
        for boundary in boundaries {
            // The jump just before, at and just after the edge of column 100.
            for x in [99.999, 100.0, 100.001] {
                let start = (boundary as f64 - x * samples_per_px) / 1000.0;
                let scene = fx.scene(id, start..start + span, SIZE);
                let level = level_for(fx.grid_of(id), &view(&scene)).get();
                assert!(level >= 1, "{level}");
                check(gpu, &fx, &scene, &exact(&fx, &scene));
            }
        }
    }
}

#[test]
fn isolated_samples_on_column_edges_show() {
    let Some(gpu) = gpu() else { return };
    // Every fifth sample stands alone between gaps. Zoomed in at whole and half pixels per
    // sample, many land exactly on the edge between two columns; f32 rounding mustn't lose
    // them to both.
    let n = 50_000u64;
    let segments = (0..n).step_by(5).map(|i| i..i + 1).collect();
    let mut rng = Rng::new(23);
    let fx = Fixture::new(
        vec![channel("dots", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, segments, SampleType::I16, 0..1)],
        vec![i16_rows(vec![walk(
            &mut rng, n as usize, 200.0, -10_000.0, 10_000.0,
        )])],
    );
    let id = ChannelId(0);
    let mut seen = 0;
    for px_per_sample in [1.0, 1.5, 2.0, 3.0, 7.0, 0.5] {
        for first in [0u64, 4096 - 101, 17_003, 33_333] {
            let start = first as f64 / 1000.0;
            let span = f64::from(SIZE[0]) / px_per_sample / 1000.0;
            let scene = fx.scene(id, start..start + span, SIZE);
            let image = check(gpu, &fx, &scene, &exact(&fx, &scene));
            let row = &scene.rows[0];
            for i in (first..first + (span * 1000.0) as u64).filter(|i| i % 5 == 0) {
                let x = (i - first) as f64 * px_per_sample;
                let y = row.center - (fx.physical(id, i).unwrap() - row.baseline) * row.px_per_unit;
                if x.fract() != 0.0 || x < 1.0 || !(1.0..f64::from(SIZE[1]) - 1.0).contains(&y) {
                    continue;
                }
                seen += 1;
                let (column, r) = (x as u32, y.floor() as u32);
                let lit = |c: u32| (r.saturating_sub(1)..=r + 1).any(|r| image.trace(c, r));
                assert!(
                    lit(column) || lit(column - 1),
                    "sample {i} at x {x}, y {y} vanished ({px_per_sample} px per sample)"
                );
            }
        }
    }
    assert!(seen > 500, "{seen}");
}

/// The ancestor `levels` above `key`.
fn ancestor(key: TileKey, levels: u8) -> TileKey {
    (0..levels).fold(key, |k, _| k.parent().unwrap())
}

#[test]
fn a_coarser_tile_stands_in_for_missing_ones() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    for id in [ChannelId(0), ChannelId(3)] {
        let rate = fx.grid_of(id).rate();
        for time in views(fx.recording.duration(), rate) {
            let scene = fx.scene(id, time, SIZE);
            let wanted = tiles_for_view(&fx.recording, &view(&scene));
            let top = max_level(fx.grid_of(id).len());
            for up in [1, 2] {
                // Every other wanted tile, and coarser ones for the rest.
                let keys = wanted.iter().enumerate().map(|(i, &k)| {
                    if i % 2 == 0 && k.level.get() + up <= top.get() {
                        ancestor(k, up)
                    } else {
                        k
                    }
                });
                let cache = fx.cache(keys);
                check(gpu, &fx, &scene, &cache);
            }
        }
    }
}

#[test]
fn loading_parts_are_hatched() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    let id = ChannelId(0);
    // Three tiles at level 0; the middle one is missing.
    let start = 2.0;
    let scene = fx.scene(id, start..start + 2.0 * TILE_LEN as f64 / 1000.0, SIZE);
    let wanted = tiles_for_view(&fx.recording, &view(&scene));
    assert_eq!(wanted.len(), 3);
    let cache = fx.cache([wanted[0], wanted[2]]);
    // Where the missing tile starts and ends.
    let x = |t: f64| (t - start) / (scene.time.end - start) * f64::from(SIZE[0]);
    let (a, b) = (
        x(TILE_LEN as f64 / 1000.0),
        x(2.0 * TILE_LEN as f64 / 1000.0),
    );
    let image = check_some(
        gpu,
        &fx,
        &scene,
        &cache,
        &[0..a as u32 - 1, b as u32 + 2..SIZE[0]],
    );

    for x in 0..SIZE[0] {
        // Pixel centres decide.
        let centre = f64::from(x) + 0.5;
        let missing = a <= centre && centre <= b;
        for y in 0..SIZE[1] {
            // Diagonal lines every 6 pixels, over the whole row.
            let line = (x + y) % 6 == 0;
            assert_eq!(image.hatch(x, y), missing && line, "hatch at ({x}, {y})");
            if a <= f64::from(x) && f64::from(x) + 1.0 <= b {
                assert!(!image.trace(x, y), "trace at ({x}, {y}) in {a}..{b}");
            }
        }
    }
    let x = a.ceil() as u32 + 1;
    assert_eq!(
        image.at(x, (6 - x % 6) % 6),
        HATCH.map(|c| (c * 255.0).round() as u8)
    );
}

#[test]
fn the_gpu_budget_evicts_old_tiles() {
    let Some(gpu) = gpu() else { return };
    let fx = mixed();
    let id = ChannelId(0);
    let frame = |time: std::ops::Range<f64>| {
        let scene = fx.scene(id, time, SIZE);
        let cache = exact(&fx, &scene);
        let query = query(&fx.recording, &view(&scene), &cache);
        Frame::new(&fx.recording, &scene, &query)
    };
    let a = frame(10.0..20.0);
    let b = frame(100.0..110.0);
    let one_tile = 4096 / 32 * 4 + 2 * 4096 * 2;

    let mut renderer = gpu.renderer();
    let first = gpu.render(&mut renderer, &a);
    let after_a = renderer.gpu_bytes();
    assert!(after_a >= one_tile);

    // A budget too small for even one frame keeps that frame's tiles and nothing else.
    renderer.set_budget(1);
    let _ = gpu.render(&mut renderer, &b);
    assert_eq!(renderer.resident_tiles(), b.draws.len());
    let again = gpu.render(&mut renderer, &a);
    assert_eq!(renderer.resident_tiles(), a.draws.len());
    assert!(
        first == again,
        "the frame changed after evicting and uploading again"
    );

    // Enough room for both keeps both.
    renderer.set_budget(usize::MAX);
    let _ = gpu.render(&mut renderer, &b);
    assert_eq!(renderer.resident_tiles(), a.draws.len() + b.draws.len());
    renderer.clear();
    assert_eq!(renderer.gpu_bytes(), 0);
}
