//! The viewer: what is shown and how it moves, the key bindings, the menu bar as data, the
//! trace renderer, and the window built from them. The binary (`main.rs`) parses the
//! command line and opens the window.

pub mod action;
pub mod app;
pub mod document;
pub mod keymap;
pub mod menu;
#[cfg(any(target_os = "macos", windows))]
pub mod native_menu;
pub mod render;
pub mod shortcuts;
pub mod ui;
pub mod view;
pub mod viewer;
