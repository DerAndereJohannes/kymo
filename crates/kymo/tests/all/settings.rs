//! The settings: kept in and read back from eframe's storage, with defaults and ranges.

use std::collections::HashMap;

use eframe::Storage;
use eframe::egui::Color32;
use kymo::annotations::AnnotationColors;
use kymo::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB};
use kymo::settings::{load_annotation_colors, save_annotation_colors};
use kymo::view::KINDS;
use proptest::prelude::*;

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
    assert!(!defaults.antialias);
    assert!(!defaults.auto_scale_on_open);
    assert_eq!(Settings::load(None), defaults);
    assert_eq!(Settings::load(Some(&Memory::default())), defaults);
}

#[test]
fn saved_settings_read_back() {
    let settings = Settings {
        tile_cache_mb: 1024,
        gpu_memory_mb: 96,
        default_window_s: 7.5,
        antialias: true,
        auto_scale_on_open: true,
        type_colors: [
            None,
            Some(Color32::from_rgb(10, 200, 30)),
            None,
            None,
            None,
            Some(Color32::from_rgb(0, 0, 0)),
        ],
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
        antialias: true,
        auto_scale_on_open: true,
        type_colors: [
            None,
            Some(Color32::from_rgb(255, 0, 128)),
            None,
            None,
            None,
            None,
        ],
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
        (
            "kymo.antialias",
            Settings {
                antialias: defaults.antialias,
                ..saved
            },
        ),
        (
            "kymo.auto_scale_on_open",
            Settings {
                auto_scale_on_open: defaults.auto_scale_on_open,
                ..saved
            },
        ),
        (
            "kymo.color.eog",
            Settings {
                type_colors: defaults.type_colors,
                ..saved
            },
        ),
    ];
    for (key, want) in cases {
        let colors = ["#12345", "#12345g", "ff0080", "#+1+2+3", "#aé€"];
        for value in ["lots", "", "-1.5e", "NaN"].into_iter().chain(colors) {
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
        antialias: false,
        auto_scale_on_open: false,
        type_colors: [None; KINDS.len()],
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

proptest! {
    /// Any descriptions (line breaks, tabs, backslashes, `#`, non-ASCII) and colors read
    /// back exactly.
    #[test]
    fn annotation_colors_read_back(
        colors in proptest::collection::btree_map(any::<String>(), any::<[u8; 3]>(), 0..12),
    ) {
        let mut storage = Memory::default();
        save_annotation_colors(&colors, &mut storage);
        prop_assert_eq!(load_annotation_colors(Some(&storage)), colors);
    }
}

#[test]
fn annotation_colors_skip_unreadable_lines() {
    assert_eq!(load_annotation_colors(None), AnnotationColors::new());
    let mut storage = Memory::default();
    storage.set_string(
        "kymo.annotation_colors",
        "garbage\n#00ff00\tgood\n#zzzzzz\tbad color\n#00ff00\tbad \\x escape\n\
         #0000ff\tline\\nbreak \\\\ and\ttab\n#00ff0\tshort\n"
            .into(),
    );
    let want = AnnotationColors::from([
        ("good".to_owned(), [0, 255, 0]),
        ("line\nbreak \\ and\ttab".to_owned(), [0, 0, 255]),
    ]);
    assert_eq!(load_annotation_colors(Some(&storage)), want);
}

#[test]
fn a_reset_type_color_is_not_read_back() {
    let mut picked = Settings::default();
    picked.type_colors[0] = Some(Color32::from_rgb(1, 2, 3));
    let mut storage = Memory::default();
    picked.save(&mut storage);
    Settings::default().save(&mut storage);
    assert_eq!(Settings::load(Some(&storage)), Settings::default());
}

#[test]
fn awkward_descriptions_read_back() {
    let colors: AnnotationColors = [
        "",
        "#",
        "\r",
        "a\r",
        "\r\n",
        "\n",
        "x\\",
        "\\n",
        "tab\there",
        "\t",
        "ü€😀",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, d)| (d.to_owned(), [i as u8, 0, 255]))
    .collect();
    let mut storage = Memory::default();
    save_annotation_colors(&colors, &mut storage);
    assert_eq!(load_annotation_colors(Some(&storage)), colors);
}
