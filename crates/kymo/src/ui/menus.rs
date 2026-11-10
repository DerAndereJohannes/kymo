//! The menu bar drawn by egui (Linux, where there is no native one), the shortcut overview
//! and the About window. Native menus are in `native_menu`.

use eframe::egui::accesskit::Role;
use eframe::egui::{
    self, Atom, Button, IdSalt, Response, RichText, ScrollArea, Shape, StrokeKind, Ui, Vec2,
    WidgetInfo, pos2,
};

use crate::action::Action;
use crate::keymap::Platform;
use crate::menu::{Item, SystemItem, menu_bar};
use crate::shortcuts::overview;
use crate::ui::Overlay;

/// What a menu shows for an action: whether it can run now, and its checkmark if it has
/// one.
pub trait MenuState {
    fn enabled(&self, action: Action) -> bool;
    /// `Some(on)` for checkable items.
    fn checked(&self, action: Action) -> Option<bool>;
}

/// The menu bar of `platform` drawn by egui. Returns the action of a clicked item.
pub fn egui_menu_bar(ui: &mut Ui, platform: Platform, state: &dyn MenuState) -> Option<Action> {
    let mut clicked = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menu_bar(platform) {
            ui.menu_button(menu.title, |ui| {
                menu_items(ui, &menu.items, platform, state, &mut clicked);
            });
        }
    });
    clicked
}

/// Draws `items` into an open menu; `clicked` gets the action of a clicked item.
fn menu_items(
    ui: &mut Ui,
    items: &[Item],
    platform: Platform,
    state: &dyn MenuState,
    clicked: &mut Option<Action>,
) {
    for item in items {
        match item {
            Item::Action {
                action,
                label,
                shortcut,
                ..
            } => {
                let shortcut = shortcut.map(|chord| chord.display(platform));
                let enabled = state.enabled(*action);
                let response = match state.checked(*action) {
                    Some(on) => check_item(ui, enabled, on, action.radio(), label, shortcut),
                    None => {
                        let mut button = Button::new(*label);
                        if let Some(shortcut) = shortcut {
                            button = button.shortcut_text(shortcut);
                        }
                        ui.add_enabled(enabled, button)
                    }
                };
                if response.clicked() {
                    *clicked = Some(*action);
                    ui.close();
                }
            }
            // Only macOS has system items, and its menus are native.
            Item::System(SystemItem::About) => {
                if ui.button(SystemItem::About.label()).clicked() {
                    *clicked = Some(Action::About);
                    ui.close();
                }
            }
            Item::System(_) => {}
            Item::Separator => {
                ui.separator();
            }
            Item::Submenu { title, items } => {
                ui.menu_button(*title, |ui| menu_items(ui, items, platform, state, clicked));
            }
        }
    }
}

/// A menu item with a checkbox (or a radio button, if `radio`), `on` or not, and its
/// shortcut: a button, so it highlights as the other items do, with the box drawn into room
/// left at its start (egui's selectable buttons only change their background, and its
/// checkboxes and radio buttons have no shortcut).
fn check_item(
    ui: &mut Ui,
    enabled: bool,
    on: bool,
    radio: bool,
    label: &str,
    shortcut: Option<String>,
) -> Response {
    let icon = IdSalt::new("check");
    let size = ui.spacing().icon_width;
    let mut button = Button::new((Atom::custom(icon, Vec2::splat(size)), label));
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(shortcut);
    }
    let atoms = ui.add_enabled_ui(enabled, |ui| button.atom_ui(ui)).inner;
    let response = atoms.response.clone();
    if let Some(rect) = atoms.rect(icon) {
        let (small, big) = ui.spacing().icon_rectangles(rect);
        // The app's look, as menus take the outline off inactive widgets.
        let style = ui.ctx().global_style();
        let widgets = &style.visuals.widgets;
        let visuals = if !enabled {
            &widgets.noninteractive
        } else if response.hovered() {
            &widgets.hovered
        } else {
            &widgets.inactive
        };
        let painter = ui.painter();
        if radio {
            // As egui's radio buttons.
            painter.circle(
                big.center(),
                big.width() / 2.0,
                visuals.bg_fill,
                visuals.bg_stroke,
            );
            if on {
                painter.circle_filled(small.center(), small.width() / 3.0, visuals.fg_stroke.color);
            }
        } else {
            // As egui's checkboxes: rounding at most 30 % of the width, so it stays a square.
            painter.rect(
                big,
                visuals.corner_radius.at_most((0.3 * size).round() as u8),
                visuals.bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
            if on {
                painter.add(Shape::line(
                    vec![
                        pos2(small.left(), small.center().y),
                        pos2(small.center().x, small.bottom()),
                        pos2(small.right(), small.top()),
                    ],
                    visuals.fg_stroke,
                ));
            }
        }
    }
    let role = if radio {
        Role::RadioButton
    } else {
        Role::CheckBox
    };
    response.widget_info(|| WidgetInfo::selected(role, enabled, on, label));
    response
}

/// The shortcut overview, while `open`.
pub fn shortcuts_window(ctx: &egui::Context, open: &mut bool, platform: Platform) {
    egui::Window::new(Action::Shortcuts.label())
        .id(Overlay::Shortcuts.id())
        .open(open)
        .collapsible(false)
        .default_height(480.0)
        .show(ctx, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                for (i, section) in overview(platform).iter().enumerate() {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    ui.label(RichText::new(section.title).strong());
                    egui::Grid::new(("shortcuts", i))
                        .num_columns(2)
                        .spacing([24.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for row in &section.rows {
                                ui.label(RichText::new(&row.keys).monospace());
                                ui.label(row.action);
                                ui.end_row();
                            }
                        });
                }
            });
        });
}

/// The About window (Windows and Linux; macOS uses the system's About panel).
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new(Action::About.label())
        .id(Overlay::About.id())
        .open(open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Kymo");
                ui.label(format!("Version {}", kymo_core::version()));
                ui.add_space(6.0);
                ui.label(ABOUT);
                ui.add_space(6.0);
                ui.label(RichText::new(LICENSE).weak());
            });
        });
}

/// Adds macOS's font for its shortcut symbols (⌘ ⌥ ⇧ and arrows) as egui's last fallback.
/// egui's own fonts lack them, and the system font fallback eframe installs only searches
/// generic families, which lack them too; Lucida Grande is what macOS itself falls back to.
/// Elsewhere shortcuts are written as words ("Ctrl+O"), so nothing is needed.
pub fn add_symbol_font(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/LucidaGrande.ttc") {
        use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
        let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
            .map(|family| InsertFontFamily {
                family,
                priority: FontPriority::Lowest,
            })
            .to_vec();
        ctx.add_font(FontInsert {
            name: "Lucida Grande".into(),
            data: egui::FontData::from_owned(bytes),
            families,
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = ctx;
}

/// What Kymo is, for About windows and panels.
pub const ABOUT: &str = "A viewer for large EEG and MEG recordings.";
pub const LICENSE: &str = "BSD 3-Clause License";
