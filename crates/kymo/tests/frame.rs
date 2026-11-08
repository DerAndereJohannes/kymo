//! The CPU side of trace drawing: coefficients against f64, where tiles land on screen, and
//! the scene built from the view state.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::OnceLock;

use common::{Fixture, Rng, channel, grid, i16_rows, i32_rows, view, walk};
use eframe::egui_wgpu::wgpu;
use kymo::render::{
    Coefficients, EDGE_TOLERANCE, Frame, Native, RenderError, Row, Scene, check_support,
};
use kymo::view::ViewState;
use kymo_core::tile::{Level, TILE_LEN, TileData, query, tiles_for_view};
use kymo_core::{ChannelId, ChannelKind, SampleType};
use proptest::prelude::*;

fn row(center: f64, px_per_unit: f64, baseline: f64) -> Row {
    Row {
        channel: ChannelId(0),
        center,
        height: 40.0,
        px_per_unit,
        baseline,
        color: [1.0; 4],
        clip: false,
    }
}

/// y in f64, straight from the definition.
fn exact_y(native: f64, gain: f64, offset: f64, row: &Row) -> f64 {
    row.center - (native * gain + offset - row.baseline) * row.px_per_unit
}

/// A gain of either sign between 1e-3 and 1e3 in size.
fn gain() -> impl Strategy<Value = f64> {
    (-3.0f64..3.0, any::<bool>()).prop_map(|(e, negative)| {
        let g = 10f64.powf(e);
        if negative { -g } else { g }
    })
}

proptest! {
    /// Integer samples stay within a thousandth of a pixel of f64 anywhere on screen,
    /// whatever the offset and baseline.
    #[test]
    fn integer_coefficients_match_f64(
        gain in gain(),
        offset in -1.0e7f64..1.0e7,
        at_baseline in -8_000_000i32..8_000_000,
        fraction in 0.0f64..1.0,
        px_per_native in -3.0f64..1.7,
        center in 0.0f64..2000.0,
        steps in prop::collection::vec(-1.0f64..1.0, 20),
    ) {
        let px_per_unit = 10f64.powf(px_per_native) / gain.abs();
        let baseline = (f64::from(at_baseline) + fraction) * gain + offset;
        let row = row(center, px_per_unit, baseline);
        let c = Coefficients::new(SampleType::I32, gain, offset, &row);
        // Values whose y is within 4000 px of the centre.
        let reach = (4000.0 / (gain.abs() * px_per_unit)).min(1.0e6);
        for step in steps {
            let native = (f64::from(at_baseline) + step * reach).round() as i32;
            let y = c.y(Native::Int(native)).unwrap();
            let exact = exact_y(f64::from(native), gain, offset, &row);
            prop_assert!(
                (f64::from(y) - exact).abs() < 1e-3,
                "native {native}: {y} vs {exact}"
            );
        }
    }

    /// Float samples keep f32's own precision: the error stays within a few units in the
    /// last place of the value, in pixels.
    #[test]
    fn float_coefficients_match_f64(
        gain in gain(),
        offset in -1.0e3f64..1.0e3,
        baseline in -1.0e3f64..1.0e3,
        px_per_unit in 0.01f64..100.0,
        values in prop::collection::vec(-1.0e4f32..1.0e4, 20),
    ) {
        let row = row(500.0, px_per_unit, baseline);
        let c = Coefficients::new(SampleType::F32, gain, offset, &row);
        for v in values {
            let y = f64::from(c.y(Native::Float(v)).unwrap());
            let exact = exact_y(f64::from(v), gain, offset, &row);
            let ulps = (f64::from(v.abs()) + ((baseline - offset) / gain).abs()) * 1.2e-7;
            let tolerance = 4.0 * ulps * gain.abs() * px_per_unit + 1e-3 + exact.abs() * 1e-6;
            prop_assert!((y - exact).abs() < tolerance, "{v}: {y} vs {exact}");
        }
    }
}

#[test]
fn larger_values_go_up_unless_the_gain_is_negative() {
    let r = row(100.0, 2.0, 0.0);
    let up = Coefficients::new(SampleType::I16, 0.5, 0.0, &r);
    assert_eq!(up.y(Native::Int(0)), Some(100.0));
    assert_eq!(up.y(Native::Int(10)), Some(90.0));
    let flipped = Coefficients::new(SampleType::I16, -0.5, 0.0, &r);
    assert_eq!(flipped.y(Native::Int(10)), Some(110.0));
    // The baseline is drawn at the centre.
    let dc = Coefficients::new(SampleType::I16, 0.5, 3.0, &row(100.0, 2.0, 8.0));
    assert_eq!(dc.y(Native::Int(10)), Some(100.0));
}

#[test]
fn extreme_values_saturate_instead_of_wrapping() {
    // The baseline sits near the top of the i32 range: the bottom of the range is far
    // below the screen, never wrapped around onto it.
    let gain = 1.0;
    let r = row(100.0, 1.0, f64::from(i32::MAX) - 50.0);
    let c = Coefficients::new(SampleType::I32, gain, 0.0, &r);
    assert_eq!(c.y(Native::Int(i32::MAX - 50)), Some(100.0));
    assert!(c.y(Native::Int(i32::MIN)).unwrap() > 1.0e9);
    // A baseline outside the type's range still puts the values in the right direction.
    let beyond = Coefficients::new(SampleType::I16, 1.0, 0.0, &row(100.0, 1.0, 1.0e12));
    assert!(beyond.y(Native::Int(i32::from(i16::MIN))).unwrap() > 1.0e9);
}

#[test]
fn skips_non_finite_floats_and_unusable_gains() {
    let r = row(50.0, 1.0, 0.0);
    let c = Coefficients::new(SampleType::F32, 1.0, 0.0, &r);
    assert_eq!(c.y(Native::Float(f32::NAN)), None);
    assert_eq!(c.y(Native::Float(f32::INFINITY)), None);
    assert_eq!(c.y(Native::Float(2.0)), Some(48.0));
    // A zero gain draws every value at the offset.
    let flat = Coefficients::new(SampleType::I16, 0.0, 7.0, &r);
    assert_eq!(flat.y(Native::Int(123)), Some(43.0));
    assert_eq!(flat.y(Native::Int(-9)), Some(43.0));
}

/// 64 channels in two blocks at 1000 Hz, 100 s; built once.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(build_fixture)
}

fn build_fixture() -> Fixture {
    let mut rng = Rng::new(5);
    let n = 100_000u64;
    let rows = (0..64)
        .map(|_| walk(&mut rng, n as usize, 50.0, -3000.0, 3000.0))
        .collect();
    Fixture::new(
        (0..64)
            .map(|c| channel(&format!("ch{c}"), 0, 0.25, -1.0))
            .collect(),
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..64)],
        vec![i16_rows(rows)],
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every item of every placed tile lies where its first sample's time puts it, within
    /// a thousandth of a pixel, and the tiles of a block cover the screen without gaps.
    #[test]
    fn tiles_land_at_their_samples_times(
        start in 0.0f64..100.0,
        span_log in -2.0f64..2.0,
        width in 50u32..3000,
    ) {
        let fx = fixture();
        let span = 10f64.powf(span_log).min(100.0);
        let start = start.min(100.0 - span);
        let mut scene = fx.scene(ChannelId(3), start..start + span, [width, 100]);
        scene.rows.push(Row { channel: ChannelId(40), ..scene.rows[0].clone() });
        let cache = fx.cache(tiles_for_view(&fx.recording, &view(&scene)));
        let q = query(&fx.recording, &view(&scene), &cache);
        let frame = Frame::new(&fx.recording, &scene, &q);
        let grid = fx.grid_of(ChannelId(0));
        let per_px = f64::from(width) / span;

        let mut ends: Vec<(u32, f32)> = Vec::new();
        for draw in &frame.draws {
            let key = draw.tile.key();
            let bucket = key.level.bucket_len();
            for item in draw.items.clone() {
                let sample = key.sample_range().start + u64::from(item) * bucket;
                let exact = (grid.time_of(sample) - start) * per_px;
                if !(-2.0..f64::from(width) + 2.0).contains(&exact) {
                    continue;
                }
                let placed = f64::from(draw.anchor_x)
                    + (f64::from(item) - f64::from(draw.anchor)) * f64::from(draw.item_width);
                prop_assert!((placed - exact).abs() < 1e-3, "{key:?} item {item}: {placed} vs {exact}");
            }
            // One channel of each block: 3 in the first, 40 in the second.
            prop_assert_eq!(draw.channels.len(), 1);
            prop_assert_eq!(draw.channels[0].row, if key.block.index == 0 { 3 } else { 8 });
            prop_assert!(draw.columns.start < draw.columns.end && draw.columns.end <= width);
            if let Some(&(block, end)) = ends.last()
                && block == key.block.index
            {
                prop_assert!((draw.domain[0] - end).abs() < EDGE_TOLERANCE, "{end} then {:?}", draw.domain);
            }
            ends.push((key.block.index, draw.domain[1]));
        }
        // Both blocks are drawn from the left edge to the right edge, or to the last sample
        // where the recording ends on screen.
        let last = (grid.time_of(grid.len() - 1) - start) * per_px;
        let right = (last.floor() as u32 + 1).min(width);
        for block in [0, 1] {
            let draws: Vec<_> = frame.draws.iter().filter(|d| d.tile.key().block.index == block).collect();
            prop_assert!(!draws.is_empty());
            prop_assert_eq!(draws[0].columns.start, 0);
            prop_assert_eq!(draws.last().unwrap().columns.end, right);
        }
    }
}

#[test]
fn drawing_continues_into_the_next_tile() {
    let fx = fixture();
    let tile_seconds = TILE_LEN as f64 / 1000.0;
    let frame_of = |fx: &Fixture, scene: &Scene, cache: &kymo_core::tile::TileCache| {
        Frame::new(
            &fx.recording,
            scene,
            &query(&fx.recording, &view(scene), cache),
        )
    };

    // Zoomed in across a tile boundary: the line runs to the next tile's first sample.
    let scene = fx.scene(
        ChannelId(0),
        tile_seconds - 0.05..tile_seconds + 0.05,
        [400, 100],
    );
    let wanted = tiles_for_view(&fx.recording, &view(&scene));
    let frame = frame_of(fx, &scene, &fx.cache(wanted.iter().copied()));
    let draw = frame.draws[0].clone();
    assert_eq!(draw.tile.key().level, Level::RAW);
    assert!(draw.continues);
    assert_eq!(*draw.items.end(), TILE_LEN as u32);
    let TileData::I16(d) = frame.draws[1].tile.data() else {
        panic!()
    };
    let first = Native::Int(i32::from(d[0]));
    assert_eq!(draw.channels[0].next, Some([first, first]));

    // Without the next tile, the line ends at the tile's last sample.
    let frame = frame_of(fx, &scene, &fx.cache(wanted.iter().copied().take(1)));
    let draw = frame.draws[0].clone();
    assert!(!draw.continues);
    assert_eq!(*draw.items.end(), TILE_LEN as u32 - 1);
    assert_eq!(draw.channels[0].next, None);
    // The rest is loading, hatched over the channel's row.
    assert_eq!(frame.hatches.len(), 1);
    let [x0, y0, x1, y1] = frame.hatches[0].rect;
    assert!((x0 - 200.0).abs() < 0.01 && x1 == 400.0, "{x0} {x1}");
    assert_eq!((y0, y1), (0.0, 100.0));

    // A level-0 line doesn't run on into a coarser stand-in's envelope.
    let frame = frame_of(
        fx,
        &scene,
        &fx.cache([wanted[0], wanted[1].parent().unwrap()]),
    );
    assert_eq!(frame.draws[1].tile.key().level.get(), 1);
    assert!(!frame.draws[0].continues);
    // But the stand-in continues to the next exact tile's first sample.
    let frame = frame_of(
        fx,
        &scene,
        &fx.cache([wanted[0].parent().unwrap(), wanted[1]]),
    );
    assert!(frame.draws[0].continues);
    assert_eq!(frame.draws[0].channels[0].next, Some([first, first]));

    // Zoomed out, an envelope runs on to the next tile's first bucket.
    let n = 300_000u64;
    let mut rng = Rng::new(9);
    let long = Fixture::new(
        vec![channel("long", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..1)],
        vec![i16_rows(vec![walk(
            &mut rng, n as usize, 50.0, -3000.0, 3000.0,
        )])],
    );
    let scene = long.scene(ChannelId(0), 120.0..140.0, [400, 100]);
    let frame = frame_of(&long, &scene, &exact_cache(&long, &scene));
    assert_eq!(frame.draws.len(), 2);
    let draw = &frame.draws[0];
    assert_eq!(draw.tile.key().level.get(), 1);
    assert!(draw.continues);
    assert_eq!(*draw.items.end(), TILE_LEN as u32);
    let TileData::I16(d) = frame.draws[1].tile.data() else {
        panic!()
    };
    let (min, max) = (Native::Int(i32::from(d[0])), Native::Int(i32::from(d[1])));
    assert_eq!(draw.channels[0].next, Some([min, max]));
    assert!(!frame.draws[1].continues);
}

#[test]
fn a_stand_in_straddling_the_next_tile_ends_there() {
    // Level 5 buckets are 8,192 samples: a level-5 stand-in for level-0 tile 0 ends halfway
    // through a bucket, so its envelope can't run on to tile 1's first sample.
    let n = 9_000_000u64;
    let fx = Fixture::new(
        vec![channel("long", 0, 1.0, 0.0)],
        vec![grid(1000.0, n, vec![0..n], SampleType::I16, 0..1)],
        vec![i16_rows(vec![(0..n).map(|i| (i % 1000) as f64).collect()])],
    );
    let tile_seconds = TILE_LEN as f64 / 1000.0;
    let stand_in_runs_on = |first_tile: f64| {
        let time = (first_tile + 0.5) * tile_seconds..(first_tile + 1.5) * tile_seconds;
        let scene = fx.scene(ChannelId(0), time, [400, 100]);
        let wanted = tiles_for_view(&fx.recording, &view(&scene));
        assert_eq!(wanted.len(), 2);
        let stand_in = (0..5).fold(wanted[0], |k, _| k.parent().unwrap());
        assert_eq!(stand_in.level.bucket_len(), 8192);
        let cache = fx.cache([stand_in, wanted[1]]);
        let frame = Frame::new(
            &fx.recording,
            &scene,
            &query(&fx.recording, &view(&scene), &cache),
        );
        assert_eq!(frame.draws.len(), 2);
        frame.draws[0].continues
    };
    assert!(!stand_in_runs_on(0.0));
    // Standing in for tile 1, it ends on a bucket boundary and does run on.
    assert!(stand_in_runs_on(1.0));
}

fn exact_cache(fx: &Fixture, scene: &Scene) -> kymo_core::tile::TileCache {
    fx.cache(tiles_for_view(&fx.recording, &view(scene)))
}

#[test]
fn the_scene_follows_the_view() {
    let fx = fixture();
    let mut state = ViewState::new(&fx.recording);
    state.set_rows(4);
    state.apply(kymo::action::Action::ChannelDown, None);
    let scene = Scene::from_view(&state, [800, 400], [1.0, 0.5, 0.0, 1.0]);
    assert_eq!(scene.time, 0.0..20.0);
    let channels: Vec<_> = scene.rows.iter().map(|r| r.channel.0).collect();
    assert_eq!(channels, [1, 2, 3, 4]);
    let centers: Vec<_> = scene.rows.iter().map(|r| r.center).collect();
    assert_eq!(centers, [50.0, 150.0, 250.0, 350.0]);
    // As in MNE, ±amplitude spans a row: misc channels default to 1 unit.
    let r = &scene.rows[0];
    assert_eq!(r.height, 100.0);
    assert_eq!(r.px_per_unit, 50.0);
    assert_eq!(r.baseline, 0.0);
    assert_eq!(scene.hatch, [0.25, 0.125, 0.0, 0.25]);
}

/// A stim channel with a trigger mask: its row clips the trace, and its codes are drawn as
/// they are, whatever the channel's gain and offset. Other rows don't clip.
#[test]
fn stim_rows_clip_and_draw_codes() {
    let len = 60_000u64;
    let eeg = channel("A1", 0, 0.5, 100.0);
    let mut stim = channel("Status", 0, 2.0, 1e6);
    stim.kind = ChannelKind::Stim;
    stim.trigger_mask = Some(0xffff);
    let codes: Vec<f64> = (0..len).map(|i| ((i / 100) % 4 * 85) as f64).collect();
    let raw: Vec<f64> = codes.iter().map(|&c| f64::from(1 << 20) + c).collect();
    let fx = Fixture::new(
        vec![eeg, stim],
        vec![grid(100.0, len, vec![0..len], SampleType::I32, 0..2)],
        vec![i32_rows(vec![vec![0.0; len as usize], raw])],
    );
    let state = ViewState::new(&fx.recording);
    let scene = Scene::from_view(&state, [800, 400], [1.0; 4]);
    assert_eq!(
        scene.rows.iter().map(|r| r.clip).collect::<Vec<_>>(),
        [false, true]
    );
    for zoom_out in [false, true] {
        let mut state = ViewState::new(&fx.recording);
        if zoom_out {
            // Ten minutes: buckets from the pyramid instead of samples.
            for _ in 0..10 {
                state.apply(kymo::action::Action::ZoomOut, None);
            }
        }
        let scene = Scene::from_view(&state, [800, 400], [1.0; 4]);
        let cache = exact_cache(&fx, &scene);
        let frame = Frame::new(
            &fx.recording,
            &scene,
            &query(&fx.recording, &view(&scene), &cache),
        );
        let (mut seen_eeg, mut seen_stim) = (false, false);
        assert!(
            frame
                .draws
                .iter()
                .all(|d| (d.tile.key().level > Level::RAW) == zoom_out)
        );
        for draw in &frame.draws {
            for c in &draw.channels {
                let row = &scene.rows[c.row as usize];
                if row.channel == ChannelId(1) {
                    seen_stim = true;
                    let half = row.height / 2.0;
                    let [top, bottom] = c.clip.unwrap();
                    assert!((f64::from(top) - (row.center - half)).abs() < 1e-3);
                    assert!(f64::from(bottom) < row.center + half);
                    assert!(f64::from(bottom) > row.center + half - 0.01);
                    assert_eq!(
                        c.coefficients,
                        Coefficients::new(SampleType::I32, 1.0, 0.0, row)
                    );
                    // Code 0 on the middle of the row; code 1 at its top, as ±1 spans it.
                    let y = |code| f64::from(c.coefficients.y(Native::Int(code)).unwrap());
                    assert!((y(0) - row.center).abs() < 1e-3);
                    assert!((y(1) - (row.center - half)).abs() < 1e-3);
                    // The tile holds codes, not raw values.
                    let TileData::I32(values) = draw.tile.data() else {
                        panic!("int32 tiles");
                    };
                    let per = values.len() / draw.tile.channels();
                    let mine = &values[c.row as usize * per..][..per];
                    assert!(
                        mine.iter().all(|v| [0, 85, 170, 255].contains(v)),
                        "{zoom_out}"
                    );
                } else {
                    seen_eeg = true;
                    assert_eq!(c.clip, None);
                    assert_eq!(
                        c.coefficients,
                        Coefficients::new(SampleType::I32, 0.5, 100.0, row)
                    );
                }
            }
        }
        assert!(seen_eeg && seen_stim, "{zoom_out}");
    }
}

#[test]
fn opengl_devices_are_refused_with_a_clear_error() {
    let error = check_support(&wgpu::Limits::downlevel_webgl2_defaults()).unwrap_err();
    assert!(matches!(
        error,
        RenderError::NoVertexStorage { available: 0, .. }
    ));
    assert!(error.to_string().contains("OpenGL"));
    check_support(&wgpu::Limits::default()).unwrap();
    check_support(&wgpu::Limits::downlevel_defaults()).unwrap();
}
