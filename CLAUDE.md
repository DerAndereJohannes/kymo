# Kymo

Kymo (after the kymograph) is a Rust viewer for large offline EEG/MEG recordings.
Frame cost and memory depend on the screen, never on the file size.
Later phases ship it in a Python wheel as an MNE-Python browser backend and launch it from MNELAB.

**Read `docs/design.md` before changing anything architectural.** It is the source of truth.
If an implementation detail has to deviate from it, say so explicitly and update the doc in the same change.
Phase 3's plan, `docs/phase3-plan.md`, and phase 2's, `docs/phase2-plan.md`, hold the decisions they settled (phase 2's the controls).

## Current scope

Phases 1 to 4 (see "Plan" in `docs/design.md`) are done; phase 3 added viewer features (anti-aliasing, automatic scale, colors, annotation editing with saving and loading, theme selection), phase 4 file formats, part 1 (BrainVision, EEGLAB, GDF, FIFF, XDF with regular and irregular streams, BVRF with several participants as streams; `docs/phase4-plan.md`), an independent review of every algorithm against specifications and first principles (`docs/review.md`, with write-ups for upstream that only the user decides to report) and the measurements ("Phase 4 measurements" in `docs/design.md`); its open points stay in its plan. Phase 5 (channels and measuring: a recording info window, channel types as names (no fixed list) with DC removal and scaling per type, changed by hand and saved to a BIDS `channels.tsv`, zooming a single channel, measuring by ⇧-dragging) is in progress: steps 0 to 6 are done, step 7 (its measurements and docs) remains; its scope is agreed in `docs/phase5-plan.md`. Phase 6 (display filters, `docs/phase6-plan.md`) follows. Each step's details are proposed before it starts. The crates:

1. `crates/kymo-core`: recording model, EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF readers, annotation and `channels.tsv` files, index, pyramid with DC sums, tile cache, fixed-grid query.
2. `crates/kymo`: egui + wgpu binary on top of `kymo-core`.

Out of scope for now: formats beyond EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF until later parts add them (`docs/phase4-plan.md`), filters (phase 6, `docs/phase6-plan.md`), re-referencing (later), Python packaging (phase 7). SSP and bipolar montages are out of scope altogether.
But don't build anything that assumes EDF's record layout or a single sampling rate above the reader interface.

## Layout

```
crates/kymo-core     library, no GUI dependencies
crates/kymo          eframe/egui-wgpu viewer (`kymo path/to/file.bdf`); everything lives in its lib, `main.rs` only dispatches the parsed command
crates/kymo-testgen  generates synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files for tests and benchmarks
scripts/             Python helpers (validation against MNE)
docs/design.md       design handoff
CONTRIBUTING.md      toolchain setup per platform, build and checks
CHANGELOG.md         release notes in MNELAB's format; user-visible changes add a line under [UNRELEASED]
docs/phase2-plan.md  plan for phase 2 (the viewer), done
docs/phase3-plan.md  plan for phase 3 (viewer features), done
docs/phase4-plan.md  plan for phase 4 (file formats, part 1)
docs/phase5-plan.md  plan for phase 5 (channels and measuring)
docs/phase6-plan.md  plan for phase 6 (display filters)
docs/release-plan.md plan for the first GitHub release (0.1.0, after phase 5)
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
scripts/compare_mne.py file.bdf      # validation against MNE (needs uv); EDF, BDF, BrainVision (.vhdr), EEGLAB (.set), GDF, FIFF, XDF (against pyxdf and mnextend) and BVRF (.bvrh, against pybvrf); also takes directories
scripts/compare_mne.py --testgen     # check kymo-testgen presets against MNE
scripts/compare_mne.py --annotations ...   # also annotation files both ways
scripts/compare_mne.py --channels ...      # also channels.tsv files both ways, against MNE-BIDS
scripts/icons.sh                       # renders the icon's PNGs from crates/kymo/assets/*.svg (needs resvg)
gh workflow run binaries.yml            # release binaries as artifacts (macOS arm64, Windows, Linux x86_64 and arm64), each its .tar.gz (.zip on Windows), not zipped again
gh api repos/cbrnr/kymo/actions/runs/RUN/artifacts --jq '.artifacts[] | "\(.id) \(.name)"'   # their ids; then
gh api repos/cbrnr/kymo/actions/artifacts/ID/zip > NAME   # fetches one as is (`gh run download` expects a zip and fails)
```

## Working style

- Work in small, reviewable steps; each ends with passing tests and a commit.
- Keep commit messages short: a subject line of about 72 characters at most saying what changed; details, if needed, in a brief body.
- A user-visible change adds one line to `CHANGELOG.md` under `[UNRELEASED]` (section, imperative sentence, its pull request and author, or its commit if there is none) and updates that heading's date.
- Never add a `Co-Authored-By` line or any other AI attribution, anywhere: commits, PRs, issues, comments.
- Before a larger step, propose a short plan and wait for approval.
- When unsure about a requirement, ask rather than guess.
