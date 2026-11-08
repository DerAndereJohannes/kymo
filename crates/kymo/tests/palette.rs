//! Trace colours per channel type, in light and dark themes.

use eframe::egui::{Color32, Visuals};
use kymo::ui::palette::{bad_color, trace_color};
use kymo_core::ChannelKind;

/// Sum of the channel differences, ignoring alpha.
fn distance(a: Color32, b: Color32) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y));
    d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
}

/// `color` drawn over `background`.
fn over(color: Color32, background: Color32) -> Color32 {
    let a = f32::from(color.a()) / 255.0;
    // Premultiplied: the colour's channels already carry its alpha.
    let mix = |c: u8, b: u8| (f32::from(c) + f32::from(b) * (1.0 - a)).round() as u8;
    Color32::from_rgb(
        mix(color.r(), background.r()),
        mix(color.g(), background.g()),
        mix(color.b(), background.b()),
    )
}

#[test]
fn types_are_told_apart_in_both_themes() {
    for visuals in [Visuals::light(), Visuals::dark()] {
        let background = visuals.extreme_bg_color;
        let text = visuals.text_color();
        for kind in [ChannelKind::Eeg, ChannelKind::Emg, ChannelKind::Misc] {
            assert_eq!(trace_color(kind, &visuals), text, "{kind:?}");
        }
        let coloured = [ChannelKind::Eog, ChannelKind::Ecg, ChannelKind::Stim];
        let bad = over(bad_color(&visuals), background);
        for (i, &kind) in coloured.iter().enumerate() {
            let color = trace_color(kind, &visuals);
            assert!(distance(color, text) >= 60, "{kind:?} looks like text");
            assert!(
                distance(color, background) >= 150,
                "{kind:?} on the background"
            );
            assert!(distance(color, bad) >= 60, "{kind:?} looks bad");
            for &other in &coloured[i + 1..] {
                assert!(
                    distance(color, trace_color(other, &visuals)) >= 60,
                    "{kind:?} and {other:?}"
                );
            }
        }
        assert!(distance(text, bad) >= 60, "bad looks like text");
        assert!(distance(bad, background) >= 60, "bad disappears");
    }
}
