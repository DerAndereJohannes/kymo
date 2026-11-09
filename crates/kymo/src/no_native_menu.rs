//! No native menu bar on this platform (Linux and other Unix systems): egui draws the menus.
//! [`NativeMenu`] has no values here, so the app uses it without asking the platform.

use crate::action::Action;
use crate::settings::Theme;
use crate::ui::menus::MenuState;

/// A native menu bar, of which this platform has none.
pub enum NativeMenu {}

impl NativeMenu {
    pub fn attach(_: &eframe::CreationContext<'_>) -> anyhow::Result<NativeMenu> {
        anyhow::bail!("not on this platform")
    }

    pub fn set_theme(&self, _: Theme) {
        match *self {}
    }

    pub fn poll(&self) -> Vec<Action> {
        match *self {}
    }

    pub fn update(&mut self, _: &dyn MenuState) {
        match *self {}
    }
}
