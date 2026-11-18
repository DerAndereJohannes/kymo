# Kymo

**A fast viewer for EEG and MEG recordings.** Kymo opens even huge recordings in under a second and lets you scroll and zoom through them smoothly, using only a few hundred megabytes of memory. It doesn't matter whether your file is 10 MB or 50 GB.

Kymo is named after the kymograph, the rotating drum that first recorded physiological signals in the 19th century.

> Kymo is under active development and has no release yet. It already runs on macOS, Windows and Linux as a standalone viewer, and integration with [MNE-Python](https://mne.tools) and [MNELAB](https://github.com/cbrnr/mnelab) is planned (see [Roadmap](#roadmap)).

## Why Kymo

- Kymo **opens recordings instantly**. The first view appears in under a second, while an overview of the whole recording builds in the background at disk speed. Kymo caches this overview, so the next time you open the file, everything is there instantaneously.
- **Scrolling and zooming stay smooth** at 60 frames per second or more, whether you look at a few samples or at the entire recording at once. If some data isn't ready yet, Kymo shows a coarser version in the meantime, so the window doesn't freeze.
- Kymo **uses very little memory** because it never loads the whole recording. It only reads what you see on the screen, so a 45 GB file needs about as much memory as a small one.
- You **see every peak**. When you zoom out, each pixel shows the minimum and maximum of all the samples behind it, so spikes and artifacts don't disappear.
- Kymo **draws on the GPU** (Metal, Vulkan or DirectX 12), with optional anti-aliasing for smoother lines.

### Benchmarks

Here is how Kymo compares with [MNE-Qt-Browser](https://github.com/mne-tools/mne-qt-browser), MNE-Python's default browser, on the same files (M2 Pro with 32 GB of RAM):

| Recording | Viewer | Frame time while scrolling (p99) | Zoom out to the whole recording | Peak memory |
| --- | --- | --- | --- | --- |
| 64 channels, 1 h, EDF | **Kymo** | **10 ms** | **8 ms per step** | **125 MB** |
| | MNE-Qt-Browser | 16 ms to 2.5 s | 2.8 s per step | 7.7 GB |
| 306 channels, 1 h, EDF | **Kymo** | **9 ms** | **8 ms per step** | **158 MB** |
| | MNE-Qt-Browser | 22 ms to 4.3 s | 4.6 s per step | 13.9 GB |
| 256 channels, 8 h, BDF (45 GB) | **Kymo** | **11 ms** | **31 ms per step** | **280 MB** |
| | MNE-Qt-Browser | did not finish | stopped after 10 min | 67.6 GB |

These Kymo numbers are for files opened before. The first time you open a file, scrolling is just as fast, and the zoomed-out view fills in while the overview builds. You can find all the details in [docs/design.md](docs/design.md#plan).

## Features

### Supported file formats

| Format | Files |
| --- | --- |
| EDF, EDF+ and BDF, BDF+ (continuous and discontinuous) | `.edf`, `.bdf` |
| BrainVision | `.vhdr` with `.eeg` and `.vmrk` |
| [EEGLAB](https://eeglab.org) | `.set`, with or without `.fdt` |
| GDF 1.x and 2.x | `.gdf` |
| FIFF (MNE-Python, Neuromag/MEGIN), including split files | `.fif` |
| XDF ([Lab Streaming Layer](https://labstreaminglayer.org)) | `.xdf` |
| BVRF (BrainVision Recording Format), including several participants in one recording | `.bvrh` with `.bvrd` and `.bvrm` |

- Kymo recognizes the format from the file's contents, so the extension doesn't matter. You can open any file of a set (such as a BrainVision `.eeg` or a BVRF `.bvrd`) to get the whole recording.
- Channels can have different sampling rates, and recordings with gaps show the gaps as empty space.
- XDF streams are synchronized and dejittered the same way [PyXDF](https://github.com/xdf-modules/pyxdf) does it. Irregular streams are drawn at their own time stamps, and marker streams become annotations.
- In BVRF recordings of several participants, each participant's channels and markers form a group of their own, and the channels are named after their participant (`Fp1 (P1)`), as in [PyBVRF](https://github.com/cbrnr/pybvrf).
- We check samples and annotations against MNE-Python's readers (PyXDF for XDF, PyBVRF for BVRF), and channel files against [MNE-BIDS](https://mne.tools/mne-bids).
- Kymo never modifies your files.

### Navigation

- Scroll and zoom with your trackpad, mouse wheel, or keyboard. You can also jump to the start, the end or any point in time, or fit the whole recording on the screen.
- Page through channels, and show more or fewer of them at once (also by ⇧⌘-scrolling or -pinching).
- Hover over a trace to see its value at that point.
- Open files from the command line, with the Open dialog, from the list of recent files, or by dropping them on the window.
- **File → Recording Info** shows everything Kymo knows about the file: the format's own header fields, start, duration, sampling rates, streams, warnings, and a table of each group of channels with their types, units, ranges and filters. You can copy it all as text.

### Channels and scaling

- The sidebar lists all channels grouped by type, with their units (and by stream for XDF files, or by participant for BVRF recordings of several participants). You can fold groups and show or hide any channel or group.
- Select channels as in any list (click, ⌘-click and ⇧-click), and mark channels as bad. If the file already marks channels as bad, Kymo does too.
- **Channel types are names**, as the file gives them, not a fixed list. Kymo knows the common ones (MNE-Python's types and their BIDS and EDF+ spellings), and keeps any other name as it is.
- **Change the type** of one or more channels from the menu at the end of their rows: pick one of the recording's types, any other type Kymo knows, or a new name of your own. **From File** brings back the type from the file.
- Each type has its own color, DC removal and scaling, set from its row in the sidebar or in the **Channel Types** window. Kymo remembers them by name for all recordings.
- **Save channel types and bad channels** as a [BIDS](https://bids.neuroimaging.io) `channels.tsv` file, and load them from one (for example, one written by MNE-BIDS). Your recording stays untouched.
- Channels of the same type and quantity share one amplitude scale. Types whose channels measure different things (such as misc channels) scale each channel on its own. Let **Auto Scale** pick the scales based on what you see in the current window, or set them yourself.
- **Scale a single channel** by ⌥⇧⌘-scrolling or -pinching over its trace, or scale the selected channels with ⌥⇧⌘+ and ⌥⇧⌘−. A ↕ next to a channel's name marks it, and a click on the ↕ returns it to its type's scale.
- Scale bars show the amplitude of each group of channels in a fitting unit (2 mV rather than 2000 µV), and readings under the pointer use the same unit.
- **DC removal** keeps channels with large offsets centered in their rows. It applies only to types where the level doesn't matter, so a temperature or oxygen saturation channel keeps its level.

### Measuring

- ⇧-drag across the traces to measure the time between two points. Both ends snap to the samples of the channel you started on, so the time difference is exact to the sample.
- The label shows the time difference, the matching frequency, and the channel's peak-to-peak amplitude between the two ends. The amplitude is exact at any zoom level, even when you look at the whole recording.
- The status line adds the times of both ends, the number of samples, and the values at both ends with their difference.
- Measuring also works while you edit annotations. A click or Esc removes the measurement.

### Annotations and events

- Annotations from the file show up as colored bands and lines. The legend lets you show, hide and recolor them by description.
- Kymo detects events in trigger (stim) channels. You can pick whether every change counts, only changes from zero, or only increases.
- In editing mode, drag to create an annotation, and then move, resize, rename, merge or delete it. Undo and redo work for all of these.
- Save annotations as a [BIDS](https://bids.neuroimaging.io)-style events table (`.tsv`), and load them from one or from an MNE-Python annotations CSV file.

### Look and feel

- Choose a light or dark theme, or follow your system.
- Pick your own colors for channel types and annotations. Kymo remembers them for all recordings.
- Kymo uses native menus and the usual shortcuts on each platform (**Help → Keyboard Shortcuts** lists all of them). On Windows and Linux, Ctrl takes the place of ⌘, Alt that of ⌥ and Shift that of ⇧.
- Your settings are kept between sessions.

## Installation

Until the first release, you need to build Kymo from source. This requires [Rust](https://rustup.rs) 1.95 or newer and a C compiler. [CONTRIBUTING.md](CONTRIBUTING.md) describes the setup for each platform.

```sh
git clone https://github.com/cbrnr/kymo.git
cd kymo
cargo build --release -p kymo
```

You will find the executable in `target/release/kymo` (`kymo.exe` on Windows).

Kymo needs a graphics driver that supports Metal (any supported Mac), DirectX 12 or Vulkan (Windows), or Vulkan (Linux).

## Usage

```sh
kymo recording.bdf          # open a recording
kymo                        # pick a file in a dialog
kymo --info recording.fif   # print a summary of the recording without opening a window
```

## Roadmap

- **Display filters:** high-pass and low-pass filters like in MNE-Python's `raw.plot()`, applied to the whole recording.
- **Python package:** `pip install kymo` and `kymo.plot(raw)` for any MNE-Python `Raw` object, with bad channels and annotations passed back to Python.
- **MNE-Python backend:** `raw.plot()` opens Kymo directly.
- **MNELAB:** Kymo as the signal viewer in MNELAB.
- **More:** additional file formats, re-referencing, and names for event codes.

## Contributing

Contributions are welcome! [CONTRIBUTING.md](CONTRIBUTING.md) explains how to set up your toolchain, build Kymo and run the checks, and [docs/design.md](docs/design.md) describes the architecture.

## License

Kymo is licensed under the [BSD 3-Clause License](LICENSE).
