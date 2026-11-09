//! The native menu bar on macOS and Windows, built with `muda` from the menu description.
//!
//! On macOS the menu takes its items' ⌘ shortcuts as key equivalents, so the system hands
//! them to the menu (and flashes its title) before the window sees the key. Every menu
//! shortcut has ⌘ for that reason: a macOS menu item can only show a key by taking it, and
//! a bare key taken by the menu would never reach a text field. On Windows every item shows
//! its shortcut after a tab, and egui handles the keys (see
//! [`Platform::menu_takes_shortcuts`]).

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Key as EguiKey};
use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};
use muda::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

use crate::action::Action;
use crate::keymap::{Chord, Platform};
use crate::menu::{Item, Role, SystemItem, menu_bar};
use crate::ui::menus::{ABOUT, LICENSE, MenuState};

/// The menu bar and its action items.
pub struct NativeMenu {
    menu: Menu,
    items: Vec<(Action, Entry)>,
    actions: HashMap<MenuId, Action>,
    events: Receiver<MenuId>,
    /// What each item showed at the last [`update`](Self::update).
    shown: HashMap<Action, (bool, Option<bool>)>,
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
    /// Builds the menu bar for `platform`. Menu events wake `ctx`. Call on the main thread.
    pub fn new(platform: Platform, ctx: &egui::Context) -> muda::Result<NativeMenu> {
        let (tx, events) = mpsc::channel();
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event.id);
            wake.request_repaint();
        }));

        let menu = Menu::new();
        let mut items = Vec::new();
        for description in menu_bar(platform) {
            let submenu = Submenu::new(description.title, true);
            append(&submenu, &description.items, platform, &mut items)?;
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
        let actions = items
            .iter()
            .map(|(action, _)| (action_id(*action), *action))
            .collect();
        Ok(NativeMenu {
            menu,
            items,
            actions,
            events,
            shown: HashMap::new(),
        })
    }

    /// Makes this the application's menu bar, replacing winit's default one.
    #[cfg(target_os = "macos")]
    pub fn install(&self) {
        self.menu.init_for_nsapp();
    }

    /// Attaches the menu bar to `window`.
    #[cfg(windows)]
    pub fn install(
        &self,
        window: &impl eframe::egui_wgpu::wgpu::rwh::HasWindowHandle,
    ) -> anyhow::Result<()> {
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
        Ok(())
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
    }
}

/// Appends `items` to `submenu`, and their action items to `entries`.
fn append(
    submenu: &Submenu,
    items: &[Item],
    platform: Platform,
    entries: &mut Vec<(Action, Entry)>,
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
                append(&inner, items, platform, entries)?;
                submenu.append(&inner)?;
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

/// Lets the menus follow Windows' dark mode. muda draws the menu bar dark (its theme `Auto`)
/// only in a window allowed dark mode, and the menus that drop down from it are dark only
/// in an app that opts in; winit does neither, as it only darkens the title bar. The
/// functions are uxtheme's, undocumented and found by ordinal, as muda and winit find
/// theirs; where they are missing (before Windows 10 1809), the menus stay light.
#[cfg(windows)]
fn allow_dark_mode(hwnd: isize) {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    // `AllowDarkModeForWindow(HWND, bool) -> bool`.
    const ALLOW_DARK_MODE_FOR_WINDOW: usize = 133;
    // `SetPreferredAppMode(PreferredAppMode) -> PreferredAppMode` since Windows 10 1903,
    // `AllowDarkModeForApp(bool) -> bool` before; 1 means "follow the system" in both.
    const SET_PREFERRED_APP_MODE: usize = 135;
    // `FlushMenuThemes()`: menus pick up the mode.
    const FLUSH_MENU_THEMES: usize = 136;
    // SAFETY: uxtheme.dll is a system library that stays loaded; each function is looked
    // up by its ordinal and called with the signature it has had since Windows 10 1809
    // (the same ones muda and winit use), with `hwnd`, eframe's live window.
    #[allow(unsafe_code)]
    unsafe {
        let uxtheme = LoadLibraryA(c"uxtheme.dll".as_ptr().cast());
        if uxtheme.is_null() {
            return;
        }
        let find = |ordinal: usize| GetProcAddress(uxtheme, ordinal as *const u8);
        if let Some(f) = find(SET_PREFERRED_APP_MODE) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set(1);
        }
        if let Some(f) = find(ALLOW_DARK_MODE_FOR_WINDOW) {
            let allow: unsafe extern "system" fn(isize, bool) -> bool = std::mem::transmute(f);
            allow(hwnd, true);
        }
        if let Some(f) = find(FLUSH_MENU_THEMES) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}
