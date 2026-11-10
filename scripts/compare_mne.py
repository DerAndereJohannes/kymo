#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mne", "numpy", "pandas"]
# ///
"""Compare Kymo's reading of EDF/BDF, BrainVision, EEGLAB and GDF files with MNE-Python's.

Usage:
    scripts/compare_mne.py FILE_OR_DIR [...]   # directories are searched recursively
    scripts/compare_mne.py --testgen           # files from kymo-testgen presets
    scripts/compare_mne.py --annotations ...   # also annotation files, both ways

Needs numpy, mne and pandas (pandas only with --annotations). The dependencies are
declared inline (PEP 723), so uv installs them on its own. Run the script directly or
through uv:
    uv run scripts/compare_mne.py ...

For each file this runs `cargo run --release -p kymo-core --example dump`, reads the
same file with mne.io.read_raw_edf/read_raw_bdf/read_raw_brainvision/read_raw_eeglab/
read_raw_gdf (preload=False, so large files work; BrainVision files are given by their
.vhdr, EEGLAB datasets by their .set),
and compares channel names, sampling rate, start time, samples (in SI units, at float32
precision) and annotations. It exits with status 1 if anything differs.

Known differences between the readers, which the comparison accounts for:
- MNE resamples channels below the highest sampling rate. Only channels at the highest
  rate are compared sample by sample; the others are listed as skipped.
- MNE reads EDF+D/BDF+D as if continuous (records joined, gaps dropped), while Kymo
  keeps the gaps. Present samples are compared at their position in MNE's joined data,
  and Kymo must report NaN exactly in its gaps.
- MNE counts annotation onsets from the first record's onset, Kymo from the header's
  start time. MNE onsets are compared after adding the first record's onset back.
- MNE crops annotations at the end of the data; Kymo keeps them as the file has them.
  Kymo's durations are compared cropped the same way.
- MNE makes duplicate channel names unique ("POL" -> "POL-0", "POL-1", ...); Kymo keeps
  the labels as stored. Kymo's names are compared after applying MNE's own renaming.
- MNE treats channels named Status or Trigger as stim channels: in BDF it ignores their
  scaling and keeps only the lower 17 bits of the raw value. Kymo scales them like any
  channel. For BDF stim channels, MNE's value is compared with Kymo's digital value &
  0x1FFFF.
- Stim events: Kymo's (every change to a non-zero code, the low 16 bits in BDF) are
  compared with mne.find_events(consecutive=True, shortest_event=1, mask=0xFFFF for
  BDF), at their position in MNE's joined data. MNE counts only positive codes there,
  so Kymo's events with negative codes (possible in EDF only) are left out.
- Float samples may be NaN (BrainVision float32); they must be NaN in both readers.
- Kymo scales units MNE reads as they are (MNE scales only the units in MNE_SCALED):
  in EDF, nV and the magnetic units (fT, fT/cm, ...); in BrainVision, units with the Greek
  mu ("\u03bcV", U+03BC, as LiveAmp's PyCorder writes; MNE also makes such channels misc)
  and the magnetic units. MNE's values of these channels are compared after scaling them
  as Kymo does.

BrainVision, where Kymo deliberately differs from MNE:
- Kymo trims whitespace around channel names ("Dummy\t\t" is "Dummy"); MNE's names are
  compared trimmed.
- Kymo skips every New Segment marker at position 1 (the recording start); MNE skips the
  first marker if it is a New Segment, wherever it is. MNE's New Segment annotations at
  onset 0 are left out.
- Markers of size 1 are points in time (duration 0) in Kymo; MNE makes them one sample
  long. MNE's one-sample durations are compared as 0.
- Kymo's description of a marker with an empty type or description is the other part
  alone ("New Segment", not "New Segment/"); MNE's are compared without the lone "/".
- Kymo's start time is the New Segment date moved back to the first sample; MNE takes the
  date as it is. They agree for New Segments at position 1, as in every file seen so far.
- Kymo repairs names, units and marker descriptions that hold UTF-8 text decoded as
  Windows-1252 when written ("\u00cf\u2030X" for "\u03c9X", as EEGLAB's export writes);
  MNE keeps them as stored. MNE's names and descriptions are compared after the same
  repair.
- Channel kinds and filters are not compared.

EEGLAB, where Kymo deliberately differs from MNE:
- Kymo refuses MAT v7.3 (HDF5) files, which MNE reads; they are skipped. Epoched datasets
  are refused by both and skipped too.
- MNE is given uint16_codec="utf-16-le": MATLAB stores characters as UTF-16, which Kymo
  reads, and MNE's default turns non-ASCII text into U+FFFD ("µ" in an event type).
- Stim channels hold codes in Kymo; MNE scales them by 1e-6 like every channel. MNE's
  values are compared after scaling them back, and its find_events finds no codes on
  them, so stim events are not compared.
- Events whose duration isn't a number >= 0 are points in time in Kymo; MNE keeps NaN.
  MNE's NaN durations are compared as 0.
- Kymo leaves out events before the first sample (latency < 1); MNE keeps them. MNE's
  annotations with negative onsets are left out.
- Float64 samples are read relative to each channel's first sample (see "Readers" in
  docs/design.md), so their float32 precision follows their distance from it, not their
  size. Those channels may differ by float32 precision of that reference too.
- Channel kinds are not compared: Kymo reads the kinds of channels without an MNE type
  from their names, as for BrainVision (MNE makes them EEG), and the types mag, grad and
  ref_meg as such (MNE makes them EEG too).

GDF, where Kymo deliberately differs from MNE:
- MNE reads only files whose channels share one sample type (it refuses types of
  different sizes and misreads different types of one size), and no GDF 2 file with a
  tag-length-value header; Kymo reads both. Such files are skipped.
- Kymo reads the smallest value of a signed integer type (-32768 for int16) as a missing
  sample (NaN), as GDF writes them (sample_nan.gdf); MNE scales it like any value. Where
  Kymo has NaN, MNE's value must be that of the type's minimum.
- MNE scales only uV in GDF 1 (labels starting with "uV", not "\u00b5V") and the codes
  for uV and mV in GDF 2; Kymo scales every unit it knows. MNE's values of the other
  units are compared after scaling them as Kymo does. Stim channels are unscaled in both.
- GDF 2 stores the start time in steps of 2**-32 days (about 20 us); Kymo rounds it to
  the coarsest of whole seconds, milliseconds and microseconds within half a step, MNE
  to the microsecond. Start times are compared within a step.
- Event onsets and durations count samples of the event table's rate in Kymo (the
  highest channel rate if it is 0); MNE counts them at its sampling rate. MNE's onsets
  and durations are compared after converting them to the event table's rate.
- Events of mode 1, and of mode 3 with duration 0, are points in time in Kymo; MNE makes
  them one sample long. MNE's one-sample durations are compared as 0 where Kymo's are 0.
- Kymo leaves out events at position 0 (before the first sample) and entries of type
  0x7FFF (sparse samples, not events); MNE keeps them. They are left out of MNE's too.
- Channel kinds and filters are not compared (Kymo reads kinds from the labels, "EOG:1"
  or "EOG 1", as for EDF; MNE makes every channel but stim channels EEG).

With --annotations, annotation files are checked both ways (`kymo-core --example
annotations`): the recording's annotations saved by Kymo as an events table and read
with `read_events_table` (the snippet in the docs) equal MNE's own reading of the file;
and MNE's CSV of them (`Annotations.save`) loaded by Kymo equals Kymo's own reading.
"""

import argparse
import datetime
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
# float32 rounding is at most 2**-24 relative; leave a little room for MNE's own
# float64 math.
RTOL = 1.5e-7
STIM_NAMES = ("status", "trigger")  # MNE's default stim channel names
STIM_MASK = 2**17 - 1  # MNE keeps the 16 trigger bits and bit 16
# The unit labels each MNE reader scales to SI; it reads other units as they are.
MNE_SCALED = {
    "EDF": {"\u00b5V", "\u03bcV", "\x83\xcaV", "uV", "mV", "V"},
    "BrainVision": {"V", "\u00b5V", "uV", "mV", "nV", "\u00b5S", "uS", "S"},
}
# GDF's type codes: size in bytes, and the smallest value of signed integer types (which
# Kymo reads as a missing sample).
GDF_TYPES = {
    1: (1, -(2**7)),
    2: (1, None),
    3: (2, -(2**15)),
    4: (2, None),
    5: (4, -(2**31)),
    6: (4, None),
    7: (8, -(2**63)),
    8: (8, None),
    16: (4, None),
    17: (8, None),
    279: (3, -(2**23)),
    535: (3, None),
}

# GDF types Kymo reads relative to each channel's first sample (and int32 too, unless
# in a stim channel, since its smallest value is a missing sample).
GDF_RELATIVE = {6, 7, 8, 17}


def gdf_info(path):
    """What the comparison needs from a GDF header: version, each channel's type code
    and samples per record, whether it has a tag-length-value part, the event rate."""
    with open(path, "rb") as f:
        head = f.read(256)
        version = float(head[4:8].decode("latin-1"))
        gdf2 = version >= 1.9
        if gdf2:
            header_bytes = int.from_bytes(head[184:186], "little") * 256
            ns = int.from_bytes(head[252:254], "little")
        else:
            header_bytes = int.from_bytes(head[184:192], "little", signed=True)
            ns = int.from_bytes(head[252:256], "little")
        records = int.from_bytes(head[236:244], "little", signed=True)
        f.seek(256 + 216 * ns)  # samples per record and type follow 216 bytes of fields
        rest = f.read(8 * ns)
        spr = np.frombuffer(rest[: 4 * ns], "<i4")
        types = np.frombuffer(rest[4 * ns :], "<u4")
        record_bytes = sum(
            GDF_TYPES.get(int(t), (0, None))[0] * int(n) for t, n in zip(types, spr)
        )
        event_rate = None
        if records > 0:
            f.seek(header_bytes + records * record_bytes)
            table = f.read(8)
            if len(table) == 8:
                if version < 1.94:
                    event_rate = int.from_bytes(table[1:4], "little")
                else:
                    event_rate = float(np.frombuffer(table[4:8], "<f4")[0])
    return {
        "version": version,
        "types": [int(t) for t in types],
        "spr": [int(n) for n in spr],
        "tlv": gdf2 and header_bytes > 256 * (ns + 1),
        "event_rate": event_rate or None,
    }


def mne_scales(channel, scaled, gdf_info=None):
    """Whether MNE scales the channel's unit to SI: the units in `scaled`, or for GDF
    (given its `gdf_info`) uV in GDF 1 and the codes for uV and mV in GDF 2."""
    unit = channel["unit"].strip()
    if gdf_info is None:
        return unit in scaled
    if channel["kind"] == "Stim":
        return False
    if gdf_info["version"] < 1.9:
        return unit.startswith("uV")
    return unit in ("\u00b5V", "mV")


def gdf_skip_reason(info):
    """Why MNE can't read a GDF file, if it can't."""
    used = {t for t, n in zip(info["types"], info["spr"]) if n > 0}
    if len(used) > 1:
        return "channels of different sample types, which MNE doesn't read"
    if info["tlv"]:
        return "a tag-length-value header (GDF 2), which MNE doesn't read"
    return None


def repair(text):
    """Kymo's repair of UTF-8 text decoded as Windows-1252 or Latin-1 (and written out
    again), as often as it was; the text as it is if it isn't such text."""
    while not text.isascii():
        try:
            data = bytes(
                ord(c) if ord(c) < 256 else c.encode("cp1252")[0] for c in text
            )
            text = data.decode("utf-8")
        except (UnicodeError, ValueError):
            break
    return text


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


def read_events_table(path):
    """Kymo's events table as mne.Annotations (see docs/phase3-plan.md, step 5)."""
    import pandas as pd

    events = pd.read_csv(
        path, sep="\t", keep_default_na=False, dtype={"trial_type": str}
    )
    channels = events["channels"] if "channels" in events else [""] * len(events)
    return mne.Annotations(
        onset=events["onset"],
        duration=pd.to_numeric(events["duration"].replace("n/a", 0)),
        description=events["trial_type"],
        # Names joined by ";" (a name with ";" or "\\" in it is escaped with "\\").
        ch_names=[c.split(";") if c else [] for c in channels],
    )


def compare_annotation_files(path, raw, annotations, first_onset, tmp, sample=None):
    """Problems with annotation files saved by one side and loaded by the other. With
    `sample`, Kymo's points in time (duration 0) match MNE's durations of one sample of
    that length (BrainVision's and GDF's)."""
    problems = []

    def same_duration(ours, theirs, tolerance):
        point = sample is not None and ours == 0 and abs(theirs - sample) < 1e-9
        return abs(ours - theirs) <= tolerance or point

    table = tmp / "kymo.tsv"
    cargo(
        "run",
        "-q",
        "--release",
        "-p",
        "kymo-core",
        "--example",
        "annotations",
        "--",
        "save",
        str(path),
        str(table),
    )
    theirs = raw.annotations
    ours = read_events_table(table)
    copy = raw.copy().set_annotations(ours, verbose="error")
    got = copy.annotations
    if len(got) != len(theirs):
        problems.append(
            f"Kymo's table in MNE: {len(got)} annotations, MNE has {len(theirs)}"
        )
    else:
        for k in range(len(got)):
            if (
                abs(got.onset[k] - theirs.onset[k]) > 1e-9
                or not same_duration(got.duration[k], theirs.duration[k], 1e-9)
                or got.description[k] != theirs.description[k]
            ):
                problems.append(
                    f"Kymo's table in MNE, annotation {k}: {got[k]}, "
                    f"MNE has {theirs[k]}"
                )
                break

    csv = tmp / "mne.csv"
    theirs.save(csv, overwrite=True, verbose="error")
    out = subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "--release",
            "-p",
            "kymo-core",
            "--example",
            "annotations",
            "--",
            "load",
            str(path),
            str(csv),
        ],
        check=True,
        cwd=REPO,
        capture_output=True,
        text=True,
    ).stdout
    loaded = json.loads(out)
    if loaded["outside"] or loaded["unknown_channels"]:
        problems.append(f"MNE's CSV in Kymo skipped some: {loaded}")
    got = sorted(
        (a["onset"], a["duration"], a["description"]) for a in loaded["annotations"]
    )
    want = cropped(annotations, data_end(raw, first_onset))
    if len(got) != len(want):
        problems.append(
            f"MNE's CSV in Kymo: {len(got)} annotations, Kymo has {len(want)}"
        )
    else:
        for a, b in zip(got, want):
            # MNE writes onsets in milliseconds.
            if (
                abs(a[0] - b[0]) > 5e-4
                or not same_duration(b[1], a[1], 1e-6)
                or a[2] != b[2]
            ):
                problems.append(f"MNE's CSV in Kymo: {a}, Kymo has {b}")
                break
    return problems


def compare(path, max_values=None, annotation_files=False):
    """Returns a list of problems (empty if the readers agree), or None for a file
    neither reader reads, and prints a summary."""
    problems, notes = [], []
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        try:
            dump(path, tmp, max_values)
        except subprocess.CalledProcessError as e:
            if path.suffix.lower() == ".set" and refused_by_mne(path):
                print(f"SKIPPED: {path}\n  neither Kymo nor MNE reads it")
                return None
            return [f"Kymo could not read the file ({e})"]
        header = json.loads((tmp / "header.json").read_text())
        annotations = json.loads((tmp / "annotations.json").read_text())
        windows = json.loads((tmp / "windows.json").read_text())

        brainvision = header["format"] == "BrainVision"
        eeglab = header["format"] == "EEGLAB"
        gdf = header["format"] == "GDF"
        if gdf:
            info = gdf_info(path)
            reason = gdf_skip_reason(info)
            if reason:
                print(f"SKIPPED: {path}\n  {reason}")
                return None
        try:
            if gdf:
                raw = mne.io.read_raw_gdf(path, preload=False, verbose="error")
            elif brainvision:
                raw = mne.io.read_raw_brainvision(path, preload=False, verbose="error")
            elif eeglab:
                raw = mne.io.read_raw_eeglab(
                    path, preload=False, verbose="error", uint16_codec="utf-16-le"
                )
            else:
                reader = (
                    mne.io.read_raw_bdf
                    if path.suffix.lower() == ".bdf"
                    else mne.io.read_raw_edf
                )
                raw = reader(path, preload=False, verbose="error", infer_types=False)
        except Exception as e:  # noqa: BLE001 - report whatever MNE raises
            return [f"MNE could not read the file: {e!r}"]

        channels = header["channels"]
        names = [c["name"] for c in channels]
        mne_names = names
        if len(set(names)) < len(names):
            with warnings.catch_warnings():
                warnings.simplefilter(
                    "ignore"
                )  # MNE warns about the duplicates it renames
                mne_names = _unique_channel_names(list(names))
        if mne_names != names:
            notes.append("duplicate channel names, compared after MNE's renaming")
        theirs_names = raw.ch_names
        if brainvision and [n.strip() for n in theirs_names] != theirs_names:
            theirs_names = [n.strip() for n in theirs_names]
            notes.append("MNE's channel names compared trimmed")
        if brainvision and [repair(n) for n in theirs_names] != theirs_names:
            theirs_names = [repair(n) for n in theirs_names]
            notes.append("MNE's channel names compared repaired")
        if theirs_names != mne_names:
            problems.append(
                f"channel names differ:\n    Kymo {names}\n    MNE  {raw.ch_names}"
            )
        fmax = max(g["rate"] for g in header["grids"])
        if raw.info["sfreq"] != fmax:
            problems.append(
                f"sampling rate: Kymo {fmax} Hz, MNE {raw.info['sfreq']} Hz"
            )

        if header["start_time"] is not None and raw.info["meas_date"] is not None:
            date = raw.info["meas_date"].replace(tzinfo=None)
            if gdf:
                # GDF 2 stores the time in steps of 2**-32 days (about 20 us), which Kymo
                # rounds to the coarsest whole unit within half a step, MNE to the
                # microsecond.
                ours = datetime.datetime.fromisoformat(header["start_time"])
                if abs((ours - date).total_seconds()) > 2**-32 * 86400:
                    problems.append(
                        f"start time: Kymo {header['start_time']}, MNE {date}"
                    )
            elif brainvision:
                # Kymo prints microseconds without trailing zeros.
                theirs = date.isoformat(timespec="microseconds").rstrip("0").rstrip(".")
            else:
                theirs = date.replace(microsecond=0).isoformat()
            if not gdf and theirs != header["start_time"]:
                problems.append(
                    f"start time: Kymo {header['start_time']}, MNE {theirs}"
                )

        compared, worst = 0, 0.0
        for g, grid in enumerate(header["grids"]):
            ids = grid["channels"]
            if grid["rate"] != fmax:
                notes.append(
                    f"skipped {len(ids)} channel(s) at {grid['rate']} Hz "
                    f"(MNE resamples them to {fmax} Hz): "
                    f"{', '.join(names[c] for c in ids)}"
                )
                continue
            scale = np.array([channels[c]["to_si"] or 1.0 for c in ids])
            # Units Kymo scales and MNE doesn't: MNE's values are in the file's unit.
            # MNE scales every EEGLAB channel.
            scaled = MNE_SCALED["BrainVision" if brainvision else "EDF"]

            unscaled = [
                not eeglab
                and channels[c]["to_si"] is not None
                and not mne_scales(channels[c], scaled, info if gdf else None)
                for c in ids
            ]
            theirs_scale = np.where(unscaled, scale, 1.0)
            if eeglab:
                codes = [channels[c]["kind"] == "Stim" for c in ids]
                theirs_scale = np.where(codes, 1e6, theirs_scale)
                if any(codes):
                    notes.append(
                        "stim channel(s) compared as codes (MNE's values scaled by "
                        "1e6): " + ", ".join(names[c] for c, k in zip(ids, codes) if k)
                    )
            if any(unscaled):
                notes.append(
                    "unit(s) MNE leaves unscaled, its values scaled as Kymo's: "
                    + ", ".join(
                        f"{names[c]} ({channels[c]['unit']})"
                        for c, u in zip(ids, unscaled)
                        if u
                    )
                )
            unknown = [names[c] for c in ids if channels[c]["to_si"] is None]
            if unknown:
                notes.append(
                    f"unit without SI factor, compared unscaled: {', '.join(unknown)}"
                )
            stim = [
                (header["format"].startswith("BDF") or gdf)
                and names[c].lower() in STIM_NAMES
                for c in ids
            ]
            # MNE masks the physical value of GDF stim channels; BDF's are unscaled.
            stim_scaling = [
                (channels[c]["gain"], channels[c]["offset"]) if gdf else (1.0, 0.0)
                for c in ids
            ]
            if any(stim):
                what = "int(physical)" if gdf else "digital"
                notes.append(
                    f"stim channel(s) compared as {what} & 0x1FFFF, as MNE reads "
                    "them: " + ", ".join(names[c] for c, s in zip(ids, stim) if s)
                )
            # The reference value of float64 samples, in SI units (0 for other formats).
            reference = np.array(
                [
                    abs(channels[c]["offset"]) * (channels[c]["to_si"] or 1.0)
                    if eeglab
                    else 0.0
                    for c in ids
                ]
            )
            if gdf:
                # Types read relative to the first sample: Kymo's offset holds it, on top
                # of the file's offset (MNE's).
                extras = raw._raw_extras[0]
                data_channels = [k for k, n in enumerate(info["spr"]) if n > 0]
                for k, c in enumerate(ids):
                    t = info["types"][data_channels[c]]
                    relative = t in GDF_RELATIVE or (
                        t == 5 and channels[c]["kind"] != "Stim"
                    )
                    if relative:
                        reference[k] = (
                            abs(channels[c]["offset"]) * scale[k]
                            + abs(extras["offsets"][c] * extras["units"][c])
                            * theirs_scale[k]
                        )
            # GDF: MNE's value of a sample Kymo reads as missing, per channel (None for
            # types without one, and for stim channels).
            missing = [None] * len(ids)
            if gdf:
                for k, c in enumerate(ids):
                    t = info["types"][data_channels[c]]
                    minimum = GDF_TYPES.get(t, (0, None))[1]
                    if minimum is not None and channels[c]["kind"] != "Stim":
                        missing[k] = (
                            minimum * extras["cal"][c] + extras["offsets"][c]
                        ) * extras["units"][c]
            p, w, n = compare_grid(
                raw,
                tmp,
                g,
                grid,
                windows[g],
                ids,
                scale,
                theirs_scale,
                stim,
                names,
                reference,
                missing,
                notes,
                stim_scaling,
            )
            problems += p
            worst = max(worst, w)
            compared += n

        problems += compare_annotations(
            raw,
            annotations,
            header["grids"][0]["start"],
            header["format"],
            (info["event_rate"] or fmax) if gdf else None,
        )
        if annotation_files:
            problems += compare_annotation_files(
                path,
                raw,
                annotations,
                header["grids"][0]["start"],
                tmp,
                1 / raw.info["sfreq"] if brainvision or gdf else None,
            )
        events = json.loads((tmp / "events.json").read_text())
        if eeglab:
            if events:
                notes.append("stim events not compared (MNE scales the codes by 1e-6)")
            n = sum(np.array(e["events"]).reshape(-1, 2).shape[0] for e in events)
        else:
            p, n = compare_events(raw, header, events, fmax, notes)
            problems += p

    status = "OK" if not problems else "DIFFERS"
    print(f"{status}: {path}")
    print(
        f"  {header['format']}, {len(names)} channels, {compared} samples compared, "
        f"max relative error {worst:.1e}, {len(annotations)} annotations, "
        f"{n} stim events"
    )
    for w in header["warnings"]:
        print(f"  Kymo warning: {w}")
    for n in notes:
        print(f"  note: {n}")
    for p in problems:
        print(f"  PROBLEM: {p}")
    return problems


def compare_grid(
    raw,
    tmp,
    g,
    grid,
    windows,
    ids,
    scale,
    theirs_scale,
    stim,
    names,
    reference,
    missing,
    notes,
    stim_scaling,
):
    problems, worst, compared, n_missing = [], 0.0, 0, 0
    data = np.fromfile(tmp / f"grid{g}.f32", dtype="<f4")
    if grid["sample_type"] != "F32":
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
        block = (
            data[offset : offset + len(ids) * n].reshape(len(ids), n).astype(np.float64)
        )
        if stim.any():
            if grid["sample_type"] != "F32":
                raw_values = digital[offset : offset + len(ids) * n].reshape(
                    len(ids), n
                )
                gain, shift = np.array(stim_scaling).T[:, :, None]
                physical = raw_values * gain + shift
            else:
                physical = np.nan_to_num(block)  # float grids have no digital values
            values = np.trunc(physical).astype(np.int64)  # as MNE's int()
            block[stim] = np.where(
                np.isnan(block[stim]),
                np.nan,
                np.bitwise_and(values[stim], STIM_MASK),
            )
        offset += len(ids) * n
        idx = np.arange(a, b)
        seg = np.searchsorted(segments[:, 0], idx, side="right") - 1
        present = (seg >= 0) & (idx < segments[np.maximum(seg, 0), 1])
        nan = np.isnan(block)
        if (~nan & ~present[None, :]).any():
            problems.append(f"window {a}..{b}: Kymo has values in its gaps")
        if not present.any():
            continue
        mne_idx = joined_start[seg[present]] + idx[present] - segments[seg[present], 0]
        lo, hi = int(mne_idx.min()), int(mne_idx.max()) + 1
        theirs = raw.get_data(picks=ids, start=lo, stop=hi)[:, mne_idx - lo]
        ours = block[:, present] * scale[:, None]
        for k, value in enumerate(missing):
            if value is None:
                continue
            # Kymo's missing samples: MNE must have the type minimum's value there.
            gone = np.isnan(ours[k]) & ~np.isnan(theirs[k])
            wrong = gone & ~np.isclose(theirs[k], value, rtol=RTOL, atol=0)
            if wrong.any():
                col = np.argmax(wrong)
                problems.append(
                    f"window {a}..{b}: Kymo has NaN in {names[ids[k]]} at sample "
                    f"{idx[present][col]}, MNE {theirs[k, col]!r} (a missing sample "
                    f"would be {value!r})"
                )
            theirs[k, gone] = np.nan
            n_missing += int(gone.sum())
        theirs = theirs * theirs_scale[:, None]
        if (np.isnan(ours) != np.isnan(theirs)).any():
            problems.append(f"window {a}..{b}: NaNs differ between Kymo and MNE")
        error = np.abs(np.nan_to_num(ours) - np.nan_to_num(theirs))
        allowed = RTOL * (np.abs(np.nan_to_num(theirs)) + reference[:, None])
        bad = error > allowed + 1e-30
        if bad.any():
            row, col = np.argwhere(bad)[0]
            problems.append(
                f"window {a}..{b}: {bad.sum()} sample(s) differ, first in "
                f"{names[ids[row]]} at sample {idx[present][col]}: "
                f"Kymo {ours[row, col]!r}, "
                f"MNE {theirs[row, col]!r}"
            )
        nonzero = np.abs(np.nan_to_num(theirs)) > 0
        if nonzero.any():
            worst = max(worst, float((error[nonzero] / np.abs(theirs[nonzero])).max()))
        compared += ours.size
    if n_missing:
        notes.append(
            f"{n_missing} missing sample(s) (NaN in Kymo, the type's minimum in MNE)"
        )
    return problems, worst, compared


def compare_events(raw, header, events, fmax, notes):
    """Compares Kymo's stim events with mne.find_events; returns problems and count."""
    problems, count = [], 0
    bdf = header["format"].startswith("BDF")
    gdf = header["format"] == "GDF"
    for entry in events:
        c = entry["channel"]
        channel = header["channels"][c]
        grid = header["grids"][channel["grid"]]
        if grid["rate"] != fmax:
            notes.append(
                f"skipped events of {channel['name']} (MNE resamples its channel)"
            )
            continue
        ours = np.array(entry["events"], dtype=np.int64).reshape(-1, 2)
        if gdf:
            # MNE masks GDF stim channels' physical values: events of the masked values,
            # under the same rule (every change to a non-zero code, none at the start).
            masked, previous = [], None
            for sample, value in entry["changes"]:
                code = int(value * channel["gain"] + channel["offset"])
                code &= STIM_MASK
                if previous is not None and code != previous and code != 0:
                    masked.append((sample, code))
                previous = code
            ours = np.array(masked, dtype=np.int64).reshape(-1, 2)
        ours = ours[ours[:, 1] > 0]
        # Each sample's position in MNE's joined data (gaps dropped).
        segments = np.array(grid["segments"], dtype=np.int64).reshape(-1, 2)
        joined_start = np.concatenate(
            [[0], np.cumsum(segments[:, 1] - segments[:, 0])[:-1]]
        )
        seg = np.searchsorted(segments[:, 0], ours[:, 0], side="right") - 1
        ours[:, 0] = joined_start[seg] + ours[:, 0] - segments[seg, 0]
        theirs = mne.find_events(
            raw,
            stim_channel=raw.ch_names[c],
            consecutive=True,
            shortest_event=1,
            initial_event=False,
            mask=0xFFFF if bdf else None,
            mask_type="and",
            verbose="error",
        )
        theirs = theirs[:, [0, 2]] - [raw.first_samp, 0]
        count += len(ours)
        if ours.shape != theirs.shape or (ours != theirs).any():
            k = next(
                (
                    k
                    for k in range(min(len(ours), len(theirs)))
                    if (ours[k] != theirs[k]).any()
                ),
                min(len(ours), len(theirs)),
            )
            first = lambda e: tuple(e[k]) if k < len(e) else None  # noqa: E731
            problems.append(
                f"events of {channel['name']}: Kymo {len(ours)}, MNE {len(theirs)}; "
                f"first difference at event {k}: Kymo {first(ours)}, "
                f"MNE {first(theirs)}"
            )
    return problems, count


def cropped(annotations, end):
    """(onset, duration, description) of Kymo's annotations, cropped at `end`."""
    return sorted(
        (
            a["onset"],
            min(a["onset"] + a["duration"], end) - a["onset"],
            a["description"],
        )
        for a in annotations
    )


def data_end(raw, first_onset):
    """The end of MNE's data, in Kymo's time."""
    return first_onset + raw.n_times / raw.info["sfreq"]


def compare_annotations(raw, annotations, first_onset, format_name, event_rate=None):
    ours = cropped(annotations, data_end(raw, first_onset))
    theirs = zip(
        raw.annotations.onset, raw.annotations.duration, raw.annotations.description
    )
    if format_name == "GDF":
        # Kymo's rules for events; see the docstring.
        sfreq = raw.info["sfreq"]
        theirs = [
            (o * sfreq / event_rate, d * sfreq / event_rate, s)
            for o, d, s in theirs
            if o >= 0 and s != str(0x7FFF)
        ]
        ours = sorted(ours, key=lambda a: (a[0], a[2], a[1]))
        theirs = sorted(theirs, key=lambda a: (a[0], a[2], a[1]))
        if len(ours) != len(theirs):
            return [f"annotation count: Kymo {len(ours)}, MNE {len(theirs)}"]
        for k, (a, b) in enumerate(zip(ours, theirs)):
            point = a[1] == 0 and abs(b[1] - 1 / event_rate) < 1e-9
            same_duration = abs(a[1] - b[1]) <= 1e-6 or point
            if abs(a[0] - b[0]) > 1e-6 or not same_duration or a[2] != b[2]:
                return [f"annotation {k} differs: Kymo {a}, MNE {b}"]
        return []
    if format_name == "EEGLAB":
        # Kymo's rules for events; see the docstring.
        theirs = [(o, 0.0 if np.isnan(d) else d, s) for o, d, s in theirs if o >= 0]
    if format_name == "BrainVision":
        # Kymo's own rules for markers; see the docstring.
        sample = 1 / raw.info["sfreq"]
        theirs = [
            (o, 0.0 if abs(d - sample) < 1e-9 else d, repair(s.strip("/")))
            for o, d, s in theirs
            if not (s.startswith("New Segment/") and o == 0)
        ]
    theirs = sorted((o + first_onset, d, s) for o, d, s in theirs)
    if len(ours) != len(theirs):
        return [f"annotation count: Kymo {len(ours)}, MNE {len(theirs)}"]
    for k, (a, b) in enumerate(zip(ours, theirs)):
        if abs(a[0] - b[0]) > 1e-6 or abs(a[1] - b[1]) > 1e-6 or a[2] != b[2]:
            return [f"annotation {k} differs: Kymo {a}, MNE {b}"]
    return []


def is_mat_v73(path):
    with open(path, "rb") as f:
        return f.read(19) == b"MATLAB 7.3 MAT-file"


def refused_by_mne(path):
    try:
        mne.io.read_raw_eeglab(path, preload=False, verbose="error")
    except Exception:  # noqa: BLE001 - any refusal counts
        return True
    return False


def testgen_files(directory):
    """Writes files from the kymo-testgen presets (the large ones shortened)."""
    presets = [
        ("edf+d-mixed", "edf", []),
        ("bdf+c-small", "bdf", []),
        ("edf+c-kinds", "edf", []),
        ("edf-64ch-1h", "edf", ["--duration", "60"]),
        ("bdf-256ch-8h", "bdf", ["--duration", "20"]),
        ("bv-int16-multiplexed", "vhdr", []),
        ("bv-int32-vectorized", "vhdr", []),
        ("bv-float32-multiplexed", "vhdr", []),
        ("eeglab-fdt", "set", []),
        ("eeglab-set-single", "set", []),
        ("eeglab-set-double", "set", []),
        ("gdf1-float64", "gdf", []),
        ("gdf2-int32", "gdf", []),
        ("gdf2-mixed", "gdf", []),
    ]
    files = []
    for name, ext, extra in presets:
        path = directory / f"{name}.{ext}"
        cargo(
            "run",
            "-q",
            "--release",
            "-p",
            "kymo-testgen",
            "--",
            name,
            str(path),
            *extra,
        )
        files.append(path)
    return files


def find_files(arguments):
    files = []
    for a in map(Path, arguments):
        if a.is_dir():
            files += sorted(
                p
                for p in a.rglob("*")
                if p.suffix.lower() in (".edf", ".bdf", ".vhdr", ".set", ".gdf")
            )
        else:
            files.append(a)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "paths",
        nargs="*",
        help="EDF/BDF/BrainVision (.vhdr)/EEGLAB (.set)/GDF files or directories",
    )
    parser.add_argument(
        "--testgen", action="store_true", help="check kymo-testgen presets"
    )
    parser.add_argument(
        "--annotations",
        action="store_true",
        help="also check annotation files, both ways",
    )
    parser.add_argument(
        "--max-values",
        type=int,
        help="compare windows of at most this many values per grid "
        "(default: Kymo's dump default)",
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
            sys.exit("no EDF, BDF, BrainVision, EEGLAB or GDF files found")
        skipped = 0
        for path in files:
            if path.suffix.lower() == ".set" and is_mat_v73(path):
                print(f"SKIPPED: {path}\n  MAT v7.3 (HDF5), which Kymo doesn't read")
                skipped += 1
                continue
            problems = compare(path, args.max_values, args.annotations)
            if problems is None:
                skipped += 1
            else:
                failed += bool(problems)
    compared = len(files) - skipped
    print(
        f"{compared - failed} of {compared} file(s) agree"
        + (f", {skipped} skipped" if skipped else "")
    )
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
