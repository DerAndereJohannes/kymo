//! Key lookups per platform, and scroll and pinch gestures.

use eframe::egui::{Key, Modifiers, MouseWheelUnit, Vec2, vec2};
use kymo::action::{Action, Gesture};
use kymo::keymap::{Chord, Keymap, Platform};

const NONE: Modifiers = Modifiers::NONE;

/// Modifiers as egui reports ⌘ (or Ctrl), ⌥ and Shift on `platform`.
fn mods(platform: Platform, command: bool, alt: bool, shift: bool) -> Modifiers {
    let mac = platform == Platform::Mac;
    Modifiers {
        alt,
        ctrl: command && !mac,
        shift,
        mac_cmd: command && mac,
        command,
    }
}

fn cmd(platform: Platform) -> Modifiers {
    mods(platform, true, false, false)
}

#[test]
fn command_is_cmd_on_macos_and_ctrl_elsewhere() {
    let mac = Keymap::new(Platform::Mac);
    assert_eq!(
        mac.action(Key::ArrowLeft, Modifiers::MAC_CMD),
        Some(Action::ToStart)
    );
    assert_eq!(mac.action(Key::ArrowLeft, Modifiers::CTRL), None);
    for platform in [Platform::Windows, Platform::Linux] {
        let keymap = Keymap::new(platform);
        assert_eq!(
            keymap.action(Key::ArrowLeft, Modifiers::CTRL),
            Some(Action::ToStart)
        );
        assert_eq!(keymap.action(Key::O, Modifiers::CTRL), Some(Action::Open));
    }
}

#[test]
fn looks_up_the_tables_bindings() {
    for platform in Platform::ALL {
        let keymap = Keymap::new(platform);
        let lookup =
            |key, command, alt, shift| keymap.action(key, mods(platform, command, alt, shift));
        let cases = [
            (Key::ArrowLeft, false, false, false, Action::PanLeft),
            (Key::ArrowRight, false, true, false, Action::PageRight),
            (Key::ArrowRight, true, false, false, Action::ToEnd),
            (Key::Home, false, false, false, Action::ToStart),
            (Key::End, false, false, false, Action::ToEnd),
            (Key::ArrowDown, false, false, false, Action::ChannelDown),
            (Key::ArrowUp, false, true, false, Action::ChannelPageUp),
            (Key::PageDown, false, false, false, Action::ChannelPageDown),
            (Key::ArrowUp, true, false, false, Action::FirstChannels),
            (Key::Plus, true, false, false, Action::ZoomIn),
            (Key::Equals, true, false, false, Action::ZoomIn),
            (Key::Minus, true, false, false, Action::ZoomOut),
            (Key::Num0, true, false, false, Action::DefaultWindow),
            (Key::Num9, true, false, false, Action::Fit),
            (Key::Plus, true, false, true, Action::MoreChannels),
            (Key::Minus, true, false, true, Action::FewerChannels),
            (Key::Plus, true, true, false, Action::ScaleUp),
            (Key::Equals, true, true, false, Action::ScaleUp),
            (Key::Minus, true, true, false, Action::ScaleDown),
            (Key::D, true, false, false, Action::ToggleDc),
            (Key::W, true, false, false, Action::Close),
            (Key::S, true, false, false, Action::SaveAnnotations),
            (Key::Q, true, false, false, Action::Quit),
            (Key::Comma, true, false, false, Action::Settings),
            (Key::Slash, true, false, false, Action::Shortcuts),
        ];
        for (key, command, alt, shift, action) in cases {
            assert_eq!(
                lookup(key, command, alt, shift),
                Some(action),
                "{platform:?} {key:?}"
            );
        }
        // Keys that need Shift on some layouts: + on a US keyboard, / on a German one.
        assert_eq!(lookup(Key::Plus, true, true, true), Some(Action::ScaleUp));
        assert_eq!(
            lookup(Key::Slash, true, false, true),
            Some(Action::Shortcuts)
        );
        // Shift isn't ignored elsewhere.
        assert_eq!(lookup(Key::D, true, false, true), None);
        // Only navigation keys work without a modifier.
        for key in [Key::Plus, Key::Minus, Key::Equals, Key::D, Key::B] {
            assert_eq!(lookup(key, false, false, false), None, "{key:?}");
        }
        assert_eq!(lookup(Key::ArrowLeft, false, false, true), None);
        assert_eq!(lookup(Key::X, false, false, false), None);
    }
}

#[test]
fn leaves_keys_a_native_menu_takes_to_the_menu() {
    let mac = Keymap::new(Platform::Mac);
    assert_eq!(mac.egui_action(Key::O, cmd(Platform::Mac)), None);
    assert_eq!(mac.egui_action(Key::Plus, cmd(Platform::Mac)), None);
    assert_eq!(mac.egui_action(Key::D, cmd(Platform::Mac)), None);
    // Alternatives and navigation keys aren't menu shortcuts.
    assert_eq!(
        mac.egui_action(Key::Equals, cmd(Platform::Mac)),
        Some(Action::ZoomIn)
    );
    assert_eq!(mac.egui_action(Key::Home, NONE), Some(Action::ToStart));
    assert_eq!(mac.egui_action(Key::ArrowLeft, NONE), Some(Action::PanLeft));

    // Windows menus show their shortcuts but leave them to egui, as Linux does.
    for platform in [Platform::Windows, Platform::Linux] {
        let keymap = Keymap::new(platform);
        assert_eq!(
            keymap.egui_action(Key::O, cmd(platform)),
            Some(Action::Open)
        );
    }
}

#[test]
fn shows_chords_in_each_platforms_style() {
    let page = Chord::alt_cmd(Key::ArrowLeft);
    assert_eq!(page.display(Platform::Mac), "⌥⌘←");
    assert_eq!(page.display(Platform::Windows), "Ctrl+Alt+Left");
    let more = Chord::shift_cmd(Key::Plus);
    assert_eq!(more.display(Platform::Mac), "⇧⌘+");
    assert_eq!(more.display(Platform::Linux), "Ctrl+Shift++");
    assert_eq!(Chord::key(Key::D).display(Platform::Mac), "D");
    assert_eq!(Chord::cmd(Key::Comma).display(Platform::Windows), "Ctrl+,");
}

#[test]
fn scrolling_pans_time_and_channels() {
    let keymap = Keymap::new(Platform::Mac);
    let page = vec2(1000.0, 400.0);
    let wheel = |unit, delta, modifiers| keymap.wheel(unit, delta, modifiers, page);
    assert_eq!(
        wheel(MouseWheelUnit::Point, vec2(12.5, -3.0), NONE),
        [
            Gesture::Pan { points: 12.5 },
            Gesture::ScrollRows { points: -3.0 }
        ]
    );
    assert_eq!(
        wheel(MouseWheelUnit::Line, vec2(0.0, 1.0), NONE),
        [Gesture::ScrollRows { points: 40.0 }]
    );
    assert_eq!(
        wheel(MouseWheelUnit::Page, vec2(-1.0, 0.0), NONE),
        [Gesture::Pan { points: -1000.0 }]
    );
    // Shift turns a vertical wheel into panning.
    assert_eq!(
        wheel(MouseWheelUnit::Line, vec2(0.0, -2.0), Modifiers::SHIFT),
        [Gesture::Pan { points: -80.0 }]
    );
    assert!(wheel(MouseWheelUnit::Point, Vec2::ZERO, NONE).is_empty());
}

#[test]
fn command_scroll_zooms_and_alt_scroll_scales() {
    for platform in Platform::ALL {
        let keymap = Keymap::new(platform);
        let wheel = |dy: f32, modifiers| {
            keymap.wheel(MouseWheelUnit::Point, vec2(0.0, dy), modifiers, Vec2::ZERO)
        };
        assert_eq!(wheel(200.0, cmd(platform)), [Gesture::Zoom { factor: 2.0 }]);
        assert_eq!(
            wheel(-200.0, cmd(platform)),
            [Gesture::Zoom { factor: 0.5 }]
        );
        assert_eq!(
            wheel(400.0, Modifiers::ALT),
            [Gesture::Scale { factor: 4.0 }]
        );
    }
}

#[test]
fn scroll_amounts_scale_smoothly() {
    let keymap = Keymap::new(Platform::Mac);
    let factor = |dy: f32| match keymap.wheel(
        MouseWheelUnit::Point,
        vec2(0.0, dy),
        Modifiers::MAC_CMD,
        Vec2::ZERO,
    )[..]
    {
        [Gesture::Zoom { factor }] => factor,
        ref other => panic!("{other:?}"),
    };
    let pan = |dx: f32| match keymap.wheel(MouseWheelUnit::Point, vec2(dx, 0.0), NONE, Vec2::ZERO)[..]
    {
        [Gesture::Pan { points }] => points,
        ref other => panic!("{other:?}"),
    };
    // Many small deltas add up to one large one, and amounts never snap to steps.
    let small: f64 = (0..50).map(|_| factor(1.5)).product();
    assert!((small - factor(75.0)).abs() < 1e-9);
    let panned: f64 = (0..50).map(|_| pan(0.3)).sum();
    assert!((panned - pan(15.0)).abs() < 1e-4);
    assert!(factor(0.1) > 1.0 && factor(0.1) < factor(0.2));
    assert_eq!(pan(0.25), 0.25);
}

#[test]
fn pinching_zooms_and_alt_pinching_scales() {
    let keymap = Keymap::new(Platform::Mac);
    assert_eq!(keymap.pinch(1.25, NONE), Gesture::Zoom { factor: 1.25 });
    assert_eq!(
        keymap.pinch(0.5, Modifiers::ALT),
        Gesture::Scale { factor: 0.5 }
    );
}
