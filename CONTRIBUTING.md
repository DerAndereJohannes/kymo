# Contributing to Kymo

## Toolchain

Kymo builds with stable Rust, 1.95 or newer, installed through [rustup](https://rustup.rs). The default rustup profile includes `rustfmt` and `clippy`, which the checks below need. Every platform also needs a C compiler, for the BLAKE3 crate's build script.

### macOS

1. Install the Xcode Command Line Tools, which provide the C compiler, the linker and git: `xcode-select --install`
2. Install rustup, either way:
   - with the official script: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - with Homebrew: `brew install rustup`, then follow its caveats to put it on your `PATH`
3. Install the stable toolchain and make it the default: `rustup default stable`. The script already does this, so after it the command only confirms it.

The viewer draws with Metal, which every supported Mac has.

### Windows

1. Install the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload, which provides the MSVC compiler and linker. The rustup installer in step 2 offers to install them if they're missing.
2. Install rustup and the stable toolchain with [rustup-init.exe](https://rustup.rs), using the default `x86_64-pc-windows-msvc` host.

The viewer draws with DX12 or Vulkan, so a current graphics driver is enough.

### Linux

1. Install a C compiler and a Vulkan driver. The viewer draws with Vulkan, and the renderer's tests need a driver too: on a machine without a GPU, Mesa's software driver (lavapipe) works.
   - Arch: `sudo pacman -S base-devel` and your GPU's Vulkan driver (`vulkan-radeon`, `vulkan-intel` or `nvidia-utils`), or `vulkan-swrast` without a GPU
   - Debian and Ubuntu: `sudo apt install build-essential mesa-vulkan-drivers`
   - Fedora: `sudo dnf install gcc mesa-vulkan-drivers`
2. Install rustup, either way:
   - with the official script: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - with your package manager: `sudo pacman -S rustup` on Arch, `sudo apt install rustup` on Debian 13 and Ubuntu 24.04 or newer, `sudo dnf install rustup` on Fedora, followed there by `rustup-init`
3. Install the stable toolchain and make it the default: `rustup default stable`. The script and Fedora's `rustup-init` already do this, so after them the command only confirms it.

The build doesn't need any other development packages: X11, Wayland and fontconfig are loaded at run time, and file dialogs and the theme go through the desktop portal.

## Build and test

```
cargo build --workspace
cargo test --workspace
cargo run -p kymo --release -- file.bdf
```

`cargo run -p kymo-testgen -- --help` lists the synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files the generator can write, if you need a recording to open.

Without a GPU, the renderer's tests pass and print that they were skipped. Set `KYMO_REQUIRE_GPU=1` to make them fail instead, as CI does on Linux.

Some tests need real recordings. These are kept outside the repository: point `KYMO_TEST_DATA` to them and run `cargo test --workspace -- --ignored`. `scripts/compare_mne.py` checks Kymo's reader against MNE-Python and needs [uv](https://docs.astral.sh/uv/).

## Before you commit

CI runs these on Linux, macOS and Windows, and all three must be clean:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If your change is visible to users, add a line for it to `CHANGELOG.md` under `[UNRELEASED]`, in the section that fits (✨ Added, 🌀 Changed, 🗑️ Removed, 🔧 Fixed): one sentence starting with a verb in the imperative, followed by your pull request and name, for example `([#12](https://github.com/cbrnr/kymo/pull/12) by [Clemens Brunner](https://github.com/cbrnr))`.

Read `docs/design.md` before changing anything architectural, and `CLAUDE.md` for the project's rules and conventions.

## Releasing

Releases are made from `main` by pushing a tag; `.github/workflows/release.yml` does the rest up to a draft release.

1. Set the version in the workspace's `Cargo.toml` (`version = "X.Y.Z"`; a pre-release such as `X.Y.Z-rc.1` becomes a GitHub pre-release) and run `cargo build` to update `Cargo.lock`.
2. In `CHANGELOG.md`, rename the `[UNRELEASED]` heading to `[X.Y.Z]` with today's date.
3. Commit both, and tag and push the commit:

   ```sh
   git commit -am "Release X.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

   The workflow checks that the tag matches the version and that the changelog has an entry for it, runs the tests, builds the macOS disk image (signed and notarized), the Windows installer and archive and the Linux archives, and creates a draft release with them, `SHA256SUMS` and the changelog's entry as its notes.
4. Review the draft on the [Releases](https://github.com/cbrnr/kymo/releases) page and publish it.
5. The AUR package follows: [aur-pkgbuild-updater](https://github.com/cbrnr/aur-pkgbuild-updater) opens a pull request with the new version within a few hours; merge it to push the update to the AUR.
6. Add a new `## [UNRELEASED]` heading at the top of `CHANGELOG.md` for the next changes.
