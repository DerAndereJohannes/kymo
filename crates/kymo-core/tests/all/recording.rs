use crate::common;

use std::ops::Range;

use kymo_core::stim::{self, Change, EventOptions, StimChanges};
use kymo_core::testing::{self, MemData};
use kymo_core::{
    AnnotationData, Channel, ChannelId, Grid, GridId, ModelError, Opened, Origin, ReadError,
    Recording, SampleType, SamplesMut, Stream, StreamContent, Timebase,
};
use kymo_testgen::Spec;
use proptest::prelude::*;

const G0: GridId = GridId(0);
const G1: GridId = GridId(1);

fn regular(rate: f64) -> Timebase {
    Timebase::Regular { rate, start: 0.0 }
}

/// Grid 0: i16 at 100 Hz, 50 slots with gaps, channels 0 and 2.
/// Grid 1: i32 at 25 Hz, 13 slots without gaps, channel 1.
fn two_grids() -> Opened {
    let channels = vec![
        Channel::new("Fz", G0),
        Channel::new("Resp", G1),
        Channel {
            gain: 0.5,
            offset: -3.0,
            ..Channel::new("Cz", G0)
        },
    ];
    let grids = vec![
        Grid::new(
            regular(100.0),
            50,
            vec![0..10, 20..35, 40..50],
            SampleType::I16,
            vec![ChannelId(0), ChannelId(2)],
        )
        .unwrap(),
        Grid::continuous(regular(25.0), 13, SampleType::I32, vec![ChannelId(1)]).unwrap(),
    ];
    let data = vec![
        MemData::I16(vec![(0..50).collect(), (100..150).collect()]),
        MemData::I32(vec![(0..13).map(|i| i * 1000).collect()]),
    ];
    testing::opened(channels, grids, data)
}

fn recording(opened: Opened) -> Recording {
    Recording::new(opened).unwrap().0
}

fn model_error(opened: Opened) -> ModelError {
    Recording::new(opened).unwrap_err()
}

fn annotation(onset: f64, duration: f64, channels: Vec<ChannelId>) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: "x".into(),
        channels,
        origin: Origin::File,
        stream: None,
    }
}

#[test]
fn recording_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Recording>();
}

#[test]
fn exposes_metadata() {
    let mut opened = two_grids();
    opened.warnings.push("truncated".into());
    opened.annotations.push(annotation(0.1, 0.0, vec![]));
    let (rec, annotations) = Recording::new(opened).unwrap();

    assert_eq!(rec.format(), "MEM");
    assert_eq!(rec.channels().len(), 3);
    assert_eq!(rec.channel(ChannelId(2)).name, "Cz");
    assert_eq!(rec.grid(G1).rate(), 25.0);
    assert_eq!(rec.grid(rec.channel(ChannelId(1)).grid).len(), 13);
    // The highest rate, and the end of the longest grid (13 / 25 > 50 / 100).
    assert_eq!(rec.sfreq(), 100.0);
    assert_eq!(rec.duration(), 0.52);
    assert_eq!(rec.warnings(), ["truncated"]);
    assert_eq!(annotations, [annotation(0.1, 0.0, vec![])]);
}

#[test]
fn rejects_recording_without_channels() {
    let opened = testing::opened(vec![], vec![], vec![]);
    assert_eq!(model_error(opened), ModelError::NoChannels);
}

#[test]
fn rejects_channel_with_missing_grid() {
    let mut opened = two_grids();
    opened.header.channels[1].grid = GridId(7);
    assert_eq!(
        model_error(opened),
        ModelError::UnknownGrid {
            channel: ChannelId(1),
            grid: GridId(7)
        }
    );
}

#[test]
fn rejects_channel_listed_in_another_grid() {
    let mut opened = two_grids();
    opened.header.channels[2].grid = G1;
    assert_eq!(
        model_error(opened),
        ModelError::GridMismatch {
            channel: ChannelId(2),
            expected: G1,
            found: G0
        }
    );
}

#[test]
fn rejects_grid_listing_unknown_channel() {
    let mut opened = two_grids();
    opened.header.channels.pop();
    assert_eq!(
        model_error(opened),
        ModelError::UnknownChannel {
            grid: G0,
            channel: ChannelId(2)
        }
    );
}

#[test]
fn rejects_channel_missing_from_its_grid() {
    let mut opened = two_grids();
    opened.header.channels.push(Channel::new("orphan", G1));
    assert_eq!(
        model_error(opened),
        ModelError::ChannelNotListed(ChannelId(3))
    );
}

#[test]
fn rejects_non_finite_scaling() {
    let mut opened = two_grids();
    opened.header.channels[0].gain = f64::INFINITY;
    assert!(matches!(
        model_error(opened),
        ModelError::InvalidScaling {
            channel: ChannelId(0),
            ..
        }
    ));
    let mut opened = two_grids();
    opened.header.channels[0].offset = f64::NAN;
    assert!(matches!(
        model_error(opened),
        ModelError::InvalidScaling { .. }
    ));
}

#[test]
fn rejects_invalid_annotations() {
    let cases = [
        annotation(f64::NAN, 0.0, vec![]),
        annotation(1.0, -0.5, vec![]),
        annotation(1.0, f64::INFINITY, vec![]),
        annotation(1.0, 0.0, vec![ChannelId(3)]),
    ];
    for case in cases {
        let mut opened = two_grids();
        opened.annotations = vec![annotation(0.0, 1.0, vec![ChannelId(2)]), case.clone()];
        assert!(
            matches!(
                model_error(opened),
                ModelError::InvalidAnnotation { index: 1, .. }
            ),
            "{case:?}"
        );
    }
}

fn stream(id: u32, content: StreamContent) -> Stream {
    Stream {
        id,
        name: format!("stream {id}"),
        kind: "EEG".to_owned(),
        content,
    }
}

fn channels(ids: &[u32]) -> StreamContent {
    StreamContent::Channels(ids.iter().copied().map(ChannelId).collect())
}

#[test]
fn streams_hold_every_channel_once_and_in_runs() {
    let not_shown = || StreamContent::NotShown("it has no samples".into());
    let valid = vec![
        stream(4, StreamContent::Annotations(3)),
        stream(1, channels(&[0, 1])),
        stream(2, not_shown()),
        stream(9, channels(&[2])),
    ];
    let mut opened = two_grids();
    opened.header.streams = valid.clone();
    assert_eq!(recording(opened).streams(), valid);

    let invalid = [
        (vec![channels(&[0, 1]), channels(&[2, 3])], 1),
        (vec![channels(&[0, 1]), channels(&[1, 2])], 1),
        (vec![channels(&[0, 2]), channels(&[1])], 0),
        (vec![channels(&[1, 0]), channels(&[2])], 0),
        (vec![channels(&[0, 1, 2]), channels(&[])], 1),
    ];
    for (contents, index) in invalid {
        let mut opened = two_grids();
        opened.header.streams = contents
            .iter()
            .enumerate()
            .map(|(i, c)| stream(i as u32, c.clone()))
            .collect();
        let error = model_error(opened);
        assert!(
            matches!(error, ModelError::InvalidStream { index: i, .. } if i == index),
            "{contents:?}: {error:?}"
        );
    }
    for (contents, missing) in [
        (vec![channels(&[0, 1]), not_shown()], 2),
        (vec![StreamContent::Annotations(1)], 0),
    ] {
        let mut opened = two_grids();
        opened.header.streams = contents.into_iter().map(|c| stream(1, c)).collect();
        assert_eq!(
            model_error(opened),
            ModelError::ChannelWithoutStream(ChannelId(missing))
        );
    }
}

#[test]
fn formats_without_streams_list_none() {
    for preset in [
        "edf+c-kinds",
        "bv-int16-multiplexed",
        "eeglab-fdt",
        "gdf2-int32",
        "fif-float32-meg",
    ] {
        let file = common::TempFile::written(&Spec::preset(preset).unwrap());
        let (rec, _) = Recording::new(kymo_core::open(file.path()).unwrap()).unwrap();
        assert!(rec.streams().is_empty(), "{preset}");
    }
}

#[test]
fn reads_native_samples_in_requested_channel_order() {
    let data = MemData::I16(vec![(0..20).collect(), (100..120).collect()]);
    let rec = recording(testing::single_grid(250.0, data));
    let mut out = vec![0i16; 12];
    rec.read_native(
        G0,
        &[ChannelId(1), ChannelId(0)],
        3..9,
        SamplesMut::I16(&mut out),
    )
    .unwrap();
    assert_eq!(out, [103, 104, 105, 106, 107, 108, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn native_read_across_gaps_zeroes_them() {
    let rec = recording(two_grids());
    let mut out = vec![-1i16; 2 * 17];
    rec.read_native(
        G0,
        &[ChannelId(0), ChannelId(2)],
        5..22,
        SamplesMut::I16(&mut out),
    )
    .unwrap();
    let mut expected: Vec<i16> = [5, 6, 7, 8, 9].into();
    expected.extend([0; 10]);
    expected.extend([20, 21]);
    expected.extend([105, 106, 107, 108, 109]);
    expected.extend([0; 10]);
    expected.extend([120, 121]);
    assert_eq!(out, expected);

    let mut single = vec![-1i16; 17];
    rec.read_native(G0, &[ChannelId(2)], 5..22, SamplesMut::I16(&mut single))
        .unwrap();
    assert_eq!(single, expected[17..]);
}

#[test]
fn range_entirely_in_a_gap_reads_without_the_reader() {
    // MemReader fails on any request that touches a gap, so success means it was not called.
    let rec = recording(two_grids());
    let mut native = vec![-1i16; 2 * 6];
    rec.read_native(
        G0,
        &[ChannelId(0), ChannelId(2)],
        12..18,
        SamplesMut::I16(&mut native),
    )
    .unwrap();
    assert_eq!(native, [0; 12]);

    let mut physical = vec![0.0f32; 6];
    rec.read_physical(G0, &[ChannelId(2)], 12..18, &mut physical)
        .unwrap();
    assert!(physical.iter().all(|x| x.is_nan()));
}

#[test]
fn physical_read_marks_gaps_as_nan() {
    let rec = recording(two_grids());
    let mut out = vec![0.0f32; 12];
    rec.read_physical(G0, &[ChannelId(2)], 30..42, &mut out)
        .unwrap();
    let expected: Vec<f32> = (30..42)
        .map(|i| {
            if (35..40).contains(&i) {
                f32::NAN
            } else {
                (100 + i) as f32 * 0.5 - 3.0
            }
        })
        .collect();
    assert_eq!(bits(&out), bits(&expected));
}

#[test]
fn physical_read_handles_other_grids_and_types() {
    let rec = recording(two_grids());
    let mut out = vec![0.0f32; 3];
    rec.read_physical(G1, &[ChannelId(1)], 10..13, &mut out)
        .unwrap();
    assert_eq!(out, [10000.0, 11000.0, 12000.0]);
}

#[test]
fn huge_offset_is_applied_in_f64() {
    // physical = 32767 * 0.1 - 3276.6 = 0.1: f32 arithmetic would cancel catastrophically.
    let (gain, offset) = (0.1, -3276.6);
    let digital: Vec<i16> = vec![32767, 32766, 0, -32768];
    let mut opened = testing::single_grid(256.0, MemData::I16(vec![digital.clone()]));
    opened.header.channels[0].gain = gain;
    opened.header.channels[0].offset = offset;
    let rec = recording(opened);

    let mut out = vec![0.0f32; 4];
    rec.read_physical(G0, &[ChannelId(0)], 0..4, &mut out)
        .unwrap();
    let expected: Vec<f32> = digital
        .iter()
        .map(|&d| (f64::from(d) * gain + offset) as f32)
        .collect();
    assert_eq!(out, expected);
    assert_eq!(out[0], 0.1);
    let naive = f32::from(digital[0]) * gain as f32 + offset as f32;
    assert_ne!(naive, out[0], "the case should need f64");
}

#[test]
fn negative_gain_flips_polarity() {
    let mut opened = testing::single_grid(100.0, MemData::I32(vec![vec![-3, 0, 4, 8_388_607]]));
    opened.header.channels[0].gain = -2.0;
    opened.header.channels[0].offset = 10.0;
    let rec = recording(opened);
    let mut out = vec![0.0f32; 4];
    rec.read_physical(G0, &[ChannelId(0)], 0..4, &mut out)
        .unwrap();
    assert_eq!(out, [16.0, 10.0, 2.0, -16_777_204.0]);
}

#[test]
fn float_samples_are_scaled_too() {
    let mut opened = testing::single_grid(100.0, MemData::F32(vec![vec![1.5, -2.0]]));
    opened.header.channels[0].gain = 1e-6;
    let rec = recording(opened);
    let mut native = vec![0.0f32; 2];
    rec.read_native(G0, &[ChannelId(0)], 0..2, SamplesMut::F32(&mut native))
        .unwrap();
    assert_eq!(native, [1.5, -2.0]);
    let mut physical = vec![0.0f32; 2];
    rec.read_physical(G0, &[ChannelId(0)], 0..2, &mut physical)
        .unwrap();
    assert_eq!(physical, [1.5e-6, -2.0e-6]);
}

#[test]
fn empty_requests_succeed() {
    let rec = recording(two_grids());
    rec.read_native(G0, &[], 0..50, SamplesMut::I16(&mut []))
        .unwrap();
    rec.read_native(G0, &[ChannelId(0)], 7..7, SamplesMut::I16(&mut []))
        .unwrap();
    rec.read_physical(G0, &[ChannelId(0)], 50..50, &mut [])
        .unwrap();
}

#[test]
fn rejects_invalid_requests() {
    let rec = recording(two_grids());
    let mut buf = [0i16; 10];
    let mut native = |grid, channels: &[ChannelId], range: Range<u64>, len: usize| {
        rec.read_native(grid, channels, range, SamplesMut::I16(&mut buf[..len]))
            .unwrap_err()
    };
    assert!(matches!(
        native(GridId(2), &[ChannelId(0)], 0..1, 1),
        ReadError::UnknownGrid(GridId(2))
    ));
    assert!(matches!(
        native(G0, &[ChannelId(1)], 0..1, 1),
        ReadError::ChannelNotInGrid {
            channel: ChannelId(1),
            grid: G0
        }
    ));
    assert!(matches!(
        native(G0, &[ChannelId(9)], 0..1, 1),
        ReadError::ChannelNotInGrid { .. }
    ));
    assert!(matches!(
        native(G0, &[ChannelId(0)], 45..51, 6),
        ReadError::OutOfRange { len: 50, .. }
    ));
    let (start, end) = (5, 3);
    assert!(matches!(
        native(G0, &[ChannelId(0)], start..end, 0),
        ReadError::OutOfRange { .. }
    ));
    assert!(matches!(
        native(G0, &[ChannelId(0)], 0..5, 4),
        ReadError::BufferMismatch {
            expected: 5,
            found: 4,
            ..
        }
    ));

    let mut wrong_type = vec![0i32; 5];
    let err = rec
        .read_native(G0, &[ChannelId(0)], 0..5, SamplesMut::I32(&mut wrong_type))
        .unwrap_err();
    assert!(matches!(
        err,
        ReadError::BufferMismatch {
            expected_type: SampleType::I16,
            found_type: SampleType::I32,
            ..
        }
    ));

    let mut physical = vec![0.0f32; 9];
    let err = rec
        .read_physical(G0, &[ChannelId(0), ChannelId(2)], 0..5, &mut physical)
        .unwrap_err();
    assert!(matches!(
        err,
        ReadError::BufferMismatch {
            expected: 10,
            found: 9,
            ..
        }
    ));
}

fn bits(values: &[f32]) -> Vec<u32> {
    // NaN != NaN, so compare bit patterns (all NaNs we produce are f32::NAN).
    values.iter().map(|x| x.to_bits()).collect()
}

/// A random single-grid recording with gaps, plus a random read request.
#[derive(Debug)]
struct Case {
    sample_type: SampleType,
    len: u64,
    segments: Vec<Range<u64>>,
    rows: Vec<Vec<i16>>,
    scaling: Vec<(f64, f64)>,
    channels: Vec<ChannelId>,
    range: Range<u64>,
}

fn case() -> impl Strategy<Value = Case> {
    let sample_type = prop_oneof![
        Just(SampleType::I16),
        Just(SampleType::I32),
        Just(SampleType::F32)
    ];
    (sample_type, common::len_and_segments(120), 1usize..5)
        .prop_flat_map(|(sample_type, (len, segments), n_channels)| {
            let rows = proptest::collection::vec(
                proptest::collection::vec(any::<i16>(), len as usize),
                n_channels,
            );
            let scaling = proptest::collection::vec((-1e3f64..1e3, -1e7f64..1e7), n_channels);
            let channels = proptest::collection::vec(0..n_channels as u32, 0..6);
            (
                Just(sample_type),
                Just(len),
                Just(segments),
                rows,
                scaling,
                channels,
                common::sub_range(len),
            )
        })
        .prop_map(
            |(sample_type, len, segments, rows, scaling, channels, range)| Case {
                sample_type,
                len,
                segments,
                rows,
                scaling,
                channels: channels.into_iter().map(ChannelId).collect(),
                range,
            },
        )
}

impl Case {
    fn recording(&self) -> Recording {
        let ids = (0..self.rows.len() as u32).map(ChannelId).collect();
        let grid = Grid::new(
            regular(512.0),
            self.len,
            self.segments.clone(),
            self.sample_type,
            ids,
        )
        .unwrap();
        let channels = self
            .scaling
            .iter()
            .enumerate()
            .map(|(i, &(gain, offset))| Channel {
                gain,
                offset,
                ..Channel::new(format!("ch{i}"), G0)
            })
            .collect();
        recording(testing::opened(channels, vec![grid], vec![self.data()]))
    }

    /// The rows in the case's sample type (scaled so I32 and F32 use more than 16 bits).
    fn data(&self) -> MemData {
        let rows = &self.rows;
        match self.sample_type {
            SampleType::I16 => MemData::I16(rows.clone()),
            SampleType::I32 => MemData::I32(convert(rows, |x| i32::from(x) * 256)),
            _ => MemData::F32(convert(rows, |x| f32::from(x) / 8.0)),
        }
    }

    /// Brute force: per sample, the native value as f64, or `None` in a gap.
    fn expected_native(&self, data: &MemData) -> Vec<Option<f64>> {
        let mut expected = Vec::new();
        for &c in &self.channels {
            for i in self.range.clone() {
                let present = common::is_present(&self.segments, i);
                expected.push(present.then(|| data.get(c.0 as usize, i as usize)));
            }
        }
        expected
    }
}

fn convert<T>(rows: &[Vec<i16>], f: impl Fn(i16) -> T) -> Vec<Vec<T>> {
    rows.iter()
        .map(|row| row.iter().copied().map(&f).collect())
        .collect()
}

fn native_as_f64(rec: &Recording, case: &Case) -> Vec<f64> {
    let n = case.channels.len() * (case.range.end - case.range.start) as usize;
    let (channels, range) = (&case.channels, case.range.clone());
    match case.sample_type {
        SampleType::I16 => {
            let mut out = vec![-1i16; n];
            rec.read_native(G0, channels, range, SamplesMut::I16(&mut out))
                .unwrap();
            out.into_iter().map(f64::from).collect()
        }
        SampleType::I32 => {
            let mut out = vec![-1i32; n];
            rec.read_native(G0, channels, range, SamplesMut::I32(&mut out))
                .unwrap();
            out.into_iter().map(f64::from).collect()
        }
        _ => {
            let mut out = vec![-1.0f32; n];
            rec.read_native(G0, channels, range, SamplesMut::F32(&mut out))
                .unwrap();
            out.into_iter().map(f64::from).collect()
        }
    }
}

proptest! {
    #[test]
    fn reads_match_brute_force(case in case()) {
        let rec = case.recording();
        let expected = case.expected_native(&case.data());

        let native = native_as_f64(&rec, &case);
        let zeroed: Vec<f64> = expected.iter().map(|e| e.unwrap_or(0.0)).collect();
        prop_assert_eq!(&native, &zeroed);

        let mut physical = vec![0.0f32; expected.len()];
        rec.read_physical(G0, &case.channels, case.range.clone(), &mut physical).unwrap();
        let n = (case.range.end - case.range.start) as usize;
        let expected_physical: Vec<f32> = expected
            .iter()
            .enumerate()
            .map(|(k, e)| {
                let (gain, offset) = case.scaling[case.channels[k / n].0 as usize];
                e.map_or(f32::NAN, |d| (d * gain + offset) as f32)
            })
            .collect();
        prop_assert_eq!(bits(&physical), bits(&expected_physical));
    }
}

/// Streams for [`two_grids`]: annotations, Fz and Resp, one not shown, Cz.
fn two_grids_streams() -> Vec<Stream> {
    vec![
        stream(4, StreamContent::Annotations(1)),
        stream(1, channels(&[0, 1])),
        stream(2, StreamContent::NotShown("it has no samples".into())),
        stream(9, channels(&[2])),
    ]
}

#[test]
fn annotations_belong_to_streams_that_hold_something() {
    let with_stream = |stream| AnnotationData {
        stream,
        ..annotation(1.0, 0.0, vec![])
    };
    for (stream, valid) in [
        (None, true),
        (Some(0), true),
        (Some(1), true),
        (Some(2), false),
        (Some(3), true),
        (Some(4), false),
    ] {
        let mut opened = two_grids();
        opened.header.streams = two_grids_streams();
        opened.annotations = vec![annotation(0.0, 1.0, vec![]), with_stream(stream)];
        match Recording::new(opened) {
            Ok((_, annotations)) => {
                assert!(valid, "{stream:?}");
                assert_eq!(annotations[1].stream, stream);
            }
            Err(error) => {
                assert!(!valid, "{stream:?}");
                assert!(
                    matches!(error, ModelError::InvalidAnnotation { index: 1, .. }),
                    "{error:?}"
                );
            }
        }
    }
    // Without streams, none.
    let mut opened = two_grids();
    opened.annotations = vec![with_stream(Some(0))];
    assert!(matches!(
        model_error(opened),
        ModelError::InvalidAnnotation { index: 0, .. }
    ));
}

#[test]
fn stim_events_belong_to_their_channels_stream() {
    let changes = |channel| StimChanges {
        channel: ChannelId(channel),
        // The first is the channel's value at its start, not an event.
        changes: vec![
            Change {
                sample: 0,
                value: 0,
            },
            Change {
                sample: 3,
                value: 5,
            },
        ],
    };
    let mut opened = two_grids();
    opened.header.streams = two_grids_streams();
    let rec = recording(opened);
    assert_eq!(rec.stream_of(ChannelId(0)), Some(1));
    assert_eq!(rec.stream_of(ChannelId(1)), Some(1));
    assert_eq!(rec.stream_of(ChannelId(2)), Some(3));
    let options = EventOptions::default();
    let events = stim::annotations(&rec, &[changes(1), changes(2)], options);
    let mut streams: Vec<(ChannelId, Option<usize>)> =
        events.iter().map(|a| (a.channels[0], a.stream)).collect();
    streams.sort();
    assert_eq!(streams, [(ChannelId(1), Some(1)), (ChannelId(2), Some(3))]);

    let rec = recording(two_grids());
    assert_eq!(rec.stream_of(ChannelId(2)), None);
    let events = stim::annotations(&rec, &[changes(2)], options);
    assert_eq!(events[0].stream, None);
}
