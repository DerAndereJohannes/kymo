//! The native menu bar on macOS and Windows, built with `muda` from the menu description.
//!
//! On macOS the menu takes its items' ⌘ shortcuts as key equivalents, so the system hands
//! them to the menu (and flashes its title) before the window sees the key. Every menu
//! shortcut has ⌘ for that reason: a macOS menu item can only show a key by taking it, and
//! a bare key taken by the menu would never reach a text field. On Windows every item shows
//! its shortcut after a tab, and egui handles the keys (see
//! [`Platform::menu_takes_shortcuts`]).

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Key as EguiKey};
use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};
use muda::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

use crate::action::Action;
use crate::keymap::{Chord, Platform};
use crate::menu::{Item, Role, SystemItem, menu_bar};
use crate::recent::Recent;
use crate::settings::Theme;
use crate::ui::menus::{ABOUT, LICENSE, MenuState, RECENT_TITLE};

/// The menu bar and its action items.
pub struct NativeMenu {
    /// Never dropped: muda takes a dropped menu bar off its window, which on Windows then
    /// shows a frame without it (taller traces) before closing. The menu lives as long as
    /// the app, and Windows destroys it with the window.
    menu: ManuallyDrop<Menu>,
    items: Vec<(Action, Entry)>,
    actions: HashMap<MenuId, Action>,
    events: Receiver<MenuId>,
    /// What each item showed at the last [`update`](Self::update).
    shown: HashMap<Action, (bool, Option<bool>)>,
    /// File → Open Recent, and the labels it shows (`None` until first filled).
    recent: Option<Submenu>,
    recent_shown: Option<Vec<String>>,
    /// The window the menu bar is attached to.
    #[cfg(windows)]
    hwnd: Option<isize>,
}

enum Entry {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

/// The id of `action`'s menu item.
pub fn action_id(action: Action) -> MenuId {
    MenuId::new(format!("kymo.{action:?}"))
}

/// The key equivalent of `chord` with ⌘ as the command key, or `None` for keys a menu
/// can't show.
pub fn key_accelerator(chord: Chord) -> Option<KeyAccelerator> {
    let character = |c: &str| Some(Key::Character(c.to_owned()));
    let key = match chord.key {
        EguiKey::ArrowLeft => Some(Key::Named(NamedKey::ArrowLeft)),
        EguiKey::ArrowRight => Some(Key::Named(NamedKey::ArrowRight)),
        EguiKey::ArrowUp => Some(Key::Named(NamedKey::ArrowUp)),
        EguiKey::ArrowDown => Some(Key::Named(NamedKey::ArrowDown)),
        EguiKey::Home => Some(Key::Named(NamedKey::Home)),
        EguiKey::End => Some(Key::Named(NamedKey::End)),
        EguiKey::PageUp => Some(Key::Named(NamedKey::PageUp)),
        EguiKey::PageDown => Some(Key::Named(NamedKey::PageDown)),
        EguiKey::Backspace => Some(Key::Named(NamedKey::Backspace)),
        EguiKey::Delete => Some(Key::Named(NamedKey::Delete)),
        EguiKey::Escape => Some(Key::Named(NamedKey::Escape)),
        EguiKey::Plus => character("+"),
        EguiKey::Minus => character("-"),
        EguiKey::Equals => character("="),
        EguiKey::Comma => character(","),
        EguiKey::Slash => character("/"),
        key => {
            let name = key.name();
            // Letters and digits: "O", "0".
            (name.len() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric()))
                .then(|| Key::Character(name.to_ascii_lowercase()))
        }
    }?;
    let mut modifiers = Modifiers::empty();
    for (on, modifier) in [
        (chord.command, Modifiers::META),
        (chord.alt, Modifiers::ALT),
        (chord.shift, Modifiers::SHIFT),
    ] {
        if on {
            modifiers |= modifier;
        }
    }
    Some(KeyAccelerator::new(modifiers, key))
}

impl NativeMenu {
    /// Builds the menu bar and puts it in place, once the window exists: as the
    /// application's on macOS, replacing winit's default one, on the window on Windows.
    /// Menu events wake the window. Call on the main thread.
    pub fn attach(cc: &eframe::CreationContext<'_>) -> anyhow::Result<NativeMenu> {
        let mut menu = NativeMenu::new(Platform::current(), &cc.egui_ctx)?;
        menu.install(cc)?;
        Ok(menu)
    }

    /// Builds the menu bar for `platform`. Menu events wake `ctx`.
    fn new(platform: Platform, ctx: &egui::Context) -> muda::Result<NativeMenu> {
        let (tx, events) = mpsc::channel();
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event.id);
            wake.request_repaint();
        }));

        let menu = Menu::new();
        let mut items = Vec::new();
        let mut recent = None;
        for description in menu_bar(platform) {
            let submenu = Submenu::new(description.title, true);
            append(
                &submenu,
                &description.items,
                platform,
                &mut items,
                &mut recent,
            )?;
            menu.append(&submenu)?;
            #[cfg(target_os = "macos")]
            match description.role {
                Role::Window => submenu.set_as_windows_menu_for_nsapp(),
                Role::Help => submenu.set_as_help_menu_for_nsapp(),
                Role::App | Role::Plain => {}
            }
            #[cfg(not(target_os = "macos"))]
            let _: Role = description.role;
        }
        // Open Recent's items come and go with the list.
        let recent_actions = (0..Recent::MAX as u8)
            .map(Action::OpenRecent)
            .chain([Action::ClearRecent]);
        let actions = items
            .iter()
            .map(|(action, _)| *action)
            .chain(recent_actions)
            .map(|action| (action_id(action), action))
            .collect();
        Ok(NativeMenu {
            menu: ManuallyDrop::new(menu),
            items,
            actions,
            events,
            shown: HashMap::new(),
            recent,
            recent_shown: None,
            #[cfg(windows)]
            hwnd: None,
        })
    }

    #[cfg(target_os = "macos")]
    fn install(&mut self, _: &eframe::CreationContext<'_>) -> anyhow::Result<()> {
        self.menu.init_for_nsapp();
        Ok(())
    }

    #[cfg(windows)]
    fn install(&mut self, window: &eframe::CreationContext<'_>) -> anyhow::Result<()> {
        use eframe::egui_wgpu::wgpu::rwh::HasWindowHandle as _;
        use eframe::egui_wgpu::wgpu::rwh::RawWindowHandle;
        let handle = window.window_handle()?;
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        let hwnd = win32.hwnd.get();
        allow_dark_mode(hwnd);
        // SAFETY: `hwnd` comes from eframe's live window handle, so it is a valid window
        // for as long as the app runs; muda only needs it to attach the menu.
        #[allow(unsafe_code)]
        unsafe {
            self.menu.init_for_hwnd(hwnd)?;
        }
        // The theme follows from the app (`KymoApp::start`).
        self.hwnd = Some(hwnd);
        Ok(())
    }

    /// Shows the menus light or dark as `theme` says; [`Theme::System`] follows the system
    /// as it changes. egui and the title bar are the caller's.
    #[cfg(target_os = "macos")]
    pub fn set_theme(&self, theme: Theme) {
        use objc2_app_kit::{NSAppearance, NSApplication};
        use objc2_foundation::{MainThreadMarker, ns_string};
        // The menus take the application's appearance (the window's own is winit's).
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // The values of `NSAppearanceNameAqua` and `NSAppearanceNameDarkAqua`.
        let appearance = match theme {
            Theme::System => None,
            Theme::Light => NSAppearance::appearanceNamed(ns_string!("NSAppearanceNameAqua")),
            Theme::Dark => NSAppearance::appearanceNamed(ns_string!("NSAppearanceNameDarkAqua")),
        };
        NSApplication::sharedApplication(mtm).setAppearance(appearance.as_deref());
    }

    /// Shows the menus light or dark as `theme` says; [`Theme::System`] follows the default
    /// app mode as it changes. The menus that drop down take the app's mode, the menu bar
    /// muda's theme for the window. egui and the title bar are the caller's.
    #[cfg(windows)]
    pub fn set_theme(&self, theme: Theme) {
        set_app_mode(theme);
        let Some(hwnd) = self.hwnd else { return };
        let bar = match theme {
            Theme::System => muda::MenuTheme::Auto,
            Theme::Light => muda::MenuTheme::Light,
            Theme::Dark => muda::MenuTheme::Dark,
        };
        // SAFETY: `hwnd` is eframe's live window, which `install` attached the menu to; muda
        // sends it a message to redraw the bar.
        #[allow(unsafe_code)]
        let _ = unsafe { self.menu.set_theme_for_hwnd(hwnd, bar) };
    }

    /// The actions of the menu items chosen since the last call.
    pub fn poll(&self) -> Vec<Action> {
        self.events
            .try_iter()
            .filter_map(|id| self.actions.get(&id).copied())
            .collect()
    }

    /// Enables, disables and checks items as `state` says; touches only what changed.
    pub fn update(&mut self, state: &dyn MenuState) {
        for (action, entry) in &self.items {
            let now = (state.enabled(*action), state.checked(*action));
            if self.shown.get(action) == Some(&now) {
                continue;
            }
            match entry {
                Entry::Plain(item) => item.set_enabled(now.0),
                Entry::Check(item) => {
                    item.set_enabled(now.0);
                    item.set_checked(now.1.unwrap_or(false));
                }
            }
            self.shown.insert(*action, now);
        }
        let labels = state.recent();
        if self.recent_shown.as_ref() != Some(&labels) {
            if let Some(submenu) = &self.recent {
                // A failed item leaves it out; the menu stays usable.
                let _ = fill_recent(submenu, &labels);
            }
            self.recent_shown = Some(labels);
        }
    }
}

/// Puts the recordings `labels` names into Open Recent, then Clear Menu (disabled without
/// any), in place of what it showed.
fn fill_recent(submenu: &Submenu, labels: &[String]) -> muda::Result<()> {
    while submenu.remove_at(0).is_some() {}
    for (index, label) in labels.iter().enumerate() {
        let action = Action::OpenRecent(index as u8);
        submenu.append(&MenuItem::with_id(action_id(action), label, true, None))?;
    }
    if !labels.is_empty() {
        submenu.append(&PredefinedMenuItem::separator())?;
    }
    let clear = Action::ClearRecent;
    submenu.append(&MenuItem::with_id(
        action_id(clear),
        clear.label(),
        !labels.is_empty(),
        None,
    ))
}

/// Appends `items` to `submenu`, their action items to `entries`, and Open Recent (empty,
/// filled by [`NativeMenu::update`]) to `recent`.
fn append(
    submenu: &Submenu,
    items: &[Item],
    platform: Platform,
    entries: &mut Vec<(Action, Entry)>,
    recent: &mut Option<Submenu>,
) -> muda::Result<()> {
    for item in items {
        match item {
            Item::Action {
                action,
                label,
                shortcut,
                attached,
                checkable,
            } => {
                let text = match shortcut {
                    Some(chord) if !platform.menu_takes_shortcuts() => {
                        format!("{label}\t{}", chord.display(platform))
                    }
                    _ => (*label).to_owned(),
                };
                let accelerator = shortcut.filter(|_| *attached).and_then(key_accelerator);
                let entry = if *checkable {
                    let item = CheckMenuItem::with_id(action_id(*action), text, true, false, None);
                    item.set_key_accelerator(accelerator)?;
                    submenu.append(&item)?;
                    Entry::Check(item)
                } else {
                    let item = MenuItem::with_id(action_id(*action), text, true, None);
                    item.set_key_accelerator(accelerator)?;
                    submenu.append(&item)?;
                    Entry::Plain(item)
                };
                entries.push((*action, entry));
            }
            Item::System(system) => submenu.append(&system_item(*system))?,
            Item::Separator => submenu.append(&PredefinedMenuItem::separator())?,
            Item::Submenu { title, items } => {
                let inner = Submenu::new(*title, true);
                append(&inner, items, platform, entries, recent)?;
                submenu.append(&inner)?;
            }
            Item::Recent => {
                let inner = Submenu::new(RECENT_TITLE, true);
                submenu.append(&inner)?;
                *recent = Some(inner);
            }
        }
    }
    Ok(())
}

fn system_item(item: SystemItem) -> PredefinedMenuItem {
    let label = Some(item.label());
    match item {
        SystemItem::About => PredefinedMenuItem::about(
            label,
            Some(AboutMetadata {
                name: Some("Kymo".into()),
                version: Some(kymo_core::version().into()),
                comments: Some(ABOUT.into()),
                license: Some(LICENSE.into()),
                ..Default::default()
            }),
        ),
        SystemItem::Services => PredefinedMenuItem::services(label),
        SystemItem::Hide => PredefinedMenuItem::hide(label),
        SystemItem::HideOthers => PredefinedMenuItem::hide_others(label),
        SystemItem::ShowAll => PredefinedMenuItem::show_all(label),
        SystemItem::Minimize => PredefinedMenuItem::minimize(label),
        SystemItem::Zoom => PredefinedMenuItem::maximize(label),
        SystemItem::BringAllToFront => PredefinedMenuItem::bring_all_to_front(label),
    }
}

/// uxtheme's function with `ordinal`. Windows' dark mode for menus is undocumented; its
/// functions are found by ordinal, as muda and winit find theirs, and are missing before
/// Windows 10 1809 (the menus stay light there).
#[cfg(windows)]
fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    // SAFETY: uxtheme.dll is a system library that stays loaded once loaded; a missing one
    // or function gives null, which is checked.
    #[allow(unsafe_code)]
    unsafe {
        let uxtheme = LoadLibraryA(c"uxtheme.dll".as_ptr().cast());
        if uxtheme.is_null() {
            return None;
        }
        GetProcAddress(uxtheme, ordinal as *const u8)
    }
}

/// Lets the menus of `hwnd` be dark: muda draws the menu bar dark (its theme `Auto` or
/// `Dark`) only in a window allowed dark mode; winit doesn't allow it, as it only darkens
/// the title bar.
#[cfg(windows)]
fn allow_dark_mode(hwnd: isize) {
    // `AllowDarkModeForWindow(HWND, bool) -> bool`.
    const ALLOW_DARK_MODE_FOR_WINDOW: usize = 133;
    if let Some(f) = uxtheme(ALLOW_DARK_MODE_FOR_WINDOW) {
        // SAFETY: the function has had this signature since Windows 10 1809 (as muda and
        // winit use it), and `hwnd` is eframe's live window.
        #[allow(unsafe_code)]
        unsafe {
            let allow: unsafe extern "system" fn(isize, bool) -> bool = std::mem::transmute(f);
            allow(hwnd, true);
        }
    }
}

/// Sets the mode of the app's menus that drop down for `theme` (and of its context menus,
/// should it get any): the default app mode as it changes for [`Theme::System`], light or
/// dark whatever it is otherwise. Returns the mode it replaced (0 default, 1 follow the
/// app mode, 2 dark, 3 light), or `None` where Windows lacks the function.
#[cfg(windows)]
pub fn set_app_mode(theme: Theme) -> Option<i32> {
    // `SetPreferredAppMode(PreferredAppMode) -> PreferredAppMode` since Windows 10 1903
    // (`AllowDarkModeForApp(bool)` in 1809, where 1 also follows the app mode).
    const SET_PREFERRED_APP_MODE: usize = 135;
    // `FlushMenuThemes()`: open menus pick up the mode.
    const FLUSH_MENU_THEMES: usize = 136;
    // `RefreshImmersiveColorPolicyState()`: reads the default app mode again, which
    // uxtheme otherwise keeps from the start.
    const REFRESH_IMMERSIVE_COLOR_POLICY_STATE: usize = 104;
    let mode = match theme {
        Theme::System => 1,
        Theme::Dark => 2,
        Theme::Light => 3,
    };
    // SAFETY: each function has had this signature since Windows 10 1903. In 1809 the
    // mode's ordinal is `AllowDarkModeForApp(bool) -> bool`, which reads any mode here as
    // true: the menus follow the app mode there whatever the theme.
    #[allow(unsafe_code)]
    unsafe {
        if let Some(f) = uxtheme(REFRESH_IMMERSIVE_COLOR_POLICY_STATE) {
            let refresh: unsafe extern "system" fn() = std::mem::transmute(f);
            refresh();
        }
        let replaced = uxtheme(SET_PREFERRED_APP_MODE).map(|f| {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set(mode)
        });
        if let Some(f) = uxtheme(FLUSH_MENU_THEMES) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
        replaced
    }
}
