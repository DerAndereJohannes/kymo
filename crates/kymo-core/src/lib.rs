//! Core library of the Kymo viewer: recording model, readers, min/max pyramid,
//! tile cache and fixed-grid tile query. No GUI dependencies.
//!
//! See `docs/design.md` for the architecture.

/// Version of this crate, as set in `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(version(), "0.1.0");
    }
}
