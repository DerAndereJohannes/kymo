//! What the user can set in the Settings window, kept between runs in eframe's storage.

use std::ops::RangeInclusive;

use eframe::Storage;
use kymo_core::tile::DEFAULT_BUDGET;

use crate::render::DEFAULT_GPU_BUDGET;
use crate::view::DEFAULT_SPAN;

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
const AUTO_SCALE_KEY: &str = "kymo.auto_scale_on_open";

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
    /// Whether a recording opens with Auto Scale instead of the default amplitudes.
    pub auto_scale_on_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tile_cache_mb: (DEFAULT_BUDGET >> 20) as u32,
            gpu_memory_mb: (DEFAULT_GPU_BUDGET >> 20) as u32,
            default_window_s: DEFAULT_SPAN,
            antialias: false,
            auto_scale_on_open: false,
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
            auto_scale_on_open: get(AUTO_SCALE_KEY)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.auto_scale_on_open),
        }
        .clamped()
    }

    /// Keeps the settings in `storage`.
    pub fn save(&self, storage: &mut dyn Storage) {
        storage.set_string(TILE_CACHE_KEY, self.tile_cache_mb.to_string());
        storage.set_string(GPU_MEMORY_KEY, self.gpu_memory_mb.to_string());
        storage.set_string(DEFAULT_WINDOW_KEY, self.default_window_s.to_string());
        storage.set_string(ANTIALIAS_KEY, self.antialias.to_string());
        storage.set_string(AUTO_SCALE_KEY, self.auto_scale_on_open.to_string());
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
            auto_scale_on_open: self.auto_scale_on_open,
        }
    }

    pub fn tile_cache_bytes(&self) -> usize {
        (self.tile_cache_mb as usize) << 20
    }

    pub fn gpu_memory_bytes(&self) -> usize {
        (self.gpu_memory_mb as usize) << 20
    }
}
