//! Integration tests of kymo-core, one module per area.
//!
//! One binary for all of them, so they link once and run in parallel.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;

mod annotations;
mod edf_header;
mod edf_plus;
mod edf_reader;
mod grid;
mod prefetch;
mod pyramid;
mod query;
mod raw_tiles;
mod real_data;
mod recording;
mod service;
mod sidecar;
mod stim;
mod tile_cache;
mod tiles;
mod window_mean;
