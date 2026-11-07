//! The value under the pointer against brute force over the samples.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::OnceLock;

use common::{Fixture, Rng, channel, f32_rows, grid, i16_rows, walk};
use kymo::ui::readout::{Reading, read};
use kymo_core::tile::{
    Level, SignalVersion, TileCache, TileKey, View, level_for, max_level, tiles_for_view,
};
use kymo_core::{ChannelId, SampleType};

const FAST: u64 = 150_000;
const SLOW: u64 = 40_000;

/// 34 int16 channels at 1000 Hz (two channel blocks) with a gap, and two float channels at
/// 250 Hz with NaNs, one with a negative gain.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let mut rng = Rng::new(7);
        let fast: Vec<Vec<f64>> = (0..34)
            .map(|_| walk(&mut rng, FAST as usize, 300.0, -30000.0, 30000.0))
            .collect();
        let mut slow: Vec<Vec<f64>> = (0..2)
            .map(|_| walk(&mut rng, SLOW as usize, 0.5, -100.0, 100.0))
            .collect();
        for row in &mut slow {
            for i in (0..SLOW as usize).step_by(997) {
                row[i] = f64::NAN;
            }
        }
        let mut channels: Vec<_> = (0..34)
            .map(|i| channel(&format!("f{i}"), 0, 0.25 + f64::from(i) * 0.01, 10.0))
            .collect();
        channels.push(channel("float", 1, 1.5, -2.0));
        channels.push(channel("flipped", 1, -0.5, 100.0));
        Fixture::new(
            channels,
            vec![
                grid(
                    1000.0,
                    FAST,
                    vec![0..60_000, 70_000..FAST],
                    SampleType::I16,
                    0..34,
                ),
                grid(250.0, SLOW, vec![0..SLOW], SampleType::F32, 34..36),
            ],
            vec![i16_rows(fast), f32_rows(slow)],
        )
    })
}

fn view(time: std::ops::Range<f64>, width_px: u32, channels: &[ChannelId]) -> View {
    View {
        version: SignalVersion::RAW,
        time,
        width_px,
        channels: channels.to_vec(),
    }
}

/// What the channel shows at `time`, from the samples: the nearest sample at level 0
/// (the earlier one on a tie), else the extremes of the bucket of `level` holding `time`.
fn expected(fx: &Fixture, id: ChannelId, level: Level, time: f64) -> Option<Reading> {
    let grid = fx.grid_of(id);
    let last = grid.len() - 1;
    if level == Level::RAW {
        if time < grid.time_of(0) || time > grid.time_of(last) {
            return None;
        }
        let nearest = (0..=last)
            .min_by(|&a, &b| {
                let (da, db) = (
                    (grid.time_of(a) - time).abs(),
                    (grid.time_of(b) - time).abs(),
                );
                da.total_cmp(&db)
            })
            .unwrap();
        return fx.physical(id, nearest).map(Reading::Value);
    }
    let s = grid.sample_at(time);
    if s > last || time < grid.time_of(0) {
        return None;
    }
    let bucket = level.bucket_len();
    let first = s / bucket * bucket;
    let values: Vec<f64> = (first..(first + bucket).min(grid.len()))
        .filter_map(|i| fx.physical(id, i))
        .collect();
    (!values.is_empty()).then(|| Reading::Range {
        min: values.iter().copied().fold(f64::INFINITY, f64::min),
        max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    })
}

#[test]
fn readings_match_the_samples_at_every_zoom() {
    let fx = fixture();
    let mut rng = Rng::new(11);
    let duration = fx.recording.duration();
    let mut levels = std::collections::BTreeSet::new();
    for case in 0..400 {
        let id = ChannelId((rng.next() % 36) as u32);
        let span = (duration * 10f64.powf(-4.0 * rng.uniform())).max(0.01);
        let start = rng.uniform() * (duration - span);
        let width = 200 + (rng.next() % 2000) as u32;
        let v = view(start..start + span, width, &[id]);
        // As in the viewer, finer tiles are cached too (the prefetch's level below, and
        // level 0 from earlier views); the reading still comes from the view's level.
        let finer = |w: u32| tiles_for_view(&fx.recording, &view(v.time.clone(), w, &[id]));
        let cache = fx.cache(
            tiles_for_view(&fx.recording, &v)
                .into_iter()
                .chain(finer(width * 4))
                .chain(finer(1_000_000)),
        );
        let level = level_for(fx.grid_of(id), &v);
        levels.insert(level);
        // Times on screen, a sample's own time, and a little outside the recording.
        let time = match case % 4 {
            0 => {
                let grid = fx.grid_of(id);
                let first = grid.sample_at(start) + 1;
                let n = grid.sample_at(start + span).saturating_sub(first).max(1);
                grid.time_of(first + rng.next() % n).min(start + span)
            }
            3 if case % 40 == 3 => duration + 1.0,
            _ => start + rng.uniform() * span,
        };
        let got = read(&fx.recording, &cache, &v, id, time);
        let want = expected(fx, id, level, time);
        assert_eq!(
            got, want,
            "channel {id:?} at {time} s in {:?}, level {level:?}",
            v.time
        );
    }
    assert!(levels.len() >= 3, "levels {levels:?}");
}

#[test]
fn a_coarser_tile_stands_in_and_nothing_cached_reads_nothing() {
    let fx = fixture();
    let id = ChannelId(3);
    let grid = fx.grid_of(id);
    let v = view(10.0..12.0, 1000, &[id]);
    assert_eq!(level_for(grid, &v), Level::RAW);
    let time = 11.0;
    assert_eq!(
        read(&fx.recording, &TileCache::new(1 << 30), &v, id, time),
        None
    );

    // Only the top level is cached: its bucket is what the trace area shows.
    let top = max_level(grid.len());
    let key = TileKey {
        level: top,
        time: 0,
        ..tiles_for_view(&fx.recording, &v)[0]
    };
    let cache = fx.cache([key]);
    let got = read(&fx.recording, &cache, &v, id, time);
    assert_eq!(got, expected(fx, id, top, time));
    assert!(matches!(got, Some(Reading::Range { .. })), "{got:?}");
}

#[test]
fn gaps_and_nans_read_nothing() {
    let fx = fixture();
    let gap = ChannelId(0);
    let v = view(64.0..66.0, 1000, &[gap]);
    let cache = fx.cache(tiles_for_view(&fx.recording, &v));
    assert_eq!(read(&fx.recording, &cache, &v, gap, 65.0), None);

    let float = ChannelId(34);
    let grid = fx.grid_of(float);
    let nan = grid.time_of(997 * 5);
    let v = view(nan - 0.5..nan + 0.5, 1000, &[float]);
    let cache = fx.cache(tiles_for_view(&fx.recording, &v));
    assert_eq!(read(&fx.recording, &cache, &v, float, nan), None);
    assert!(read(&fx.recording, &cache, &v, float, nan + 1.0 / 250.0).is_some());
}
