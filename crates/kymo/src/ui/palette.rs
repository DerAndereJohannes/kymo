//! Trace colours per channel type, close to MNE's, and annotation colours, for light and dark
//! themes.

use eframe::egui::{Color32, Visuals};

use crate::annotations::COLORS;
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

/// Matplotlib's default colour cycle ("tab10"), which MNE's browsers use for annotations.
const ANNOTATION_COLORS: [Color32; COLORS] = [
    Color32::from_rgb(31, 119, 180),
    Color32::from_rgb(255, 127, 14),
    Color32::from_rgb(44, 160, 44),
    Color32::from_rgb(214, 39, 40),
    Color32::from_rgb(148, 103, 189),
    Color32::from_rgb(140, 86, 75),
    Color32::from_rgb(227, 119, 194),
    Color32::from_rgb(127, 127, 127),
    Color32::from_rgb(188, 189, 34),
    Color32::from_rgb(23, 190, 207),
];

/// The colour of annotation colour `index` (taken modulo the number of colours), lighter
/// in dark themes.
pub fn annotation_color(index: usize, visuals: &Visuals) -> Color32 {
    let color = ANNOTATION_COLORS[index % COLORS];
    if visuals.dark_mode {
        color.lerp_to_gamma(Color32::WHITE, 0.25)
    } else {
        color
    }
}
