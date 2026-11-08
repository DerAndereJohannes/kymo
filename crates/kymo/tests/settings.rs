//! The settings: kept in and read back from eframe's storage, with defaults and ranges.

use std::collections::HashMap;

use eframe::Storage;
use kymo::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB};

/// Storage in memory.
#[derive(Default)]
struct Memory(HashMap<String, String>);

impl Storage for Memory {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn flush(&mut self) {}
}

#[test]
fn defaults_without_storage_or_values() {
    let defaults = Settings::default();
    assert_eq!(defaults.tile_cache_mb, 256);
    assert_eq!(defaults.gpu_memory_mb, 256);
    assert_eq!(defaults.default_window_s, 20.0);
    assert_eq!(Settings::load(None), defaults);
    assert_eq!(Settings::load(Some(&Memory::default())), defaults);
}

#[test]
fn saved_settings_read_back() {
    let settings = Settings {
        tile_cache_mb: 1024,
        gpu_memory_mb: 96,
        default_window_s: 7.5,
    };
    let mut storage = Memory::default();
    settings.save(&mut storage);
    assert_eq!(Settings::load(Some(&storage)), settings);
    assert_eq!(settings.tile_cache_bytes(), 1024 << 20);
    assert_eq!(settings.gpu_memory_bytes(), 96 << 20);
}

#[test]
fn unreadable_values_are_defaults_and_others_kept() {
    let saved = Settings {
        tile_cache_mb: 512,
        gpu_memory_mb: 128,
        default_window_s: 5.0,
    };
    let defaults = Settings::default();
    let cases = [
        (
            "kymo.tile_cache_mb",
            Settings {
                tile_cache_mb: defaults.tile_cache_mb,
                ..saved
            },
        ),
        (
            "kymo.gpu_memory_mb",
            Settings {
                gpu_memory_mb: defaults.gpu_memory_mb,
                ..saved
            },
        ),
        (
            "kymo.default_window_s",
            Settings {
                default_window_s: defaults.default_window_s,
                ..saved
            },
        ),
    ];
    for (key, want) in cases {
        for value in ["lots", "", "-1.5e", "NaN"] {
            let mut storage = Memory::default();
            saved.save(&mut storage);
            assert!(storage.0.contains_key(key), "{key} not saved");
            storage.set_string(key, value.into());
            assert_eq!(Settings::load(Some(&storage)), want, "{key} = {value:?}");
        }
    }
}

#[test]
fn values_out_of_range_are_clamped() {
    let mut storage = Memory::default();
    Settings {
        tile_cache_mb: 1,
        gpu_memory_mb: u32::MAX,
        default_window_s: 1e9,
    }
    .save(&mut storage);
    let loaded = Settings::load(Some(&storage));
    assert_eq!(loaded.tile_cache_mb, *TILE_CACHE_MB.start());
    assert_eq!(loaded.gpu_memory_mb, *GPU_MEMORY_MB.end());
    assert_eq!(loaded.default_window_s, *DEFAULT_WINDOW_S.end());
    storage.set_string("kymo.default_window_s", "-3".into());
    assert_eq!(
        Settings::load(Some(&storage)).default_window_s,
        *DEFAULT_WINDOW_S.start()
    );
}
