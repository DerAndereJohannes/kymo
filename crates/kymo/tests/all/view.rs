//! The view state: defaults, every action, gestures, clamping and the tile request.

use std::ops::Range;

use kymo::action::{Action, Gesture};
use kymo::view::{Area, DEFAULT_ROWS, KINDS, ViewState};
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
pub fn recording(eeg: u32, seconds: u64) -> Recording {
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
        sources: vec![],
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
    assert!(view.apply(action), "{action:?}");
}

#[test]
fn starts_with_20_s_and_20_rows() {
    let (_, view) = state();
    assert_time(&view, 0.0..20.0);
    assert_eq!(view.rows(), DEFAULT_ROWS);
    assert_eq!(view.visible_rows(), 0..20);
    assert!(!view.dc_removed());
    assert_eq!(view.bad_channels().count(), 0);
    // MNE's scalings for raw.plot.
    use ChannelKind::*;
    for (kind, amplitude) in [
        (Eeg, 20e-6),
        (Eog, 150e-6),
        (Ecg, 5e-4),
        (Emg, 1e-3),
        (Stim, 1.0),
        (Misc, 1.0),
        (Mag, 1e-12),
        (Grad, 4e-11),
        (RefMeg, 1e-12),
        (Seeg, 1e-4),
        (Ecog, 1e-4),
        (Dbs, 1e-4),
        (Resp, 1.0),
        (Bio, 1e-6),
    ] {
        assert_eq!(view.amplitude(kind), amplitude, "{kind:?}");
    }
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
fn zooms_from_the_start_down_to_ten_samples() {
    let (_, mut view) = state();
    act(&mut view, Action::PageRight);
    act(&mut view, Action::ZoomIn);
    assert_time(&view, 20.0..30.0);
    act(&mut view, Action::ZoomOut);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 20.0..60.0);
    act(&mut view, Action::ZoomIn);
    assert_time(&view, 20.0..40.0);
    for _ in 0..30 {
        act(&mut view, Action::ZoomIn);
    }
    // Ten samples of the 1000 Hz grid, the fastest.
    assert!(close(view.span(), 0.01), "{}", view.span());
    assert!(close(view.start(), 20.0));
    for _ in 0..30 {
        act(&mut view, Action::ZoomOut);
    }
    assert_time(&view, 0.0..600.0);
}

#[test]
fn zooming_in_and_out_returns_to_the_same_window() {
    let (_, mut view) = state();
    // At the start, where zooming around the center would drift later.
    for _ in 0..3 {
        act(&mut view, Action::ZoomOut);
        act(&mut view, Action::ZoomIn);
        act(&mut view, Action::ZoomIn);
        act(&mut view, Action::ZoomOut);
    }
    assert_time(&view, 0.0..20.0);
    view.pan(100.0);
    for _ in 0..3 {
        act(&mut view, Action::ZoomOut);
    }
    for _ in 0..3 {
        act(&mut view, Action::ZoomIn);
    }
    assert_time(&view, 100.0..120.0);
    // At the end, zooming out has to move the start back, and zooming in keeps it.
    act(&mut view, Action::ToEnd);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 560.0..600.0);
    act(&mut view, Action::ZoomIn);
    assert_time(&view, 560.0..580.0);
}

#[test]
fn default_window_keeps_the_start_and_fit_shows_everything() {
    let (_, mut view) = state();
    view.pan(100.0);
    act(&mut view, Action::ZoomOut);
    act(&mut view, Action::ZoomOut);
    assert_time(&view, 100.0..180.0);
    act(&mut view, Action::DefaultWindow);
    assert_time(&view, 100.0..120.0);
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

    view.toggle_bad(ChannelId(2));
    assert_eq!(view.bad_channels().collect::<Vec<_>>(), [ChannelId(2)]);
    view.toggle_bad(ChannelId(2));
    assert!(!view.is_bad(ChannelId(2)));
    view.toggle_bad(ChannelId(99));
    assert_eq!(view.bad_channels().count(), 0, "not a channel");
}

fn ids(ids: &[u32]) -> Vec<ChannelId> {
    ids.iter().map(|&i| ChannelId(i)).collect()
}

#[test]
fn hidden_channels_lose_their_rows_and_the_top_channel_stays_first() {
    let (_, mut view) = state();
    view.set_top(5.0);
    view.set_shown(ChannelId(2), false);
    view.set_shown(ChannelId(7), false);
    assert!(!view.is_shown(ChannelId(2)) && view.is_shown(ChannelId(3)));
    assert_eq!(view.row_count(), 30);
    assert_eq!(view.row_of(ChannelId(2)), None);
    assert_eq!(view.row_of(ChannelId(8)), Some(6));
    // Channel 5 is still first, one row higher up.
    assert_eq!(view.top(), 4.0);
    assert_eq!(view.visible_channels()[..4], ids(&[5, 6, 8, 9]));
    assert_eq!(view.visible_rows(), 4..24);

    // Hiding the top channel makes the next shown one first.
    view.set_shown(ChannelId(5), false);
    assert_eq!(view.visible_channels()[..3], ids(&[6, 8, 9]));
    // Showing it again puts it back above, keeping channel 6 first.
    view.set_shown(ChannelId(5), true);
    view.set_shown(ChannelId(2), true);
    assert_eq!(view.visible_channels()[..2], ids(&[6, 8]));
    assert_eq!(view.top(), 6.0);
    // Unknown channels change nothing.
    view.set_shown(ChannelId(99), false);
    assert!(!view.is_shown(ChannelId(99)));
    assert_eq!(view.row_count(), 31);
}

#[test]
fn types_are_shown_and_hidden_together() {
    // 30 EEG channels, then EOG and stim.
    let (_, mut view) = state();
    assert_eq!(view.kind_shown(ChannelKind::Eeg), (30, 30));
    assert_eq!(view.kind_shown(ChannelKind::Eog), (1, 1));
    assert_eq!(view.kind_shown(ChannelKind::Ecg), (0, 0));
    view.set_shown(ChannelId(4), false);
    assert_eq!(view.kind_shown(ChannelKind::Eeg), (29, 30));

    view.set_kind_shown(ChannelKind::Eeg, false);
    assert_eq!(view.kind_shown(ChannelKind::Eeg), (0, 30));
    assert_eq!(view.visible_channels(), ids(&[30, 31]));
    assert_eq!(view.rows(), 2);
    assert_eq!(view.top(), 0.0);

    // Nothing shown: no rows, and nothing breaks.
    view.set_kind_shown(ChannelKind::Eog, false);
    view.set_kind_shown(ChannelKind::Stim, false);
    assert_eq!(view.row_count(), 0);
    assert!(view.visible_channels().is_empty());
    assert_eq!(view.channel_at(10.0, 400.0), None);
    for action in Action::ALL {
        view.apply(action);
    }
    assert!(view.visible_channels().is_empty());

    // All back: the 20 rows chosen before, from the top.
    view.set_kind_shown(ChannelKind::Eeg, true);
    assert_eq!(view.kind_shown(ChannelKind::Eeg), (30, 30));
    assert_eq!(view.rows(), DEFAULT_ROWS);
    assert_eq!(view.visible_rows(), 0..20);
}

#[test]
fn the_chosen_number_of_rows_returns_with_hidden_channels() {
    let (_, mut view) = state();
    view.set_rows(10);
    view.set_kind_shown(ChannelKind::Eeg, false);
    assert_eq!(view.rows(), 2);
    // More rows than there are channels keeps the choice.
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 2);
    view.set_kind_shown(ChannelKind::Eeg, true);
    assert_eq!(view.rows(), 10);

    // Fewer rows than channels is a new choice.
    view.set_kind_shown(ChannelKind::Eeg, false);
    act(&mut view, Action::FewerChannels);
    assert_eq!(view.rows(), 1);
    view.set_kind_shown(ChannelKind::Eeg, true);
    assert_eq!(view.rows(), 1);
}

#[test]
fn scrolls_to_a_shown_channel() {
    let (_, mut view) = state();
    assert!(view.scroll_to(ChannelId(7)));
    assert_eq!(view.visible_channels()[0], ChannelId(7));
    // Near the end, as far as the rows allow.
    assert!(view.scroll_to(ChannelId(30)));
    assert_eq!(view.visible_rows(), 12..32);
    view.set_shown(ChannelId(3), false);
    assert!(!view.scroll_to(ChannelId(3)));
    assert!(!view.scroll_to(ChannelId(99)));
    assert!(view.scroll_to(ChannelId(4)));
    assert_eq!(view.top(), 3.0);
}

#[test]
fn bad_channels_stay_bad_while_hidden() {
    let (_, mut view) = state();
    view.toggle_bad(ChannelId(1));
    view.set_shown(ChannelId(1), false);
    assert!(view.is_bad(ChannelId(1)));
    view.toggle_bad(ChannelId(1));
    view.set_shown(ChannelId(1), true);
    assert!(!view.is_bad(ChannelId(1)));
}

#[test]
fn the_default_window_can_be_set() {
    let rec = recording(4, 600);
    let mut view = ViewState::with_default_span(&rec, 7.5);
    assert_time(&view, 0.0..7.5);
    assert_eq!(view.default_span(), 7.5);
    view.pan(100.0);
    act(&mut view, Action::ZoomOut);
    // A new default leaves the window alone until Default Window.
    view.set_default_span(30.0);
    assert_time(&view, 100.0..115.0);
    act(&mut view, Action::DefaultWindow);
    assert_time(&view, 100.0..130.0);
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        view.set_default_span(bad);
        assert_eq!(view.default_span(), 30.0, "{bad}");
    }
    // Longer than the recording: all of it.
    let short = recording(1, 5);
    assert_time(&ViewState::with_default_span(&short, 60.0), 0.0..5.0);
    assert_time(&ViewState::with_default_span(&short, f64::NAN), 0.0..5.0);
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
        assert!(!view.apply(action), "{action:?}");
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
    Show(u32, bool),
    ShowKind(ChannelKind, bool),
    ScrollTo(u32),
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
        (0u32..45, any::<bool>()).prop_map(|(c, on)| Step::Show(c, on)),
        (
            proptest::sample::select(vec![ChannelKind::Eeg, ChannelKind::Eog, ChannelKind::Stim]),
            any::<bool>()
        )
            .prop_map(|(k, on)| Step::ShowKind(k, on)),
        (0u32..45).prop_map(Step::ScrollTo),
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
        let channels = rec.channels().len();
        for step in steps {
            match step {
                Step::Act(action) => {
                    view.apply(action);
                }
                Step::Gesture(gesture, x) => view.gesture(gesture, area, x),
                Step::Pan(seconds) => view.pan(seconds),
                Step::Zoom(factor, anchor) => view.zoom(factor, anchor),
                Step::Rows(n) => view.set_rows(n),
                Step::Show(c, on) => view.set_shown(ChannelId(c), on),
                Step::ShowKind(kind, on) => view.set_kind_shown(kind, on),
                Step::ScrollTo(c) => {
                    let shown = view.is_shown(ChannelId(c));
                    prop_assert_eq!(view.scroll_to(ChannelId(c)), shown);
                    if shown {
                        let first = view.visible_channels()[0];
                        prop_assert!(first == ChannelId(c) || view.visible_rows().end == view.row_count());
                    }
                }
            }
            // The rows are the shown channels in file order.
            let shown: Vec<_> = (0..channels as u32).map(ChannelId).filter(|&c| view.is_shown(c)).collect();
            prop_assert_eq!(view.row_count(), shown.len());
            prop_assert_eq!(view.visible_channels(), &shown[view.visible_rows()]);
            let rows = shown.len();
            let time = view.time();
            prop_assert!(time.start >= 0.0 && time.end <= duration * (1.0 + 1e-12), "{time:?}");
            prop_assert!(view.span() >= (0.01f64).min(duration) * (1.0 - 1e-12), "{}", view.span());
            prop_assert!((1..=rows.max(1)).contains(&view.rows()));
            prop_assert!(view.top() >= 0.0 && view.top() + view.rows().min(rows) as f64 <= rows as f64);
            // Every row on screen is filled while there are enough (one more is partly
            // visible while scrolling).
            prop_assert!(view.visible_rows().len() >= view.rows().min(rows));
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

#[test]
fn dc_windows_follow_the_view_while_dc_removal_is_on() {
    let (rec, mut view) = state();
    // Off: nothing to ask for.
    assert_eq!(view.dc_request(&rec), None);
    act(&mut view, Action::ToggleDc);
    // Each visible channel but stim ones, top to bottom, over the samples in view of its
    // own grid.
    let expected = |view: &ViewState| -> Vec<(ChannelId, Range<u64>)> {
        view.visible_channels()
            .iter()
            .filter(|&&c| rec.channel(c).kind != ChannelKind::Stim)
            .map(|&c| {
                let grid = rec.grid(rec.channel(c).grid);
                let t = view.time();
                let first = (t.start * grid.rate()).floor() as u64;
                let end = ((t.end * grid.rate()).ceil() as u64).min(grid.len());
                (c, first..end)
            })
            .collect()
    };
    let windows = view.dc_request(&rec).unwrap();
    assert_eq!(windows, expected(&view));
    assert_eq!(windows.len(), view.visible_channels().len());
    // Unchanged: nothing new.
    assert_eq!(view.dc_request(&rec), None);
    // Panned: the new windows.
    view.pan(1.25);
    assert_eq!(view.dc_request(&rec).unwrap(), expected(&view));
    // Scrolled to the last rows, where the 250 Hz grid's channels are.
    act(&mut view, Action::LastChannels);
    let windows = view.dc_request(&rec).unwrap();
    assert_eq!(windows, expected(&view));
    assert_eq!(rec.channel(windows.last().unwrap().0).grid, GridId(1));
    // Status is on screen, but DC removal leaves it alone.
    let status = *view.visible_channels().last().unwrap();
    assert_eq!(rec.channel(status).kind, ChannelKind::Stim);
    assert!(windows.iter().all(|&(c, _)| c != status));
    assert_eq!(windows.len(), view.visible_channels().len() - 1);
    // Off and on again: asked for afresh.
    act(&mut view, Action::ToggleDc);
    assert_eq!(view.dc_request(&rec), None);
    act(&mut view, Action::ToggleDc);
    assert_eq!(view.dc_request(&rec).unwrap(), expected(&view));
}

#[test]
fn set_amplitudes_takes_only_positive_finite_ones() {
    let rec = recording(4, 60);
    let mut view = ViewState::new(&rec);
    let before = KINDS.map(|k| view.amplitude(k));
    let mut amplitudes = [None; KINDS.len()];
    amplitudes[..5].copy_from_slice(&[
        Some(0.0),
        Some(-1.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
        None,
    ]);
    amplitudes[5] = Some(3e-6);
    view.set_amplitudes(amplitudes);
    let after = KINDS.map(|k| view.amplitude(k));
    assert_eq!(after[..5], before[..5]);
    assert_eq!(after[5], 3e-6);
    assert_eq!(after[6..], before[6..]);
}
