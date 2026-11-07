//! The view state: defaults, every action, gestures, clamping and the tile request.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

use std::ops::Range;

use kymo::action::{Action, Gesture};
use kymo::view::{Area, DEFAULT_ROWS, ViewState};
use kymo_core::tile::{Direction, prefetch, tiles_for_view};
use kymo_core::{
    BlockReader, Channel, ChannelId, ChannelKind, Grid, GridId, Header, Opened, ReadError,
    Recording, SampleType, SamplesMut, Timebase, Unit,
};
use proptest::prelude::*;

/// The view never reads samples.
struct NoReader;

impl BlockReader for NoReader {
    fn read(
        &self,
        _: GridId,
        _: &[ChannelId],
        _: Range<u64>,
        _: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        unreachable!("the view state never reads samples")
    }
}

/// `eeg` EEG channels in µV at 1000 Hz, plus an EOG channel in mV and a stim channel at
/// 250 Hz, over `seconds`.
fn recording(eeg: u32, seconds: u64) -> Recording {
    let mut channels: Vec<Channel> = (0..eeg)
        .map(|c| {
            let mut ch = Channel::new(format!("EEG {c}"), GridId(0));
            ch.kind = ChannelKind::Eeg;
            ch.unit = Unit {
                label: "uV".into(),
                to_si: Some(1e-6),
            };
            ch
        })
        .collect();
    let mut eog = Channel::new("EOG", GridId(1));
    eog.kind = ChannelKind::Eog;
    eog.unit = Unit {
        label: "mV".into(),
        to_si: Some(1e-3),
    };
    let mut stim = Channel::new("Status", GridId(1));
    stim.kind = ChannelKind::Stim;
    channels.extend([eog, stim]);
    let rate = |rate| Timebase::Regular { rate, start: 0.0 };
    let grids = vec![
        Grid::continuous(
            rate(1000.0),
            seconds * 1000,
            SampleType::I16,
            (0..eeg).map(ChannelId).collect(),
        )
        .unwrap(),
        Grid::continuous(
            rate(250.0),
            seconds * 250,
            SampleType::I16,
            vec![ChannelId(eeg), ChannelId(eeg + 1)],
        )
        .unwrap(),
    ];
    let opened = Opened {
        header: Header {
            format: "TEST",
            start_time: None,
            channels,
            grids,
            extra: vec![],
        },
        annotations: vec![],
        warnings: vec![],
        reader: Box::new(NoReader),
    };
    Recording::new(opened).unwrap().0
}

/// 30 EEG channels (32 rows) over 10 minutes.
fn state() -> (Recording, ViewState) {
    let rec = recording(30, 600);
    let view = ViewState::new(&rec);
    (rec, view)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn assert_time(view: &ViewState, time: Range<f64>) {
    let shown = view.time();
    assert!(
        close(shown.start, time.start) && close(shown.end, time.end),
        "{shown:?} != {time:?}"
    );
}

fn act(view: &mut ViewState, action: Action) {
    assert!(view.apply(action, None), "{action:?}");
}

#[test]
fn starts_with_20_s_and_20_rows() {
    let (_, view) = state();
    assert_time(&view, 0.0..20.0);
    assert_eq!(view.rows(), DEFAULT_ROWS);
    assert_eq!(view.visible_rows(), 0..20);
    assert!(!view.dc_removed());
    assert_eq!(view.bad_channels().count(), 0);
    assert_eq!(view.amplitude(ChannelKind::Eeg), 20e-6);
    assert_eq!(view.amplitude(ChannelKind::Eog), 150e-6);
    assert_eq!(view.amplitude(ChannelKind::Ecg), 5e-3);
    assert_eq!(view.amplitude(ChannelKind::Emg), 1e-3);
    assert_eq!(view.amplitude(ChannelKind::Stim), 1.0);
    assert_eq!(view.amplitude(ChannelKind::Misc), 1.0);
}

#[test]
fn a_short_recording_shows_everything() {
    let rec = recording(1, 5);
    let view = ViewState::new(&rec);
    assert_time(&view, 0.0..5.0);
    assert_eq!(view.rows(), 3);
    assert_eq!(
        view.visible_channels(),
        [ChannelId(0), ChannelId(1), ChannelId(2)]
    );
}

#[test]
fn amplitudes_follow_each_channels_unit() {
    let (_, mut view) = state();
    assert!(close(view.channel_amplitude(ChannelId(0)), 20.0)); // µV
    assert!(close(view.channel_amplitude(ChannelId(30)), 0.15)); // mV
    assert!(close(view.channel_amplitude(ChannelId(31)), 1.0)); // no unit
    for _ in 0..4 {
        act(&mut view, Action::ScaleUp);
    }
    assert!(close(view.channel_amplitude(ChannelId(0)), 10.0));
    assert!(close(view.amplitude(ChannelKind::Eog), 75e-6));
    act(&mut view, Action::ScaleDown);
    act(&mut view, Action::ScaleDown);
    assert!(close(
        view.channel_amplitude(ChannelId(0)),
        10.0 * 2f64.sqrt()
    ));
}

#[test]
fn pans_by_quarters_and_windows_within_the_recording() {
    let (_, mut view) = state();
    act(&mut view, Action::PanRight);
    assert_time(&view, 5.0..25.0);
    act(&mut view, Action::PageRight);
    assert_time(&view, 25.0..45.0);
    act(&mut view, Action::PanLeft);
    assert_time(&view, 20.0..40.0);
    act(&mut view, Action::PageLeft);
    act(&mut view, Action::PageLeft);
    assert_time(&view, 0.0..20.0);
    act(&mut view, Action::ToEnd);
    assert_time(&view, 580.0..600.0);
    act(&mut view, Action::PanRight);
    assert_time(&view, 580.0..600.0);
    act(&mut view, Action::PageLeft);
    act(&mut view, Action::ToStart);
    assert_time(&view, 0.0..20.0);
}

#[test]
fn zooms_around_the_centre_down_to_ten_samples() {
    let (_, mut view) = state();
    act(&mut view, Action::PageRight);
    act(&mut view, Action::ZoomIn);
    assert_time(&view, 25.0..35.0);
    act(&mut view, Action::ZoomOut);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 10.0..50.0);
    for _ in 0..30 {
        act(&mut view, Action::ZoomIn);
    }
    // Ten samples of the 1000 Hz grid, the fastest.
    assert!(close(view.span(), 0.01), "{}", view.span());
    assert!(close(view.start() + view.span() / 2.0, 30.0));
    for _ in 0..30 {
        act(&mut view, Action::ZoomOut);
    }
    assert_time(&view, 0.0..600.0);
}

#[test]
fn default_window_keeps_the_start_and_fit_shows_everything() {
    let (_, mut view) = state();
    view.pan(100.0);
    act(&mut view, Action::ZoomOut);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 70.0..150.0);
    act(&mut view, Action::DefaultWindow);
    assert_time(&view, 70.0..90.0);
    act(&mut view, Action::Fit);
    assert_time(&view, 0.0..600.0);
    // From a wider window at the end: 20 s from its start.
    act(&mut view, Action::ToEnd);
    view.zoom(30.0, 1.0);
    assert_time(&view, 580.0..600.0);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 560.0..600.0);
    act(&mut view, Action::DefaultWindow);
    assert_time(&view, 560.0..580.0);
}

#[test]
fn steps_through_channels_within_the_list() {
    let (_, mut view) = state();
    act(&mut view, Action::ChannelUp);
    assert_eq!(view.top(), 0.0);
    act(&mut view, Action::ChannelDown);
    assert_eq!(view.visible_rows(), 1..21);
    act(&mut view, Action::ChannelPageDown);
    // 32 rows, 20 on screen: the last page starts at row 12.
    assert_eq!(view.top(), 12.0);
    act(&mut view, Action::ChannelDown);
    assert_eq!(view.top(), 12.0);
    act(&mut view, Action::ChannelPageUp);
    assert_eq!(view.top(), 0.0);
    act(&mut view, Action::LastChannels);
    assert_eq!(view.visible_rows(), 12..32);
    act(&mut view, Action::FirstChannels);
    assert_eq!(view.visible_rows(), 0..20);
}

#[test]
fn channel_steps_land_on_whole_rows_after_smooth_scrolling() {
    let (_, mut view) = state();
    let area = Area {
        width: 1000.0,
        height: 400.0,
    };
    // 20 rows in 400 points: 20 points per row; scroll 2.5 rows down.
    view.gesture(Gesture::ScrollRows { points: -50.0 }, area, None);
    assert!(close(view.top(), 2.5));
    assert_eq!(view.visible_rows(), 2..23);
    act(&mut view, Action::ChannelDown);
    assert_eq!(view.top(), 3.0);
    view.gesture(Gesture::ScrollRows { points: 10.0 }, area, None);
    act(&mut view, Action::ChannelUp);
    assert_eq!(view.top(), 2.0);
}

#[test]
fn changes_the_number_of_rows_between_one_and_all() {
    let (_, mut view) = state();
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 25);
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 31);
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 32);
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 32);
    for _ in 0..20 {
        act(&mut view, Action::FewerChannels);
    }
    assert_eq!(view.rows(), 1);
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 2);

    // Growing at the bottom moves the top up.
    view.set_rows(20);
    act(&mut view, Action::LastChannels);
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.visible_rows(), 7..32);
}

#[test]
fn toggles_dc_removal_and_bad_channels() {
    let (_, mut view) = state();
    act(&mut view, Action::ToggleDc);
    assert!(view.dc_removed());
    act(&mut view, Action::ToggleDc);
    assert!(!view.dc_removed());

    let height = 400.0;
    let under = view.channel_at(45.0, height); // third row
    assert_eq!(under, Some(ChannelId(2)));
    assert!(view.apply(Action::ToggleBad, under));
    assert!(view.is_bad(ChannelId(2)));
    assert!(view.apply(Action::ToggleBad, None));
    assert_eq!(view.bad_channels().collect::<Vec<_>>(), [ChannelId(2)]);
    assert!(view.apply(Action::ToggleBad, under));
    assert!(!view.is_bad(ChannelId(2)));
    view.toggle_bad(ChannelId(99));
    assert_eq!(view.bad_channels().count(), 0, "not a channel");
}

#[test]
fn leaves_other_actions_to_the_app() {
    let (_, mut view) = state();
    for action in [
        Action::Open,
        Action::Close,
        Action::Quit,
        Action::Settings,
        Action::Shortcuts,
        Action::About,
    ] {
        assert!(!view.apply(action, None), "{action:?}");
    }
    assert_time(&view, 0.0..20.0);
}

#[test]
fn dragging_moves_the_traces_with_the_pointer() {
    let (_, mut view) = state();
    view.pan(100.0);
    let area = Area {
        width: 1000.0,
        height: 400.0,
    };
    // Dragging right by a quarter of the width shows a quarter window earlier.
    view.gesture(Gesture::Pan { points: 250.0 }, area, None);
    assert_time(&view, 95.0..115.0);
    let t = view.time_at(300.0, area.width);
    view.pan_points(-120.0, area.width);
    assert!(close(view.x_at(t, area.width), 180.0));
}

#[test]
fn pinching_zooms_around_the_pointer() {
    let (_, mut view) = state();
    view.pan(100.0);
    let area = Area {
        width: 1000.0,
        height: 400.0,
    };
    let x = 800.0;
    let t = view.time_at(x, area.width);
    view.gesture(Gesture::Zoom { factor: 1.7 }, area, Some(x));
    assert!(close(view.time_at(x, area.width), t));
    assert!(close(view.span(), 20.0 / 1.7));
    view.gesture(Gesture::Scale { factor: 2.0 }, area, Some(x));
    assert!(close(view.amplitude(ChannelKind::Eeg), 10e-6));
}

#[test]
fn converts_between_points_time_and_rows() {
    let (_, mut view) = state();
    view.pan(50.0);
    assert!(close(view.time_at(0.0, 800.0), 50.0));
    assert!(close(view.time_at(800.0, 800.0), 70.0));
    assert!(close(view.x_at(60.0, 800.0), 400.0));
    assert!(close(view.row_at(30.0, 400.0), 1.5));
    assert!(close(view.y_of_row(3.0, 400.0), 60.0));
    assert_eq!(view.channel_at(-1.0, 400.0), None);
    view.set_rows(32);
    assert_eq!(view.channel_at(399.0, 400.0), Some(ChannelId(31)));
}

#[test]
fn requests_tiles_once_per_change_with_newer_generations() {
    let (rec, mut view) = state();
    let (first, keys) = view.request(&rec, 2000, usize::MAX).unwrap();
    let screen = tiles_for_view(&rec, &view.tile_view(2000));
    assert!(!screen.is_empty());
    assert_eq!(
        keys,
        prefetch(&rec, &view.tile_view(2000), Direction::Forward, usize::MAX)
    );
    assert!(view.request(&rec, 2000, usize::MAX).is_none());

    // Panning back looks further back.
    view.pan(100.0);
    view.request(&rec, 2000, usize::MAX).unwrap();
    act(&mut view, Action::PanLeft);
    let (second, keys) = view.request(&rec, 2000, usize::MAX).unwrap();
    assert!(second > first + 1);
    assert_eq!(
        keys,
        prefetch(&rec, &view.tile_view(2000), Direction::Backward, usize::MAX)
    );

    // A wider area or other channels are a new request too.
    assert!(view.request(&rec, 2400, usize::MAX).is_some());
    act(&mut view, Action::ChannelDown);
    assert!(view.request(&rec, 2400, usize::MAX).is_some());
    assert_eq!(
        view.tile_view(2400).channels,
        (1..21).map(ChannelId).collect::<Vec<_>>()
    );
}

#[derive(Clone, Debug)]
enum Step {
    Act(Action),
    Gesture(Gesture, Option<f64>),
    Pan(f64),
    Zoom(f64, f64),
    Rows(usize),
}

fn steps() -> impl Strategy<Value = Step> {
    let factor = prop_oneof![
        0.01f64..100.0,
        Just(0.0),
        Just(f64::INFINITY),
        Just(f64::NAN)
    ];
    let points = prop_oneof![-5000.0f64..5000.0, Just(f64::NAN)];
    prop_oneof![
        proptest::sample::select(Action::ALL.to_vec()).prop_map(Step::Act),
        (points.clone(), proptest::option::of(-100.0f64..1100.0))
            .prop_map(|(points, x)| Step::Gesture(Gesture::Pan { points }, x)),
        points.prop_map(|points| Step::Gesture(Gesture::ScrollRows { points }, None)),
        (factor.clone(), proptest::option::of(-100.0f64..1100.0))
            .prop_map(|(factor, x)| Step::Gesture(Gesture::Zoom { factor }, x)),
        factor
            .clone()
            .prop_map(|factor| Step::Gesture(Gesture::Scale { factor }, None)),
        (-1000.0f64..1000.0).prop_map(Step::Pan),
        (factor, -1.0f64..2.0).prop_map(|(f, a)| Step::Zoom(f, a)),
        (0usize..50).prop_map(Step::Rows),
    ]
}

proptest! {
    #[test]
    fn always_stays_inside_the_recording(
        shape in (1u32..40, 1u64..700),
        steps in proptest::collection::vec(steps(), 1..40),
    ) {
        let rec = recording(shape.0, shape.1);
        let mut view = ViewState::new(&rec);
        let area = Area { width: 1000.0, height: 400.0 };
        let duration = rec.duration();
        let rows = rec.channels().len();
        for step in steps {
            match step {
                Step::Act(action) => {
                    view.apply(action, view.channel_at(10.0, area.height));
                }
                Step::Gesture(gesture, x) => view.gesture(gesture, area, x),
                Step::Pan(seconds) => view.pan(seconds),
                Step::Zoom(factor, anchor) => view.zoom(factor, anchor),
                Step::Rows(n) => view.set_rows(n),
            }
            let time = view.time();
            prop_assert!(time.start >= 0.0 && time.end <= duration * (1.0 + 1e-12), "{time:?}");
            prop_assert!(view.span() >= (0.01f64).min(duration) * (1.0 - 1e-12), "{}", view.span());
            prop_assert!((1..=rows).contains(&view.rows()));
            prop_assert!(view.top() >= 0.0 && view.top() + view.rows() as f64 <= rows as f64);
            prop_assert!(view.visible_rows().end <= rows);
            for kind in [ChannelKind::Eeg, ChannelKind::Stim] {
                let a = view.amplitude(kind);
                prop_assert!(a.is_finite() && a > 0.0);
            }
        }
    }

    #[test]
    fn zooming_keeps_the_time_under_the_pointer(
        start in 100.0f64..400.0,
        factor in 0.2f64..20.0,
        x in 0.0f64..1000.0,
    ) {
        let (_, mut view) = state();
        view.pan(start);
        let area = Area { width: 1000.0, height: 400.0 };
        let t = view.time_at(x, area.width);
        view.gesture(Gesture::Zoom { factor }, area, Some(x));
        prop_assert!(close(view.time_at(x, area.width), t), "{} != {t}", view.time_at(x, area.width));
        prop_assert!(close(view.span(), 20.0 / factor));
    }
}
