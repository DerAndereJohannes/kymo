//! The settings: kept in and read back from eframe's storage, with defaults and ranges.

use std::collections::HashMap;

use eframe::Storage;
use eframe::egui::Color32;
use kymo::annotations::AnnotationColors;
use kymo::settings::{DEFAULT_WINDOW_S, GPU_MEMORY_MB, Settings, TILE_CACHE_MB, Theme};
use kymo::settings::{load_annotation_colors, save_annotation_colors};
use kymo::ui::palette::TypeColors;
use kymo::view::{KINDS, kind_index};
use kymo_core::ChannelKind;
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
    assert!(defaults.auto_scale_on_open);
    assert_eq!(defaults.theme, Theme::System);
    assert_eq!(Settings::load(None), defaults);
    assert_eq!(Settings::load(Some(&Memory::default())), defaults);
}

/// Type colors with `picked` set and the others default.
fn colors(picked: &[(ChannelKind, Color32)]) -> TypeColors {
    let mut colors = [None; KINDS.len()];
    for &(kind, color) in picked {
        colors[kind_index(kind)] = Some(color);
    }
    colors
}

#[test]
fn saved_settings_read_back() {
    let settings = Settings {
        tile_cache_mb: 1024,
        gpu_memory_mb: 96,
        default_window_s: 7.5,
        antialias: true,
        scale_bars: false,
        auto_scale_on_open: true,
        type_colors: colors(&[
            (ChannelKind::Eog, Color32::from_rgb(10, 200, 30)),
            (ChannelKind::Misc, Color32::from_rgb(0, 0, 0)),
            (ChannelKind::RefMeg, Color32::from_rgb(1, 2, 3)),
        ]),
        theme: Theme::Dark,
    };
    let mut storage = Memory::default();
    settings.save(&mut storage);
    assert_eq!(Settings::load(Some(&storage)), settings);
    // Keyed by MNE's names of the types.
    for key in ["kymo.color.eog", "kymo.color.misc", "kymo.color.ref_meg"] {
        assert!(storage.0.contains_key(key), "{key}");
    }
    for theme in Theme::ALL {
        Settings { theme, ..settings }.save(&mut storage);
        assert_eq!(Settings::load(Some(&storage)).theme, theme);
    }
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
        scale_bars: false,
        auto_scale_on_open: true,
        type_colors: colors(&[(ChannelKind::Grad, Color32::from_rgb(255, 0, 128))]),
        theme: Theme::Light,
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
            "kymo.scale_bars",
            Settings {
                scale_bars: defaults.scale_bars,
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
            "kymo.color.grad",
            Settings {
                type_colors: defaults.type_colors,
                ..saved
            },
        ),
        (
            "kymo.theme",
            Settings {
                theme: defaults.theme,
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
        scale_bars: true,
        auto_scale_on_open: false,
        type_colors: [None; KINDS.len()],
        theme: Theme::System,
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

#[test]
fn recent_recordings_are_newest_first_each_once_and_capped() {
    use kymo::recent::Recent;
    use std::path::{Path, PathBuf};
    let mut recent = Recent::default();
    for k in 0..12 {
        recent.add(Path::new(&format!("/data/r{k}.edf")));
    }
    assert_eq!(recent.paths().len(), Recent::MAX);
    assert_eq!(recent.get(0), Some(Path::new("/data/r11.edf")));
    assert_eq!(recent.get(9), Some(Path::new("/data/r2.edf")));
    // Opened again, it moves to the top rather than showing twice.
    recent.add(Path::new("/data/r5.edf"));
    assert_eq!(recent.get(0), Some(Path::new("/data/r5.edf")));
    assert_eq!(recent.paths().len(), Recent::MAX);
    recent.remove(Path::new("/data/r5.edf"));
    assert!(!recent.paths().contains(&PathBuf::from("/data/r5.edf")));
    // Relative paths are kept as absolute ones.
    recent.add(Path::new("relative.edf"));
    assert!(recent.get(0).unwrap().is_absolute());
    recent.clear();
    assert!(recent.paths().is_empty());
}

#[test]
fn recent_labels_name_the_folder_only_for_names_shared() {
    use kymo::recent::Recent;
    use std::path::Path;
    let mut recent = Recent::default();
    recent.add(Path::new("/data/day1/sleep.edf"));
    recent.add(Path::new("/data/other.bdf"));
    recent.add(Path::new("/data/day2/sleep.edf"));
    assert_eq!(
        recent.labels(),
        ["sleep.edf — day2", "other.bdf", "sleep.edf — day1"]
    );
}

#[test]
fn recent_recordings_are_kept_in_storage() {
    use kymo::recent::Recent;
    use std::path::Path;
    let mut recent = Recent::default();
    recent.add(Path::new("/data/a.edf"));
    recent.add(Path::new("/data/with\nbreak and \\ backslash.bdf"));
    let mut storage = Memory::default();
    recent.save(&mut storage);
    assert_eq!(Recent::load(Some(&storage)), recent);
    assert_eq!(Recent::load(None), Recent::default());
    storage.set_string("kymo.recent", "\n/x.edf\n/x.edf\n".into());
    assert_eq!(Recent::load(Some(&storage)).paths().len(), 1);
}
