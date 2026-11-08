//! The automatic amplitude scale: against brute force over the samples, and what it
//! follows (the signal's amplitude) and ignores (artifacts, offsets, sign, bad channels).

use std::f64::consts::TAU;
use std::sync::OnceLock;

use crate::common::{Fixture, Rng, channel, f32_rows, grid, i16_rows, walk};
use kymo::scale::{PERCENTILE, fit};
use kymo::view::{KINDS, ViewState, kind_index};
use kymo_core::tile::{Level, query, samples_in, tiles_for_view};
use kymo_core::{Channel, ChannelId, ChannelKind, SampleType, Unit};
use proptest::prelude::*;

fn typed(mut ch: Channel, kind: ChannelKind, unit: &str, to_si: Option<f64>) -> Channel {
    ch.kind = kind;
    ch.unit = Unit {
        label: unit.into(),
        to_si,
    };
    ch
}

/// What `view` fits in a trace area `width` pixels wide, with every tile it wants cached.
fn fitted(fx: &Fixture, view: &ViewState, width: u32) -> [Option<f64>; KINDS.len()] {
    let tiles = view.tile_view(width);
    let cache = fx.cache(tiles_for_view(&fx.recording, &tiles));
    fit(
        &fx.recording,
        view,
        &query(&fx.recording, &tiles, &cache),
        width,
    )
}

const FAST: u64 = 30_000;
const SLOW: u64 = 7_500;

/// 34 int16 channels at 1000 Hz with a gap (two channel blocks: EEG in µV, EOG in mV across
/// the block boundary, an EMG channel without an SI unit, a stim channel), and two float
/// channels at 250 Hz with NaNs and a negative gain (EEG and misc). Offsets are large.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let mut rng = Rng::new(3);
        let fast: Vec<Vec<f64>> = (0..34)
            .map(|_| walk(&mut rng, FAST as usize, 400.0, -30000.0, 30000.0))
            .collect();
        let mut slow: Vec<Vec<f64>> = (0..2)
            .map(|_| walk(&mut rng, SLOW as usize, 0.5, -100.0, 100.0))
            .collect();
        for row in &mut slow {
            for i in (0..SLOW as usize).step_by(331) {
                row[i] = f64::NAN;
            }
        }
        let mut channels: Vec<Channel> = (0..34)
            .map(|i| {
                let gain = 0.1 + f64::from(i) * 0.01;
                let ch = channel(&format!("c{i}"), 0, gain, 1000.0 * f64::from(i));
                match i {
                    0..28 => typed(ch, ChannelKind::Eeg, "uV", Some(1e-6)),
                    28..32 => typed(ch, ChannelKind::Eog, "mV", Some(1e-3)),
                    32 => typed(ch, ChannelKind::Emg, "au", None),
                    _ => typed(ch, ChannelKind::Stim, "", None),
                }
            })
            .collect();
        channels.push(typed(
            channel("float", 1, 1.5, -2.0e4),
            ChannelKind::Eeg,
            "uV",
            Some(1e-6),
        ));
        channels.push(typed(
            channel("flipped", 1, -0.5, 100.0),
            ChannelKind::Misc,
            "V",
            Some(1.0),
        ));
        Fixture::new(
            channels,
            vec![
                grid(
                    1000.0,
                    FAST,
                    vec![0..12_000, 14_000..FAST],
                    SampleType::I16,
                    0..34,
                ),
                grid(250.0, SLOW, vec![0..SLOW], SampleType::F32, 34..36),
            ],
            vec![i16_rows(fast), f32_rows(slow)],
        )
    })
}

/// Nearest-rank quantile, as the scale takes it.
fn quantile(mut values: Vec<f64>, q: f64) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let rank = ((values.len().checked_sub(1)?) as f64 * q).round() as usize;
    Some(values[rank])
}

/// The scale from the samples: per good, non-stim channel on screen, what its level draws
/// in the window (samples, or the extremes of each bucket with a present sample), gathered
/// into the pixel columns they start in; its center (the median of the columns'
/// midpoints), and each column's farther end from it; per type, the [`PERCENTILE`] of all
/// of them, flat channels left out.
fn brute(fx: &Fixture, view: &ViewState, width: u32) -> [Option<f64>; KINDS.len()] {
    let tiles = view.tile_view(width);
    let mut deviations: [Vec<f64>; KINDS.len()] = Default::default();
    for &id in view.visible_channels() {
        if view.is_bad(id) || view.kind_of(id) == ChannelKind::Stim {
            continue;
        }
        let grid = fx.grid_of(id);
        let level = kymo_core::tile::level_for(grid, &tiles);
        let samples = samples_in(grid, &tiles.time);
        let to_si = view.to_si(id);
        // (first sample, low, high)
        let items: Vec<(u64, f64, f64)> = if level == Level::RAW {
            samples
                .filter_map(|i| fx.physical(id, i).map(|v| (i, v * to_si, v * to_si)))
                .collect()
        } else if samples.is_empty() {
            Vec::new()
        } else {
            let b = level.bucket_len();
            (samples.start / b..=(samples.end - 1) / b)
                .filter_map(|k| {
                    let values: Vec<f64> = (k * b..((k + 1) * b).min(grid.len()))
                        .filter_map(|i| fx.physical(id, i))
                        .collect();
                    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    (!values.is_empty()).then_some((k * b, lo * to_si, hi * to_si))
                })
                .collect()
        };
        let time = &tiles.time;
        let mut columns = std::collections::BTreeMap::<i64, (f64, f64)>::new();
        for (first, lo, hi) in items {
            let x = ((grid.time_of(first) - time.start) / (time.end - time.start)
                * f64::from(width))
            .floor() as i64;
            let entry = columns
                .entry(x.clamp(0, i64::from(width) - 1))
                .or_insert((lo, hi));
            *entry = (entry.0.min(lo), entry.1.max(hi));
        }
        let items: Vec<(f64, f64)> = columns.into_values().collect();
        let Some(center) = quantile(items.iter().map(|&(lo, hi)| (lo + hi) / 2.0).collect(), 0.5)
        else {
            continue;
        };
        let own: Vec<f64> = items
            .iter()
            .map(|&(lo, hi)| (hi - center).max(center - lo))
            .collect();
        if own.iter().any(|&d| d != 0.0) {
            deviations[kind_index(view.kind_of(id))].extend(own);
        }
    }
    deviations.map(|d| quantile(d, PERCENTILE).filter(|&a| a > 0.0))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_scale_matches_brute_force_over_the_samples(
        span_exp in -2.0f64..0.0,
        start_frac in 0.0f64..1.0,
        width in 50u32..2000,
        bad in proptest::collection::vec(any::<bool>(), 36),
        hidden in 0usize..36,
        rows in 10usize..40,
    ) {
        let fx = fixture();
        let duration = fx.recording.duration();
        let span = duration * 10f64.powf(span_exp);
        let mut view = ViewState::with_default_span(&fx.recording, span);
        view.pan(start_frac * (duration - span));
        view.set_rows(rows);
        for (i, &bad) in bad.iter().enumerate() {
            if bad && i % 3 == 0 {
                view.toggle_bad(ChannelId(i as u32));
            }
        }
        view.set_shown(ChannelId(hidden as u32), false);
        prop_assert_eq!(fitted(fx, &view, width), brute(fx, &view, width));
    }
}

/// One second of a sine of `amplitude` (native units) at 10 Hz, sampled at 1000 Hz, for
/// `seconds`.
fn sine(amplitude: f64, seconds: usize) -> Vec<f64> {
    (0..seconds * 1000)
        .map(|i| (amplitude * (TAU * 10.0 * i as f64 / 1000.0).sin()).round())
        .collect()
}

/// 30 s at 1000 Hz: four EEG channels with a 20 µV sine, the third with a 0.2 s artifact
/// of 15 mV at 5 s; a bad EEG channel 100 times larger; an EOG channel with a 0.6 mV sine;
/// an EMG channel with a slow 100 µV triangle (a period of 1997 samples, so its columns'
/// deviations spread evenly); a stim channel; nine
/// flat EMG channels, as unused inputs are; a flat misc channel. EEG offsets and gain
/// signs as given.
fn sines(offsets: [f64; 4], gains: [f64; 4]) -> Fixture {
    let mut rows: Vec<Vec<f64>> = (0..4).map(|_| sine(40.0, 30)).collect();
    for v in &mut rows[2][5000..5200] {
        *v = 30_000.0;
    }
    rows.push(sine(4000.0, 30));
    rows.push(sine(60.0, 30));
    rows.push(
        (0..30_000)
            .map(|i| (2.0 * (f64::from(i % 1997) - 998.5).abs() - 998.5) * 100.0 / 998.5)
            .collect(),
    );
    rows.push((0..30_000).map(|i| f64::from(i % 1000 < 5)).collect());
    rows.extend((0..10).map(|_| vec![7.0; 30_000]));
    let mut channels: Vec<Channel> = (0..4)
        .map(|i| {
            typed(
                channel(&format!("EEG {i}"), 0, gains[i] * 0.5, offsets[i]),
                ChannelKind::Eeg,
                "uV",
                Some(1e-6),
            )
        })
        .collect();
    channels.push(typed(
        channel("EEG bad", 0, 0.5, 0.0),
        ChannelKind::Eeg,
        "uV",
        Some(1e-6),
    ));
    channels.push(typed(
        channel("EOG", 0, 0.01, 0.0),
        ChannelKind::Eog,
        "mV",
        Some(1e-3),
    ));
    channels.push(typed(
        channel("EMG", 0, 1.0, 0.0),
        ChannelKind::Emg,
        "uV",
        Some(1e-6),
    ));
    channels.push(typed(
        channel("Status", 0, 1.0, 0.0),
        ChannelKind::Stim,
        "",
        None,
    ));
    for i in 0..9 {
        channels.push(typed(
            channel(&format!("EMG {i}"), 0, 1.0, 0.0),
            ChannelKind::Emg,
            "uV",
            Some(1e-6),
        ));
    }
    channels.push(typed(
        channel("Misc", 0, 1.0, 0.0),
        ChannelKind::Misc,
        "",
        None,
    ));
    Fixture::new(
        channels,
        vec![grid(
            1000.0,
            30_000,
            vec![0..30_000],
            SampleType::I16,
            0..18,
        )],
        vec![i16_rows(rows)],
    )
}

fn view_of(fx: &Fixture) -> ViewState {
    let mut view = ViewState::new(&fx.recording);
    view.toggle_bad(ChannelId(4));
    view
}

fn amplitude(fitted: &[Option<f64>; KINDS.len()], kind: ChannelKind) -> Option<f64> {
    fitted[kind_index(kind)]
}

#[test]
fn the_scale_follows_the_amplitude_not_an_artifact() {
    let fx = sines([0.0; 4], [1.0; 4]);
    let view = view_of(&fx);
    // Samples (20 per pixel) and envelopes (200 per pixel) alike.
    for width in [1000, 100] {
        let fitted = fitted(&fx, &view, width);
        let eeg = amplitude(&fitted, ChannelKind::Eeg).unwrap();
        assert!((19e-6..=21e-6).contains(&eeg), "{width} px: EEG {eeg}");
        let eog = amplitude(&fitted, ChannelKind::Eog).unwrap();
        assert!((0.57e-3..=0.63e-3).contains(&eog), "{width} px: EOG {eog}");
        // Flat channels say nothing: EMG follows its live channel, and misc and stim keep
        // their scale.
        let emg = amplitude(&fitted, ChannelKind::Emg).unwrap();
        assert!((97e-6..=103e-6).contains(&emg), "{width} px: EMG {emg}");
        assert_eq!(amplitude(&fitted, ChannelKind::Misc), None);
        assert_eq!(amplitude(&fitted, ChannelKind::Stim), None);
    }
}

#[test]
fn offsets_and_flipped_signs_change_nothing() {
    let plain = sines([0.0; 4], [1.0; 4]);
    let plain = fitted(&plain, &view_of(&plain), 1000);
    let moved = sines([2.0e4, -5.0e3, 1.0e5, 0.0], [-1.0, 1.0, -1.0, 1.0]);
    let moved = fitted(&moved, &view_of(&moved), 1000);
    let (a, b) = (
        amplitude(&plain, ChannelKind::Eeg).unwrap(),
        amplitude(&moved, ChannelKind::Eeg).unwrap(),
    );
    assert!((a - b).abs() <= a * 1e-9, "{a} against {b}");
}

/// Bad and hidden channels don't count, so their type's amplitude comes from the rest.
#[test]
fn bad_and_hidden_channels_are_left_out() {
    let fx = sines([0.0; 4], [1.0; 4]);
    let mut view = ViewState::new(&fx.recording);
    let with_bad = amplitude(&fitted(&fx, &view, 1000), ChannelKind::Eeg).unwrap();
    assert!(
        with_bad > 1e-3,
        "the bad channel counts while it is good: {with_bad}"
    );
    view.set_shown(ChannelId(4), false);
    let hidden = amplitude(&fitted(&fx, &view, 1000), ChannelKind::Eeg).unwrap();
    assert!(hidden < 21e-6, "{hidden}");
    // Without any good EEG channel on screen, EEG keeps its scale.
    for i in 0..4 {
        view.toggle_bad(ChannelId(i));
    }
    assert_eq!(amplitude(&fitted(&fx, &view, 1000), ChannelKind::Eeg), None);
}
