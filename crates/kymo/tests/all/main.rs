//! Integration tests of the viewer, one module per area.
//!
//! One binary for all of them, so they link once and run in parallel.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

mod annotations;
mod axis;
mod frame;
mod history;
mod keymap;
mod menu;
mod native_menu;
mod palette;
mod readout;
mod render;
mod scale;
mod settings;
mod shortcuts;
mod stim_annotations;
mod ui;
mod view;
mod widgets;
