# Kymo

Kymo (after the kymograph) is a Rust viewer for large offline EEG/MEG recordings.
Frame cost and memory depend on the screen, never on the file size.
Later phases ship it in a Python wheel as an MNE-Python browser backend and launch it from MNELAB.

**Read `docs/design.md` before changing anything architectural.** It is the source of truth.
If an implementation detail has to deviate from it, say so explicitly and update the doc in the same change.
Phase 3's plan, `docs/phase3-plan.md`, and phase 2's, `docs/phase2-plan.md`, hold the decisions they settled (phase 2's the controls).

## Current scope

Phases 1 to 3 (see "Plan" in `docs/design.md`) are done; phase 3 added viewer features (anti-aliasing, automatic scale, colors, annotation editing with saving and loading, theme selection). Phase 4 (file formats, part 1: BrainVision, EEGLAB, GDF, FIFF, XDF) is under way, BrainVision, EEGLAB, GDF and FIFF (raw data; split files, annotations and bads next) are read; its scope is agreed in `docs/phase4-plan.md`. Phase 5 (display filters: high-pass and low-pass) follows; its scope is agreed in `docs/phase5-plan.md`. Each step's details are proposed before it starts. The crates:

1. `crates/kymo-core`: recording model, EDF/BDF, BrainVision, EEGLAB, GDF and FIFF readers, index, pyramid with DC sums, tile cache, fixed-grid query.
2. `crates/kymo`: egui + wgpu binary on top of `kymo-core`.

Out of scope for now: formats beyond EDF/BDF, BrainVision, EEGLAB, GDF and FIFF until phase 4 adds them (`docs/phase4-plan.md`), filters (phase 5, `docs/phase5-plan.md`), re-referencing (later), Python packaging (phase 6). SSP and bipolar montages are out of scope altogether.
But don't build anything that assumes EDF's record layout or a single sampling rate above the reader interface.

## Layout

```
crates/kymo-core     library, no GUI dependencies
crates/kymo          eframe/egui-wgpu viewer (`kymo path/to/file.bdf`); everything lives in its lib, `main.rs` only dispatches the parsed command
crates/kymo-testgen  generates synthetic EDF/BDF, BrainVision, EEGLAB, GDF and FIFF files for tests and benchmarks
scripts/             Python helpers (validation against MNE)
docs/design.md       design handoff
CONTRIBUTING.md      toolchain setup per platform, build and checks
docs/phase2-plan.md  plan for phase 2 (the viewer), done
docs/phase3-plan.md  plan for phase 3 (viewer features), done
docs/phase4-plan.md  plan for phase 4 (file formats, part 1)
docs/phase5-plan.md  plan for phase 5 (display filters)
```

## Non-negotiables

- Never load a whole recording into memory. Source files are memory-mapped read-only and never modified.
- Peaks must never disappear: zoomed out = min/max envelopes, never subsampling. Gaps (EDF+D) stay gaps (NaN).
- Bucket boundaries are fixed in sample-index space (global tile grid), not relative to the view start.
- The UI thread never blocks on disk or computation; missing tiles fall back to coarser ones.
- Physical value = digital × gain + offset; EDF/BDF offsets can be huge.
- DC removal is a setting (in Settings, remembered), on by default (off until Nov 11, 2026).
- Default initial view: ~20 s from the start of the recording.

## Conventions

- Stable Rust, edition 2024. `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean.
- Error handling: `thiserror` in `kymo-core`, `anyhow` only in binaries. No `unwrap()` on file input.
- `unsafe` only where unavoidable (memory mapping; attaching the native menu to the window on Windows, as muda requires, and letting it follow the theme there (uxtheme, and the app mode from the registry); attaching to the terminal's console on Windows; reading peak memory in the frames benchmark), with a `// SAFETY:` comment.
- Tests are behavioral: they check outputs (decoded samples, tile values, annotations), not internal structure.
  Use `proptest` to compare tiles against a brute-force min/max pass.
- Integration tests live in one binary per crate, `tests/all/`, a module per area (`main.rs` lists them), so they link once
  and run in parallel. Proptest keeps their failure seeds in `tests/proptest-regressions/<module>.txt`; commit them.
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
cargo bench -p kymo --bench frames -- file.bdf [--fresh] [--antialias] [--edit]   # viewer frame times and memory; --edit adds annotation editing (on `kymo-testgen edf+c-annotations`)
scripts/compare_mne.py file.bdf      # validation against MNE (needs uv); EDF, BDF, BrainVision (.vhdr), EEGLAB (.set), GDF and FIFF; also takes directories
scripts/compare_mne.py --testgen     # check kymo-testgen presets against MNE
scripts/compare_mne.py --annotations ...   # also annotation files both ways
gh workflow run binaries.yml            # release binaries as artifacts (macOS arm64, Windows, Linux x86_64 and arm64), each its .tar.gz (.zip on Windows), not zipped again
gh api repos/cbrnr/kymo/actions/runs/RUN/artifacts --jq '.artifacts[] | "\(.id) \(.name)"'   # their ids; then
gh api repos/cbrnr/kymo/actions/artifacts/ID/zip > NAME   # fetches one as is (`gh run download` expects a zip and fails)
```

## Working style

- Work in small, reviewable steps; each ends with passing tests and a commit.
- Never add a `Co-Authored-By` line or any other AI attribution, anywhere: commits, PRs, issues, comments.
- Before a larger step, propose a short plan and wait for approval.
- When unsure about a requirement, ask rather than guess.
