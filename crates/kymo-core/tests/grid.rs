// Test code: panicking on unexpected results is the point, and one-segment grids are cases.
#![allow(clippy::unwrap_used, clippy::single_range_in_vec_init)]

mod common;

use std::ops::Range;

use kymo_core::{ChannelId, Grid, ModelError, SampleType, Timebase};
use proptest::prelude::*;

fn regular(rate: f64, start: f64) -> Timebase {
    Timebase::Regular { rate, start }
}

fn grid(len: u64, segments: Vec<Range<u64>>) -> Result<Grid, ModelError> {
    Grid::new(
        regular(100.0, 0.0),
        len,
        segments,
        SampleType::I16,
        vec![ChannelId(0)],
    )
}

#[test]
fn keeps_sorted_disjoint_segments() {
    let g = grid(20, vec![0..5, 7..9, 12..20]).unwrap();
    assert_eq!(g.segments(), [0..5, 7..9, 12..20]);
    assert_eq!(g.len(), 20);
}

#[test]
fn merges_touching_segments() {
    let g = grid(20, vec![0..5, 5..8, 10..12, 12..13]).unwrap();
    assert_eq!(g.segments(), [0..8, 10..13]);
}

#[test]
fn rejects_unsorted_segments() {
    assert_eq!(
        grid(20, vec![5..8, 0..3]),
        Err(ModelError::UnsortedSegments {
            first: 5..8,
            second: 0..3
        })
    );
}

#[test]
fn rejects_overlapping_segments() {
    assert_eq!(
        grid(20, vec![0..5, 4..8]),
        Err(ModelError::UnsortedSegments {
            first: 0..5,
            second: 4..8
        })
    );
}

#[test]
fn rejects_empty_segment() {
    assert_eq!(grid(20, vec![3..3]), Err(ModelError::EmptySegment(3..3)));
}

#[test]
fn rejects_segment_past_end() {
    assert_eq!(
        grid(10, vec![5..11]),
        Err(ModelError::SegmentPastEnd {
            segment: 5..11,
            len: 10
        })
    );
}

#[test]
fn rejects_invalid_time_base() {
    for rate in [0.0, -250.0, f64::NAN, f64::INFINITY] {
        let result = Grid::continuous(regular(rate, 0.0), 10, SampleType::I16, vec![ChannelId(0)]);
        assert!(matches!(result, Err(ModelError::InvalidRate(_))), "{rate}");
    }
    let result = Grid::continuous(
        regular(100.0, f64::NAN),
        10,
        SampleType::I16,
        vec![ChannelId(0)],
    );
    assert!(matches!(result, Err(ModelError::InvalidStart(_))));
}

#[test]
fn rejects_empty_or_duplicate_channels() {
    let tb = regular(100.0, 0.0);
    assert_eq!(
        Grid::continuous(tb, 10, SampleType::I16, vec![]),
        Err(ModelError::EmptyGrid)
    );
    assert_eq!(
        Grid::continuous(
            tb,
            10,
            SampleType::I16,
            vec![ChannelId(3), ChannelId(1), ChannelId(3)]
        ),
        Err(ModelError::DuplicateChannel(ChannelId(3)))
    );
}

#[test]
fn continuous_grid_is_one_segment() {
    let tb = regular(100.0, 0.0);
    let g = Grid::continuous(tb, 10, SampleType::F32, vec![ChannelId(0)]).unwrap();
    assert_eq!(g.segments(), [0..10]);
    let empty = Grid::continuous(tb, 0, SampleType::F32, vec![ChannelId(0)]).unwrap();
    assert!(empty.segments().is_empty());
    assert!(empty.is_empty());
}

#[test]
fn time_of_uses_rate_and_start() {
    let g = Grid::continuous(
        regular(256.0, 0.5),
        1000,
        SampleType::I16,
        vec![ChannelId(0)],
    )
    .unwrap();
    assert_eq!(g.rate(), 256.0);
    assert_eq!(g.time_of(0), 0.5);
    assert_eq!(g.time_of(512), 2.5);
}

#[test]
fn sample_at_floors_and_clamps() {
    let g = Grid::continuous(
        regular(100.0, 0.5),
        1000,
        SampleType::I16,
        vec![ChannelId(0)],
    )
    .unwrap();
    assert_eq!(g.sample_at(0.5), 0);
    assert_eq!(g.sample_at(0.515), 1);
    assert_eq!(g.sample_at(0.519_999), 1);
    assert_eq!(g.sample_at(0.0), 0);
    assert_eq!(g.sample_at(-1e9), 0);
    assert_eq!(g.sample_at(f64::NAN), 0);
    assert_eq!(g.sample_at(f64::NEG_INFINITY), 0);
    assert_eq!(g.sample_at(1e300), 1000);
    assert_eq!(g.sample_at(f64::INFINITY), 1000);
}

proptest! {
    #[test]
    fn present_in_matches_brute_force(
        (len, segments, range) in common::len_and_segments(300)
            .prop_flat_map(|(len, s)| (Just(len), Just(s), common::sub_range(len)))
    ) {
        let g = grid(len, segments.clone()).unwrap();
        let parts: Vec<Range<u64>> = g.present_in(range.clone()).collect();
        for part in &parts {
            prop_assert!(part.start < part.end, "empty part {part:?}");
        }
        for pair in parts.windows(2) {
            prop_assert!(pair[0].end < pair[1].start, "parts {pair:?} touch or overlap");
        }
        for i in 0..len {
            let expected = range.contains(&i) && common::is_present(&segments, i);
            let found = parts.iter().any(|p| p.contains(&i));
            prop_assert_eq!(found, expected, "sample {}", i);
        }
    }

    #[test]
    fn time_round_trips_to_sample(
        rate in prop_oneof![
            Just(256.0), Just(512.0), Just(1000.0), Just(2048.0), Just(1000.0 / 3.0),
            0.1f64..100_000.0
        ],
        start in -10.0f64..10.0,
        sample in 0u64..(1 << 34),
    ) {
        let g = Grid::continuous(regular(rate, start), 1 << 34, SampleType::I32, vec![ChannelId(0)])
            .unwrap();
        prop_assert_eq!(g.sample_at(g.time_of(sample)), sample);
        // Anywhere inside the sample period maps to the sample.
        let mid = g.time_of(sample) + 0.5 / rate;
        prop_assert_eq!(g.sample_at(mid), sample);
    }
}
