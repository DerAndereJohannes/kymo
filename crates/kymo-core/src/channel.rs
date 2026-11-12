//! Per-channel metadata as stored in the file.

use crate::GridId;

/// Index of a channel in [`Recording::channels`](crate::Recording::channels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(pub u32);

/// Channel type: MNE's types that it scales or draws differently. MNE's other types
/// (chpi, fNIRS, temperature, ...) are `Misc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChannelKind {
    Eeg,
    Eog,
    Ecg,
    Emg,
    Stim,
    Misc,
    /// MEG magnetometer.
    Mag,
    /// MEG planar gradiometer.
    Grad,
    /// MEG reference sensor (magnetometer or gradiometer).
    RefMeg,
    /// Stereo-EEG (depth electrodes).
    Seeg,
    /// Electrocorticography.
    Ecog,
    /// Deep brain stimulation electrodes.
    Dbs,
    /// Respiration.
    Resp,
    /// Other biological signals (MNE's "bio": oximetry, plethysmography, ...).
    Bio,
}

/// MNE's channel types that are `Misc` in kymo.
const MNE_MISC: [&str; 22] = [
    "chpi",
    "csd",
    "dipole",
    "exci",
    "eyegaze",
    "fnirs_cw_amplitude",
    "fnirs_fd_ac_amplitude",
    "fnirs_fd_phase",
    "fnirs_od",
    "fnirs_td_gated_amplitude",
    "fnirs_td_moments_intensity",
    "fnirs_td_moments_mean",
    "fnirs_td_moments_variance",
    "gof",
    "gsr",
    "hbo",
    "hbr",
    "ias",
    "pupil",
    "syst",
    "temperature",
    "whitened",
];

impl ChannelKind {
    pub const ALL: [ChannelKind; 14] = [
        ChannelKind::Eeg,
        ChannelKind::Eog,
        ChannelKind::Ecg,
        ChannelKind::Emg,
        ChannelKind::Stim,
        ChannelKind::Misc,
        ChannelKind::Mag,
        ChannelKind::Grad,
        ChannelKind::RefMeg,
        ChannelKind::Seeg,
        ChannelKind::Ecog,
        ChannelKind::Dbs,
        ChannelKind::Resp,
        ChannelKind::Bio,
    ];

    /// MNE's name of this type, e.g. `"ref_meg"`.
    pub fn mne_name(self) -> &'static str {
        match self {
            ChannelKind::Eeg => "eeg",
            ChannelKind::Eog => "eog",
            ChannelKind::Ecg => "ecg",
            ChannelKind::Emg => "emg",
            ChannelKind::Stim => "stim",
            ChannelKind::Misc => "misc",
            ChannelKind::Mag => "mag",
            ChannelKind::Grad => "grad",
            ChannelKind::RefMeg => "ref_meg",
            ChannelKind::Seeg => "seeg",
            ChannelKind::Ecog => "ecog",
            ChannelKind::Dbs => "dbs",
            ChannelKind::Resp => "resp",
            ChannelKind::Bio => "bio",
        }
    }

    /// The kind of an MNE channel type name (as EEGLAB's `chanlocs.type` and XDF's channel
    /// types use them), ignoring case and surrounding whitespace. MNE's types without a kind
    /// of their own are `Misc`; `None` if the name is not an MNE type.
    pub fn from_mne(name: &str) -> Option<ChannelKind> {
        let name = name.trim().to_ascii_lowercase();
        ChannelKind::ALL
            .into_iter()
            .find(|k| k.mne_name() == name)
            .or_else(|| {
                MNE_MISC
                    .contains(&name.as_str())
                    .then_some(ChannelKind::Misc)
            })
    }
}

/// Physical unit of a channel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// As stored in the file, e.g. `"uV"`.
    pub label: String,
    /// Factor to the SI unit, e.g. `1e-6` for `"uV"`; `None` if the label is not recognized.
    pub to_si: Option<f64>,
}

/// What a unit measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Quantity {
    /// V.
    Voltage,
    /// S.
    Conductance,
    /// T.
    MagneticField,
    /// T/m.
    FieldGradient,
}

impl Quantity {
    /// The SI unit's symbol.
    pub fn si_label(self) -> &'static str {
        match self {
            Quantity::Voltage => "V",
            Quantity::Conductance => "S",
            Quantity::MagneticField => "T",
            Quantity::FieldGradient => "T/m",
        }
    }
}

/// The factor to SI and the quantity of a unit label, if it is a known unit.
fn parse_unit(label: &str) -> Option<(f64, Quantity)> {
    let label = label.trim();
    // Seen in EDF files.
    if matches!(label, "UV" | "uv") {
        return Some((1e-6, Quantity::Voltage));
    }
    if let Some(unit) = unit_word(label) {
        return Some(unit);
    }
    let (numerator, per) = match label.split_once('/') {
        Some((n, "m")) => (n, 1.0),
        Some((n, "cm")) => (n, 1e-2),
        Some((n, "mm")) => (n, 1e-3),
        Some(_) => return None,
        None => (label, 0.0),
    };
    let base = numerator.chars().last()?;
    let prefix = &numerator[..numerator.len() - base.len_utf8()];
    let factor = match prefix {
        "" => 1.0,
        "m" => 1e-3,
        "\u{b5}" | "\u{3bc}" | "u" => 1e-6,
        "n" => 1e-9,
        "p" => 1e-12,
        "f" => 1e-15,
        _ => return None,
    };
    match (base, per) {
        ('V', 0.0) if prefix != "p" && prefix != "f" => Some((factor, Quantity::Voltage)),
        ('S', 0.0) if prefix != "p" && prefix != "f" => Some((factor, Quantity::Conductance)),
        ('T', 0.0) => Some((factor, Quantity::MagneticField)),
        ('T', per) if per > 0.0 => Some((factor / per, Quantity::FieldGradient)),
        _ => None,
    }
}

/// A unit written as a word, ignoring case ("microvolts", as XDF streams write them).
fn unit_word(label: &str) -> Option<(f64, Quantity)> {
    let word = label.to_ascii_lowercase();
    let word = word.strip_suffix('s').unwrap_or(&word);
    Some(match word {
        "volt" => (1.0, Quantity::Voltage),
        "millivolt" => (1e-3, Quantity::Voltage),
        "microvolt" => (1e-6, Quantity::Voltage),
        "nanovolt" => (1e-9, Quantity::Voltage),
        "tesla" => (1.0, Quantity::MagneticField),
        "picotesla" => (1e-12, Quantity::MagneticField),
        "femtotesla" => (1e-15, Quantity::MagneticField),
        // "siemens" without its "s".
        "siemen" => (1.0, Quantity::Conductance),
        "microsiemen" => (1e-6, Quantity::Conductance),
        _ => return None,
    })
}

impl Unit {
    /// The unit of `label`, with its SI factor if it is a known unit: volts and siemens
    /// (plain or with the prefix m, µ or n), teslas (also p and f) and teslas per meter,
    /// centimeter or millimeter ("fT/cm"). The micro prefix may be the micro sign (U+00B5),
    /// the Greek mu (U+03BC, which looks the same) or "u". Surrounding whitespace is ignored.
    /// Volts (with milli, micro, nano), teslas (pico, femto) and siemens (micro) may also be
    /// written as words, in any case and singular or plural ("microvolts").
    pub fn from_label(label: impl Into<String>) -> Unit {
        let label = label.into();
        let to_si = parse_unit(&label).map(|(factor, _)| factor);
        Unit { label, to_si }
    }

    /// What the unit measures, if its label is a known unit.
    pub fn quantity(&self) -> Option<Quantity> {
        parse_unit(&self.label).map(|(_, quantity)| quantity)
    }
}

/// File metadata of one channel. Display state (visible, color, scale, bad) lives elsewhere.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub kind: ChannelKind,
    pub unit: Unit,
    /// `physical = digital * gain + offset`, computed in f64. Gain may be negative.
    pub gain: f64,
    /// Can be huge for EDF/BDF.
    pub offset: f64,
    /// The grid this channel belongs to; the sampling rate comes from there.
    pub grid: GridId,
    /// Prefiltering as stored in the file.
    pub prefilter: String,
    pub transducer: String,
    /// Physical (min, max) as stored; informational. `None` if the format has none.
    pub physical_range: Option<(f64, f64)>,
    /// Digital (min, max) as stored; `None` for float formats.
    pub digital_range: Option<(f64, f64)>,
    /// For stim channels: the bits of the raw value that carry event codes, e.g. the low 16
    /// of a BDF status channel, whose upper bits are the amplifier's own status. `None`
    /// means all bits.
    pub trigger_mask: Option<u32>,
    /// For stim channels: whether its value changes are events (stim annotations); the
    /// pyramid build records changes only for those. Off for stim channels a combined one
    /// covers (FIFF's STI001 to STI016 beside STI101), which are still drawn as codes.
    pub events: bool,
}

impl Channel {
    /// Gain and offset that turn the values in this channel's tiles into the values shown:
    /// [`gain`](Self::gain) and [`offset`](Self::offset), except for stim channels, whose
    /// tiles hold codes, shown as they are (1 and 0), as MNE shows stim channels. Those are
    /// the digital values, masked for a channel with a [`trigger_mask`](Self::trigger_mask).
    pub fn tile_scaling(&self) -> (f64, f64) {
        if self.kind == ChannelKind::Stim {
            (1.0, 0.0)
        } else {
            (self.gain, self.offset)
        }
    }

    /// The unit of the values shown from this channel's tiles: [`unit`](Self::unit)'s
    /// label, or none for stim channels, whose tiles hold codes (BioSemi files call the unit
    /// of their Status channel "Boolean").
    pub fn tile_unit(&self) -> &str {
        if self.kind == ChannelKind::Stim {
            ""
        } else {
            &self.unit.label
        }
    }

    /// A channel of kind `Misc` with unit gain, zero offset and empty metadata.
    pub fn new(name: impl Into<String>, grid: GridId) -> Self {
        Channel {
            name: name.into(),
            kind: ChannelKind::Misc,
            unit: Unit::default(),
            gain: 1.0,
            offset: 0.0,
            grid,
            prefilter: String::new(),
            transducer: String::new(),
            physical_range: None,
            digital_range: None,
            trigger_mask: None,
            events: true,
        }
    }
}

/// The kind of a channel with a voltage unit, from its name (BrainVision's rule, also for
/// EEGLAB channels without an MNE type): EOG for names starting with
/// EOG, HEOG or VEOG, ECG for ECG or EKG, EMG for EMG; EEG otherwise.
pub(crate) fn kind_from_name(name: &str) -> ChannelKind {
    let upper = name.trim().to_ascii_uppercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| upper.starts_with(p));
    if starts(&["EOG", "HEOG", "VEOG"]) {
        ChannelKind::Eog
    } else if starts(&["ECG", "EKG"]) {
        ChannelKind::Ecg
    } else if starts(&["EMG"]) {
        ChannelKind::Emg
    } else {
        ChannelKind::Eeg
    }
}
