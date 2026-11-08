//! Trace colours per channel type, close to MNE's, for light and dark themes.

use eframe::egui::{Color32, Visuals};
use kymo_core::ChannelKind;

/// The colour of a channel type's traces and names: EEG, EMG and misc in the theme's text
/// colour, EOG blue, ECG magenta and stim amber. Dark themes get lighter variants.
pub fn trace_color(kind: ChannelKind, visuals: &Visuals) -> Color32 {
    let dark = visuals.dark_mode;
    match kind {
        ChannelKind::Eog if dark => Color32::from_rgb(110, 155, 255),
        ChannelKind::Eog => Color32::from_rgb(0, 60, 220),
        ChannelKind::Ecg if dark => Color32::from_rgb(230, 115, 230),
        ChannelKind::Ecg => Color32::from_rgb(185, 0, 185),
        ChannelKind::Stim if dark => Color32::from_rgb(240, 160, 50),
        ChannelKind::Stim => Color32::from_rgb(200, 100, 0),
        _ => visuals.text_color(),
    }
}

/// The colour of bad channels' traces and names, whatever their type.
pub fn bad_color(visuals: &Visuals) -> Color32 {
    visuals.weak_text_color().gamma_multiply(0.6)
}
