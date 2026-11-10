//! Time axis ticks and labels.

use kymo::ui::axis::{Clock, format_time, step, ticks};
use proptest::prelude::*;

/// Seconds from a label as the axis writes it ("12.5 s", "2:30", "1:02:03.25").
fn parse(label: &str) -> f64 {
    if let Some(seconds) = label.strip_suffix(" s") {
        return seconds.parse().unwrap();
    }
    label.split(':').fold(0.0, |total, part| {
        total * 60.0 + part.parse::<f64>().unwrap()
    })
}

/// Whether `step` is a round step: 1, 2 or 5 × 10ᵏ s below a second, a listed step up to a
/// day, whole days × 1, 2 or 5 × 10ᵏ above.
fn is_round(step: f64) -> bool {
    let one_two_five = |x: f64| {
        let m = x / 10f64.powf(x.log10().floor());
        [1.0, 2.0, 5.0, 10.0].iter().any(|r| (m - r).abs() < 1e-9)
    };
    let listed = [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0,
        10800.0, 21600.0, 43200.0, 86400.0,
    ];
    if step < 1.0 {
        one_two_five(step)
    } else if step <= 86400.0 {
        listed.contains(&step)
    } else {
        one_two_five(step / 86400.0)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn ticks_are_round_spaced_and_labeled_with_their_times(
        start in prop_oneof![Just(0.0), 0.0..100_000.0f64],
        span_log in -3.0..5.5f64,
        width in 50.0..4000.0f64,
        min_spacing in 30.0..150.0f64,
    ) {
        let span = 10f64.powf(span_log);
        let time = start..start + span;
        let ticks = ticks(time.clone(), width, min_spacing);
        let step = step(span, width, min_spacing).unwrap();
        prop_assert!(is_round(step), "step {step}");
        // Never denser than asked, and never more than 2.5 times sparser (the largest
        // ratio between neighboring round steps).
        let px = step / span * width;
        prop_assert!(px >= min_spacing * (1.0 - 1e-9), "{px} px apart");
        prop_assert!(px <= min_spacing * 2.5 * (1.0 + 1e-9), "{px} px apart");
        if px * 1.001 < width {
            prop_assert!(!ticks.is_empty());
        }
        // Times as far from 0 as a day are only as exact as their last bits, which can be
        // more than a millionth of a microsecond step.
        let slack = |t: f64| step * 1e-6 + 4.0 * f64::EPSILON * t.abs();
        for pair in ticks.windows(2) {
            prop_assert!(((pair[1].time - pair[0].time) - step).abs() <= slack(pair[1].time));
            prop_assert_ne!(&pair[0].label, &pair[1].label);
        }
        for tick in &ticks {
            prop_assert!(time.contains(&tick.time) || tick.time == time.end);
            prop_assert!((tick.time - (tick.time / step).round() * step).abs() <= slack(tick.time));
            let read = parse(&tick.label);
            prop_assert!(
                (read - tick.time).abs() <= step * 1e-3,
                "{} reads as {read}, not {}", tick.label, tick.time
            );
        }
    }
}

#[test]
fn labels_follow_the_view_length_and_step() {
    let labels = |time: std::ops::Range<f64>, width: f64| -> Vec<String> {
        ticks(time, width, 80.0)
            .into_iter()
            .map(|t| t.label)
            .collect()
    };
    assert_eq!(
        labels(0.0..20.0, 800.0),
        [
            "0 s", "2 s", "4 s", "6 s", "8 s", "10 s", "12 s", "14 s", "16 s", "18 s", "20 s"
        ]
    );
    assert_eq!(labels(1.0..1.5, 800.0)[..3], ["1.00 s", "1.05 s", "1.10 s"]);
    assert_eq!(labels(600.0..900.0, 400.0)[..2], ["10:00", "11:00"]);
    assert_eq!(labels(3600.0..3700.0, 400.0)[..2], ["1:00:00", "1:00:30"]);
    assert_eq!(
        labels(3600.0..3601.0, 400.0)[..2],
        ["1:00:00.0", "1:00:00.2"]
    );
    assert_eq!(labels(0.0..8.0 * 3600.0, 800.0)[1], "1:00:00");
}

#[test]
fn times_round_once_and_carry() {
    assert_eq!(format_time(59.96, 1, Clock::Minutes), "1:00.0");
    assert_eq!(format_time(3599.9996, 3, Clock::Hours), "1:00:00.000");
    assert_eq!(format_time(12.5, 3, Clock::Seconds), "12.500 s");
    assert_eq!(format_time(90061.5, 1, Clock::Hours), "25:01:01.5");
}

#[test]
fn nothing_to_divide_gives_no_ticks() {
    assert!(ticks(5.0..5.0, 800.0, 80.0).is_empty());
    assert!(ticks(0.0..10.0, 0.0, 80.0).is_empty());
    assert!(ticks(0.0..f64::INFINITY, 800.0, 80.0).is_empty());
    assert!(ticks(f64::NAN..1.0, 800.0, 80.0).is_empty());
}

#[test]
fn times_parse_as_seconds_minutes_or_hours() {
    use kymo::ui::axis::parse_time;
    assert_eq!(parse_time("90"), Some(90.0));
    assert_eq!(parse_time(" 12.5 s "), Some(12.5));
    assert_eq!(parse_time("12,5"), Some(12.5));
    assert_eq!(parse_time("1:30"), Some(90.0));
    assert_eq!(parse_time("1:30.25"), Some(90.25));
    assert_eq!(parse_time("1:02:03.5"), Some(3723.5));
    assert_eq!(parse_time("0:00:00"), Some(0.0));
    // Minutes beyond an hour are fine in front, not after a colon.
    assert_eq!(parse_time("75:00"), Some(4500.0));
    for bad in [
        "", "s", "abc", "1:60", "1:5", "1:02:60", "-3", "1::2", "1:2:3:4", "nan", "inf",
    ] {
        assert_eq!(parse_time(bad), None, "{bad:?}");
    }
}

proptest! {
    /// What the axis writes reads back as its time.
    #[test]
    fn written_times_parse_back(units in 0u64..100_000_000, decimals in 0usize..4) {
        use kymo::ui::axis::parse_time;
        let t = units as f64 / 1000.0;
        let t = (t * 10f64.powi(decimals as i32)).round() / 10f64.powi(decimals as i32);
        for clock in [Clock::Seconds, Clock::Minutes, Clock::Hours] {
            let text = format_time(t, decimals, clock);
            let back = parse_time(&text).unwrap();
            prop_assert!((back - t).abs() < 1e-6, "{text} read as {back}, not {t}");
        }
    }
}
