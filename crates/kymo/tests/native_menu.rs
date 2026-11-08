//! What the native menu bar is built from: menu ids and key equivalents. The menu bar
//! itself is checked by hand (macOS needs the main thread to build it).

#![cfg(any(target_os = "macos", windows))]
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use eframe::egui::Key as EguiKey;
use kymo::action::Action;
use kymo::keymap::{BINDINGS, Chord, Platform};
use kymo::menu::{Item, leaves, menu_bar};
use kymo::native_menu::{action_id, key_accelerator};
use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};

#[test]
fn every_action_has_its_own_menu_id() {
    let ids: HashSet<_> = Action::ALL.iter().map(|&a| action_id(a)).collect();
    assert_eq!(ids.len(), Action::ALL.len());
}

#[test]
fn every_shortcut_has_a_key_equivalent() {
    // Attached items need one; the others would too, should they be attached later.
    for (action, chord) in BINDINGS {
        assert!(key_accelerator(*chord).is_some(), "{action:?} {chord:?}");
    }
    for item in menu_bar(Platform::Mac)
        .iter()
        .flat_map(|m| leaves(&m.items))
    {
        if let Item::Action {
            shortcut: Some(chord),
            attached: true,
            ..
        } = item
        {
            assert!(chord.command, "{chord:?}");
        }
    }
}

#[test]
fn key_equivalents_keep_keys_and_modifiers() {
    let character = |c: &str| Key::Character(c.into());
    let cases = [
        (Chord::cmd(EguiKey::O), Modifiers::META, character("o")),
        (Chord::cmd(EguiKey::Comma), Modifiers::META, character(",")),
        (Chord::cmd(EguiKey::Slash), Modifiers::META, character("/")),
        (Chord::cmd(EguiKey::Num9), Modifiers::META, character("9")),
        (Chord::cmd(EguiKey::Plus), Modifiers::META, character("+")),
        (
            Chord::shift_cmd(EguiKey::Plus),
            Modifiers::META | Modifiers::SHIFT,
            character("+"),
        ),
        (
            Chord::cmd(EguiKey::ArrowLeft),
            Modifiers::META,
            Key::Named(NamedKey::ArrowLeft),
        ),
        (
            Chord::alt(EguiKey::ArrowDown),
            Modifiers::ALT,
            Key::Named(NamedKey::ArrowDown),
        ),
        (Chord::cmd(EguiKey::D), Modifiers::META, character("d")),
        (
            Chord::alt_cmd(EguiKey::Minus),
            Modifiers::META | Modifiers::ALT,
            character("-"),
        ),
        (
            Chord::key(EguiKey::Home),
            Modifiers::empty(),
            Key::Named(NamedKey::Home),
        ),
    ];
    for (chord, modifiers, key) in cases {
        assert_eq!(
            key_accelerator(chord),
            Some(KeyAccelerator::new(modifiers, key)),
            "{chord:?}"
        );
    }
}
