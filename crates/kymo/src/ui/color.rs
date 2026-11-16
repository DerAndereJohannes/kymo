//! A color swatch that opens a color picker, with Reset, for colors the user can pick.

use eframe::egui::color_picker::{Alpha, color_picker_color32};
use eframe::egui::{
    Button, Color32, CursorIcon, Popup, PopupCloseBehavior, Response, Sense, Stroke, Ui, Vec2,
};

/// What the user did with a [`swatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Picked(Color32),
    /// Back to the default color.
    Reset,
}

/// A swatch of `size` in `color`, labeled `label` for screen readers. A click opens a color
/// picker starting at `color`, and Reset, enabled while the color is `picked` rather than
/// the default. Returns what was chosen in this frame.
pub fn swatch(
    ui: &mut Ui,
    label: &str,
    color: Color32,
    picked: bool,
    size: Vec2,
) -> Option<Choice> {
    let response = swatch_button(ui, label, color, size);
    let mut choice = None;
    Popup::menu(&response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            if let Some(c) = picker(ui, color) {
                choice = Some(Choice::Picked(c));
            }
            if ui.add_enabled(picked, Button::new("Reset")).clicked() {
                choice = Some(Choice::Reset);
            }
        });
    choice
}

/// A swatch of `size` in `color` that can be clicked, labeled `label` for screen readers,
/// outlined while hovered.
pub fn swatch_button(ui: &mut Ui, label: &str, color: Color32, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let label = label.to_owned();
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
    let stroke = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke
    } else {
        Stroke::NONE
    };
    ui.painter()
        .rect(rect, 2.0, color, stroke, eframe::egui::StrokeKind::Outside);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A color picker starting at `color`; the color picked in this frame, if one was.
pub fn picker(ui: &mut Ui, color: Color32) -> Option<Color32> {
    ui.spacing_mut().slider_width = 275.0;
    let mut edited = color;
    color_picker_color32(ui, &mut edited, Alpha::Opaque).then_some(edited)
}
