//! Measurements: ends snapped to the nearest samples, Δt exact to the sample, and the
//! label's texts.

use std::sync::Arc;

use kymo::measure::{Measurement, Span, interval_text, nearest_sample, values_text};
use kymo::ui::readout::Reading;
use kymo_core::{ChannelId, Grid, Piece, SampleType, TimeTable, Timebase};
use proptest::prelude::*;

fn regular(rate: f64, start: f64, len: u64) -> Grid {
    Grid::continuous(
        Timebase::Regular { rate, start },
        len,
        SampleType::I16,
        vec![ChannelId(0)],
    )
    .unwrap()
}

fn measure(grid: &Grid, press: f64, reach: f64) -> Span {
    Measurement {
        channel: ChannelId(0),
        press,
        reach,
    }
    .span_on(grid)
}

proptest! {
    /// On a regular grid the ends are the samples nearest to the times (the first or
    /// last beyond them), and Δt is their distance in samples over the rate, either way.
    #[test]
    fn ends_snap_to_the_nearest_samples(
        rate in prop_oneof![Just(256.0), Just(1000.0), Just(2048.0), 0.5f64..5000.0],
        start in prop_oneof![Just(0.0), 0.0f64..100.0],
        len in 1u64..1_000_000,
        press in -10.0f64..1000.0,
        reach in -10.0f64..1000.0,
    ) {
        let grid = regular(rate, start, len);
        let span = measure(&grid, press, reach);
        prop_assert_eq!(span, measure(&grid, reach, press));
        let first = nearest_sample(&grid, press.min(reach));
        let last = nearest_sample(&grid, press.max(reach));
        prop_assert_eq!(span.samples, Some(last - first));
        prop_assert_eq!(span.dt, (last - first) as f64 / rate);
        prop_assert_eq!(span.start, grid.time_of(first));
        prop_assert_eq!(span.end, grid.time_of(last));
        // Nearest: no other sample is closer to the time.
        for (time, s) in [(press.min(reach), first), (press.max(reach), last)] {
            let off = |n: u64| (grid.time_of(n) - time).abs();
            prop_assert!(s < len);
            if s > 0 {
                prop_assert!(off(s) <= off(s - 1) + 1e-9, "{} vs {}", s, s - 1);
            }
            if s + 1 < len {
                prop_assert!(off(s) <= off(s + 1) + 1e-9, "{} vs {}", s, s + 1);
            }
        }
    }
}

#[test]
fn a_known_interval() {
    // 320 samples at 256 Hz from 2 s on: 1.25 s, 0.8 Hz.
    let grid = regular(256.0, 0.0, 256 * 60);
    // A little off the samples, as a pointer is.
    let span = measure(&grid, 3.251, 2.0013);
    assert_eq!(span.start, 2.0);
    assert_eq!(span.end, 3.25);
    assert_eq!(span.dt, 1.25);
    assert_eq!(span.samples, Some(320));
    assert_eq!(
        interval_text(&span),
        "\u{394}t 1.250 s \u{b7} 320 samples \u{b7} 0.8000 Hz"
    );
    // Both ends on one sample: no frequency.
    let span = measure(&grid, 2.0001, 2.0002);
    assert_eq!(interval_text(&span), "\u{394}t 0.000 s \u{b7} 0 samples");
    let span = measure(&grid, 2.0, 2.0 + 1.0 / 256.0);
    assert_eq!(
        interval_text(&span),
        "\u{394}t 0.004 s \u{b7} 1 sample \u{b7} 256.0 Hz"
    );
}

#[test]
fn segmented_grids_measure_across_their_pieces() {
    // 100 samples at 100 Hz from 0 s, then 100 more from 1.5 s.
    let grid = Grid::continuous(
        Timebase::Segmented {
            rate: 100.0,
            pieces: vec![
                Piece {
                    first: 0,
                    start: 0.0,
                    rate: 100.0,
                },
                Piece {
                    first: 100,
                    start: 1.5,
                    rate: 100.0,
                },
            ],
        },
        200,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    let within = measure(&grid, 1.5, 1.75);
    assert_eq!((within.samples, within.dt), (Some(25), 0.25));
    let across = measure(&grid, 0.5, 1.6);
    assert_eq!(across.samples, Some(60));
    assert!((across.dt - 1.1).abs() < 1e-12, "{across:?}");
}

#[test]
fn irregular_grids_measure_the_pointer_times() {
    let times = TimeTable::new(&[0.0, 0.3, 0.35, 1.0], 1.1).unwrap();
    let grid = Grid::continuous(
        Timebase::Irregular {
            rate: 4.0,
            hold: false,
            times: Arc::new(times),
        },
        4,
        SampleType::F32,
        vec![ChannelId(0)],
    )
    .unwrap();
    let span = measure(&grid, 0.9, 0.4);
    assert_eq!((span.start, span.end, span.samples), (0.4, 0.9, None));
    assert_eq!(interval_text(&span), "\u{394}t 0.5 s \u{b7} 2.000 Hz");
}

#[test]
fn values_and_their_difference() {
    let v = |x| Some(Reading::Value(x));
    assert_eq!(
        values_text("Fz", "\u{b5}V", v(12.3), v(-4.1)),
        "Fz: 12.30 \u{b5}V \u{2192} \u{2212}4.100 \u{b5}V \u{b7} \u{394} \u{2212}16.40 \u{b5}V"
    );
    // An envelope has no difference, nor has a missing end.
    let range = Some(Reading::Range {
        min: -1.0,
        max: 2.0,
    });
    assert_eq!(
        values_text("Fz", "\u{b5}V", range, v(1.0)),
        "Fz: \u{2212}1.000 \u{2026} 2.000 \u{b5}V \u{2192} 1.000 \u{b5}V"
    );
    assert_eq!(
        values_text("STI", "", v(5.0), None),
        "STI: 5.000 \u{2192} \u{2013}"
    );
}
