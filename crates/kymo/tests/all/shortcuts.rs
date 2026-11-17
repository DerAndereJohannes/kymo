//! The shortcut overview lists every binding and gesture, from the same tables the keys use.

use kymo::action::Action;
use kymo::keymap::{BINDINGS, Keymap, Platform};
use kymo::shortcuts::overview;

#[test]
fn every_binding_is_listed_once_with_its_action() {
    for platform in Platform::ALL {
        let sections = overview(platform);
        let rows: Vec<_> = sections.iter().skip(1).flat_map(|s| &s.rows).collect();
        for (action, chord) in BINDINGS {
            let listed: Vec<_> = rows.iter().filter(|r| r.action == action.label()).collect();
            assert_eq!(
                listed.len(),
                1,
                "{platform:?}: {action:?} listed {} times",
                listed.len()
            );
            let keys = listed[0].keys.split(" or ").collect::<Vec<_>>();
            assert!(
                keys.contains(&chord.display(platform).as_str()),
                "{platform:?}: {action:?} without {}",
                chord.display(platform)
            );
            // The menu's chord first.
            assert_eq!(keys[0], Keymap::chord(*action).unwrap().display(platform));
        }
        // Nothing listed without a binding.
        for row in &rows {
            let action = Action::ALL
                .iter()
                .find(|a| a.label() == row.action)
                .unwrap();
            assert!(Keymap::chord(*action).is_some(), "{action:?}");
        }
        assert!(sections.iter().all(|s| !s.rows.is_empty()));
    }
}

#[test]
fn sections_follow_the_menus_then_the_rest() {
    let titles = |p| overview(p).into_iter().map(|s| s.title).collect::<Vec<_>>();
    assert_eq!(
        titles(Platform::Mac),
        [
            "Trackpad and Mouse",
            "Kymo",
            "File",
            "Edit",
            "View",
            "Go",
            "Help",
            "Other Keys"
        ]
    );
    assert_eq!(
        titles(Platform::Linux),
        [
            "Trackpad and Mouse",
            "File",
            "Edit",
            "View",
            "Go",
            "Help",
            "Other Keys"
        ]
    );
    let other = overview(Platform::Linux).pop().unwrap();
    let actions: Vec<_> = other.rows.iter().map(|r| r.action).collect();
    assert!(actions.contains(&Action::PanLeft.label()));
    assert!(actions.contains(&Action::ChannelUp.label()));
    assert!(!actions.contains(&Action::ZoomIn.label()));
}

#[test]
fn gestures_use_the_platform_modifiers() {
    let gestures = |p| overview(p).remove(0);
    let mac = gestures(Platform::Mac);
    assert_eq!(mac.title, "Trackpad and Mouse");
    let keys = |section: &kymo::shortcuts::Section, action: &str| {
        section
            .rows
            .iter()
            .find(|r| r.action == action)
            .unwrap()
            .keys
            .clone()
    };
    assert_eq!(
        keys(&mac, "Zoom Time Around the Pointer"),
        "Pinch or ⌘ + scroll"
    );
    assert_eq!(keys(&mac, "Scale Amplitudes"), "⌥ + pinch or ⌥ + scroll");
    let linux = gestures(Platform::Linux);
    assert_eq!(
        keys(&linux, "Zoom Time Around the Pointer"),
        "Pinch or Ctrl + scroll"
    );
    assert!(keys(&linux, "Pan Time").contains("Shift + wheel"));
    let measure = "Measure Time and Values";
    assert_eq!(keys(&mac, measure), "⇧ + drag the traces");
    assert_eq!(keys(&linux, measure), "Shift + drag the traces");
    let fold = "Collapse or Expand All Groups";
    assert_eq!(keys(&mac, fold), "⌥ + click a group's arrow in the sidebar");
    assert_eq!(
        keys(&linux, fold),
        "Alt + click a group's arrow in the sidebar"
    );
    // A row per action.
    for section in [mac, linux] {
        let mut actions: Vec<_> = section.rows.iter().map(|r| r.action).collect();
        actions.sort_unstable();
        let count = actions.len();
        actions.dedup();
        assert_eq!(actions.len(), count, "{actions:?}");
    }
}
