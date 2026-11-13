//! The view state: defaults, every action, gestures, clamping and the tile request.

use std::ops::Range;

use kymo::action::{Action, Gesture};
use kymo::view::{Area, DEFAULT_ROWS, KINDS, ViewState, kind_index};
use kymo_core::tile::{Direction, prefetch, tiles_for_view};
use kymo_core::{
    BlockReader, Channel, ChannelId, ChannelKind, Grid, GridId, Header, Opened, ReadError,
    Recording, SampleType, SamplesMut, Stream, StreamContent, Timebase, Unit,
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
    recording_with(eeg, seconds, false)
}

/// As [`recording`], with streams if `streams`: the first half of the EEG channels (at
/// least one), an annotation stream, the other half (if any), then EOG and stim.
fn recording_with(eeg: u32, seconds: u64, streams: bool) -> Recording {
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
            bad_channels: Vec::new(),
            streams: if streams {
                eeg_streams(eeg)
            } else {
                Vec::new()
            },
        },
        annotations: vec![],
        warnings: vec![],
        sources: vec![],
        reader: Box::new(NoReader),
    };
    Recording::new(opened).unwrap().0
}

fn eeg_streams(eeg: u32) -> Vec<Stream> {
    let stream = |name: &str, content| Stream {
        id: 0,
        name: name.to_owned(),
        kind: String::new(),
        content,
    };
    let channels = |r: Range<u32>| StreamContent::Channels(r.map(ChannelId).collect());
    let half = eeg.div_ceil(2);
    let mut streams = vec![
        stream("EEG 1", channels(0..half)),
        stream("Markers", StreamContent::Annotations(3)),
    ];
    if half < eeg {
        streams.push(stream("EEG 2", channels(half..eeg)));
    }
    streams.push(stream("Body", channels(eeg..eeg + 2)));
    streams
}

/// Channels of types not in file order, without streams: stim, EEG, EOG, EEG, misc, EEG.
fn mixed_recording() -> Recording {
    use ChannelKind::*;
    let kinds = [Stim, Eeg, Eog, Eeg, Misc, Eeg];
    let channels = kinds
        .iter()
        .enumerate()
        .map(|(i, &kind)| {
            let mut ch = Channel::new(format!("C{i}"), GridId(0));
            ch.kind = kind;
            ch
        })
        .collect();
    let grid = Grid::continuous(
        Timebase::Regular {
            rate: 100.0,
            start: 0.0,
        },
        6000,
        SampleType::I16,
        (0..kinds.len() as u32).map(ChannelId).collect(),
    )
    .unwrap();
    let opened = Opened {
        header: Header {
            format: "TEST",
            start_time: None,
            channels,
            grids: vec![grid],
            extra: vec![],
            bad_channels: Vec::new(),
            streams: Vec::new(),
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
    assert!(view.dc_removed());
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
    // The app's setting decides; the action changes the setting, not the view.
    assert!(!view.apply(Action::ToggleDc));
    assert!(view.dc_removed());
    view.set_dc_removed(false);
    assert!(!view.dc_removed());
    view.set_dc_removed(true);
    assert!(view.dc_removed());

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
fn channels_open_by_type_and_move_within_it() {
    let rec = mixed_recording();
    let mut view = ViewState::new(&rec);
    // EEG, EOG, stim, misc, each in file order.
    assert_eq!(view.order(), ids(&[1, 3, 5, 2, 0, 4]));
    assert_eq!(view.visible_channels(), ids(&[1, 3, 5, 2, 0, 4]));
    assert_eq!(view.group_of(ChannelId(3)), (None, ChannelKind::Eeg));

    // Within its type: before another, to its end, before itself.
    assert!(view.move_channel(ChannelId(5), Some(ChannelId(1))));
    assert_eq!(view.order(), ids(&[5, 1, 3, 2, 0, 4]));
    assert_eq!(view.row_of(ChannelId(5)), Some(0));
    assert!(view.move_channel(ChannelId(5), None));
    assert!(view.move_channel(ChannelId(1), Some(ChannelId(1))));
    assert_eq!(view.order(), ids(&[1, 3, 5, 2, 0, 4]));
    // A type of one channel: to its end, where it is.
    assert!(view.move_channel(ChannelId(0), None));
    // Not into another type, nor unknown channels.
    assert!(!view.move_channel(ChannelId(0), Some(ChannelId(1))));
    assert!(!view.move_channel(ChannelId(6), None));
    assert!(!view.move_channel(ChannelId(1), Some(ChannelId(99))));
    assert_eq!(view.order(), ids(&[1, 3, 5, 2, 0, 4]));

    // A type with its channels: before another type, to the end, before itself.
    assert!(view.move_kind(None, ChannelKind::Stim, Some(ChannelKind::Eeg)));
    assert_eq!(view.order(), ids(&[0, 1, 3, 5, 2, 4]));
    assert!(view.move_kind(None, ChannelKind::Eeg, None));
    assert_eq!(view.order(), ids(&[0, 2, 4, 1, 3, 5]));
    assert!(view.move_kind(None, ChannelKind::Eog, Some(ChannelKind::Eog)));
    // Not types it doesn't have, nor before them, nor in a stream it doesn't have.
    assert!(!view.move_kind(None, ChannelKind::Ecg, None));
    assert!(!view.move_kind(None, ChannelKind::Eeg, Some(ChannelKind::Ecg)));
    assert!(!view.move_kind(Some(0), ChannelKind::Eeg, None));
    assert_eq!(view.order(), ids(&[0, 2, 4, 1, 3, 5]));

    // Back to how it opened.
    view.move_channel(ChannelId(5), Some(ChannelId(1)));
    view.reset_order();
    assert_eq!(view.order(), ids(&[1, 3, 5, 2, 0, 4]));
    assert!(view.apply(Action::ResetOrder));

    // A type is shown and hidden as a whole.
    view.set_shown(ChannelId(3), false);
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (2, 3));
    view.set_group_shown(None, ChannelKind::Eeg, false);
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (0, 3));
    assert_eq!(view.visible_channels(), ids(&[2, 0, 4]));
    view.set_group_shown(None, ChannelKind::Eeg, true);
    assert_eq!(view.visible_channels(), ids(&[1, 3, 5, 2, 0, 4]));
}

#[test]
fn channels_stay_within_their_stream_and_streams_move_together() {
    // Streams: 0 = EEG 0 to 2, 1 = annotations, 2 = EEG 3 to 5, 3 = EOG 6 and stim 7.
    let rec = recording_with(6, 60, true);
    let mut view = ViewState::new(&rec);
    assert_eq!(view.group_of(ChannelId(7)), (Some(3), ChannelKind::Stim));
    assert!(view.move_channel(ChannelId(2), Some(ChannelId(0))));
    assert!(view.move_channel(ChannelId(0), None));
    assert_eq!(view.order(), ids(&[2, 1, 0, 3, 4, 5, 6, 7]));
    // Not into another stream.
    assert!(!view.move_channel(ChannelId(0), Some(ChannelId(4))));
    assert_eq!(view.order(), ids(&[2, 1, 0, 3, 4, 5, 6, 7]));

    assert!(view.move_stream(3, Some(0)));
    assert_eq!(view.order(), ids(&[6, 7, 2, 1, 0, 3, 4, 5]));
    assert!(view.move_stream(0, None));
    assert_eq!(view.order(), ids(&[6, 7, 3, 4, 5, 2, 1, 0]));
    // Streams without channels don't move, nor do others before them.
    assert!(!view.move_stream(1, None));
    assert!(!view.move_stream(0, Some(1)));
    assert!(!view.move_stream(9, None));
    assert_eq!(view.order(), ids(&[6, 7, 3, 4, 5, 2, 1, 0]));

    // A hidden channel keeps its place and moves with its stream.
    view.set_shown(ChannelId(4), false);
    assert_eq!(view.stream_shown(2), (2, 3));
    assert_eq!(view.stream_shown(1), (0, 0));
    assert!(view.move_stream(2, None));
    assert_eq!(view.order(), ids(&[6, 7, 2, 1, 0, 3, 4, 5]));
    assert_eq!(view.visible_channels(), ids(&[6, 7, 2, 1, 0, 3, 5]));
    assert_eq!(view.row_of(ChannelId(4)), None);
    assert_eq!(view.row_of(ChannelId(5)), Some(6));

    // Types move within their stream: stim before EOG, not before another stream's EEG.
    assert!(view.move_kind(Some(3), ChannelKind::Stim, Some(ChannelKind::Eog)));
    assert!(!view.move_kind(Some(3), ChannelKind::Stim, Some(ChannelKind::Eeg)));
    assert_eq!(view.order(), ids(&[7, 6, 2, 1, 0, 3, 4, 5]));
    // A stream's only type stays where it is.
    assert!(view.move_kind(Some(0), ChannelKind::Eeg, None));
    assert_eq!(view.order(), ids(&[7, 6, 2, 1, 0, 3, 4, 5]));
    // Each type's channels, then each stream's types, then the streams back in order.
    view.reset_order();
    assert_eq!(view.order(), ids(&[0, 1, 2, 3, 4, 5, 6, 7]));

    // A stream's channels shown and hidden together.
    view.set_stream_shown(2, false);
    assert_eq!(view.stream_shown(2), (0, 3));
    assert_eq!(view.visible_channels(), ids(&[0, 1, 2, 6, 7]));
    view.set_stream_shown(2, true);
    assert_eq!(view.stream_shown(2), (3, 3));
    assert_eq!(view.row_count(), 8);
    view.set_stream_shown(1, false);
    assert_eq!(view.row_count(), 8);

    // A stream of one channel: moving it within its stream leaves it where it is.
    let rec = recording_with(2, 60, true);
    let mut view = ViewState::new(&rec);
    assert!(view.move_stream(0, None));
    assert!(view.move_channel(ChannelId(0), None));
    assert_eq!(view.order(), ids(&[1, 2, 3, 0]));
}

#[test]
fn the_rows_on_screen_stay_while_channels_move() {
    let (_, mut view) = state();
    view.set_top(5.0);
    // The top channel goes to the end: the next one is first.
    view.move_channel(ChannelId(5), None);
    assert_eq!(view.top(), 5.0);
    assert_eq!(view.visible_channels()[..2], ids(&[6, 7]));
    // One from below goes to the top of the screen.
    view.move_channel(ChannelId(20), Some(ChannelId(6)));
    assert_eq!(view.visible_channels()[..2], ids(&[20, 6]));
    view.reset_order();
    assert_eq!(view.top(), 5.0);
    assert_eq!(view.visible_channels()[0], ChannelId(5));
}

#[test]
fn types_are_shown_and_hidden_together() {
    // 30 EEG channels, then EOG and stim.
    let (_, mut view) = state();
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (30, 30));
    assert_eq!(view.group_shown(None, ChannelKind::Eog), (1, 1));
    assert_eq!(view.group_shown(None, ChannelKind::Ecg), (0, 0));
    view.set_shown(ChannelId(4), false);
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (29, 30));

    view.set_group_shown(None, ChannelKind::Eeg, false);
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (0, 30));
    assert_eq!(view.visible_channels(), ids(&[30, 31]));
    assert_eq!(view.rows(), 2);
    assert_eq!(view.top(), 0.0);

    // Nothing shown: no rows, and nothing breaks.
    view.set_group_shown(None, ChannelKind::Eog, false);
    view.set_group_shown(None, ChannelKind::Stim, false);
    assert_eq!(view.row_count(), 0);
    assert!(view.visible_channels().is_empty());
    assert_eq!(view.channel_at(10.0, 400.0), None);
    for action in Action::ALL {
        view.apply(action);
    }
    assert!(view.visible_channels().is_empty());

    // All back: the 20 rows chosen before, from the top.
    view.set_group_shown(None, ChannelKind::Eeg, true);
    assert_eq!(view.group_shown(None, ChannelKind::Eeg), (30, 30));
    assert_eq!(view.rows(), DEFAULT_ROWS);
    assert_eq!(view.visible_rows(), 0..20);
}

#[test]
fn the_chosen_number_of_rows_returns_with_hidden_channels() {
    let (_, mut view) = state();
    view.set_rows(10);
    view.set_group_shown(None, ChannelKind::Eeg, false);
    assert_eq!(view.rows(), 2);
    // More rows than there are channels keeps the choice.
    act(&mut view, Action::MoreChannels);
    assert_eq!(view.rows(), 2);
    view.set_group_shown(None, ChannelKind::Eeg, true);
    assert_eq!(view.rows(), 10);

    // Fewer rows than channels is a new choice.
    view.set_group_shown(None, ChannelKind::Eeg, false);
    act(&mut view, Action::FewerChannels);
    assert_eq!(view.rows(), 1);
    view.set_group_shown(None, ChannelKind::Eeg, true);
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
    ShowGroup(Option<usize>, ChannelKind, bool),
    ScrollTo(u32),
    MoveChannel(u32, Option<u32>),
    MoveKind(Option<usize>, ChannelKind, Option<ChannelKind>),
    MoveStream(usize, Option<usize>),
    Reset,
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
        (proptest::option::of(0usize..5), kinds(), any::<bool>())
            .prop_map(|(s, k, on)| Step::ShowGroup(s, k, on)),
        (0u32..45).prop_map(Step::ScrollTo),
        (0u32..45, proptest::option::of(0u32..45)).prop_map(|(c, b)| Step::MoveChannel(c, b)),
        (
            proptest::option::of(0usize..5),
            kinds(),
            proptest::option::of(kinds())
        )
            .prop_map(|(s, k, b)| Step::MoveKind(s, k, b)),
        (0usize..5, proptest::option::of(0usize..5)).prop_map(|(s, b)| Step::MoveStream(s, b)),
        Just(Step::Reset),
    ]
}

/// The recordings' types, and one they don't have.
fn kinds() -> impl Strategy<Value = ChannelKind> + Clone {
    proptest::sample::select(vec![
        ChannelKind::Eeg,
        ChannelKind::Eog,
        ChannelKind::Stim,
        ChannelKind::Ecg,
    ])
}

/// The stream of each channel of `rec` (all `None` without streams).
fn streams_of(rec: &Recording) -> Vec<Option<usize>> {
    let mut of = vec![None; rec.channels().len()];
    for (s, stream) in rec.streams().iter().enumerate() {
        if let StreamContent::Channels(channels) = &stream.content {
            for c in channels {
                of[c.0 as usize] = Some(s);
            }
        }
    }
    of
}

/// `order` without the channels `out` leaves out.
fn without(order: &[ChannelId], out: impl Fn(ChannelId) -> bool) -> Vec<ChannelId> {
    order.iter().copied().filter(|&c| !out(c)).collect()
}

/// The keys of `order`'s runs of channels with the same key, top to bottom.
fn runs<K: PartialEq>(order: &[ChannelId], key: impl Fn(ChannelId) -> K) -> Vec<K> {
    let mut runs: Vec<K> = order.iter().map(|&c| key(c)).collect();
    runs.dedup();
    runs
}

proptest! {
    #[test]
    fn always_stays_inside_the_recording(
        shape in (1u32..40, 1u64..700, any::<bool>()),
        steps in proptest::collection::vec(steps(), 1..40),
    ) {
        let rec = recording_with(shape.0, shape.1, shape.2);
        let mut view = ViewState::new(&rec);
        let area = Area { width: 1000.0, height: 400.0 };
        let duration = rec.duration();
        let channels = rec.channels().len();
        let of = streams_of(&rec);
        let has_channels = |s: usize| of.contains(&Some(s));
        let stream = |c: ChannelId| of[c.0 as usize];
        let group = |c: ChannelId| (of[c.0 as usize], rec.channel(c).kind);
        for step in steps {
            let before = view.order().to_vec();
            let top = view.top();
            match step {
                Step::Act(action) => {
                    view.apply(action);
                }
                Step::Gesture(gesture, x) => view.gesture(gesture, area, x),
                Step::Pan(seconds) => view.pan(seconds),
                Step::Zoom(factor, anchor) => view.zoom(factor, anchor),
                Step::Rows(n) => view.set_rows(n),
                Step::Show(c, on) => view.set_shown(ChannelId(c), on),
                Step::ShowGroup(s, kind, on) => {
                    let shown_before: Vec<bool> = (0..channels as u32).map(|c| view.is_shown(ChannelId(c))).collect();
                    view.set_group_shown(s, kind, on);
                    for c in 0..channels as u32 {
                        let expected = if group(ChannelId(c)) == (s, kind) { on } else { shown_before[c as usize] };
                        prop_assert_eq!(view.is_shown(ChannelId(c)), expected);
                    }
                    let all = (0..channels as u32).filter(|&c| group(ChannelId(c)) == (s, kind)).count();
                    prop_assert_eq!(view.group_shown(s, kind), (if on { all } else { 0 }, all));
                }
                Step::ScrollTo(c) => {
                    let shown = view.is_shown(ChannelId(c));
                    prop_assert_eq!(view.scroll_to(ChannelId(c)), shown);
                    if shown {
                        let first = view.visible_channels()[0];
                        prop_assert!(first == ChannelId(c) || view.visible_rows().end == view.row_count());
                    }
                }
                Step::MoveChannel(c, b) => {
                    let valid = (c as usize) < channels
                        && b.is_none_or(|b| (b as usize) < channels && group(ChannelId(b)) == group(ChannelId(c)));
                    prop_assert_eq!(view.move_channel(ChannelId(c), b.map(ChannelId)), valid);
                    let order = view.order();
                    if valid {
                        // The others keep their order; the channel is where it was put.
                        let moved = |x: ChannelId| x == ChannelId(c);
                        prop_assert_eq!(without(order, moved), without(&before, moved));
                        let at = order.iter().position(|&x| x == ChannelId(c)).unwrap();
                        match b {
                            Some(b) if b != c => prop_assert_eq!(order[at + 1], ChannelId(b)),
                            Some(_) => prop_assert_eq!(order, &before[..]),
                            None => prop_assert!(order[at + 1..].iter().all(|&x| group(x) != group(ChannelId(c)))),
                        }
                    } else {
                        prop_assert_eq!(order, &before[..]);
                    }
                    prop_assert_eq!(view.top(), top);
                }
                Step::MoveStream(s, b) => {
                    let valid = has_channels(s) && b.is_none_or(has_channels);
                    prop_assert_eq!(view.move_stream(s, b), valid);
                    let order = view.order();
                    let ours = |x: ChannelId| of[x.0 as usize] == Some(s);
                    // Each stream keeps its channels' order.
                    prop_assert_eq!(without(order, |x| !ours(x)), without(&before, |x| !ours(x)));
                    prop_assert_eq!(without(order, ours), without(&before, ours));
                    if valid && b != Some(s) {
                        let runs = runs(order, stream);
                        let at = runs.iter().position(|&r| r == Some(s)).unwrap();
                        prop_assert_eq!(runs.get(at + 1).copied().flatten(), b);
                    } else {
                        prop_assert_eq!(order, &before[..]);
                    }
                    prop_assert_eq!(view.top(), top);
                }
                Step::MoveKind(s, k, b) => {
                    let has = |k: ChannelKind| (0..channels as u32).any(|c| group(ChannelId(c)) == (s, k));
                    let valid = has(k) && b.is_none_or(has);
                    prop_assert_eq!(view.move_kind(s, k, b), valid);
                    let order = view.order();
                    let ours = |x: ChannelId| group(x) == (s, k);
                    // The type keeps its channels' order, the others theirs.
                    prop_assert_eq!(without(order, |x| !ours(x)), without(&before, |x| !ours(x)));
                    prop_assert_eq!(without(order, ours), without(&before, ours));
                    if valid && b != Some(k) {
                        // Followed by the type it was put before, or by no channel of its
                        // stream.
                        let runs = runs(order, group);
                        let at = runs.iter().position(|&r| r == (s, k)).unwrap();
                        match b {
                            Some(b) => prop_assert_eq!(runs.get(at + 1).copied(), Some((s, b))),
                            None => prop_assert!(runs[at + 1..].iter().all(|r| r.0 != s)),
                        }
                    } else {
                        prop_assert_eq!(order, &before[..]);
                    }
                    prop_assert_eq!(view.top(), top);
                }
                Step::Reset => {
                    view.reset_order();
                    // The streams in file order, each by type, a type in file order.
                    let key = |c: ChannelId| (stream(c), kind_index(view.kind_of(c)), c);
                    for pair in view.order().windows(2) {
                        prop_assert!(key(pair[0]) < key(pair[1]), "{:?}", view.order());
                    }
                    prop_assert_eq!(view.top(), top);
                }
            }
            // Every channel once, each stream's channels together, and within it each type's.
            let mut sorted = view.order().to_vec();
            sorted.sort_unstable();
            prop_assert_eq!(sorted, (0..channels as u32).map(ChannelId).collect::<Vec<_>>());
            let streams = runs(view.order(), stream);
            let groups = runs(view.order(), |c| (stream(c), kind_index(rec.channel(c).kind)));
            for (runs, distinct) in [
                (streams.len(), streams.iter().collect::<std::collections::BTreeSet<_>>().len()),
                (groups.len(), groups.iter().collect::<std::collections::BTreeSet<_>>().len()),
            ] {
                prop_assert_eq!(runs, distinct, "{:?}", view.order());
            }
            // The rows are the shown channels in that order.
            let shown: Vec<_> = view.order().iter().copied().filter(|&c| view.is_shown(c)).collect();
            prop_assert_eq!(view.row_count(), shown.len());
            prop_assert_eq!(view.visible_channels(), &shown[view.visible_rows()]);
            for c in 0..channels as u32 {
                prop_assert_eq!(view.row_of(ChannelId(c)), shown.iter().position(|&x| x == ChannelId(c)));
            }
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
    // On from the start. Each visible channel but stim ones, top to bottom, over the samples in view of its
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
    view.set_dc_removed(false);
    assert_eq!(view.dc_request(&rec), None);
    view.set_dc_removed(true);
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

#[test]
fn channels_the_file_marks_bad_start_out_bad() {
    // A FIFF file with two bad channels: MEG 0113 and EEG 003.
    let file = kymo_testgen::TempFile::preset("fif-float32-meg");
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    let mut view = ViewState::new(&rec);
    assert_eq!(
        view.bad_channels().collect::<Vec<_>>(),
        [ChannelId(2), ChannelId(6)]
    );
    // They are marked good like any other.
    view.toggle_bad(ChannelId(2));
    assert_eq!(view.bad_channels().collect::<Vec<_>>(), [ChannelId(6)]);
}
