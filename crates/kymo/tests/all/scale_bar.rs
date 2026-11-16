//! Scale bars: which block of rows each type's bar keeps to, where it sits as the rows
//! scroll, the round value each stands for, and its label.

use std::ops::Range;

use kymo::action::Action;
use kymo::scale_bar::{ScaleBar, round_below, scale_bars};
use kymo::view::{ScaleGroup, ViewState};
use kymo_core::{
    BlockReader, Channel, ChannelId, ChannelKind, Grid, GridId, Header, Opened, ReadError,
    Recording, SampleType, SamplesMut, Stream, StreamContent, Timebase, Unit,
};
use proptest::prelude::*;

use crate::view::recording;

/// Scale bars never read samples.
struct NoReader;

impl BlockReader for NoReader {
    fn read(
        &self,
        _: GridId,
        _: &[ChannelId],
        _: Range<u64>,
        _: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        unreachable!("scale bars never read samples")
    }
}

/// A channel of each of `kinds`, in that order, all in µV at 100 Hz for a minute, in
/// streams of the channels in `streams` (none: no streams).
fn recording_in(kinds: &[ChannelKind], streams: &[Range<u32>]) -> Recording {
    let channels = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| {
            let mut ch = Channel::new(format!("{kind:?} {i}"), GridId(0));
            ch.kind = kind.clone();
            ch.unit = Unit {
                label: "uV".into(),
                to_si: Some(1e-6),
            };
            ch
        })
        .collect();
    let ids = (0..kinds.len() as u32).map(ChannelId).collect();
    let timebase = Timebase::Regular {
        rate: 100.0,
        start: 0.0,
    };
    let grid = Grid::continuous(timebase, 6000, SampleType::I16, ids).unwrap();
    let opened = Opened {
        header: Header {
            format: "TEST",
            start_time: None,
            channels,
            grids: vec![grid],
            extra: vec![],
            bad_channels: Vec::new(),
            streams: streams
                .iter()
                .enumerate()
                .map(|(i, r)| Stream {
                    id: i as u32 + 1,
                    name: format!("Stream {i}"),
                    kind: String::new(),
                    content: StreamContent::Channels(r.clone().map(ChannelId).collect()),
                })
                .collect(),
            utc_offset: None,
        },
        annotations: vec![],
        warnings: vec![],
        sources: vec![],
        reader: Box::new(NoReader),
    };
    Recording::new(opened).unwrap().0
}

/// 10 EEG, 2 EOG, 10 EEG and a stim channel, 4 rows on screen; in two streams, the
/// second starting at the second EEG block, as a type's channels stay together within a
/// stream.
fn blocks() -> (Recording, ViewState) {
    use crate::kinds::{Eeg, Eog, Stim};
    let kinds: Vec<_> = [(Eeg, 10), (Eog, 2), (Eeg, 10), (Stim, 1)]
        .into_iter()
        .flat_map(|(kind, n)| std::iter::repeat_n(kind, n))
        .collect();
    let rec = recording_in(&kinds, &[0..12, 12..23]);
    let mut view = ViewState::new(&rec);
    view.set_rows(4);
    (rec, view)
}

/// The bars as (type, channel, middle), the middle to 9 decimals.
fn placed(rec: &Recording, view: &ViewState) -> Vec<(ChannelKind, u32, f64)> {
    scale_bars(rec, view)
        .into_iter()
        .map(|b| (b.kind.clone(), b.channel.0, (b.middle * 1e9).round() / 1e9))
        .collect()
}

#[test]
fn each_type_on_screen_gets_one_but_stim() {
    // EEG 0..3 in µV, EOG in mV, Status.
    let rec = recording(3, 60);
    let view = ViewState::new(&rec);
    let bars = scale_bars(&rec, &view);
    let got: Vec<_> = bars
        .iter()
        .map(|b| {
            (
                b.kind.clone(),
                b.channel.0,
                b.middle,
                b.value,
                b.unit.as_str(),
            )
        })
        .collect();
    // EEG at 20 µV per spacing: ±20 µV spans the row, so 20 µV is half of it. EOG at
    // 150 µV, shown in µV although stored in mV: 200 µV of its 300.
    assert_eq!(
        got,
        [
            (ChannelKind::EEG, 0, 0.5, 20.0, "µV"),
            (ChannelKind::EOG, 3, 3.5, 200.0, "µV"),
        ]
    );
    assert!((bars[0].fraction - 0.5).abs() < 1e-12);
    assert!((bars[1].fraction - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(bars[0].label(), "20 µV");
}

#[test]
fn a_bar_stays_put_while_its_block_scrolls_and_leaves_with_it() {
    use crate::kinds::{Eeg, Eog};
    let (rec, mut view) = blocks();
    // Within the first EEG block, at the top row's middle wherever the rows are.
    for step in 0..=80 {
        view.set_top(f64::from(step) / 10.0);
        assert_eq!(
            placed(&rec, &view)[0],
            (Eeg, view.visible_channels()[0].0, 0.5)
        );
    }
    // The block's last row rises above the top row's middle and pushes it up, until less
    // than the bar's half (a quarter row) is left above; then the next EEG block has it.
    let eeg = |view: &ViewState| placed(&rec, view).into_iter().find(|b| b.0 == Eeg);
    for (top, middle) in [(9.0, 0.5), (9.1, 0.4), (9.2, 0.3), (9.25, 0.25)] {
        view.set_top(top);
        assert_eq!(eeg(&view), Some((Eeg, 9, middle)), "top {top}");
    }
    view.set_top(9.3);
    // Rows 12.. are EEG again: its first row's middle, 12.5 − 9.3.
    assert_eq!(eeg(&view), Some((Eeg, 12, 3.2)));
    // The EOG block, at its first row's middle once all of the bar shows.
    let eog = |view: &ViewState| placed(&rec, view).into_iter().find(|b| b.0 == Eog);
    view.set_top(6.0);
    assert_eq!(eog(&view), None, "below the screen");
    view.set_top(6.8);
    // Row 10's middle, 3.7 rows down, leaves too little room for a bar of 2/3 row.
    assert_eq!(eog(&view), None);
    view.set_top(7.0);
    assert_eq!(eog(&view), Some((Eog, 10, 3.5)));
    view.set_top(11.0);
    assert_eq!(eog(&view), Some((Eog, 11, 0.5)));
    // Stim, the last row, never.
    view.apply(Action::LastChannels);
    assert!(placed(&rec, &view).iter().all(|b| b.0 != ChannelKind::STIM));
}

#[test]
fn bad_rows_keep_it_and_hidden_ones_join_blocks() {
    use crate::kinds::Eeg;
    let (rec, mut view) = blocks();
    view.toggle_bad(ChannelId(0));
    assert_eq!(placed(&rec, &view)[0], (Eeg, 0, 0.5));
    view.toggle_bad(ChannelId(0));
    // Without the EOG rows the two EEG blocks are one, which keeps the bar at the top.
    view.set_shown(ChannelId(10), false);
    view.set_shown(ChannelId(11), false);
    view.set_top(9.3);
    assert_eq!(view.top(), 9.3);
    assert_eq!(placed(&rec, &view)[0], (Eeg, 9, 0.5));
}

#[test]
fn the_value_follows_the_scale() {
    let rec = recording(2, 60);
    let mut view = ViewState::new(&rec);
    for _ in 0..12 {
        view.apply(Action::ScaleUp);
        let bar = scale_bars(&rec, &view).into_iter().next().unwrap();
        let span = 2.0 * view.amplitude(&ChannelKind::EEG) * 1e6;
        assert_eq!(Some(bar.value), round_below(span));
        assert!((bar.fraction - bar.value / span).abs() < 1e-12);
    }
}

#[test]
fn labels_have_the_decimals_they_need() {
    let label = |value: f64, unit: &str| {
        ScaleBar {
            kind: ChannelKind::EEG,
            group: ScaleGroup::Channel(ChannelId(0)),
            channel: ChannelId(0),
            middle: 0.5,
            value,
            unit: unit.into(),
            fraction: 1.0,
        }
        .label()
    };
    assert_eq!(label(20.0, "µV"), "20 µV");
    assert_eq!(label(0.5, "mV"), "0.5 mV");
    assert_eq!(label(0.002, "V"), "0.002 V");
    assert_eq!(label(1e-3, "V"), "0.001 V");
    assert_eq!(label(1000.0, "fT"), "1000 fT");
    assert_eq!(label(5.0, ""), "5");
    assert_eq!(round_below(0.0), None);
    assert_eq!(round_below(-1.0), None);
    assert_eq!(round_below(f64::NAN), None);
    assert_eq!(round_below(f64::INFINITY), None);
    assert_eq!(round_below(40.0), Some(20.0));
    assert_eq!(round_below(100.0), Some(100.0));
    assert_eq!(round_below(4.99), Some(2.0));
}

proptest! {
    /// 1, 2 or 5 times a power of ten, up to the limit and more than its 2.5th part.
    #[test]
    fn round_values_are_one_two_or_five_below_the_limit(exponent in -15.0..15.0f64) {
        let limit = 10f64.powf(exponent);
        let value = round_below(limit).unwrap();
        prop_assert!(value <= limit * (1.0 + 1e-12), "{value} > {limit}");
        prop_assert!(value * 2.5 > limit * (1.0 - 1e-12), "{value} too small for {limit}");
        let mantissa = value / 10f64.powf(value.log10().floor());
        let round = [1.0, 2.0, 5.0, 10.0].iter().any(|m| (mantissa - m).abs() < 1e-9);
        prop_assert!(round, "{value} ({mantissa})");
    }
}
