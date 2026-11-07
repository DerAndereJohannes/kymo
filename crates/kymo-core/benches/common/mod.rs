//! Benchmark inputs: files generated into Cargo's target directory, and the large recording
//! named by `KYMO_BENCH_BDF`.

// Benchmark setup: panicking on a broken input is the point.
#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use kymo_core::Recording;
use kymo_testgen::Spec;

/// Where generated files and sidecars go.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("kymo-bench")
}

/// `preset` (which must have 1 s records and no gaps) cut to `seconds`, written into
/// [`dir`] unless a file of the right size is there already.
pub fn generated(preset: &str, seconds: u64) -> PathBuf {
    let mut spec = Spec::preset(preset).unwrap();
    assert!(spec.record_duration == 1.0 && spec.gaps.is_empty());
    spec.n_records = seconds;
    let path = dir().join(format!("{preset}-{seconds}s.{}", spec.format.extension()));
    let size = spec.file_size().unwrap();
    if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(size) {
        std::fs::create_dir_all(dir()).unwrap();
        kymo_testgen::write(&spec, &path).unwrap();
    }
    path
}

pub fn open(path: &Path) -> Recording {
    Recording::new(kymo_core::open(path).unwrap()).unwrap().0
}

/// The 8 h 256-channel BDF named by `KYMO_BENCH_BDF`, if set and present.
pub fn large() -> Option<PathBuf> {
    let found = std::env::var_os("KYMO_BENCH_BDF")
        .map(PathBuf::from)
        .filter(|p| p.is_file());
    if found.is_none() {
        eprintln!(
            "skipping: set KYMO_BENCH_BDF to a file written by \
             `cargo run --release -p kymo-testgen -- bdf-256ch-8h <file>` (45 GB)"
        );
    }
    found
}

/// Seconds as a short human-readable duration.
pub fn seconds(s: f64) -> String {
    if s < 1.0 {
        format!("{:.1} ms", s * 1e3)
    } else {
        format!("{s:.1} s")
    }
}
