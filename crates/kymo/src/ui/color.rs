//! A color swatch that opens a color picker, with Reset, for colors the user can pick.

use eframe::egui::color_picker::{Alpha, color_picker_color32};
use eframe::egui::{
    Button, Color32, CursorIcon, Popup, PopupCloseBehavior, Sense, Stroke, Ui, Vec2,
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
    let response = response.on_hover_cursor(CursorIcon::PointingHand);

    let mut choice = None;
    Popup::menu(&response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.spacing_mut().slider_width = 275.0;
            let mut edited = color;
            if color_picker_color32(ui, &mut edited, Alpha::Opaque) {
                choice = Some(Choice::Picked(edited));
            }
            if ui.add_enabled(picked, Button::new("Reset")).clicked() {
                choice = Some(Choice::Reset);
            }
        });
    choice
}
