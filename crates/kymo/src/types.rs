//! Channel types' settings: what each type starts with, and what was set for it, by name.
//!
//! A type is a name ([`ChannelKind`]); kymo knows some names ([`kymo_core::KNOWN_KINDS`])
//! and gives them starting values, and any other name starts as misc does. What is set for a
//! type (its color, whether DC removal centers it, whether each of its channels is scaled on
//! its own) is kept under its name and applies wherever the name appears.

use std::collections::{BTreeMap, HashMap};

use eframe::egui::Color32;
use kymo_core::{ChannelKind, KNOWN_KINDS, Quantity};

/// What a type starts with, before anything is set for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    /// Amplitude per channel spacing in SI units, as MNE's `scalings` for `raw.plot`, in
    /// [`quantity`](Self::quantity).
    pub amplitude: f64,
    /// The quantity the amplitude is in (volts for EEG); `None` for types of any quantity
    /// (misc, respiration), whose channels start at the amplitude in whatever unit.
    pub quantity: Option<Quantity>,
    /// Whether DC removal centers its channels.
    pub remove_dc: bool,
    /// Whether each of its channels is scaled on its own.
    pub own_scale: bool,
}

/// What `kind` starts with. Brain signals, EOG, ECG and EMG are centered and share an
/// amplitude per quantity, which is compared across channels (a flat or bad channel must
/// not be blown up, an artifact must not shrink its channel); respiration is centered and
/// scaled per channel. Other biological signals, MNE's other types, misc and types kymo
/// doesn't know keep their level (temperature, oxygen saturation, eye position) and are
/// scaled per channel, as their channels measure different things. Stim channels are never
/// centered nor scaled per channel.
pub fn start(kind: &ChannelKind) -> Start {
    use Quantity::{FieldGradient, MagneticField, Voltage};
    let shared = |amplitude, quantity| Start {
        amplitude,
        quantity: Some(quantity),
        remove_dc: true,
        own_scale: false,
    };
    let level = |amplitude, quantity| Start {
        amplitude,
        quantity,
        remove_dc: false,
        own_scale: true,
    };
    match kind.key() {
        "eeg" => shared(20e-6, Voltage),
        "eog" | "heog" | "veog" => shared(150e-6, Voltage),
        "ecg" => shared(5e-4, Voltage),
        "emg" => shared(1e-3, Voltage),
        "mag" | "ref_meg" => shared(1e-12, MagneticField),
        "grad" => shared(4e-11, FieldGradient),
        "seeg" | "ecog" | "dbs" => shared(1e-4, Voltage),
        "resp" => Start {
            remove_dc: true,
            ..level(1.0, None)
        },
        "bio" | "sao2" | "ppg" => level(1e-6, Some(Voltage)),
        "stim" => Start {
            amplitude: 1.0,
            quantity: None,
            remove_dc: false,
            own_scale: false,
        },
        "chpi" => level(1e-4, None),
        "hbo" | "hbr" => level(1e-5, None),
        "fnirs_cw_amplitude" | "fnirs_fd_ac_amplitude" | "fnirs_od" | "csd" => level(0.02, None),
        "fnirs_fd_phase" | "eyegaze" => level(0.2, None),
        "fnirs_td_moments_mean" => level(1e-10, None),
        "fnirs_td_moments_variance" => level(1e-20, None),
        "dipole" => level(1e-7, None),
        "gof" => level(100.0, None),
        "temperature" => level(0.1, None),
        "pupil" => level(0.01, None),
        "whitened" => level(10.0, None),
        _ => level(1.0, None),
    }
}

/// Where `kind`'s channels come within a stream: the known types in their order (EEG, MEG,
/// EOG, ECG, then the others), then the types kymo doesn't know (among themselves in the
/// order `first_seen` gives, their first channel's place in the file), then misc and stim.
pub fn rank(kind: &ChannelKind, first_seen: &HashMap<ChannelKind, usize>) -> (usize, usize) {
    let known = KNOWN_KINDS.len();
    match KNOWN_KINDS.iter().position(|k| k.key == kind.key()) {
        Some(_) if *kind == ChannelKind::MISC => (known + 1, 0),
        Some(_) if kind.is_stim() => (known + 2, 0),
        Some(i) => (i, 0),
        None => (known, first_seen.get(kind).copied().unwrap_or(usize::MAX)),
    }
}

/// What was set for one type; `None` keeps the type's start ([`start`]) or default color.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeSet {
    /// The type's name as first set (how a type kymo doesn't know is shown).
    pub name: String,
    pub color: Option<Color32>,
    pub remove_dc: Option<bool>,
    pub own_scale: Option<bool>,
}

/// What was set per type, by the type's key. A known type's entry holds only what differs
/// from its start and goes once nothing does; a type kymo doesn't know keeps its entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeSettings {
    set: BTreeMap<String, TypeSet>,
}

impl TypeSettings {
    /// Whether DC removal centers `kind`'s channels (when it is on): as set, or as it
    /// starts; never stim channels.
    pub fn remove_dc(&self, kind: &ChannelKind) -> bool {
        !kind.is_stim()
            && self
                .get(kind)
                .and_then(|s| s.remove_dc)
                .unwrap_or_else(|| start(kind).remove_dc)
    }

    /// Whether each of `kind`'s channels is scaled on its own: as set, or as it starts;
    /// never stim channels.
    pub fn own_scale(&self, kind: &ChannelKind) -> bool {
        !kind.is_stim()
            && self
                .get(kind)
                .and_then(|s| s.own_scale)
                .unwrap_or_else(|| start(kind).own_scale)
    }

    /// The color picked for `kind`, if one was.
    pub fn color(&self, kind: &ChannelKind) -> Option<Color32> {
        self.get(kind).and_then(|s| s.color)
    }

    pub fn set_remove_dc(&mut self, kind: &ChannelKind, on: bool) {
        let differs = on != start(kind).remove_dc;
        self.update(kind, |s| s.remove_dc = differs.then_some(on));
    }

    pub fn set_own_scale(&mut self, kind: &ChannelKind, on: bool) {
        let differs = on != start(kind).own_scale;
        self.update(kind, |s| s.own_scale = differs.then_some(on));
    }

    /// Picks a color for `kind`, or with `None` returns it to its default.
    pub fn set_color(&mut self, kind: &ChannelKind, color: Option<Color32>) {
        self.update(kind, |s| s.color = color);
    }

    /// Whether anything set for `kind` differs from its start (its color, DC removal or
    /// scaling).
    pub fn changed(&self, kind: &ChannelKind) -> bool {
        self.get(kind)
            .is_some_and(|s| s.color.is_some() || s.remove_dc.is_some() || s.own_scale.is_some())
    }

    /// Whether `kind`'s DC removal or scaling differs from its start (not its color, which
    /// its swatch shows).
    pub fn options_changed(&self, kind: &ChannelKind) -> bool {
        self.get(kind)
            .is_some_and(|s| s.remove_dc.is_some() || s.own_scale.is_some())
    }

    /// Whether `kind` has an entry: a known type that something differs for, or a type kymo
    /// doesn't know that something was ever set for (until it is forgotten).
    pub fn stored(&self, kind: &ChannelKind) -> bool {
        self.get(kind).is_some()
    }

    /// Returns `kind` to its start and default color. A type kymo doesn't know keeps its
    /// entry (see [`forget`](Self::forget)).
    pub fn reset(&mut self, kind: &ChannelKind) {
        self.update(kind, |s| {
            s.color = None;
            s.remove_dc = None;
            s.own_scale = None;
        });
    }

    /// Removes `kind`'s entry, which returns it to its start.
    pub fn forget(&mut self, kind: &ChannelKind) {
        self.set.remove(kind.key());
    }

    /// The types something was set for, by key, with what was set.
    pub fn entries(&self) -> impl Iterator<Item = &TypeSet> {
        self.set.values()
    }

    /// Keeps `set` for its type (by its name), as it is.
    pub fn insert(&mut self, set: TypeSet) {
        let key = ChannelKind::named(&set.name).key().to_owned();
        self.set.insert(key, set);
    }

    fn get(&self, kind: &ChannelKind) -> Option<&TypeSet> {
        self.set.get(kind.key())
    }

    fn update(&mut self, kind: &ChannelKind, change: impl FnOnce(&mut TypeSet)) {
        let set = self
            .set
            .entry(kind.key().to_owned())
            .or_insert_with(|| TypeSet {
                name: kind.name().to_owned(),
                ..TypeSet::default()
            });
        change(set);
        let empty = set.color.is_none() && set.remove_dc.is_none() && set.own_scale.is_none();
        if empty && kind.known().is_some() {
            self.set.remove(kind.key());
        }
    }
}
