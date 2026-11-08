//! The shortcut overview (⌘/) as data: every key binding and gesture, grouped as the
//! menus are, built from the same tables so it can't drift from them.

use crate::action::Action;
use crate::keymap::{BINDINGS, Keymap, Platform};
use crate::menu::{Role, menu_bar};

/// One line: the keys (or gesture) and what they do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub keys: String,
    pub action: &'static str,
}

/// A titled group of rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// The overview for `platform`: trackpad and mouse first, then each menu's actions with
/// keys, then the keys without a menu item.
pub fn overview(platform: Platform) -> Vec<Section> {
    let mut sections = vec![Section {
        title: "Trackpad and Mouse",
        rows: gestures(platform),
    }];
    let mut listed = Vec::new();
    for menu in menu_bar(platform) {
        let title = match menu.role {
            Role::App => "Kymo",
            _ => menu.title,
        };
        // Submenus' items too.
        let rows: Vec<Row> = menu
            .actions()
            .filter_map(|action| row(platform, action))
            .collect();
        listed.extend(menu.actions());
        if !rows.is_empty() {
            sections.push(Section { title, rows });
        }
    }
    let mut others: Vec<Action> = Vec::new();
    for (action, _) in BINDINGS {
        if !listed.contains(action) && !others.contains(action) {
            others.push(*action);
        }
    }
    sections.push(Section {
        title: "Other Keys",
        rows: others
            .into_iter()
            .filter_map(|a| row(platform, a))
            .collect(),
    });
    sections
}

/// `action` with all its keys, the one menus show first; `None` without keys.
fn row(platform: Platform, action: Action) -> Option<Row> {
    let keys: Vec<String> = Keymap::chords(action)
        .map(|c| c.display(platform))
        .collect();
    (!keys.is_empty()).then(|| Row {
        keys: keys.join(" or "),
        action: action.label(),
    })
}

/// The trackpad and mouse gestures, with the platform's modifier names.
fn gestures(platform: Platform) -> Vec<Row> {
    let (command, alt, shift) = match platform {
        Platform::Mac => ("⌘", "⌥", "⇧"),
        Platform::Windows | Platform::Linux => ("Ctrl", "Alt", "Shift"),
    };
    let row = |keys: String, action| Row { keys, action };
    vec![
        row("Scroll sideways".into(), "Pan time"),
        row("Scroll up or down".into(), "Scroll channels"),
        row("Pinch".into(), "Zoom time around the pointer"),
        row(
            format!("{command} + scroll"),
            "Zoom time around the pointer",
        ),
        row(format!("{shift} + wheel"), "Pan time"),
        row(
            format!("{alt} + scroll or {alt} + pinch"),
            "Scale amplitudes",
        ),
        row("Drag the traces".into(), "Pan time"),
        row("Click a channel name".into(), "Toggle bad channel"),
    ]
}
