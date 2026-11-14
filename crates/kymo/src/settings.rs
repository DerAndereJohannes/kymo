//! What the user can set in the Settings window, kept between runs in eframe's storage.

use std::ops::RangeInclusive;

use eframe::Storage;
use eframe::egui::{self, Color32};
use kymo_core::ChannelKind;
use kymo_core::tile::DEFAULT_BUDGET;

use crate::annotations::{AnnotationColors, Rgb};
use crate::render::DEFAULT_GPU_BUDGET;
use crate::ui::palette::TypeColors;
use crate::view::{DEFAULT_SPAN, KINDS};

/// Megabytes the tile cache may take.
pub const TILE_CACHE_MB: RangeInclusive<u32> = 64..=65_536;
/// Megabytes of GPU memory the trace renderer may take for tiles.
pub const GPU_MEMORY_MB: RangeInclusive<u32> = 64..=16_384;
/// Seconds the default window may span.
pub const DEFAULT_WINDOW_S: RangeInclusive<f64> = 1.0..=3600.0;

const TILE_CACHE_KEY: &str = "kymo.tile_cache_mb";
const GPU_MEMORY_KEY: &str = "kymo.gpu_memory_mb";
const DEFAULT_WINDOW_KEY: &str = "kymo.default_window_s";
const ANTIALIAS_KEY: &str = "kymo.antialias";
const SCALE_BARS_KEY: &str = "kymo.scale_bars";
const REMOVE_DC_KEY: &str = "kymo.remove_dc";
const AUTO_SCALE_KEY: &str = "kymo.auto_scale_on_open";
const THEME_KEY: &str = "kymo.theme";
const SYSTEM_FONT_KEY: &str = "kymo.system_font";
const ANNOTATION_COLORS_KEY: &str = "kymo.annotation_colors";

/// The key of a channel type's color, by MNE's name of the type, e.g. `kymo.color.eeg` or
/// `kymo.color.ref_meg`.
fn type_color_key(kind: ChannelKind) -> String {
    format!("kymo.color.{}", kind.mne_name())
}

/// Light or dark: the system's choice, or one of them whatever the system uses. On
/// Windows, the system's choice is the default app mode (not the default Windows mode,
/// which is for the taskbar and Start), as for other apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    /// How it is kept in the storage.
    fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn parse(text: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == text.trim())
    }

    /// egui's look for it.
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        }
    }

    /// The window's title bar for it.
    pub fn window(self) -> egui::SystemTheme {
        match self {
            Theme::System => egui::SystemTheme::SystemDefault,
            Theme::Light => egui::SystemTheme::Light,
            Theme::Dark => egui::SystemTheme::Dark,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Budget of the tile cache, in MB (2^20 bytes).
    pub tile_cache_mb: u32,
    /// Budget for tiles on the GPU, in MB.
    pub gpu_memory_mb: u32,
    /// Seconds shown when a recording opens and after Default Window (⌘0).
    pub default_window_s: f64,
    /// Whether traces are anti-aliased.
    pub antialias: bool,
    /// Whether each channel type on screen shows a scale bar.
    pub scale_bars: bool,
    /// Whether each trace is centered on its mean over the window (DC removal).
    pub remove_dc: bool,
    /// Whether a recording opens with Auto Scale instead of the default amplitudes.
    pub auto_scale_on_open: bool,
    /// Colors picked per channel type; the others keep their defaults.
    pub type_colors: TypeColors,
    pub theme: Theme,
    /// Whether text is in the system's interface font rather than egui's (see
    /// [`crate::ui::fonts`]).
    pub system_font: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tile_cache_mb: (DEFAULT_BUDGET >> 20) as u32,
            gpu_memory_mb: (DEFAULT_GPU_BUDGET >> 20) as u32,
            default_window_s: DEFAULT_SPAN,
            antialias: false,
            scale_bars: true,
            remove_dc: true,
            auto_scale_on_open: true,
            type_colors: [None; KINDS.len()],
            theme: Theme::System,
            system_font: false,
        }
    }
}

impl Settings {
    /// The settings kept in `storage`; each one missing or unreadable is the default, and
    /// each one out of range the nearest allowed value.
    pub fn load(storage: Option<&dyn Storage>) -> Settings {
        let defaults = Settings::default();
        let get = |key: &str| storage.and_then(|s| s.get_string(key));
        Settings {
            tile_cache_mb: get(TILE_CACHE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.tile_cache_mb),
            gpu_memory_mb: get(GPU_MEMORY_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.gpu_memory_mb),
            default_window_s: get(DEFAULT_WINDOW_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.default_window_s),
            antialias: get(ANTIALIAS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.antialias),
            scale_bars: get(SCALE_BARS_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.scale_bars),
            remove_dc: get(REMOVE_DC_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.remove_dc),
            auto_scale_on_open: get(AUTO_SCALE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.auto_scale_on_open),
            type_colors: KINDS.map(|kind| {
                get(&type_color_key(kind))
                    .and_then(|v| parse_rgb(&v))
                    .map(|[r, g, b]| Color32::from_rgb(r, g, b))
            }),
            theme: get(THEME_KEY)
                .and_then(|v| Theme::parse(&v))
                .unwrap_or(defaults.theme),
            system_font: get(SYSTEM_FONT_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.system_font),
        }
        .clamped()
    }

    /// Keeps the settings in `storage`.
    pub fn save(&self, storage: &mut dyn Storage) {
        storage.set_string(TILE_CACHE_KEY, self.tile_cache_mb.to_string());
        storage.set_string(GPU_MEMORY_KEY, self.gpu_memory_mb.to_string());
        storage.set_string(DEFAULT_WINDOW_KEY, self.default_window_s.to_string());
        storage.set_string(ANTIALIAS_KEY, self.antialias.to_string());
        storage.set_string(SCALE_BARS_KEY, self.scale_bars.to_string());
        storage.set_string(REMOVE_DC_KEY, self.remove_dc.to_string());
        storage.set_string(AUTO_SCALE_KEY, self.auto_scale_on_open.to_string());
        storage.set_string(THEME_KEY, self.theme.key().to_owned());
        storage.set_string(SYSTEM_FONT_KEY, self.system_font.to_string());
        for (kind, color) in KINDS.into_iter().zip(self.type_colors) {
            let key = type_color_key(kind);
            match color {
                Some(c) => storage.set_string(&key, format_rgb([c.r(), c.g(), c.b()])),
                None => storage.remove_string(&key),
            }
        }
    }

    /// Every setting within its range; NaN seconds become the default.
    pub fn clamped(self) -> Settings {
        let clamp = |v: u32, range: RangeInclusive<u32>| v.clamp(*range.start(), *range.end());
        let seconds = if self.default_window_s.is_nan() {
            DEFAULT_SPAN
        } else {
            self.default_window_s
        };
        Settings {
            tile_cache_mb: clamp(self.tile_cache_mb, TILE_CACHE_MB),
            gpu_memory_mb: clamp(self.gpu_memory_mb, GPU_MEMORY_MB),
            default_window_s: seconds.clamp(*DEFAULT_WINDOW_S.start(), *DEFAULT_WINDOW_S.end()),
            antialias: self.antialias,
            scale_bars: self.scale_bars,
            remove_dc: self.remove_dc,
            auto_scale_on_open: self.auto_scale_on_open,
            type_colors: self.type_colors,
            theme: self.theme,
            system_font: self.system_font,
        }
    }

    pub fn tile_cache_bytes(&self) -> usize {
        (self.tile_cache_mb as usize) << 20
    }

    pub fn gpu_memory_bytes(&self) -> usize {
        (self.gpu_memory_mb as usize) << 20
    }
}

/// The colors picked for annotation descriptions, kept in `storage`; empty without one.
/// Unreadable lines are skipped.
pub fn load_annotation_colors(storage: Option<&dyn Storage>) -> AnnotationColors {
    let Some(text) = storage.and_then(|s| s.get_string(ANNOTATION_COLORS_KEY)) else {
        return AnnotationColors::new();
    };
    text.lines()
        .filter_map(|line| {
            let (rgb, description) = line.split_once('\t')?;
            Some((unescape(description)?, parse_rgb(rgb)?))
        })
        .collect()
}

/// Keeps the colors picked for annotation descriptions in `storage`: a line per
/// description, its color, a tab and the description, with backslashes and line breaks
/// escaped.
pub fn save_annotation_colors(colors: &AnnotationColors, storage: &mut dyn Storage) {
    let text: String = colors
        .iter()
        .map(|(description, &rgb)| format!("{}\t{}\n", format_rgb(rgb), escape(description)))
        .collect();
    storage.set_string(ANNOTATION_COLORS_KEY, text);
}

/// `#rrggbb`.
fn format_rgb([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A color as `#rrggbb`; `None` for anything else.
fn parse_rgb(text: &str) -> Option<Rgb> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// The inverse of [`escape`]; `None` for a backslash before anything else.
pub(crate) fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            });
        } else {
            out.push(c);
        }
    }
    Some(out)
}
