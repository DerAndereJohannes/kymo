//! Channel kinds and units: MNE's type names, unit labels and what they measure, and the
//! kinds readers give from labels and units.

use kymo_core::{ChannelKind, Quantity, Recording, Unit};
use kymo_testgen::{Spec, TempFile};

#[test]
fn type_names_are_known_by_any_spelling_and_case() {
    use crate::kinds::*;
    // MNE's names of the types kymo knows, and their names as shown.
    let named = [
        ("eeg", Eeg, "EEG"),
        ("eog", Eog, "EOG"),
        ("ecg", Ecg, "ECG"),
        ("emg", Emg, "EMG"),
        ("stim", Stim, "Stim"),
        ("misc", Misc, "Misc"),
        ("mag", Mag, "MEG Mag"),
        ("grad", Grad, "MEG Grad"),
        ("ref_meg", RefMeg, "MEG Ref"),
        ("seeg", Seeg, "sEEG"),
        ("ecog", Ecog, "ECoG"),
        ("dbs", Dbs, "DBS"),
        ("resp", Resp, "Resp"),
        ("bio", Bio, "Bio"),
    ];
    for (mne, kind, shown) in named {
        assert_eq!(kind.mne_name(), mne);
        assert_eq!(kind.key(), mne);
        assert_eq!(kind.name(), shown);
        let parsed = ChannelKind::named(mne);
        assert_eq!(parsed, kind, "{mne}");
        assert_eq!(parsed.name(), shown, "{mne}");
        // As EEGLAB and XDF write them, and as kymo shows them.
        assert_eq!(ChannelKind::named(&mne.to_uppercase()), kind, "{mne}");
        assert_eq!(ChannelKind::named(shown), kind, "{mne}");
    }
    assert_eq!(ChannelKind::named(" EEG "), Eeg);
    // MNE's other types are types of their own, shown as kymo spells them.
    for (mne, shown) in [
        ("chpi", "cHPI"),
        ("exci", "EXCI"),
        ("ias", "IAS"),
        ("syst", "SYST"),
        ("hbo", "HbO"),
        ("hbr", "HbR"),
        ("fnirs_cw_amplitude", "fNIRS CW Amplitude"),
        ("fnirs_od", "fNIRS OD"),
        ("csd", "CSD"),
        ("gsr", "GSR"),
        ("temperature", "Temperature"),
        ("eyegaze", "Eye Gaze"),
        ("pupil", "Pupil"),
        ("dipole", "Dipole"),
        ("gof", "GOF"),
    ] {
        let kind = ChannelKind::named(mne);
        assert_eq!(
            (kind.name(), kind.key(), kind.mne_name()),
            (shown, mne, mne)
        );
        assert!(kind.known().is_some(), "{mne}");
        assert_ne!(kind, Misc, "{mne}");
    }
    // Other spellings: BIDS', EDF+'s and others.
    for (other, kind) in [
        ("MEGMAG", Mag),
        ("MEGGRADPLANAR", Grad),
        ("MEGREFMAG", RefMeg),
        ("EKG", Ecg),
        ("TEMP", ChannelKind::named("temperature")),
    ] {
        assert_eq!(ChannelKind::named(other), kind, "{other}");
        assert_eq!(ChannelKind::named(other).name(), kind.name(), "{other}");
    }
    // Types of their own that MNE reads as one of its types.
    for (name, mne) in [
        ("HEOG", "eog"),
        ("VEOG", "eog"),
        ("SaO2", "bio"),
        ("PPG", "bio"),
    ] {
        let kind = ChannelKind::named(name);
        assert_eq!((kind.name(), kind.mne_name()), (name, mne));
        assert_ne!(kind, ChannelKind::named(mne), "{name}");
    }
    // Any other name is a type of its own, shown as first given, one type in any case;
    // MNE reads it as misc.
    for name in [
        "Gaze",
        "Accelerometer",
        "electrode",
        "eeg2",
        "MEG",
        "Ölfühler",
    ] {
        let kind = ChannelKind::named(&format!(" {name} "));
        assert_eq!(kind.name(), name);
        assert_eq!(kind.mne_name(), "misc");
        assert!(kind.known().is_none(), "{name}");
        assert_eq!(kind, ChannelKind::named(&name.to_uppercase()), "{name}");
        assert_eq!(kind, ChannelKind::named(&name.to_lowercase()), "{name}");
        assert_eq!(
            ChannelKind::named(&name.to_lowercase()).name(),
            name.to_lowercase()
        );
        assert_ne!(kind, Misc, "{name}");
    }
    // No name is misc.
    assert_eq!(ChannelKind::named(""), Misc);
    assert_eq!(ChannelKind::named("  "), Misc);
}

#[test]
fn every_known_type_has_one_key_and_spelling() {
    let mut types = std::collections::HashMap::new();
    for kind in kymo_core::KNOWN_KINDS {
        assert_eq!(kind.key, kind.key.to_lowercase(), "{}", kind.name);
        let spellings = [kind.name.to_lowercase(), kind.key.to_owned()]
            .into_iter()
            .chain(kind.aliases.iter().map(|a| (*a).to_owned()));
        for spelling in spellings {
            assert_eq!(spelling, spelling.to_lowercase(), "{}", kind.name);
            let before = types.insert(spelling.clone(), kind.key);
            assert!(
                before.is_none_or(|k| k == kind.key),
                "{spelling:?} names two types"
            );
            assert_eq!(ChannelKind::named(&spelling).key(), kind.key, "{spelling}");
        }
    }
}

#[test]
fn unit_labels_give_factors_and_quantities() {
    use Quantity::*;
    let cases: [(&str, Option<(f64, Quantity)>); 41] = [
        ("V", Some((1.0, Voltage))),
        ("mV", Some((1e-3, Voltage))),
        ("\u{b5}V", Some((1e-6, Voltage))),
        ("\u{3bc}V", Some((1e-6, Voltage))),
        ("uV", Some((1e-6, Voltage))),
        ("UV", Some((1e-6, Voltage))),
        ("uv", Some((1e-6, Voltage))),
        (" nV ", Some((1e-9, Voltage))),
        ("S", Some((1.0, Conductance))),
        ("\u{b5}S", Some((1e-6, Conductance))),
        ("T", Some((1.0, MagneticField))),
        ("pT", Some((1e-12, MagneticField))),
        ("fT", Some((1e-15, MagneticField))),
        ("\u{3bc}T", Some((1e-6, MagneticField))),
        ("T/m", Some((1.0, FieldGradient))),
        ("fT/cm", Some((1e-13, FieldGradient))),
        ("pT/m", Some((1e-12, FieldGradient))),
        ("fT/mm", Some((1e-12, FieldGradient))),
        // As words, as XDF streams write them.
        ("microvolts", Some((1e-6, Voltage))),
        ("Microvolt", Some((1e-6, Voltage))),
        ("millivolts", Some((1e-3, Voltage))),
        ("nanovolt", Some((1e-9, Voltage))),
        ("volts", Some((1.0, Voltage))),
        ("femtotesla", Some((1e-15, MagneticField))),
        ("picoteslas", Some((1e-12, MagneticField))),
        ("siemens", Some((1.0, Conductance))),
        ("microsiemens", Some((1e-6, Conductance))),
        ("femtotesla per centimeter", Some((1e-13, FieldGradient))),
        ("Picoteslas per Meter", Some((1e-12, FieldGradient))),
        // Not units kymo scales.
        ("nanoseconds", None),
        ("meters per second", None),
        ("volts per meter", None),
        ("kilovolts", None),
        ("fV", None),
        ("pS", None),
        ("V/m", None),
        ("T/s", None),
        ("%", None),
        ("AU", None),
        ("", None),
        ("kV", None),
    ];
    for (label, expected) in cases {
        let unit = Unit::from_label(label);
        assert_eq!(unit.label, label);
        let got = unit.to_si.zip(unit.quantity());
        match (got, expected) {
            (Some((f, q)), Some((ef, eq))) => {
                assert!((f - ef).abs() <= 1e-12 * ef, "{label}: {f} vs {ef}");
                assert_eq!(q, eq, "{label}");
            }
            (None, None) => assert_eq!(unit.to_si, None, "{label}"),
            _ => panic!("{label}: {got:?}, expected {expected:?}"),
        }
    }
}

#[test]
fn units_show_as_symbols() {
    let cases = [
        ("microvolts", "\u{b5}V"),
        ("Microvolt", "\u{b5}V"),
        ("millivolts", "mV"),
        ("nanovolts", "nV"),
        ("volts", "V"),
        ("femtotesla", "fT"),
        ("picoteslas", "pT"),
        ("Tesla", "T"),
        ("siemens", "S"),
        ("microsiemens", "\u{b5}S"),
        ("uV", "\u{b5}V"),
        ("UV", "\u{b5}V"),
        ("uv", "\u{b5}V"),
        ("\u{3bc}V", "\u{b5}V"),
        ("\u{b5}V", "\u{b5}V"),
        ("uS", "\u{b5}S"),
        ("uT/cm", "\u{b5}T/cm"),
        (" mV ", "mV"),
        ("fT/cm", "fT/cm"),
        // Units kymo doesn't scale, as words and with "u".
        ("nanoseconds", "ns"),
        ("Milliseconds", "ms"),
        ("second", "s"),
        ("kilovolts", "kV"),
        ("kilohertz", "kHz"),
        ("hertz", "Hz"),
        ("millimeters", "mm"),
        ("centimetres", "cm"),
        ("microamperes", "\u{b5}A"),
        ("kiloohms", "k\u{3a9}"),
        ("decibels", "dB"),
        ("degrees", "\u{b0}"),
        ("Degrees Celsius", "\u{b0}C"),
        ("percent", "%"),
        ("meters per second", "m/s"),
        ("femtotesla per centimeter", "fT/cm"),
        ("uA", "\u{b5}A"),
        ("us", "\u{b5}s"),
        ("\u{3bc}m", "\u{b5}m"),
        // Unknown labels stay as they are, "u" and all.
        ("normalized_signed", "normalized_signed"),
        ("units", "units"),
        ("ux", "ux"),
        ("microns", "microns"),
        ("meters per", "meters per"),
        ("per second", "per second"),
        (" AU", "AU"),
        ("", ""),
    ];
    for (label, symbol) in cases {
        assert_eq!(Unit::from_label(label).symbol(), symbol, "{label:?}");
    }
}

#[test]
fn edf_labels_and_units_give_kinds() {
    let file = TempFile::written(&Spec::preset("edf+c-kinds").unwrap());
    let (rec, _) = Recording::new(kymo_core::open(file.path()).unwrap()).unwrap();
    use crate::kinds::*;
    let kinds: Vec<(&str, ChannelKind)> = rec
        .channels()
        .iter()
        .map(|c| (c.name.as_str(), c.kind.clone()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("EEG Fz", Eeg),
            ("SEEG LA1", Seeg),
            ("ECOG G1", Ecog),
            ("DBS L1", Dbs),
            ("EOG L", Eog),
            ("ECG", Ecg),
            ("EMG chin", Emg),
            ("Resp chest", Resp),
            ("SaO2 finger", ChannelKind::named("SaO2")),
            ("MEG 0111", Mag),
            ("MEG 0112", Grad),
            ("TRIGGER", Stim),
        ]
    );
    let grad = &rec.channels()[10];
    assert_eq!(grad.unit.quantity(), Some(Quantity::FieldGradient));
    assert_eq!(grad.unit.to_si, Some(1e-13));
}
