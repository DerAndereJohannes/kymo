//! Channel kinds and units: MNE's type names, unit labels and what they measure, and the
//! kinds readers give from labels and units.

use kymo_core::{ChannelKind, Quantity, Recording, Unit};
use kymo_testgen::{Spec, TempFile};

#[test]
fn mne_type_names_map_to_kinds_and_back() {
    use ChannelKind::*;
    let named = [
        ("eeg", Eeg),
        ("eog", Eog),
        ("ecg", Ecg),
        ("emg", Emg),
        ("stim", Stim),
        ("misc", Misc),
        ("mag", Mag),
        ("grad", Grad),
        ("ref_meg", RefMeg),
        ("seeg", Seeg),
        ("ecog", Ecog),
        ("dbs", Dbs),
        ("resp", Resp),
        ("bio", Bio),
    ];
    assert_eq!(named.len(), ChannelKind::ALL.len());
    for (name, kind) in named {
        assert_eq!(kind.mne_name(), name);
        assert_eq!(ChannelKind::from_mne(name), Some(kind), "{name}");
        // As EEGLAB and XDF write them.
        assert_eq!(
            ChannelKind::from_mne(&name.to_uppercase()),
            Some(kind),
            "{name}"
        );
    }
    assert_eq!(ChannelKind::from_mne(" EEG "), Some(Eeg));
    // MNE's other types have no kind of their own.
    for other in [
        "chpi",
        "exci",
        "ias",
        "syst",
        "hbo",
        "hbr",
        "fnirs_cw_amplitude",
        "fnirs_od",
        "csd",
        "gsr",
        "temperature",
        "eyegaze",
        "pupil",
        "dipole",
        "gof",
    ] {
        assert_eq!(ChannelKind::from_mne(other), Some(Misc), "{other}");
    }
    for unknown in ["", "meg", "MEG", "Accelerometer", "electrode", "eeg2"] {
        assert_eq!(ChannelKind::from_mne(unknown), None, "{unknown:?}");
    }
}

#[test]
fn unit_labels_give_factors_and_quantities() {
    use Quantity::*;
    let cases: [(&str, Option<(f64, Quantity)>); 36] = [
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
        // Not units kymo scales.
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
fn edf_labels_and_units_give_kinds() {
    let file = TempFile::written(&Spec::preset("edf+c-kinds").unwrap());
    let (rec, _) = Recording::new(kymo_core::open(file.path()).unwrap()).unwrap();
    use ChannelKind::*;
    let kinds: Vec<(&str, ChannelKind)> = rec
        .channels()
        .iter()
        .map(|c| (c.name.as_str(), c.kind))
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
            ("SaO2 finger", Bio),
            ("MEG 0111", Mag),
            ("MEG 0112", Grad),
            ("TRIGGER", Stim),
        ]
    );
    let grad = &rec.channels()[10];
    assert_eq!(grad.unit.quantity(), Some(Quantity::FieldGradient));
    assert_eq!(grad.unit.to_si, Some(1e-13));
}
