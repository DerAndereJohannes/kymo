//! Scale bars: which rows get one, the round value each stands for, and its label.

use kymo::action::Action;
use kymo::scale_bar::{ScaleBar, round_below, scale_bars};
use kymo::view::ViewState;
use kymo_core::{ChannelId, ChannelKind};
use proptest::prelude::*;

use crate::view::recording;

/// The bars as (type, channel, value, unit, fraction to 9 decimals).
fn bars(view: &ViewState, rec: &kymo_core::Recording) -> Vec<(ChannelKind, u32, f64, String, f64)> {
    scale_bars(rec, view)
        .into_iter()
        .map(|b| {
            let fraction = (b.fraction * 1e9).round() / 1e9;
            (b.kind, b.channel.0, b.value, b.unit, fraction)
        })
        .collect()
}

#[test]
fn each_type_on_screen_gets_one_beside_its_first_row() {
    // EEG 0..3 in µV, EOG in mV, Status.
    let rec = recording(3, 60);
    let view = ViewState::new(&rec);
    // EEG at 20 µV per spacing: ±20 µV spans the row, so 20 µV is half of it. EOG at
    // 150 µV, shown in µV although stored in mV: 200 µV of its 300.
    assert_eq!(
        bars(&view, &rec),
        vec![
            (ChannelKind::Eeg, 0, 20.0, "µV".into(), 0.5),
            (ChannelKind::Eog, 3, 200.0, "µV".into(), 0.666666667),
        ]
    );
    let all = scale_bars(&rec, &view);
    assert_eq!(all[0].row, 0);
    assert_eq!(all[1].row, 3);
    assert_eq!(all[0].label(), "20 µV");
}

#[test]
fn bad_hidden_and_scrolled_off_rows_get_none() {
    let rec = recording(30, 60);
    let first_eeg = |view: &ViewState| {
        scale_bars(&rec, view)
            .into_iter()
            .find(|b| b.kind == ChannelKind::Eeg)
            .map(|b| (b.channel.0, b.row))
    };
    let mut view = ViewState::new(&rec);
    assert_eq!(first_eeg(&view), Some((0, 0)));
    view.toggle_bad(ChannelId(0));
    assert_eq!(first_eeg(&view), Some((1, 1)));
    view.set_shown(ChannelId(1), false);
    // Channel 2 now shows in the second row.
    assert_eq!(first_eeg(&view), Some((2, 1)));
    // Row 3's middle is above the top: the next row gets it.
    view.set_top(3.6);
    assert_eq!(view.top(), 3.6);
    assert_eq!(first_eeg(&view), Some((5, 4)));
    // EOG and stim at the bottom: the EOG gets its bar once its row shows, stim never.
    view.apply(Action::LastChannels);
    let kinds: Vec<_> = scale_bars(&rec, &view)
        .into_iter()
        .map(|b| b.kind)
        .collect();
    assert_eq!(kinds, [ChannelKind::Eeg, ChannelKind::Eog]);
    // Every EEG channel on screen bad: none for EEG.
    for &channel in view.visible_channels().to_vec().iter() {
        if view.kind_of(channel) == ChannelKind::Eeg {
            view.toggle_bad(channel);
        }
    }
    let kinds: Vec<_> = scale_bars(&rec, &view)
        .into_iter()
        .map(|b| b.kind)
        .collect();
    assert_eq!(kinds, [ChannelKind::Eog]);
}

#[test]
fn the_value_follows_the_scale() {
    let rec = recording(2, 60);
    let mut view = ViewState::new(&rec);
    for _ in 0..12 {
        view.apply(Action::ScaleUp);
        let bar = scale_bars(&rec, &view).into_iter().next().unwrap();
        let span = 2.0 * view.amplitude(ChannelKind::Eeg) * 1e6;
        assert_eq!(Some(bar.value), round_below(span));
        assert!((bar.fraction - bar.value / span).abs() < 1e-12);
    }
}

#[test]
fn labels_have_the_decimals_they_need() {
    let label = |value: f64, unit: &str| {
        ScaleBar {
            kind: ChannelKind::Eeg,
            channel: ChannelId(0),
            row: 0,
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
