//! Go to Time…: a small window with a field for a time from the start of the recording.

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Align2, Key, RichText, TextEdit};

use super::Overlay;
use super::axis::parse_time;
use crate::action::Action;

/// What the window's field holds between frames.
#[derive(Clone, Debug, Default)]
pub struct GoToField {
    pub text: String,
    /// Whether the last text tried wasn't a time.
    pub invalid: bool,
    /// Whether the field takes the keyboard, with its text selected, at the next frame.
    pub focus: bool,
}

impl GoToField {
    /// A field holding `text` (the current time), selected so typing replaces it.
    pub fn new(text: String) -> GoToField {
        GoToField {
            text,
            invalid: false,
            focus: true,
        }
    }
}

/// How times can be written, for the field's hint and its error.
const EXAMPLES: &str = "90, 1:30 or 1:02:03.5";

/// The Go to Time window, while `open`. Returns the time asked for, in seconds from the
/// start of the recording, once Enter or Go is pressed with a time in the field (which
/// closes the window); Esc in the field closes it too.
pub fn go_to_window(ctx: &egui::Context, open: &mut bool, field: &mut GoToField) -> Option<f64> {
    let mut asked = None;
    let mut close = false;
    egui::Window::new(Action::GoToTime.label().trim_end_matches('…'))
        .id(Overlay::GoToTime.id())
        .open(open)
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.label("Time from the Start of the Recording:");
            let output = TextEdit::singleline(&mut field.text)
                .hint_text(EXAMPLES)
                .desired_width(220.0)
                .show(ui);
            let response = output.response;
            if field.focus {
                field.focus = false;
                let mut state = output.state;
                let all =
                    CCursorRange::two(CCursor::new(0), CCursor::new(field.text.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), response.id);
                response.request_focus();
            }
            if response.changed() {
                field.invalid = false;
            }
            let (enter, escape) =
                ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            close = response.lost_focus() && escape;
            if field.invalid {
                let color = ui.visuals().error_fg_color;
                ui.label(RichText::new(format!("Write a time as {EXAMPLES}.")).color(color));
            }
            ui.add_space(4.0);
            // At the right, in a row of its own height (a layout of its own would take the
            // window's whole height).
            let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            let go = ui
                .allocate_ui_with_layout(row, layout, |ui| ui.button("Go"))
                .inner
                .clicked();
            if go || response.lost_focus() && enter {
                match parse_time(&field.text) {
                    Some(t) => asked = Some(t),
                    None => {
                        field.invalid = true;
                        field.focus = true;
                    }
                }
            }
        });
    if asked.is_some() || close {
        *open = false;
    }
    asked
}
