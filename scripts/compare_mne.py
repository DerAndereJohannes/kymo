#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mne", "mnextend", "numpy", "pandas", "pybvrf", "pyxdf"]
# ///
"""Compare Kymo's reading of EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files with
MNE-Python's (XDF: with pyxdf's and mnextend's; BVRF: with pybvrf's).

Usage:
    scripts/compare_mne.py FILE_OR_DIR [...]   # directories are searched recursively
    scripts/compare_mne.py --testgen           # files from kymo-testgen presets
    scripts/compare_mne.py --annotations ...   # also annotation files, both ways

Needs numpy, mne, pandas (only with --annotations), pyxdf and mnextend (only for XDF), and pybvrf
(only for BVRF). The dependencies are
declared inline (PEP 723), so uv installs them on its own. Run the script directly or
through uv:
    uv run scripts/compare_mne.py ...

For each file this runs `cargo run --release -p kymo-core --example dump`, reads the
same file with mne.io.read_raw_edf/read_raw_bdf/read_raw_brainvision/read_raw_eeglab/
read_raw_gdf/read_raw_fif (preload=False, so large files work; BrainVision files are given
by their .vhdr, EEGLAB datasets by their .set),
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
  0x1FFFF. MNE also leaves an EDF stim channel's unit unscaled (Status in uV in
  test_reduced.edf); its values are compared after scaling them as Kymo does.
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
- In mode 1 (and mode 3 without durations), Kymo pairs an event with the first later end
  of its type and channel (type | 0x8000) into one annotation with a duration, as biosig
  does; MNE keeps both as separate annotations. MNE's are paired the same way before they
  are compared.
- Event tables of modes 5 and 7 (with timestamps) are read like modes 1 and 3.
- Kymo reads samples at or beyond a channel's digital minimum or maximum as missing (NaN),
  as biosig's overflow detection does (biosig writes NaN as the type's smallest value, as
  in sample_nan.gdf); MNE scales them like any value. Stim channels and limits that aren't
  finite with DigMin < DigMax are exempt. Where Kymo has NaN, MNE's value must lie at or
  beyond a limit, and where MNE's does, Kymo must have NaN.
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
- Integer Status channels (BioSemi's status word, in BDF files converted to GDF) show
  their low 16 bits as codes in Kymo, as in BDF; MNE keeps 17 bits. Their samples are
  compared as MNE masks them, their events with find_events(mask=0xFFFF).

FIFF, where Kymo deliberately differs from MNE:
- FIFF files that aren't raw recordings (epochs, evoked responses, covariances, ...) are
  refused by both and skipped.
- MNE needs allow_maxshield=True for MaxShield recordings; Kymo opens them with a warning.
- Kymo's time 0 is the first sample (first_samp): its start time is meas_date +
  first_samp / sfreq, and its annotation onsets count from the first sample. MNE's are
  compared after the same shift.
- Kymo opens a later part of a split recording as the whole recording, from its first
  part; MNE reads from the part given on. Later parts are skipped (their first part is
  compared). A jump in the first sample between parts is a gap in Kymo, where MNE joins
  the parts; no file seen has one, and the comparison assumes none.
- Of the stim channels, only the combined one (STI101, else STI 014) has events in Kymo
  when there is one, its codes masked to the low 16 bits (Neuromag's STI016 makes them
  negative as 16-bit samples). Its events are compared with find_events(uint_cast=True),
  MNE's remedy; without uint_cast MNE takes negative codes' absolute value.
- Bad channels and the annotations' channels are compared too.
- Skips in the data are gaps in Kymo; MNE fills them with zeros (so its sample indices
  line up with Kymo's) and adds BAD_ACQ_SKIP annotations. Kymo must report NaN exactly
  where MNE's skips are, and MNE's BAD_ACQ_SKIP annotations are left out.
- Stim channels whose calibration (range x cal) isn't 1 are misc channels in Kymo (5 V
  trigger lines, analog inputs); their samples compare the same, and only Kymo's stim
  channels' events are compared.
- Float64 samples are read relative to each channel's first sample, as in EEGLAB.
- Channel kinds and filters are not compared.

XDF, which MNE doesn't read: values and timestamps are compared with pyxdf's (load_xdf
with its defaults: clocks synchronized, resets handled, regular streams dejittered; but
for the break threshold, see below), stream
modes with mnextend's stream_modes, the stream list (every stream in id order with
pyxdf's name and type, its channels those of its grid, its number of annotations)
with pyxdf's streams, annotation texts with mnextend's read_raw_xdf, the
start time with the file header's datetime as mnextend reads it. Where Kymo deliberately
differs:
- No resampling: every stream shown as channels is a grid of its own, compared with its
  pyxdf stream sample by sample. Each sample's time (from the grid's pieces: a rate and
  start per segment between breaks) must be pyxdf's dejittered time stamp minus the
  earliest first one of these streams, within 1 us; breaks are gaps in Kymo (mnextend
  joins the segments). The grid's rate must be pyxdf's effective rate.
- Breaks are steps of more than 1 s and 2 sample periods in Kymo, 1 s and 500 periods in
  pyxdf, which is loaded with jitter_break_threshold_samples=2 to match. With 500,
  samples after a step of lost samples shorter than 500 periods (7.8 s at 64 Hz) land up
  to its length early, as dejittering places a segment's samples as if none were missing:
  the Muse headband's stray samples in the sub-01 and sub-13 files, 3.4 to 4 s early.
- Samples before a stream's first time stamp count back from it in Kymo; pyxdf counts
  them on from 0. No file seen has any.
- Irregular numeric streams that are not of an event type, with one channel or several,
  become channels in Kymo, annotations in mnextend. They are drawn as held values
  (steps) on their synchronized time stamps, as pyxdf leaves them, which must match
  within 1 us; a time stamp before the previous one is taken as the previous one. Steps longer than
  1 s and 500 median steps (Kymo's median is a histogram's, within 1 %) are gaps, and the
  grid's rate is the average within the segments (pyxdf gives such streams none).
- Streams that can drop samples are on their synchronized time stamps too, as in pyxdf,
  drawn as lines, with gaps at steps longer than 1 s and 2 sample periods; their rate is
  the average within the segments (pyxdf's effective rate counts the gaps in).
- Channels of type "trigger(s)" are stim channels in Kymo, misc in mnextend.
- Labels two streams share are prefixed by their stream's name, type or id in Kymo; empty
  labels are named "{stream name}_{n}". Names must be the label, a prefixed label, or
  that default.
- Int64 and double64 values are read relative to each channel's first sample (unless it
  is beyond float32's range); Kymo's values must equal that float32 reading exactly.
  Stim channels' NaN reads as 0.
- Annotation onsets are compared with pyxdf's synchronized time stamps. pyxdf dejitters
  marker streams with a nominal rate too, as if they were signals; Kymo doesn't (their
  events are irregular), so theirs are compared without dejittering. Only mnextend's
  texts (and their number) are compared.
- Stim events are not compared.

BVRF, which MNE doesn't read: compared with pybvrf, the format's reference reader (its
header parsing, decoding and marker reading, and RawBVRF for the rest). Where Kymo
deliberately differs:
- pybvrf scales only V, mV, \u00b5V and nV (other units raise an error) and ignores the
  channels' coefficients. Its values are read in the resolution's units (every unit given
  to it as V), then scaled as Kymo does: by the unit's SI factor and linear coefficients
  (factor and offset); coefficients that aren't linear, by none (Kymo shows the sensor
  output, in the resolution's unit). Real files have linear ones: the PHOTO channels'
  [0, 0.001], so pybvrf's PHOTO values are 1000 times Kymo's (and BrainVision Analyzer's).
- Two markers sharing a StartEndId are one annotation in Kymo, from the first to the
  second; pybvrf makes two points. pybvrf's are paired the same way before comparing.
- Kymo leaves out "Recording Info" markers (saving and synchronization, as Analyzer does);
  pybvrf keeps them. They are left out of pybvrf's too.
- A marker of value 0 is "Type/Code0" in Kymo; pybvrf leaves the 0 out. pybvrf's are
  compared with the 0 put back.
- A type without a code or value is the type alone in Kymo ("Reconnection"); pybvrf adds a
  "/". pybvrf's are compared without the lone "/".
- With several participants, Kymo puts each participant's channels together (a stream's
  channels are consecutive), in file order within each, then the channels of all of them;
  pybvrf keeps the file's order, which may interleave them. pybvrf's channels are compared
  in Kymo's order. A single participant's channels are named as written in Kymo; pybvrf
  adds " (P1)" where they have a ParticipantId. Its names are compared without it. Which
  participant and channels a marker is for isn't compared: pybvrf has neither.
- The start time (Kymo's from the earliest dated marker) isn't compared: pybvrf has none.
  It is given Kymo's, so that MNE's CSV of the annotations (with --annotations), whose
  onsets are times of day, means the same in Kymo.
  Channel kinds and filters are not compared either.

With --annotations, annotation files are checked both ways (`kymo-core --example
annotations`): the recording's annotations saved by Kymo as an events table and read
with `read_events_table` (the snippet in the docs) equal MNE's own reading of the file;
and MNE's CSV of them (`Annotations.save`) loaded by Kymo equals Kymo's own reading.
"""

import argparse
import datetime
import json
import logging
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
# GDF's type codes: size in bytes, and the range of integer types (None for floats).
GDF_TYPES = {
    1: (1, (-(2**7), 2**7 - 1)),
    2: (1, (0, 2**8 - 1)),
    3: (2, (-(2**15), 2**15 - 1)),
    4: (2, (0, 2**16 - 1)),
    5: (4, (-(2**31), 2**31 - 1)),
    6: (4, (0, 2**32 - 1)),
    7: (8, (-(2**63), 2**63 - 1)),
    8: (8, (0, 2**64 - 1)),
    16: (4, None),
    17: (8, None),
    279: (3, (-(2**23), 2**23 - 1)),
    535: (3, (0, 2**24 - 1)),
}

# GDF types Kymo reads relative to each channel's first sample (and int32 too where it
# reads missing samples, see gdf_limited).
GDF_RELATIVE = {6, 7, 8, 17}


def gdf_limited(gdf_type, digital_min, digital_max, kind):
    """Whether Kymo reads a GDF channel's samples at or beyond its digital limits as missing
    (biosig's overflow detection): not in stim channels, only for finite limits with
    DigMin < DigMax, and for integer types only where they leave out one of its values."""
    if kind == "Stim":
        return False
    if not (np.isfinite(digital_min) and np.isfinite(digital_max)):
        return False
    if digital_min >= digital_max:
        return False
    limits = GDF_TYPES.get(gdf_type, (0, None))[1]
    if limits is None:
        return True
    return np.floor(digital_min) >= limits[0] or np.ceil(digital_max) <= limits[1]


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


def mne_scales(channel, scaled, gdf_info=None, edf=False):
    """Whether MNE scales the channel's unit to SI: the units in `scaled`, or for GDF
    (given its `gdf_info`) uV in GDF 1 and the codes for uV and mV in GDF 2. Never for EDF's
    and GDF's stim channels, whose values MNE leaves unscaled."""
    unit = channel["unit"].strip()
    if (edf or gdf_info is not None) and channel["kind"] == "Stim":
        return False
    if gdf_info is None:
        return unit in scaled
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


XDF_EVENT_TYPES = {"marker", "markers", "event", "events", "trigger", "triggers", "stim"}
# Kymo's kinds of MNE's channel types; MNE's other types are misc.
KYMO_KINDS = {
    "eeg": "Eeg",
    "eog": "Eog",
    "ecg": "Ecg",
    "emg": "Emg",
    "stim": "Stim",
    "misc": "Misc",
    "mag": "Mag",
    "grad": "Grad",
    "ref_meg": "RefMeg",
    "seeg": "Seeg",
    "ecog": "Ecog",
    "dbs": "Dbs",
    "resp": "Resp",
    "bio": "Bio",
}


def xdf_field(info, key):
    value = info.get(key) or [None]
    return value[0] if value[0] is not None else ""


def xdf_mode(stream):
    """How Kymo reads a pyxdf stream by default, as its info view says."""
    info = stream["info"]
    if len(stream["time_stamps"]) == 0:
        return "no samples"
    rate = float(xdf_field(info, "nominal_srate"))
    if xdf_field(info, "channel_format") == "string":
        return "annotations"
    if rate > 0:
        return "channels"
    kind = str(xdf_field(info, "type")).strip().lower()
    if kind not in XDF_EVENT_TYPES:
        return "channels"
    return "annotations"


def xdf_can_drop(stream):
    """Whether a pyxdf stream's header says it can drop samples."""
    try:
        value = stream["info"]["desc"][0]["synchronization"][0]["can_drop_samples"][0]
    except (TypeError, IndexError, KeyError):
        return False
    return str(value).strip().lower() == "true"


def xdf_own_timing(stamps, nominal):
    """Kymo's layout of a stream on its own (synchronized) time stamps: the stamps never
    before the previous one, the lengths of the runs between gaps (steps longer than 1 s and
    500 median steps, or for a nominal rate 1 s and 2 periods) and the average rate within
    them."""
    times = np.maximum.accumulate(np.asarray(stamps, dtype=np.float64))
    steps = np.diff(times)
    n = len(times)
    span = times[-1] - times[0] if n else 0.0
    if nominal > 0:
        usual = 1.0 / nominal
        threshold = max(1.0, 2.0 / nominal)
    else:
        ordered = np.sort(steps)
        median = ordered[(len(ordered) - 1) // 2] if len(ordered) else 0.0
        usual = median if median > 0 else (span / (n - 1) if span > 0 else 1.0)
        threshold = max(1.0, 500.0 * usual)
    gap = steps > threshold
    starts = np.concatenate([[0], np.flatnonzero(gap) + 1])
    lengths = np.diff(np.concatenate([starts, [n]]))
    within = steps[~gap]
    rate = len(within) / within.sum() if len(within) and within.sum() > 0 else 1.0 / usual
    return times, lengths, rate


def xdf_channels(stream):
    """(label, type, unit) of each channel of a pyxdf stream, empty where it has none."""
    info = stream["info"]
    count = int(xdf_field(info, "channel_count"))
    try:
        described = info["desc"][0]["channels"][0]["channel"]
    except (TypeError, IndexError, KeyError):
        described = []
    channels = [
        tuple(str((ch.get(k) or [""])[0] or "") for k in ("label", "type", "unit"))
        for ch in described
    ]
    return channels if len(channels) == count else [("", "", "")] * count


def compare_xdf(path, tmp, header, annotations, windows):
    """Compares Kymo's reading of an XDF file with pyxdf's and mnextend's; returns the
    problems and prints a summary."""
    import pyxdf
    from mnextend.io import xdf as mnextend_xdf

    problems, notes = [], []
    # pyxdf's defaults: clocks synchronized (with resets), regular streams dejittered; but
    # breaks at steps of more than 2 samples (and 1 s), as in Kymo.
    logging.getLogger("pyxdf").setLevel(logging.ERROR)
    streams, file_header = pyxdf.load_xdf(
        str(path), verbose=False, jitter_break_threshold_samples=2
    )
    by_id = {s["info"]["stream_id"]: s for s in streams}
    # Kymo's streams: what each became, its name and type.
    kymo_modes, kymo_streams = {}, {}
    for s in header["streams"]:
        if "channels" in s:
            mode = "channels"
        elif "annotations" in s:
            mode = "annotations"
        else:
            mode = "no samples" if s["not_shown"] == "it has no samples" else "not read"
        kymo_modes[s["id"]] = mode
        kymo_streams[s["id"]] = s
    ids = [s["id"] for s in header["streams"]]
    if ids != sorted(ids):
        problems.append(f"streams not in id order: {ids}")
    for sid, stream in by_id.items():
        ours = kymo_streams.get(sid)
        info = stream["info"]
        theirs = (str(xdf_field(info, "name")), str(xdf_field(info, "type")))
        if ours is None:
            problems.append(f"stream {sid} is not listed")
        elif (ours["name"], ours["kind"]) != theirs:
            problems.append(f"stream {sid}: {ours['name']!r} ({ours['kind']!r}), pyxdf {theirs}")

    # Stream modes, against Kymo's rule and mnextend's defaults.
    for info in mnextend_xdf.resolve_streams(str(path)):
        sid = info["stream_id"]
        stream = by_id.get(sid)
        expected = xdf_mode(stream) if stream is not None else "no samples"
        ours = kymo_modes.get(sid)
        if ours != expected:
            problems.append(f"stream {sid}: Kymo reads it as {ours!r}, expected {expected!r}")
        default = mnextend_xdf.stream_modes(info)[0]
        if ours == "channels" and float(info["nominal_srate"]) == 0 and default == "annotations":
            notes.append(f"stream {sid}: irregular channels, annotations in mnextend")
    shown = sorted(sid for sid, m in kymo_modes.items() if m == "channels")
    if len(shown) != len(header["grids"]):
        problems.append(f"{len(header['grids'])} grids, but {len(shown)} streams shown")
        shown = shown[: len(header["grids"])]
    t0 = min(by_id[sid]["time_stamps"][0] for sid in shown) if shown else 0.0

    # Grids: rate, length, every sample's time, names, kinds, units and values.
    channels = header["channels"]
    worst, compared, worst_time = 0.0, 0, 0.0
    for g, (grid, sid) in enumerate(zip(header["grids"], shown)):
        stream = by_id[sid]
        info = stream["info"]
        ids = grid["channels"]
        if kymo_streams[sid]["channels"] != ids:
            problems.append(f"stream {sid}: channels {kymo_streams[sid]['channels']}, grid {ids}")
        data = np.asarray(stream["time_series"])
        stamps = stream["time_stamps"]
        name = str(xdf_field(info, "name"))
        # The grid's segments hold the stream's samples one after another; gaps between
        # them stand for pyxdf's breaks.
        segments = np.array(grid["segments"], dtype=np.int64).reshape(-1, 2)
        lengths = segments[:, 1] - segments[:, 0]
        if lengths.sum() != len(stamps):
            problems.append(f"stream {sid}: {lengths.sum()} samples, pyxdf {len(stamps)}")
            continue
        before = np.concatenate([[0], np.cumsum(lengths)[:-1]])
        slots = np.concatenate(
            [np.arange(a, b) for a, b in segments]
        ) if len(segments) else np.zeros(0, dtype=np.int64)
        nominal = float(xdf_field(info, "nominal_srate"))
        own = nominal == 0 or xdf_can_drop(stream)
        if own != grid["irregular"] or grid["holds"] != (nominal == 0):
            problems.append(
                f"stream {sid}: irregular {grid['irregular']}, holds {grid['holds']}; "
                f"nominal rate {nominal}, can drop samples {xdf_can_drop(stream)}"
            )
            continue
        if own:
            # On its own synchronized time stamps, which pyxdf doesn't dejitter either.
            expected_times, expected_lengths, expected_rate = xdf_own_timing(stamps, nominal)
            if list(lengths) != list(expected_lengths):
                problems.append(
                    f"stream {sid}: segments of {list(lengths)} samples, expected "
                    f"{list(expected_lengths)}"
                )
            if not np.isclose(grid["rate"], expected_rate, rtol=1e-9, atol=0):
                problems.append(f"stream {sid}: rate {grid['rate']}, expected {expected_rate}")
            if len(expected_lengths) > 1:
                notes.append(f"stream {sid}: own time stamps, {len(expected_lengths)} segments")
            backward = int((np.diff(stamps) < 0).sum())
            if backward:
                notes.append(f"stream {sid}: {backward} time stamps before the previous one")
            all_times = np.fromfile(tmp / f"grid{g}.times", dtype="<f8")
            times = all_times[slots]
            error = np.abs(times - (expected_times - t0))
        else:
            effective = float(info["effective_srate"]) or nominal
            if not np.isclose(grid["rate"], effective, rtol=1e-9, atol=0):
                problems.append(f"stream {sid}: rate {grid['rate']}, pyxdf {effective}")
            n_segments = len(info["segments"])
            if n_segments > 1:
                notes.append(f"stream {sid}: {n_segments} segments")
            pieces = np.array(grid["pieces"], dtype=np.float64).reshape(-1, 3)
            piece = np.searchsorted(pieces[:, 0], slots, side="right") - 1
            times = pieces[piece, 1] + (slots - pieces[piece, 0]) / pieces[piece, 2]
            error = np.abs(times - (stamps - t0))
        worst_time = max(worst_time, float(error.max()) if len(error) else 0.0)
        if len(error) and error.max() > 1e-6:
            i = int(error.argmax())
            problems.append(
                f"stream {sid}: sample {i} at {times[i]} s, pyxdf at {stamps[i] - t0} s "
                f"({(error > 1e-6).sum()} differ by more than 1 us)"
            )
        discrete = mnextend_xdf.stream_modes(
            {
                "channel_format": xdf_field(info, "channel_format"),
                "nominal_srate": xdf_field(info, "nominal_srate"),
                "type": xdf_field(info, "type"),
            }
        )[0] == "discrete"
        for k, (c, (label, kind, unit)) in enumerate(zip(ids, xdf_channels(stream))):
            ch = channels[c]
            default = f"{name}_{k}"
            if not (
                ch["name"] == (label or default)
                or (label and ch["name"].endswith(" " + label))
            ):
                problems.append(f"stream {sid} channel {k}: name {ch['name']!r}, label {label!r}")
            kind = kind.strip().lower()
            if discrete or kind in ("stim", "trigger", "triggers"):
                expected = "Stim"
            else:
                expected = KYMO_KINDS.get(kind, "Misc")
            if ch["kind"] != expected:
                problems.append(f"{ch['name']}: kind {ch['kind']}, expected {expected}")
            parsed = mnextend_xdf._parse_unit(unit)
            if isinstance(parsed, tuple) and parsed[1] in (
                mne.io.constants.FIFF.FIFF_UNIT_V,
                mne.io.constants.FIFF.FIFF_UNIT_T,
                mne.io.constants.FIFF.FIFF_UNIT_S,
            ):
                if ch["to_si"] is None or not np.isclose(ch["to_si"], parsed[0]):
                    problems.append(f"{ch['name']}: unit {unit!r} scales by {ch['to_si']}")
        ours_all = np.fromfile(tmp / f"grid{g}.f32", dtype="<f4")
        reference = np.array([channels[c]["offset"] for c in ids])[:, None]
        stim = np.array([channels[c]["kind"] == "Stim" for c in ids])[:, None]
        offset = 0
        for a, b in windows[g]:
            n = b - a
            ours = ours_all[offset : offset + len(ids) * n].reshape(len(ids), n)
            offset += len(ids) * n
            # The stream's sample in each slot; gaps read as NaN in Kymo.
            slot = np.arange(a, b)
            seg = np.searchsorted(segments[:, 0], slot, side="right") - 1
            inside = (seg >= 0) & (slot < segments[np.maximum(seg, 0), 1])
            if (~np.isnan(ours[:, ~inside])).any():
                problems.append(f"stream {sid}: values in gaps")
            ours = ours[:, inside]
            sample = before[seg[inside]] + slot[inside] - segments[seg[inside], 0]
            theirs = data[sample].T.astype(np.float64)
            theirs = np.where(stim & np.isnan(theirs), 0.0, theirs)
            # Kymo's reading: float32 relative to the reference, back in float64.
            with np.errstate(over="ignore", invalid="ignore"):
                native = (theirs - reference).astype(np.float32).astype(np.float64)
                expected = (native + reference).astype(np.float32)
            same = (ours == expected) | (np.isnan(ours) & np.isnan(expected))
            if not same.all():
                k, i = np.argwhere(~same)[0]
                problems.append(
                    f"{channels[ids[k]]['name']}: sample {a + i} is {ours[k, i]}, pyxdf "
                    f"{theirs[k, i]} ({(~same).sum()} differ)"
                )
            # Relative to the value's size, or to the reference's for values read
            # relative to one.
            size = np.maximum(np.abs(theirs), np.abs(reference))
            finite = np.isfinite(theirs) & np.isfinite(ours) & (size != 0)
            if finite.any():
                rel = np.abs(ours[finite] - theirs[finite]) / size[finite]
                worst = max(worst, float(np.nanmax(rel)))
            compared += ours.size

    # Annotations: onsets from pyxdf's synchronized timestamps, texts as mnextend writes
    # them.
    annotation_ids = sorted(sid for sid, m in kymo_modes.items() if m == "annotations")
    # pyxdf dejitters marker streams with a nominal rate as if they were signals; Kymo
    # keeps their synchronized time stamps, so those come from a load without dejittering.
    regular_markers = [
        sid
        for sid in annotation_ids
        if float(xdf_field(by_id[sid]["info"], "nominal_srate")) > 0
    ]
    undejittered = {}
    if regular_markers:
        loaded, _ = pyxdf.load_xdf(
            str(path),
            select_streams=regular_markers,
            dejitter_timestamps=False,
            verbose=False,
        )
        undejittered = {s["info"]["stream_id"]: s for s in loaded}
    expected = []
    for sid in annotation_ids:
        stream = undejittered.get(sid, by_id[sid])
        string = xdf_field(stream["info"], "channel_format") == "string"
        before = len(expected)
        for t, sample in zip(stream["time_stamps"], stream["time_series"]):
            if string:
                items = [item for item in sample if item]
            else:
                values = [str(v) for v in sample if not np.isnan(v)]
                items = [",".join(values)] if values else []
            expected += [(t - t0, item, sid) for item in items]
        count = kymo_streams[sid]["annotations"]
        if count != len(expected) - before:
            problems.append(f"stream {sid}: {count} annotations, pyxdf {len(expected) - before}")
    # Sorted by onset (to the microsecond, as both are rounded), then text and stream.
    ours = sorted(
        ((a["onset"], a["description"], a["stream"]) for a in annotations),
        key=lambda a: (round(a[0], 6), a[1], a[2]),
    )
    expected.sort(key=lambda a: (round(a[0], 6), a[1], a[2]))
    if len(ours) != len(expected):
        problems.append(f"{len(ours)} annotations, pyxdf {len(expected)}")
    else:
        bad = [
            (o, e)
            for o, e in zip(ours, expected)
            if o[1:] != e[1:] or abs(o[0] - e[0]) > 1e-6
        ]
        if bad:
            problems.append(f"{len(bad)} annotations differ, first: {bad[0]}")
    if annotation_ids and shown:
        # mnextend needs a stream as channels: a regular one in its default mode (an
        # irregular one's default is annotations), else an irregular one resampled.
        regular = [
            sid for sid in shown if float(xdf_field(by_id[sid]["info"], "nominal_srate")) > 0
        ]
        modes = {regular[0]: None} if regular else {shown[0]: "continuous"}
        modes.update(dict.fromkeys(annotation_ids, "annotations"))
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            raw = mnextend_xdf.read_raw_xdf(str(path), modes, verbose="error")
        theirs = sorted(
            d for d in raw.annotations.description if d != "BAD_ACQ_SKIP"
        )
        if sorted(a["description"] for a in annotations) != theirs:
            problems.append("annotation texts differ from mnextend's")

    # The start time: the file header's datetime, in UTC.
    datetime_text = (file_header["info"].get("datetime") or [None])[0]
    if datetime_text:
        try:
            theirs = datetime.datetime.fromisoformat(datetime_text)
        except ValueError:
            theirs = datetime.datetime.fromisoformat(
                datetime_text[:-2] + ":" + datetime_text[-2:]
            )
        zone = dict(header["extra"]).get("time zone")
        ours = header["start_time"] and datetime.datetime.fromisoformat(header["start_time"])
        if ours and zone:
            sign = -1 if zone[3] == "-" else 1
            hours, minutes = (int(x) for x in zone[4:].split(":")) if len(zone) > 3 else (0, 0)
            ours = ours.replace(
                tzinfo=datetime.timezone(sign * datetime.timedelta(hours=hours, minutes=minutes))
            )
        if ours != theirs:
            problems.append(f"start time {ours}, file header {theirs}")
    elif header["start_time"] is not None:
        problems.append(f"start time {header['start_time']} without a datetime")
    notes.append("stim events not compared")

    status = "OK" if not problems else "DIFFERS"
    print(f"{status}: {path}")
    print(
        f"  XDF, {len(channels)} channels in {len(shown)} streams, {compared} samples "
        f"compared, max relative error {worst:.1e}, sample times within "
        f"{worst_time * 1e6:.3f} us, {len(annotations)} annotations"
    )
    for w in header["warnings"]:
        print(f"  Kymo warning: {w}")
    for n in notes:
        print(f"  note: {n}")
    for p in problems:
        print(f"  PROBLEM: {p}")
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
            if path.suffix.lower() in (".set", ".fif") and refused_by_mne(path):
                print(f"SKIPPED: {path}\n  neither Kymo nor MNE reads it")
                return None
            return [f"Kymo could not read the file ({e})"]
        header = json.loads((tmp / "header.json").read_text())
        if Path(header["sources"][0]).resolve() != path.resolve():
            print(
                f"SKIPPED: {path}\n  a later part of a split recording, which Kymo opens "
                f"from its first part, {Path(header['sources'][0]).name}"
            )
            return None
        annotations = json.loads((tmp / "annotations.json").read_text())
        windows = json.loads((tmp / "windows.json").read_text())
        if header["format"] == "XDF":
            return compare_xdf(path, tmp, header, annotations, windows)

        brainvision = header["format"] == "BrainVision"
        bvrf = header["format"] == "BVRF"
        eeglab = header["format"] == "EEGLAB"
        gdf = header["format"] == "GDF"
        fiff = header["format"] == "FIFF"
        if gdf:
            info = gdf_info(path)
            reason = gdf_skip_reason(info)
            if reason:
                print(f"SKIPPED: {path}\n  {reason}")
                return None
        try:
            if gdf:
                raw = mne.io.read_raw_gdf(path, preload=False, verbose="error")
            elif fiff:
                raw = mne.io.read_raw_fif(
                    path, preload=False, verbose="error", allow_maxshield=True
                )
            elif brainvision:
                raw = mne.io.read_raw_brainvision(path, preload=False, verbose="error")
            elif bvrf:
                raw, coefficients = read_bvrf(path, header["start_time"])
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
        if bvrf:
            coefficients = align_bvrf(raw, coefficients, names, notes)
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

        # BVRF: pybvrf has no start time; its meas_date is Kymo's (see read_bvrf).
        if (
            not bvrf
            and header["start_time"] is not None
            and raw.info["meas_date"] is not None
        ):
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
            elif fiff:
                # Kymo's time 0 is first_samp, on the file's local clock if it has one.
                ours = datetime.datetime.fromisoformat(header["start_time"][:26])
                theirs = date + datetime.timedelta(
                    seconds=raw.first_samp / raw.info["sfreq"]
                )
                offset = raw.info.get("utc_offset")
                if offset:
                    sign = -1 if offset.startswith("-") else 1
                    hours, minutes = offset[1:].replace(":", "")[:2], offset[-2:]
                    theirs += sign * datetime.timedelta(
                        hours=int(hours), minutes=int(minutes)
                    )
                if abs((ours - theirs).total_seconds()) > 1e-6:
                    problems.append(
                        f"start time: Kymo {header['start_time']}, MNE {theirs}"
                    )
            elif brainvision:
                # Kymo prints microseconds without trailing zeros.
                theirs = date.isoformat(timespec="microseconds").rstrip("0").rstrip(".")
            else:
                theirs = date.replace(microsecond=0).isoformat()
            if not gdf and not fiff and theirs != header["start_time"]:
                problems.append(
                    f"start time: Kymo {header['start_time']}, MNE {theirs}"
                )

        if fiff:
            problems += compare_skips(raw, header["grids"][0])

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
                and not fiff
                and channels[c]["to_si"] is not None
                and (
                    bvrf
                    or not mne_scales(
                        channels[c],
                        scaled,
                        info if gdf else None,
                        edf=header["format"].startswith("EDF"),
                    )
                )
                for c in ids
            ]
            theirs_scale = np.where(unscaled, scale, 1.0)
            theirs_offset = np.zeros(len(ids))
            if bvrf:
                # pybvrf's values are in the resolution's units: Kymo's scaling and the
                # linear coefficients (see the docstring).
                factor, shift = np.array([coefficients[c] for c in ids]).T
                theirs_offset = shift * scale
                theirs_scale = scale * factor
                unscaled = [False] * len(ids)
                if any(f != 1 or o != 0 for f, o in (coefficients[c] for c in ids)):
                    notes.append(
                        "pybvrf's values scaled by the linear coefficients: "
                        + ", ".join(
                            names[c]
                            for c in ids
                            if coefficients[c] != (1.0, 0.0)
                        )
                    )
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
                    if eeglab or fiff or bvrf
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
                    limited = gdf_limited(
                        t,
                        extras["digital_min"][c],
                        extras["digital_max"][c],
                        channels[c]["kind"],
                    )
                    relative = t in GDF_RELATIVE or (t == 5 and limited)
                    if relative:
                        reference[k] = (
                            abs(channels[c]["offset"]) * scale[k]
                            + abs(extras["offsets"][c] * extras["units"][c])
                            * theirs_scale[k]
                        )
            # GDF: the physical values (MNE's) of the digital limits, at or beyond which
            # Kymo reads a sample as missing, per channel (None where it doesn't).
            missing = [None] * len(ids)
            if gdf:
                for k, c in enumerate(ids):
                    t = info["types"][data_channels[c]]
                    lo, hi = extras["digital_min"][c], extras["digital_max"][c]
                    if gdf_limited(t, lo, hi, channels[c]["kind"]):
                        a, b = (
                            (x * extras["cal"][c] + extras["offsets"][c])
                            * extras["units"][c]
                            for x in (lo, hi)
                        )
                        # Half a digital step apart from the limits for integers, so
                        # the next value inside isn't taken for one at a limit; floats
                        # are compared in float64 on both sides.
                        step = abs(extras["cal"][c] * extras["units"][c])
                        integer = GDF_TYPES.get(t, (0, None))[1] is not None
                        tol = 0.5 * step if integer else 1e-12 * max(abs(a), abs(b))
                        missing[k] = (min(a, b), max(a, b), tol)
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
                joined=not fiff,
                theirs_offset=theirs_offset,
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
        if fiff and any(a == "BAD_ACQ_SKIP" for a in raw.annotations.description):
            notes.append("MNE's BAD_ACQ_SKIP annotations left out (Kymo has gaps)")
        if fiff:
            problems += compare_fiff_extras(raw, header, annotations)
            if len(header["sources"]) > 1:
                notes.append(f"{len(header['sources'])} split parts")
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
            p, n = compare_events(raw, header, events, fmax, notes, joined=not fiff)
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
    joined=True,
    theirs_offset=None,
):
    """With `joined`, MNE's data drops Kymo's gaps (EDF+D); without, its samples line
    up with Kymo's (FIFF, whose skips MNE fills with zeros). MNE's values are multiplied
    by `theirs_scale`, and `theirs_offset` added (per channel, none by default)."""
    if theirs_offset is None:
        theirs_offset = np.zeros(len(ids))
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
        mne_idx = (
            joined_start[seg[present]] + idx[present] - segments[seg[present], 0]
            if joined
            else idx[present]
        )
        lo, hi = int(mne_idx.min()), int(mne_idx.max()) + 1
        theirs = raw.get_data(picks=ids, start=lo, stop=hi)[:, mne_idx - lo]
        ours = block[:, present] * scale[:, None]
        for k, limits in enumerate(missing):
            if limits is None:
                continue
            # Kymo's missing samples: MNE's value must lie at or beyond a digital limit.
            lo, hi, tol = limits
            gone = np.isnan(ours[k]) & ~np.isnan(theirs[k])
            inside = (theirs[k] > lo + tol) & (theirs[k] < hi - tol)
            wrong = gone & inside
            if wrong.any():
                col = np.argmax(wrong)
                problems.append(
                    f"window {a}..{b}: Kymo has NaN in {names[ids[k]]} at sample "
                    f"{idx[present][col]}, MNE {theirs[k, col]!r} (missing samples lie "
                    f"at or beyond {lo!r} and {hi!r})"
                )
            # And the other way round: MNE's values at or beyond a limit are missing in Kymo.
            kept = ~np.isnan(ours[k]) & ~np.isnan(theirs[k]) & ~inside
            if kept.any():
                col = np.argmax(kept)
                problems.append(
                    f"window {a}..{b}: Kymo has a value in {names[ids[k]]} at sample "
                    f"{idx[present][col]}, where MNE's {theirs[k, col]!r} lies at or beyond "
                    f"the digital limits {lo!r} and {hi!r}"
                )
            theirs[k, gone] = np.nan
            n_missing += int(gone.sum())
        theirs = theirs * theirs_scale[:, None] + theirs_offset[:, None]
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
            f"{n_missing} missing sample(s) (NaN in Kymo, at or beyond the digital "
            "limits in MNE)"
        )
    return problems, worst, compared


def compare_fiff_extras(raw, header, annotations):
    """Problems with FIFF's bad channels and the annotations' channels."""
    problems = []
    names = [c["name"] for c in header["channels"]]
    ours = sorted(names[c] for c in header["bad_channels"])
    if ours != sorted(raw.info["bads"]):
        problems.append(f"bad channels: Kymo {ours}, MNE {sorted(raw.info['bads'])}")
    theirs = sorted(
        (round(o - raw.first_samp / raw.info["sfreq"], 6), s, tuple(sorted(c)))
        for o, s, c in zip(
            raw.annotations.onset, raw.annotations.description, raw.annotations.ch_names
        )
        if s != "BAD_ACQ_SKIP"
    )
    ours = sorted(
        (round(a["onset"], 6), a["description"], tuple(sorted(a["channels"])))
        for a in annotations
    )
    if [t[2] for t in ours] != [t[2] for t in theirs]:
        problems.append("annotation channels differ")
    return problems


def compare_skips(raw, grid):
    """Problems if Kymo's gaps aren't exactly MNE's skips (FIFF), in samples from
    first_samp."""
    theirs = []
    for extra in raw._raw_extras:
        bounds = extra["bounds"]
        for k, entry in enumerate(extra["ent"]):
            if entry is None:
                theirs.append([int(bounds[k]) - raw.first_samp, int(bounds[k + 1]) - raw.first_samp])
    ours, end = [], 0
    for start, stop in grid["segments"]:
        if start > end:
            ours.append([end, start])
        end = stop
    if end < grid["len"]:
        ours.append([end, grid["len"]])
    if grid["len"] != raw.n_times:
        return [f"length: Kymo {grid['len']} samples, MNE {raw.n_times}"]
    if ours != theirs:
        return [f"gaps: Kymo {ours}, MNE's skips {theirs}"]
    return []


def compare_events(raw, header, events, fmax, notes, joined=True):
    """Compares Kymo's stim events with mne.find_events; returns problems and count.
    With `joined`, MNE's data drops Kymo's gaps (see compare_grid)."""
    problems, count = [], 0
    fiff = header["format"] == "FIFF"
    bdf = header["format"].startswith("BDF")
    gdf = header["format"] == "GDF"
    for entry in events:
        c = entry["channel"]
        channel = header["channels"][c]
        if not channel.get("events", True):
            notes.append(
                f"no events of {channel['name']} (the combined stim channel covers it)"
            )
            continue
        grid = header["grids"][channel["grid"]]
        if grid["rate"] != fmax:
            notes.append(
                f"skipped events of {channel['name']} (MNE resamples its channel)"
            )
            continue
        ours = np.array(entry["events"], dtype=np.int64).reshape(-1, 2)
        trigger_mask = channel.get("trigger_mask")
        if gdf:
            # MNE masks GDF stim channels' physical values: events of the masked values,
            # under the same rule (every change to a non-zero code, none at the start).
            # Kymo's changes are of the values under its trigger mask (Status channels).
            masked, previous = [], None
            for sample, value in entry["changes"]:
                code = int(value * channel["gain"] + channel["offset"])
                code &= STIM_MASK & (trigger_mask if trigger_mask is not None else -1)
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
        if joined:
            seg = np.searchsorted(segments[:, 0], ours[:, 0], side="right") - 1
            ours[:, 0] = joined_start[seg] + ours[:, 0] - segments[seg, 0]
        theirs = mne.find_events(
            raw,
            stim_channel=raw.ch_names[c],
            consecutive=True,
            shortest_event=1,
            initial_event=False,
            mask=0xFFFF if bdf or trigger_mask is not None else None,
            mask_type="and",
            verbose="error",
            # FIFF's combined channels hold 16-bit codes (Kymo's trigger mask): MNE's own
            # remedy for STI016 making them negative.
            uint_cast=fiff and trigger_mask is not None,
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


def paired_gdf_events(events, event_rate):
    """MNE's GDF events (onset, duration, description, channels) as Kymo reads them: an
    event without a duration (MNE's one sample) ends at the first later end of its type
    and channels (type | 0x8000), which isn't kept on its own (see the docstring)."""
    def point(d):
        return d == 0 or abs(d - 1 / event_rate) < 1e-9

    order = sorted(range(len(events)), key=lambda k: events[k][0])
    starts, ends, closed = {}, {}, set()
    for k in order:
        onset, duration, description, channels = events[k]
        code = int(description)
        if code & 0x8000 == 0:
            if code != 0 and point(duration):
                starts.setdefault((code, channels), []).append(k)
        else:
            waiting = starts.get((code & 0x7FFF, channels))
            if waiting:
                ends[waiting.pop(0)] = onset
                closed.add(k)
    return [
        (o, ends[k] - o if k in ends else d, s)
        for k, (o, d, s, _) in enumerate(events)
        if k not in closed
    ]


def compare_annotations(raw, annotations, first_onset, format_name, event_rate=None):
    ours = cropped(annotations, data_end(raw, first_onset))
    theirs = zip(
        raw.annotations.onset, raw.annotations.duration, raw.annotations.description
    )
    if format_name == "GDF":
        # Kymo's rules for events; see the docstring.
        sfreq = raw.info["sfreq"]
        theirs = [
            (o * sfreq / event_rate, d * sfreq / event_rate, s, tuple(c))
            for o, d, s, c in zip(
                raw.annotations.onset,
                raw.annotations.duration,
                raw.annotations.description,
                raw.annotations.ch_names,
            )
            if o >= 0 and s != str(0x7FFF)
        ]
        theirs = paired_gdf_events(theirs, event_rate)
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
    if format_name == "FIFF":
        # Kymo counts from the first sample; see the docstring.
        shift = raw.first_samp / raw.info["sfreq"]
        theirs = [(o - shift, d, s) for o, d, s in theirs if s != "BAD_ACQ_SKIP"]
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


def bvrf_coefficients(channel):
    """A BVRF channel's linear coefficients as (factor, offset), as Kymo reads them; (1, 0)
    where they aren't linear (Kymo shows the sensor output then)."""
    c = channel.get("Coefficients") or {}

    def terms(key, default):
        values = list(default if c.get(key) is None else c[key])
        while values and values[-1] == 0:
            values.pop()
        return values

    num, denom = terms("Num", [0.0, 1.0]), terms("Denom", [1.0])
    if len(num) <= 2 and len(denom) == 1:
        num = num + [0.0] * (2 - len(num))
        return (num[1] / denom[0], num[0] / denom[0])
    return (1.0, 0.0)


def read_bvrf(path, start_time):
    """pybvrf's reading of a BVRF recording, as MNE's Raw (RawBVRF), its values in the
    resolution's units, with its annotations adjusted to Kymo's rules (see the docstring)
    and Kymo's `start_time`, and each channel's linear coefficients (factor, offset)."""
    from pybvrf.mne_io import RawBVRF
    from pybvrf.pybvrf import _read_bvrd, _read_bvrm, read_bvrf_header

    header = read_bvrf_header(path)
    n = header["n_channels"]
    data = _read_bvrd(
        path.with_suffix(".bvrd"), header["dtype"], n, ["V"] * n, header["ch_resolutions"]
    )
    markers = _read_bvrm(path.with_suffix(".bvrm"))
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        raw = RawBVRF.from_data(header, data, markers, verbose="error")
    if start_time is not None:
        # pybvrf has no start time; Kymo's, so that MNE's CSV of the annotations (with
        # times of day) means the same to both (see the docstring).
        start = datetime.datetime.fromisoformat(start_time)
        raw.set_meas_date(start.replace(tzinfo=datetime.timezone.utc))
    # MNE sorts the annotations by onset, so the markers are taken in the file's order, each
    # described by pybvrf's expression (mne_io.RawBVRF), which must give its annotations.
    fs = header["fs"]
    rows = [
        (sample / fs, f"{t}/{c}{v if v > 0 else ''}")
        for sample, t, c, v in zip(
            markers["Sample"], markers["Type"], markers["Code"], markers["Value"]
        )
    ]
    theirs = zip(raw.annotations.onset, raw.annotations.description)
    if sorted(rows) != sorted((float(o), str(d)) for o, d in theirs):
        raise ValueError("pybvrf's annotations aren't its markers as expected")
    columns = markers.dtype.names or ()
    pairs = {}
    onsets, durations, descriptions = [], [], []
    for k, (onset, description) in enumerate(rows):
        row = markers[k]
        if markers.dtype["Value"].kind in "iu" and row["Value"] == 0:
            description += "0"
        description = description.removesuffix("/")
        pair = str(row["StartEndId"]).strip() if "StartEndId" in columns else ""
        if pair and pair in pairs:
            first = pairs.pop(pair)
            if first is not None:
                start = min(onsets[first], onset)
                durations[first] = abs(onset - onsets[first])
                onsets[first] = start
            continue
        info = str(row["Type"]).strip() == "Recording Info"
        if pair:
            pairs[pair] = None if info else len(onsets)
        if info:
            continue
        onsets.append(onset)
        durations.append(0.0)
        descriptions.append(description)
    raw.set_annotations(mne.Annotations(onsets, durations, descriptions))
    raw._bvrf_header = header["yaml_header"]
    coefficients = [
        bvrf_coefficients(c) for c in header["yaml_header"]["EEGModality"]["Channels"]
    ]
    return raw, coefficients


def align_bvrf(raw, coefficients, names, notes):
    """pybvrf's channels in Kymo's order (each participant's together, where the file may
    interleave them) and named as Kymo names them: pybvrf qualifies the channels of a
    single participant too, Kymo only with several (see the docstring). Returns the
    coefficients in that order."""
    eeg = raw._bvrf_header["EEGModality"]
    listed = [p.get("Id") for p in raw._bvrf_header.get("Participants") or []]
    ids = set(listed) | {c["ParticipantId"] for c in eeg["Channels"] if c.get("ParticipantId")}
    if len(ids) < 2:
        renamed = {
            name: c["Name"]
            for name, c in zip(raw.ch_names, eeg["Channels"])
            if c.get("ParticipantId") and name != c["Name"]
        }
        if renamed:
            raw.rename_channels(renamed)
            notes.append("pybvrf's names of a single participant compared without it")
    if sorted(raw.ch_names) != sorted(names) or raw.ch_names == names:
        return coefficients
    order = [raw.ch_names.index(n) for n in names]
    raw.reorder_channels(names)
    notes.append("pybvrf's channels compared in Kymo's order, each participant's together")
    return [coefficients[k] for k in order]


def is_mat_v73(path):
    with open(path, "rb") as f:
        return f.read(19) == b"MATLAB 7.3 MAT-file"


def refused_by_mne(path):
    try:
        if path.suffix.lower() == ".fif":
            mne.io.read_raw_fif(
                path, preload=False, verbose="error", allow_maxshield=True
            )
        else:
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
        ("bvrf-int16", "bvrh", []),
        ("bvrf-float64", "bvrh", []),
        ("bvrf-participants", "bvrh", []),
        ("eeglab-fdt", "set", []),
        ("eeglab-set-single", "set", []),
        ("eeglab-set-double", "set", []),
        ("gdf1-float64", "gdf", []),
        ("gdf2-int32", "gdf", []),
        ("gdf2-mixed", "gdf", []),
        ("fif-float32-meg", "fif", []),
        ("fif-int16-skips", "fif", []),
        ("fif-int32-varying", "fif", []),
        ("fif-double-dc", "fif", []),
        ("fif-mixed", "fif", []),
        ("fif-split", "fif", []),
        ("xdf-float32", "xdf", []),
        ("xdf-mixed", "xdf", []),
        ("xdf-events", "xdf", []),
        ("xdf-clocks", "xdf", []),
        ("xdf-irregular", "xdf", []),
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
    return files + mne_split_files(directory, directory / "fif-float32-meg.fif")


def mne_split_files(directory, source):
    """Split files MNE writes of `source` (with its annotations and bad channels), in
    both of MNE's namings; the first part of each."""
    raw = mne.io.read_raw_fif(source, preload=True, verbose="error")
    files = []
    for naming, name in (("neuromag", "mne_split_raw.fif"), ("bids", "mne_split_meg.fif")):
        written = raw.save(
            directory / name,
            split_size=1_100_000,
            split_naming=naming,
            buffer_size_sec=1.0,
            overwrite=True,
            verbose="error",
        )
        files.append(Path(written[0]))
    return files


def find_files(arguments):
    files = []
    for a in map(Path, arguments):
        if a.is_dir():
            files += sorted(
                p
                for p in a.rglob("*")
                if p.suffix.lower()
                in (".edf", ".bdf", ".vhdr", ".set", ".gdf", ".fif", ".xdf", ".bvrh")
            )
        else:
            files.append(a)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "paths",
        nargs="*",
        help="EDF/BDF/BrainVision (.vhdr)/EEGLAB (.set)/GDF/FIFF/XDF/BVRF (.bvrh) files or "
        "directories",
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
            sys.exit("no EDF, BDF, BrainVision, EEGLAB, GDF, FIFF, XDF or BVRF files found")
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
