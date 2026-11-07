//! Shared test helpers and proptest strategies.

// Each test crate uses only some of these.
#![allow(dead_code)]

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;

/// A grid length up to `max_len` and random gap structure: sorted, disjoint, non-touching
/// segments (possibly none, possibly covering everything).
pub fn len_and_segments(max_len: u64) -> impl Strategy<Value = (u64, Vec<Range<u64>>)> {
    (0..=max_len)
        .prop_flat_map(|len| (Just(len), proptest::collection::btree_set(0..=len, 0..12)))
        .prop_map(|(len, cuts)| {
            let cuts: Vec<u64> = cuts.into_iter().collect();
            let segments = cuts.chunks_exact(2).map(|c| c[0]..c[1]).collect();
            (len, segments)
        })
}

/// A range inside `0..=len`, possibly empty.
pub fn sub_range(len: u64) -> impl Strategy<Value = Range<u64>> {
    (0..=len, 0..=len).prop_map(|(a, b)| a.min(b)..a.max(b))
}

/// Whether sample `i` lies in one of `segments`.
pub fn is_present(segments: &[Range<u64>], i: u64) -> bool {
    segments.iter().any(|s| s.contains(&i))
}

/// A file in the temp directory, deleted on drop.
pub struct TempFile(PathBuf);

impl TempFile {
    pub fn new(extension: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("kymo-test-{}-{n}.{extension}", std::process::id());
        TempFile(std::env::temp_dir().join(name))
    }

    pub fn written(spec: &kymo_testgen::Spec) -> Self {
        let file = TempFile::new(spec.format.extension());
        kymo_testgen::write(spec, &file.0).unwrap();
        file
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
