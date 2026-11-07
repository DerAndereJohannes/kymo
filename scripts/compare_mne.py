#!/usr/bin/env python3
"""Compare kymo's reading of EDF/BDF files with MNE-Python's.

Usage:
    python scripts/compare_mne.py FILE_OR_DIR [...]   # directories are searched recursively
    python scripts/compare_mne.py --testgen           # files generated from kymo-testgen presets

Needs numpy and mne. Without installing them, run it through uv:
    uv run --no-project --with mne python scripts/compare_mne.py ...

For each file this runs `cargo run --release -p kymo-core --example dump`, reads the same
file with mne.io.read_raw_edf/read_raw_bdf (preload=False, so large files work), and
compares channel names, sampling rate, start time, samples (in SI units, at float32
precision) and annotations. It exits with status 1 if anything differs.

Known differences between the readers, which the comparison accounts for:
- MNE resamples channels below the highest sampling rate. Only channels at the highest
  rate are compared sample by sample; the others are listed as skipped.
- MNE reads EDF+D/BDF+D as if continuous (records joined, gaps dropped), while kymo keeps
  the gaps. Present samples are compared at their position in MNE's joined data, and kymo
  must report NaN exactly in its gaps.
- MNE counts annotation onsets from the first record's onset, kymo from the header's start
  time. MNE onsets are compared after adding the first record's onset back.
- MNE makes duplicate channel names unique ("POL" -> "POL-0", "POL-1", ...); kymo keeps the
  labels as stored. kymo's names are compared after applying MNE's own renaming.
- MNE treats channels named Status or Trigger as stim channels: in BDF it ignores their
  scaling and keeps only the lower 17 bits of the raw value. kymo scales them like any
  channel. For BDF stim channels, MNE's value is compared with kymo's digital value & 0x1FFFF.
"""

import argparse
import json
import subprocess
import sys
import tempfile
import warnings
from pathlib import Path

import mne
import numpy as np
from mne._fiff.meas_info import _unique_channel_names  # MNE's rule for duplicate names

REPO = Path(__file__).resolve().parent.parent
# float32 rounding is at most 2**-24 relative; leave a little room for MNE's own float64 math.
RTOL = 1.5e-7
STIM_NAMES = ("status", "trigger")  # MNE's default stim channel names
STIM_MASK = 2**17 - 1  # MNE keeps the 16 trigger bits and bit 16


def cargo(*args):
    subprocess.run(["cargo", *args], check=True, cwd=REPO)


def dump(path, out, max_values):
    extra = ["--max-values", str(max_values)] if max_values else []
    cargo(
        "run",
        "-q",
        "--release",
        "-p",
        "kymo-core",
        "--example",
        "dump",
        "--",
        str(path),
        str(out),
        *extra,
    )


def compare(path, max_values=None):
    """Returns a list of problems (empty if the readers agree) and prints a summary."""
    problems, notes = [], []
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        try:
            dump(path, tmp, max_values)
        except subprocess.CalledProcessError as e:
            return [f"kymo could not read the file ({e})"]
        header = json.loads((tmp / "header.json").read_text())
        annotations = json.loads((tmp / "annotations.json").read_text())
        windows = json.loads((tmp / "windows.json").read_text())

        reader = mne.io.read_raw_bdf if path.suffix.lower() == ".bdf" else mne.io.read_raw_edf
        try:
            raw = reader(path, preload=False, verbose="error", infer_types=False)
        except Exception as e:  # noqa: BLE001 - report whatever MNE raises
            return [f"MNE could not read the file: {e!r}"]

        channels = header["channels"]
        names = [c["name"] for c in channels]
        mne_names = names
        if len(set(names)) < len(names):
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")  # MNE warns about the duplicates it renames
                mne_names = _unique_channel_names(list(names))
        if mne_names != names:
            notes.append("duplicate channel names, compared after MNE's renaming")
        if raw.ch_names != mne_names:
            problems.append(f"channel names differ:\n    kymo {names}\n    MNE  {raw.ch_names}")
        fmax = max(g["rate"] for g in header["grids"])
        if raw.info["sfreq"] != fmax:
            problems.append(f"sampling rate: kymo {fmax} Hz, MNE {raw.info['sfreq']} Hz")

        if header["start_time"] is not None and raw.info["meas_date"] is not None:
            theirs = raw.info["meas_date"].replace(tzinfo=None, microsecond=0).isoformat()
            if theirs != header["start_time"]:
                problems.append(f"start time: kymo {header['start_time']}, MNE {theirs}")

        compared, worst = 0, 0.0
        for g, grid in enumerate(header["grids"]):
            ids = grid["channels"]
            if grid["rate"] != fmax:
                notes.append(
                    f"skipped {len(ids)} channel(s) at {grid['rate']} Hz (MNE resamples them "
                    f"to {fmax} Hz): {', '.join(names[c] for c in ids)}"
                )
                continue
            scale = np.array([channels[c]["to_si"] or 1.0 for c in ids])
            unknown = [names[c] for c in ids if channels[c]["to_si"] is None]
            if unknown:
                notes.append(f"unit without SI factor, compared unscaled: {', '.join(unknown)}")
            stim = [
                header["format"].startswith("BDF") and names[c].lower() in STIM_NAMES for c in ids
            ]
            if any(stim):
                notes.append(
                    "BDF stim channel(s) compared as digital & 0x1FFFF, as MNE reads them: "
                    + ", ".join(names[c] for c, s in zip(ids, stim) if s)
                )
            p, w, n = compare_grid(raw, tmp, g, grid, windows[g], ids, scale, stim, names)
            problems += p
            worst = max(worst, w)
            compared += n

        problems += compare_annotations(raw, annotations, header["grids"][0]["start"])

    status = "OK" if not problems else "DIFFERS"
    print(f"{status}: {path}")
    print(
        f"  {header['format']}, {len(names)} channels, {compared} samples compared, "
        f"max relative error {worst:.1e}, {len(annotations)} annotations"
    )
    for w in header["warnings"]:
        print(f"  kymo warning: {w}")
    for n in notes:
        print(f"  note: {n}")
    for p in problems:
        print(f"  PROBLEM: {p}")
    return problems


def compare_grid(raw, tmp, g, grid, windows, ids, scale, stim, names):
    problems, worst, compared = [], 0.0, 0
    data = np.fromfile(tmp / f"grid{g}.f32", dtype="<f4")
    digital = np.fromfile(tmp / f"grid{g}.i32", dtype="<i4")
    stim = np.array(stim, dtype=bool)
    scale = np.where(stim, 1.0, scale)
    segments = np.array(grid["segments"], dtype=np.int64).reshape(-1, 2)
    # Index of each segment's first sample in MNE's joined data.
    lengths = segments[:, 1] - segments[:, 0]
    joined_start = np.concatenate([[0], np.cumsum(lengths)[:-1]])
    offset = 0
    for a, b in windows:
        n = b - a
        block = data[offset : offset + len(ids) * n].reshape(len(ids), n).astype(np.float64)
        if stim.any():
            raw_values = digital[offset : offset + len(ids) * n].reshape(len(ids), n)
            block[stim] = np.where(
                np.isnan(block[stim]), np.nan, np.bitwise_and(raw_values[stim], STIM_MASK)
            )
        offset += len(ids) * n
        idx = np.arange(a, b)
        seg = np.searchsorted(segments[:, 0], idx, side="right") - 1
        present = (seg >= 0) & (idx < segments[np.maximum(seg, 0), 1])
        nan = np.isnan(block)
        if (nan != ~present[None, :]).any():
            problems.append(f"window {a}..{b}: kymo's NaNs are not exactly the gaps")
        if not present.any():
            continue
        mne_idx = joined_start[seg[present]] + idx[present] - segments[seg[present], 0]
        lo, hi = int(mne_idx.min()), int(mne_idx.max()) + 1
        theirs = raw.get_data(picks=ids, start=lo, stop=hi)[:, mne_idx - lo]
        ours = block[:, present] * scale[:, None]
        error = np.abs(ours - theirs)
        bad = error > RTOL * np.abs(theirs) + 1e-30
        if bad.any():
            row, col = np.argwhere(bad)[0]
            problems.append(
                f"window {a}..{b}: {bad.sum()} sample(s) differ, first in "
                f"{names[ids[row]]} at sample {idx[present][col]}: kymo {ours[row, col]!r}, "
                f"MNE {theirs[row, col]!r}"
            )
        nonzero = np.abs(theirs) > 0
        if nonzero.any():
            worst = max(worst, float((error[nonzero] / np.abs(theirs[nonzero])).max()))
        compared += ours.size
    return problems, worst, compared


def compare_annotations(raw, annotations, first_onset):
    ours = sorted((a["onset"], a["duration"], a["description"]) for a in annotations)
    theirs = sorted(
        (o + first_onset, d, s)
        for o, d, s in zip(
            raw.annotations.onset, raw.annotations.duration, raw.annotations.description
        )
    )
    if len(ours) != len(theirs):
        return [f"annotation count: kymo {len(ours)}, MNE {len(theirs)}"]
    for k, (a, b) in enumerate(zip(ours, theirs)):
        if abs(a[0] - b[0]) > 1e-6 or abs(a[1] - b[1]) > 1e-6 or a[2] != b[2]:
            return [f"annotation {k} differs: kymo {a}, MNE {b}"]
    return []


def testgen_files(directory):
    """Writes files from the kymo-testgen presets (the large ones shortened)."""
    presets = [
        ("edf+d-mixed", "edf", []),
        ("bdf+c-small", "bdf", []),
        ("edf-64ch-1h", "edf", ["--duration", "60"]),
        ("bdf-256ch-8h", "bdf", ["--duration", "20"]),
    ]
    files = []
    for name, ext, extra in presets:
        path = directory / f"{name}.{ext}"
        cargo("run", "-q", "--release", "-p", "kymo-testgen", "--", name, str(path), *extra)
        files.append(path)
    return files


def find_files(arguments):
    files = []
    for a in map(Path, arguments):
        if a.is_dir():
            files += sorted(p for p in a.rglob("*") if p.suffix.lower() in (".edf", ".bdf"))
        else:
            files.append(a)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="*", help="EDF/BDF files or directories")
    parser.add_argument("--testgen", action="store_true", help="check kymo-testgen presets")
    parser.add_argument(
        "--max-values",
        type=int,
        help="compare windows of at most this many values per grid (default: kymo's dump default)",
    )
    args = parser.parse_args()
    if not args.paths and not args.testgen:
        parser.error("give files, directories or --testgen")

    print(f"MNE {mne.__version__}")
    failed = 0
    with tempfile.TemporaryDirectory() as generated:
        files = find_files(args.paths)
        if args.testgen:
            files += testgen_files(Path(generated))
        if not files:
            sys.exit("no EDF/BDF files found")
        for path in files:
            failed += bool(compare(path, args.max_values))
    print(f"{len(files) - failed} of {len(files)} file(s) agree")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
