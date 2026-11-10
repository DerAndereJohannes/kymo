//! Checks on real recordings outside the repo. Run with
//! `KYMO_TEST_DATA=/path/to/recordings cargo test --workspace -- --ignored`.
//!
//! For the comparison with MNE, run `scripts/compare_mne.py` on the same directory.

use std::ops::Range;
use std::path::{Path, PathBuf};

use kymo_core::eeglab::EeglabError;
use kymo_core::{GridId, OpenError, Recording};

fn recordings(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            recordings(&path, found);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["edf", "bdf", "vhdr", "set"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        }) {
            found.push(path);
        }
    }
}

/// The first and last 10 s and 8 windows in between (all of it for short grids).
fn windows(len: u64, rate: f64) -> Vec<Range<u64>> {
    let size = ((rate * 10.0) as u64).clamp(1, len.max(1));
    if len <= 10 * size {
        return vec![0..len];
    }
    (0..10)
        .map(|k| {
            let start = (len - size) * k / 9;
            start..start + size
        })
        .collect()
}

#[test]
#[ignore = "needs real recordings in KYMO_TEST_DATA"]
fn real_recordings_open_and_read() {
    let dir = std::env::var_os("KYMO_TEST_DATA")
        .expect("set KYMO_TEST_DATA to a directory of EDF, BDF, BrainVision and EEGLAB recordings");
    let mut files = Vec::new();
    recordings(Path::new(&dir), &mut files);
    assert!(
        !files.is_empty(),
        "no .edf, .bdf, .vhdr or .set files under {dir:?}"
    );

    for path in &files {
        let opened = match kymo_core::open(path) {
            Ok(opened) => opened,
            // EEGLAB files kymo refuses by design (MNE's testing data has both).
            Err(
                e @ OpenError::Eeglab {
                    source: EeglabError::Epoched(_),
                    ..
                },
            ) => {
                eprintln!("{e} (skipped)");
                continue;
            }
            Err(e @ OpenError::Unsupported { .. }) if e.to_string().contains("v7.3") => {
                eprintln!("{e} (skipped)");
                continue;
            }
            Err(e) => panic!("{e}"),
        };
        let (rec, annotations) =
            Recording::new(opened).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!(
            "{}: {}, {:.1} s, {} channels, {} grids, {} annotations",
            path.display(),
            rec.format(),
            rec.duration(),
            rec.channels().len(),
            rec.grids().len(),
            annotations.len()
        );
        for w in rec.warnings() {
            eprintln!("  warning: {w}");
        }
        for (g, grid) in rec.grids().iter().enumerate() {
            let ids = grid.channels();
            for w in windows(grid.len(), grid.rate()) {
                let n = (w.end - w.start) as usize;
                let mut out = vec![0.0f32; ids.len() * n];
                rec.read_physical(GridId(g as u32), ids, w.clone(), &mut out)
                    .unwrap_or_else(|e| panic!("{}: grid {g}, {w:?}: {e}", path.display()));
                // Float samples may be stored as NaN; integers are always finite.
                let float = grid.sample_type() == kymo_core::SampleType::F32;
                for (k, row) in out.chunks_exact(n.max(1)).enumerate() {
                    for (i, x) in (w.start..).zip(row) {
                        let present = grid.segments().iter().any(|s| s.contains(&i));
                        let ok = if present {
                            x.is_finite() || (float && x.is_nan())
                        } else {
                            x.is_nan()
                        };
                        assert!(
                            ok,
                            "{}: channel {:?} sample {i} ({}): {x}",
                            path.display(),
                            ids[k],
                            if present { "present" } else { "in a gap" }
                        );
                    }
                }
            }
        }
    }
}
