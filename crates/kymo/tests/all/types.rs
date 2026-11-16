//! Channel types' settings by name: resetting and forgetting a type, and which types the
//! Channel Types window lists and its filter matches.

use eframe::egui::Color32;
use kymo::types::TypeSettings;
use kymo::ui::types::{listed_types, matches};
use kymo_core::{ChannelKind, KNOWN_KINDS};

#[test]
fn reset_returns_a_type_to_its_start_and_forget_drops_it() {
    let (eeg, aux) = (ChannelKind::EEG, ChannelKind::named("AUX"));
    let mut types = TypeSettings::default();
    assert!(!types.changed(&eeg) && !types.stored(&eeg));

    // A known type keeps an entry only while something differs from its start.
    types.set_color(&eeg, Some(Color32::RED));
    types.set_own_scale(&eeg, true);
    assert!(types.changed(&eeg) && types.stored(&eeg));
    types.reset(&eeg);
    assert!(!types.own_scale(&eeg));
    assert_eq!(types.color(&eeg), None);
    assert_eq!(types, TypeSettings::default());

    // A type kymo doesn't know keeps its entry when reset, until forgotten.
    types.set_remove_dc(&aux, true);
    assert!(types.changed(&aux) && types.remove_dc(&aux));
    types.reset(&aux);
    assert!(!types.changed(&aux) && types.stored(&aux));
    assert!(!types.remove_dc(&aux));
    // By any case.
    types.forget(&ChannelKind::named("aux"));
    assert!(!types.stored(&aux));
    assert_eq!(types, TypeSettings::default());

    // Forgetting a known type resets it.
    types.set_remove_dc(&eeg, false);
    types.forget(&eeg);
    assert!(types.remove_dc(&eeg));
}

#[test]
fn the_window_lists_known_set_and_recorded_types_alphabetically() {
    let names = |types: &TypeSettings, recording: &[ChannelKind]| -> Vec<String> {
        listed_types(types, recording)
            .iter()
            .map(|k| k.name().to_owned())
            .collect()
    };
    let none = names(&TypeSettings::default(), &[]);
    assert_eq!(none.len(), KNOWN_KINDS.len());
    // Alphabetical, ignoring case ("cHPI" between "Bio" and "CSD").
    let lower: Vec<String> = none.iter().map(|n| n.to_lowercase()).collect();
    assert!(lower.is_sorted(), "{none:?}");
    let at = |name: &str| none.iter().position(|n| n == name).unwrap();
    assert!(at("Bio") < at("cHPI") && at("cHPI") < at("CSD"));

    // A type set, and the recording's own, once each, whatever their case; known types the
    // recording has aren't listed twice.
    let mut types = TypeSettings::default();
    types.set_remove_dc(&ChannelKind::named("Joystick"), true);
    let recording = [
        ChannelKind::named("eeg"),
        ChannelKind::named("JOYSTICK"),
        ChannelKind::named("PositionX"),
    ];
    let listed = names(&types, &recording);
    assert_eq!(listed.len(), KNOWN_KINDS.len() + 2);
    assert_eq!(listed.iter().filter(|n| *n == "EEG").count(), 1);
    // The name as set first.
    assert!(listed.contains(&"Joystick".to_owned()), "{listed:?}");
    assert!(listed.contains(&"PositionX".to_owned()));
    let lower: Vec<String> = listed.iter().map(|n| n.to_lowercase()).collect();
    assert!(lower.is_sorted(), "{listed:?}");
}

#[test]
fn the_filter_matches_names_spellings_and_origins() {
    let temperature = ChannelKind::named("Temperature");
    let gaze = ChannelKind::named("Gaze");
    for (filter, temp, custom) in [
        ("", true, true),
        ("  ", true, true),
        ("TEMP", true, false),
        ("erat", true, false),
        ("built in", true, false),
        ("Custom", false, true),
        (" gaz ", false, true),
        ("xyz", false, false),
    ] {
        assert_eq!(matches(&temperature, filter), temp, "{filter:?}");
        assert_eq!(matches(&gaze, filter), custom, "{filter:?}");
    }
    // Another spelling: "MEGMAG" finds "MEG Mag".
    assert!(matches(&ChannelKind::MAG, "megmag"));
    assert!(matches(&ChannelKind::ECG, "ekg"));
    assert!(!matches(&ChannelKind::ECG, "custom"));
}
