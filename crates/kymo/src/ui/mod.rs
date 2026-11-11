//! Pieces of the window: the time axis, scrollbars, the status line, the sidebar, trace
//! colors and the color picker, the egui-drawn menus and windows, the Settings window, the
//! renderer inside egui, and which of the windows is in front.

pub mod axis;
pub mod color;
pub mod go_to;
pub mod gpu;
pub mod header;
pub mod menus;
pub mod palette;
pub mod readout;
pub mod scrollbar;
pub mod settings;
pub mod status;

use eframe::egui::{Label, RichText};

/// Size of hints, in points: below the body text's 14, so a hint reads as secondary to the
/// label above it, as macOS and Windows write such text in dialogs.
pub const HINT_SIZE: f32 = 12.0;

/// A description below a field or setting: dimmed, smaller than the body text, upright, and
/// wrapped at the width it has. Settings and Go to Time write theirs with it.
pub fn hint(text: &str) -> Label {
    Label::new(hint_text(text).weak()).wrap()
}

/// `text` at the hints' size, for what sits with them in another color (an error below a
/// field).
pub fn hint_text(text: &str) -> RichText {
    RichText::new(text).size(HINT_SIZE)
}

/// The windows over the recording, which Esc and ⌘W close, the one in front first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Overlay {
    Settings,
    Shortcuts,
    About,
    GoToTime,
}

impl Overlay {
    pub const ALL: [Overlay; 4] = [
        Overlay::Settings,
        Overlay::Shortcuts,
        Overlay::About,
        Overlay::GoToTime,
    ];

    /// The window's id, which also names its layer, so the one in front can be found.
    pub fn id(self) -> eframe::egui::Id {
        eframe::egui::Id::unique(("kymo overlay", self))
    }

    /// Which of `open` is in front, from egui's layer order of the last frame; one that
    /// hasn't been drawn yet counts as behind the others.
    pub fn front(
        ctx: &eframe::egui::Context,
        open: impl IntoIterator<Item = Overlay>,
    ) -> Option<Overlay> {
        let layers: Vec<eframe::egui::Id> = ctx.memory(|m| m.layer_ids().map(|l| l.id).collect());
        open.into_iter()
            .max_by_key(|overlay| layers.iter().position(|&id| id == overlay.id()))
    }
}
