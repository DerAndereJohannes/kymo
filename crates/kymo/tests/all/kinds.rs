//! The channel types tests name most, under short names, as `use crate::kinds::*` gives
//! them (they were `ChannelKind`'s variants before types became names).
#![allow(non_upper_case_globals)]

use kymo_core::ChannelKind;

pub const Eeg: ChannelKind = ChannelKind::EEG;
pub const Eog: ChannelKind = ChannelKind::EOG;
pub const Ecg: ChannelKind = ChannelKind::ECG;
pub const Emg: ChannelKind = ChannelKind::EMG;
pub const Stim: ChannelKind = ChannelKind::STIM;
pub const Misc: ChannelKind = ChannelKind::MISC;
pub const Mag: ChannelKind = ChannelKind::MAG;
pub const Grad: ChannelKind = ChannelKind::GRAD;
pub const RefMeg: ChannelKind = ChannelKind::REF_MEG;
pub const Seeg: ChannelKind = ChannelKind::SEEG;
pub const Ecog: ChannelKind = ChannelKind::ECOG;
pub const Dbs: ChannelKind = ChannelKind::DBS;
pub const Resp: ChannelKind = ChannelKind::RESP;
pub const Bio: ChannelKind = ChannelKind::BIO;

/// The types kymo had before types were names, as Settings' channel types table lists them.
pub const KINDS: [ChannelKind; 14] = [
    Eeg, Mag, Grad, RefMeg, Eog, Ecg, Emg, Seeg, Ecog, Dbs, Resp, Bio, Misc, Stim,
];

/// The index of `kind` in [`KINDS`].
pub fn kind_index(kind: ChannelKind) -> usize {
    KINDS.iter().position(|k| *k == kind).expect("one of KINDS")
}
