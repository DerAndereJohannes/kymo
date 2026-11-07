# Kymo

Kymo (after the kymograph) is a Rust viewer for large offline EEG/MEG recordings.
Frame cost and memory depend on the screen, never on the file size.
Later phases ship it in a Python wheel as an MNE-Python browser backend and launch it from MNELAB.

**Read `docs/design.md` before changing anything architectural.** It is the source of truth.
If an implementation detail has to deviate from it, say so explicitly and update the doc in the same change.
Phase 2 work follows `docs/phase2-plan.md` (steps, decisions, controls, and what phase 1 provides).

## Current scope

Phases 1 and 2 only (see "Plan" in `docs/design.md`):

1. `crates/kymo-core`: recording model, EDF/BDF reader, index, pyramid with DC sums, tile cache, fixed-grid query.
2. `crates/kymo`: egui + wgpu binary on top of `kymo-core`.

Out of scope for now: other file formats, filters/re-referencing/SSP, Python packaging, annotation editing.
But don't build anything that assumes EDF's record layout or a single sampling rate above the reader interface.

## Layout

```
crates/kymo-core     library, no GUI dependencies
crates/kymo          eframe/egui-wgpu binary (`kymo path/to/file.bdf`)
crates/kymo-testgen  generates synthetic EDF/BDF files for tests and benchmarks
scripts/             Python helpers (validation against MNE)
docs/design.md       design handoff
docs/phase2-plan.md  agreed plan for phase 2 (the viewer)
```

## Non-negotiables

- Never load a whole recording into memory. Source files are memory-mapped read-only and never modified.
- Peaks must never disappear: zoomed out = min/max envelopes, never subsampling. Gaps (EDF+D) stay gaps (NaN).
- Bucket boundaries are fixed in sample-index space (global tile grid), not relative to the view start.
- The UI thread never blocks on disk or computation; missing tiles fall back to coarser ones.
- Physical value = digital × gain + offset; EDF/BDF offsets can be huge.
- DC removal is a toggle, off by default.
- Default initial view: ~20 s from the start of the recording.

## Conventions

- Stable Rust, edition 2024. `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean.
- Error handling: `thiserror` in `kymo-core`, `anyhow` only in binaries. No `unwrap()` on file input.
- `unsafe` only where unavoidable (memory mapping), with a `// SAFETY:` comment.
- Tests are behavioral: they check outputs (decoded samples, tile values, annotations), not internal structure.
  Use `proptest` to compare tiles against a brute-force min/max pass.
- Test data comes from `crates/kymo-testgen`; never commit large binary files. Real recordings live outside the repo,
  pointed to by the `KYMO_TEST_DATA` env var; tests that need them are `#[ignore]` by default.
- Benchmarks with `criterion` in `crates/kymo-core/benches`.

## Commands

```
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored        # needs KYMO_TEST_DATA
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p kymo-testgen -- --help
cargo run -p kymo --release -- file.bdf
cargo bench -p kymo-core                   # build and decode benches on generated files
KYMO_BENCH_BDF=big.bdf cargo bench -p kymo-core --bench query   # big.bdf from `kymo-testgen bdf-256ch-8h`
cargo run --release -p kymo-core --example pyramid -- file.bdf [--sidecar DIR]   # build speed
python scripts/compare_mne.py file.bdf      # needs mne installed; also takes directories
python scripts/compare_mne.py --testgen     # check kymo-testgen presets against MNE
uv run --no-project --with mne python scripts/compare_mne.py ...   # without installing mne
```

## Working style

- Work in small, reviewable steps; each ends with passing tests and a commit.
- Before a larger step, propose a short plan and wait for approval.
- When unsure about a requirement, ask rather than guess.
