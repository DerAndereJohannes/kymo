//! Trace colors per channel type, close to MNE's, and annotation colors, for light and dark
//! themes.

use eframe::egui::{Color32, Visuals};

use crate::annotations::{AnnotationColor, COLORS};
use crate::view::{KINDS, kind_index};
use kymo_core::ChannelKind;

/// Colors picked per channel type, in [`KINDS`] order; `None` keeps the default.
pub type TypeColors = [Option<Color32>; KINDS.len()];

/// The color of a channel type's traces and names: the one picked for it, used as it is
/// in every theme, or the default ([`trace_color`]).
pub fn type_color(kind: ChannelKind, picked: &TypeColors, visuals: &Visuals) -> Color32 {
    picked[kind_index(kind)].unwrap_or_else(|| trace_color(kind, visuals))
}

/// The default color of a channel type's traces and names, close to MNE's: EOG blue, ECG
/// magenta, stim amber, magnetometers teal, MEG references steel blue, sEEG brown and DBS
/// green; the rest in the theme's text color. (MNE draws magnetometers dark blue and
/// gradiometers blue, which kymo's blue EOG would be mistaken for.) Dark themes get lighter
/// variants.
pub fn trace_color(kind: ChannelKind, visuals: &Visuals) -> Color32 {
    let dark = visuals.dark_mode;
    let rgb = |light: (u8, u8, u8), lighter: (u8, u8, u8)| {
        let (r, g, b) = if dark { lighter } else { light };
        Color32::from_rgb(r, g, b)
    };
    match kind {
        ChannelKind::Eog => rgb((0, 60, 220), (110, 155, 255)),
        ChannelKind::Ecg => rgb((185, 0, 185), (230, 115, 230)),
        ChannelKind::Stim => rgb((200, 100, 0), (240, 160, 50)),
        ChannelKind::Mag => rgb((0, 120, 130), (70, 200, 210)),
        ChannelKind::RefMeg => rgb((70, 130, 180), (100, 170, 220)),
        ChannelKind::Seeg => rgb((139, 69, 19), (210, 145, 90)),
        ChannelKind::Dbs => rgb((30, 140, 80), (110, 210, 150)),
        _ => visuals.text_color(),
    }
}

/// The color of bad channels' traces and names, whatever their type.
pub fn bad_color(visuals: &Visuals) -> Color32 {
    visuals.weak_text_color().gamma_multiply(0.6)
}

/// Matplotlib's default color cycle ("tab10"), which MNE's browsers use for annotations.
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

/// The color of an annotation color: a palette color (its index taken modulo the number
/// of colors), lighter in dark themes, or a picked one as it is.
pub fn annotation_color(color: AnnotationColor, visuals: &Visuals) -> Color32 {
    match color {
        AnnotationColor::Palette(index) => {
            let color = ANNOTATION_COLORS[index % COLORS];
            if visuals.dark_mode {
                color.lerp_to_gamma(Color32::WHITE, 0.25)
            } else {
                color
            }
        }
        AnnotationColor::Custom([r, g, b]) => Color32::from_rgb(r, g, b),
    }
}
