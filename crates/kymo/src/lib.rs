//! The viewer: what is shown and how it moves, the annotation overlay and stim annotations,
//! the key bindings, the menu bar as data, the trace renderer, the settings, the command
//! line, and the window built from them. The binary (`main.rs`) only dispatches the
//! command.

pub mod action;
pub mod annotations;
pub mod app;
pub mod app_mode;
pub mod cli;
pub mod document;
pub mod history;
pub mod keymap;
pub mod menu;
#[cfg(any(target_os = "macos", windows))]
pub mod native_menu;
pub mod render;
pub mod scale;
pub mod settings;
pub mod shortcuts;
pub mod stim_annotations;
pub mod ui;
pub mod view;
pub mod viewer;
