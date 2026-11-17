//! Channel types and bad channels to and from a BIDS `channels.tsv`.
//!
//! kymo writes a row per channel in the file's order, stim channels too, so a reader that
//! renames a recording's channels by position when the counts agree (MNE-BIDS does) leaves
//! them as they are, duplicate names included. The columns are `name`, `type` (BIDS' type,
//! [`bids_type`]), `units` (the unit's symbol, `n/a` without one), `sampling_frequency`
//! (`n/a` for an irregular grid), `status` (`good` or `bad`) and, where a channel's type
//! doesn't read back from its BIDS type alone, as it is shown ([`reads_back`]: Bio, SaO2,
//! a name of the file's own, `accel` written as `ACCEL`), a
//! `kymo_type` column with kymo's name of every type (`n/a` in the other rows), which other
//! readers ignore.
//!
//! It reads any `channels.tsv` with `name` and `type` columns. A channel is found by name;
//! where the recording has several of a name, the n-th row of it is its n-th channel. A
//! type is `kymo_type` where given, else the BIDS type ([`kind_of_bids`]; MEGOTHER, which
//! stands for several of kymo's types, and `n/a` leave a channel's type); `status` sets
//! a channel good or bad where it is `good` or `bad`, in any case. Units are not taken (a
//! channel's values come from the recording), only counted where they differ: in quantity
//! where kymo knows both (MNE-BIDS writes V for a file's µV), else as symbols; `n/a` isn't
//! compared.

use std::collections::HashMap;
use std::io::{Read, Write};

use crate::annotation_file::{FileError, csv_error, io_error};
use crate::{ChannelId, ChannelKind, Recording, Unit};

/// kymo's known types (by key) and the BIDS type each is written as.
const WRITTEN: [(&str, &str); 32] = [
    ("eeg", "EEG"),
    ("mag", "MEGMAG"),
    ("grad", "MEGGRADPLANAR"),
    ("ref_meg", "MEGREFMAG"),
    ("eog", "EOG"),
    ("heog", "HEOG"),
    ("veog", "VEOG"),
    ("ecg", "ECG"),
    ("emg", "EMG"),
    ("seeg", "SEEG"),
    ("ecog", "ECOG"),
    ("dbs", "DBS"),
    ("resp", "RESP"),
    ("ppg", "PPG"),
    ("gsr", "GSR"),
    ("temperature", "TEMP"),
    ("eyegaze", "EYEGAZE"),
    ("pupil", "PUPIL"),
    ("fnirs_cw_amplitude", "NIRSCWAMPLITUDE"),
    ("fnirs_od", "NIRSCWOPTICALDENSITY"),
    ("hbo", "NIRSCWHBO"),
    ("hbr", "NIRSCWHBR"),
    ("chpi", "HLU"),
    // As MNE-BIDS writes them; MEGOTHER reads back as no type, so `kymo_type` keeps them.
    ("syst", "MEGOTHER"),
    ("ias", "MEGOTHER"),
    ("exci", "MEGOTHER"),
    ("sound", "AUDIO"),
    ("light", "PD"),
    ("stim", "TRIG"),
    ("misc", "MISC"),
    // MNE-BIDS writes Bio as MISC; `kymo_type` keeps it.
    ("bio", "MISC"),
    ("sao2", "MISC"),
];

/// BIDS types and kymo's known type each reads as (by key); `None` leaves a channel's type
/// as it is, as MNE-BIDS does for MEGOTHER, which stands for several.
const READ: [(&str, Option<&str>); 31] = [
    ("EEG", Some("eeg")),
    ("MEGMAG", Some("mag")),
    ("MEGGRADPLANAR", Some("grad")),
    // MNE's magnetometers include axial gradiometers (CTF's sensors).
    ("MEGGRADAXIAL", Some("mag")),
    ("MEGREFMAG", Some("ref_meg")),
    ("MEGREFGRADAXIAL", Some("ref_meg")),
    ("MEGREFGRADPLANAR", Some("ref_meg")),
    ("MEGOTHER", None),
    ("EOG", Some("eog")),
    ("HEOG", Some("heog")),
    ("VEOG", Some("veog")),
    ("ECG", Some("ecg")),
    ("EMG", Some("emg")),
    ("SEEG", Some("seeg")),
    ("ECOG", Some("ecog")),
    ("DBS", Some("dbs")),
    ("RESP", Some("resp")),
    ("PPG", Some("ppg")),
    ("GSR", Some("gsr")),
    ("TEMP", Some("temperature")),
    ("EYEGAZE", Some("eyegaze")),
    ("PUPIL", Some("pupil")),
    ("NIRSCWAMPLITUDE", Some("fnirs_cw_amplitude")),
    // MNE-BIDS reads it as BIDS' amplitudes.
    ("NIRS", Some("fnirs_cw_amplitude")),
    ("NIRSCWOPTICALDENSITY", Some("fnirs_od")),
    ("NIRSCWHBO", Some("hbo")),
    ("NIRSCWHBR", Some("hbr")),
    ("HLU", Some("chpi")),
    ("AUDIO", Some("sound")),
    ("PD", Some("light")),
    ("TRIG", Some("stim")),
];

/// BIDS types that stand for none of kymo's known types: read as a type of that name, and
/// a type of that name (in any case) is written as one.
const OTHER_BIDS_TYPES: [&str; 17] = [
    "REF",
    "SYSCLOCK",
    "ADC",
    "DAC",
    "FITERR",
    "OTHER",
    "NIRSCWFLUORESCENSEAMPLITUDE",
    "NIRSCWMUA",
    "ACCEL",
    "ANGACCEL",
    "GYRO",
    "JNTANG",
    "LATENCY",
    "MAGN",
    "ORNT",
    "POS",
    "VEL",
];

/// BIDS' channel type for `kind`: the one it is written as, a name of the file's own that
/// is another BIDS type ignoring case (as uppercase), else `MISC`.
pub fn bids_type(kind: &ChannelKind) -> String {
    if let Some((_, bids)) = WRITTEN.iter().find(|(key, _)| *key == kind.key()) {
        return (*bids).to_owned();
    }
    let upper = kind.name().to_uppercase();
    if kind.known().is_none() && OTHER_BIDS_TYPES.contains(&upper.as_str()) {
        return upper;
    }
    "MISC".to_owned()
}

/// kymo's type for a BIDS type, in any case: the known type it stands for, `None` for one
/// that leaves a channel's type as it is (MEGOTHER), else a type of that name as written
/// (`ACCEL`, or a name that isn't BIDS').
pub fn kind_of_bids(bids: &str) -> Option<ChannelKind> {
    let bids = bids.trim();
    match READ
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(bids))
    {
        Some((_, key)) => key.map(ChannelKind::named),
        None => Some(ChannelKind::named(bids)),
    }
}

/// Whether `kind` reads back from its BIDS type alone, shown as it is (a type of the file's
/// own named `accel` reads back as `ACCEL`, so it doesn't).
pub fn reads_back(kind: &ChannelKind) -> bool {
    kind_of_bids(&bids_type(kind)).is_some_and(|k| k == *kind && k.name() == kind.name())
}

/// One channel's row.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub kind: &'a ChannelKind,
    pub unit: &'a Unit,
    /// Its grid's rate, `None` for an irregular one.
    pub rate: Option<f64>,
    pub bad: bool,
}

/// The rows of `recording`'s channels, in its order, with the types `kinds` (by channel id)
/// and bad where `bad` says.
pub fn entries<'a>(
    recording: &'a Recording,
    kinds: &'a [ChannelKind],
    bad: impl Fn(ChannelId) -> bool,
) -> Vec<Entry<'a>> {
    recording
        .channels()
        .iter()
        .enumerate()
        .map(|(i, channel)| {
            let id = ChannelId(i as u32);
            let grid = recording.grid(channel.grid);
            Entry {
                name: &channel.name,
                kind: kinds.get(i).unwrap_or(&channel.kind),
                unit: &channel.unit,
                rate: grid.times().is_none().then(|| grid.rate()),
                bad: bad(id),
            }
        })
        .collect()
}

/// Writes `entries` as a `channels.tsv`.
pub fn write<'a>(
    out: impl Write,
    entries: impl IntoIterator<Item = Entry<'a>>,
) -> std::io::Result<()> {
    let entries: Vec<Entry<'a>> = entries.into_iter().collect();
    let with_kymo = entries.iter().any(|e| !reads_back(e.kind));
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .terminator(csv::Terminator::Any(b'\n'))
        // BIDS' tables have no quoting.
        .quote_style(csv::QuoteStyle::Never)
        .from_writer(out);
    let mut header = vec!["name", "type", "units", "sampling_frequency", "status"];
    if with_kymo {
        header.push("kymo_type");
    }
    writer.write_record(&header).map_err(io_error)?;
    for e in &entries {
        let units = e.unit.symbol();
        let mut record = vec![
            e.name.to_owned(),
            bids_type(e.kind),
            if units.is_empty() {
                "n/a".into()
            } else {
                units
            },
            e.rate.map_or_else(|| "n/a".into(), |r| r.to_string()),
            if e.bad { "bad" } else { "good" }.to_owned(),
        ];
        if with_kymo {
            let own = !reads_back(e.kind);
            record.push(if own { e.kind.name() } else { "n/a" }.to_owned());
        }
        writer.write_record(&record).map_err(io_error)?;
    }
    writer.flush()
}

/// What a file's names mean: taken from the recording it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct Context {
    /// Channel names, by id.
    pub names: Vec<String>,
    /// Channel units, by id (empty without one).
    pub units: Vec<String>,
}

impl Context {
    pub fn of(recording: &Recording) -> Context {
        Context {
            names: recording
                .channels()
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            units: recording
                .channels()
                .iter()
                .map(|c| c.unit.label.clone())
                .collect(),
        }
    }
}

/// Types and bad channels read from a file, and what was left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    /// Each listed channel's type, in the file's order.
    pub kinds: Vec<(ChannelId, ChannelKind)>,
    /// Channels the file marks good (`false`) or bad (`true`).
    pub bad: Vec<(ChannelId, bool)>,
    /// Rows naming a channel the recording doesn't have (or one more time than it has it).
    pub unknown: usize,
    /// Channels whose unit differs from the file's (see the module's notes).
    pub units_differ: usize,
}

/// Reads a `channels.tsv` for the recording `context` describes.
pub fn read(input: impl Read, context: &Context) -> Result<Loaded, FileError> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .flexible(false)
        // BIDS' tables have no quoting.
        .quoting(false)
        .from_reader(input);
    let header = reader.headers().map_err(csv_error)?.clone();
    let column = |name: &str| header.iter().position(|h| h.trim() == name);
    let missing = |name: &str| FileError::Header(format!("no column \"{name}\""));
    let name_column = column("name").ok_or_else(|| missing("name"))?;
    let type_column = column("type").ok_or_else(|| missing("type"))?;
    let units_column = column("units");
    let status_column = column("status");
    let kymo_column = column("kymo_type");

    // Each name's channels, and how many rows of it came so far.
    let mut by_name: HashMap<&str, (Vec<ChannelId>, usize)> = HashMap::new();
    for (i, name) in context.names.iter().enumerate() {
        by_name
            .entry(name.as_str())
            .or_default()
            .0
            .push(ChannelId(i as u32));
    }
    let mut loaded = Loaded::default();
    for record in reader.records() {
        let record = record.map_err(csv_error)?;
        let field = |i: usize| record.get(i).unwrap_or("");
        let channel = by_name.get_mut(field(name_column)).and_then(|(ids, seen)| {
            let id = ids.get(*seen).copied();
            *seen += 1;
            id
        });
        let Some(channel) = channel else {
            loaded.unknown += 1;
            continue;
        };
        let own = kymo_column
            .map(|i| field(i).trim())
            .filter(|t| !t.is_empty() && *t != "n/a");
        let kind = match own {
            Some(name) => Some(ChannelKind::named(name)),
            None => match field(type_column).trim() {
                "" | "n/a" => None,
                bids => kind_of_bids(bids),
            },
        };
        if let Some(kind) = kind {
            loaded.kinds.push((channel, kind));
        }
        status(&record, status_column, channel, &mut loaded);
        // `n/a` is a unit not known, not compared (MNE-BIDS writes it for stim channels).
        let units = units_column
            .map(|i| field(i).trim())
            .filter(|t| !t.is_empty() && *t != "n/a");
        if let Some(text) = units
            && let Some(ours) = context.units.get(channel.0 as usize)
            && !same_unit(ours, text)
        {
            loaded.units_differ += 1;
        }
    }
    Ok(loaded)
}

/// Whether the units labeled `a` and `b` measure the same: of one quantity, whatever their
/// prefixes (MNE-BIDS writes V for a file's µV), or the same as symbols.
fn same_unit(a: &str, b: &str) -> bool {
    let unit = |label: &str| Unit {
        label: label.to_owned(),
        to_si: None,
    };
    let (a, b) = (unit(a), unit(b));
    match (a.quantity(), b.quantity()) {
        (Some(x), Some(y)) => x == y,
        _ => a.symbol() == b.symbol(),
    }
}

/// Takes a row's status, where it is good or bad.
fn status(
    record: &csv::StringRecord,
    column: Option<usize>,
    channel: ChannelId,
    loaded: &mut Loaded,
) {
    let Some(text) = column.and_then(|i| record.get(i)) else {
        return;
    };
    let text = text.trim();
    if text.eq_ignore_ascii_case("bad") {
        loaded.bad.push((channel, true));
    } else if text.eq_ignore_ascii_case("good") {
        loaded.bad.push((channel, false));
    }
}
