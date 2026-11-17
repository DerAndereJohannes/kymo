//! BIDS `channels.tsv`: kymo's own round-trips (types, bad channels), MNE-BIDS' columns and
//! spellings read as MNE-BIDS reads them, names find channels as agreed, and broken files
//! give errors, never panics.

use kymo_core::annotation_file::FileError;
use kymo_core::channel_file::{Context, Entry, Loaded, bids_type, read, reads_back, write};
use kymo_core::{ChannelId, ChannelKind, KNOWN_KINDS, Unit};
use proptest::prelude::*;

fn unit(label: &str) -> Unit {
    Unit {
        label: label.into(),
        to_si: None,
    }
}

/// The file of channels named `names` with types `kinds`, units `units` and bad where
/// `bad` says.
fn file(names: &[&str], kinds: &[ChannelKind], units: &[Unit], bad: &[bool]) -> String {
    let entries = (0..names.len()).map(|i| Entry {
        name: names[i],
        kind: &kinds[i],
        unit: &units[i],
        rate: (i % 3 != 2).then_some(256.0),
        bad: bad[i],
    });
    let mut out = Vec::new();
    write(&mut out, entries).unwrap();
    String::from_utf8(out).unwrap()
}

fn context(names: &[&str], units: &[&str]) -> Context {
    Context {
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        units: units.iter().map(|u| (*u).to_owned()).collect(),
    }
}

fn load(text: &str, context: &Context) -> Result<Loaded, FileError> {
    read(text.as_bytes(), context)
}

fn ids(pairs: &[(u32, &str)]) -> Vec<(ChannelId, ChannelKind)> {
    pairs
        .iter()
        .map(|&(c, kind)| (ChannelId(c), ChannelKind::named(kind)))
        .collect()
}

/// Every type kymo knows and a few of the files' own: names of other BIDS types in any
/// case, names that are BIDS types kymo reads otherwise, and names BIDS doesn't have.
fn any_kind() -> impl Strategy<Value = ChannelKind> {
    let mut names: Vec<String> = KNOWN_KINDS.iter().map(|k| k.name.to_owned()).collect();
    names.extend(
        [
            "PositionX",
            "accel",
            "Vel",
            "AUDIO",
            "TRIG",
            "nirs",
            "MEGOTHER",
            "PD",
            "EEG 2",
        ]
        .map(String::from),
    );
    proptest::sample::select(names).prop_map(|n| ChannelKind::named(&n))
}

/// Names of characters a TSV keeps as they are, duplicates likely.
fn any_name() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('F'),
            Just('z'),
            Just(' '),
            Just('"'),
            Just('ü'),
            Just('-')
        ],
        1..4,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #[test]
    fn a_file_round_trips(
        channels in proptest::collection::vec((any_name(), any_kind(), any::<bool>()), 1..12),
    ) {
        let names: Vec<&str> = channels.iter().map(|(n, _, _)| n.as_str()).collect();
        let kinds: Vec<ChannelKind> = channels.iter().map(|(_, k, _)| k.clone()).collect();
        let bad: Vec<bool> = channels.iter().map(|(_, _, b)| *b).collect();
        let units: Vec<Unit> = (0..names.len()).map(|i| unit(["uV", "", "%"][i % 3])).collect();
        let text = file(&names, &kinds, &units, &bad);
        let symbols: Vec<String> = units.iter().map(Unit::symbol).collect();
        let symbols: Vec<&str> = symbols.iter().map(String::as_str).collect();
        let loaded = load(&text, &context(&names, &symbols)).unwrap();
        prop_assert_eq!(loaded.unknown, 0);
        prop_assert_eq!(loaded.units_differ, 0);
        let want_kinds: Vec<(ChannelId, ChannelKind)> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| (ChannelId(i as u32), k.clone()))
            .collect();
        prop_assert_eq!(&loaded.kinds, &want_kinds);
        // As shown: a known type by kymo's name, the others as given.
        for ((_, got), want) in loaded.kinds.iter().zip(&kinds) {
            prop_assert_eq!(got.name(), want.name());
        }
        let want_bad: Vec<(ChannelId, bool)> =
            bad.iter().enumerate().map(|(i, &b)| (ChannelId(i as u32), b)).collect();
        prop_assert_eq!(loaded.bad, want_bad);
        // kymo's column only where a type doesn't read back from BIDS' alone.
        let header: Vec<&str> = text.lines().next().unwrap().split('\t').collect();
        let mut want = vec!["name", "type", "units", "sampling_frequency", "status"];
        if !kinds.iter().all(reads_back) {
            want.push("kymo_type");
        }
        prop_assert_eq!(header, want);
    }

    #[test]
    fn any_text_reads_or_fails_cleanly(text in "[a-z\t\n\"/ ]{0,80}") {
        let context = context(&["a", "b"], &["", ""]);
        let _ = load(&format!("name\ttype\tstatus\n{text}"), &context);
        let _ = load(&text, &context);
    }
}

#[test]
fn writes_bids_types_units_and_rates() {
    let names = ["Fz", "HEOG", "MLC11", "SpO2", "X", "Acc", "STI 014"];
    let kinds = [
        "EEG",
        "HEOG",
        "MEG Grad",
        "SaO2",
        "PositionX",
        "accel",
        "Stim",
    ]
    .map(ChannelKind::named);
    let units = ["uV", "UV", "T/m", "%", "", "mg", ""].map(unit);
    let bad = [false, true, false, false, false, false, false];
    let text = file(&names, &kinds, &units, &bad);
    assert_eq!(
        text,
        "name\ttype\tunits\tsampling_frequency\tstatus\tkymo_type\n\
         Fz\tEEG\t\u{b5}V\t256\tgood\tn/a\n\
         HEOG\tHEOG\t\u{b5}V\t256\tbad\tn/a\n\
         MLC11\tMEGGRADPLANAR\tT/m\tn/a\tgood\tn/a\n\
         SpO2\tMISC\t%\t256\tgood\tSaO2\n\
         X\tMISC\tn/a\t256\tgood\tPositionX\n\
         Acc\tACCEL\tmg\tn/a\tgood\taccel\n\
         STI 014\tTRIG\tn/a\t256\tgood\tn/a\n"
    );
    // Without a type that needs it, no kymo column.
    let text = file(&names[..2], &kinds[..2], &units[..2], &bad[..2]);
    assert_eq!(
        text.lines().next(),
        Some("name\ttype\tunits\tsampling_frequency\tstatus")
    );
}

#[test]
fn bids_types_of_kymos_types() {
    for (kind, bids) in [
        ("EEG", "EEG"),
        ("MEG Mag", "MEGMAG"),
        ("MEG Ref", "MEGREFMAG"),
        ("VEOG", "VEOG"),
        ("Temperature", "TEMP"),
        ("HbO", "NIRSCWHBO"),
        ("cHPI", "HLU"),
        ("SYST", "MEGOTHER"),
        ("Sound", "AUDIO"),
        ("Stim", "TRIG"),
        ("Bio", "MISC"),
        ("ERG", "MISC"),
        ("Misc", "MISC"),
        ("gyro", "GYRO"),
        ("AUX", "MISC"),
    ] {
        assert_eq!(bids_type(&ChannelKind::named(kind)), bids, "{kind}");
    }
}

#[test]
fn reads_mne_bids_files() {
    // As MNE-BIDS writes them, with its columns, and spellings it warns about.
    let text = "name\ttype\tunits\tlow_cutoff\thigh_cutoff\tdescription\tsampling_frequency\
                \tstatus\tstatus_description\n\
                Fz\tEEG\t\u{b5}V\t0.1\t100\tElectroEncephaloGram\t256\tgood\tn/a\n\
                EOG1\teog\t\u{b5}V\t0.1\t100\tElectroOculoGram\t256\tBAD\tn/a\n\
                MEG 0113\tMEGGRADAXIAL\tT\t0.1\t100\tMEG\t256\tgood\tn/a\n\
                SYS201\tMEGOTHER\tn/a\t0.1\t100\tSystem\t256\tn/a\tn/a\n\
                STI 014\tTRIG\tn/a\t0.1\t100\tTrigger\t256\tgood\tn/a\n\
                Mic\tAUDIO\tV\t0.1\t100\tAudio\t256\tgood\tn/a\n\
                Wheel\tencoder\tn/a\t0.1\t100\tOther\t256\tgood\tn/a\n";
    let names = [
        "Fz", "EOG1", "MEG 0113", "SYS201", "STI 014", "Mic", "Wheel",
    ];
    let loaded = load(
        text,
        &context(&names, &["\u{b5}V", "\u{b5}V", "T", "", "", "V", ""]),
    )
    .unwrap();
    assert_eq!(
        loaded.kinds,
        ids(&[
            (0, "EEG"),
            (1, "EOG"),
            (2, "MEG Mag"),
            // MEGOTHER leaves a channel's type.
            (4, "Stim"),
            (5, "Sound"),
            (6, "encoder"),
        ])
    );
    assert_eq!(loaded.kinds[4].1.name(), "Sound");
    assert_eq!(loaded.kinds[5].1.name(), "encoder");
    assert_eq!(
        loaded.bad,
        [0, 1, 2, 4, 5, 6].map(|c| (ChannelId(c), c == 1)).to_vec()
    );
    assert_eq!((loaded.unknown, loaded.units_differ), (0, 0));
}

#[test]
fn names_find_channels_in_order() {
    // Two channels named A: the first row of A is the first, the second the second, a third
    // names none.
    let text = "name\ttype\n A\tECG\nA\tEOG\nB\tEMG\nA\tECG\nA\tRESP\nC\tEEG\n";
    let loaded = load(text, &context(&["A", "B", "A", " A"], &[""; 4])).unwrap();
    assert_eq!(
        loaded.kinds,
        ids(&[(3, "ECG"), (0, "EOG"), (1, "EMG"), (2, "ECG")])
    );
    assert_eq!(loaded.unknown, 2);
    // No status column: no channel's mark changes.
    assert!(loaded.bad.is_empty());
}

#[test]
fn kymo_type_wins_and_na_leaves_types() {
    let text = "name\ttype\tunits\tstatus\tkymo_type\n\
                a\tMISC\tn/a\tgood\tSaO2\n\
                b\tMISC\tn/a\tgood\tn/a\n\
                c\tn/a\tn/a\tbad\tn/a\n\
                d\tEEG\tS\tgood\t\n";
    let loaded = load(
        text,
        &context(&["a", "b", "c", "d"], &["", "", "", "\u{b5}V"]),
    )
    .unwrap();
    assert_eq!(loaded.kinds, ids(&[(0, "SaO2"), (1, "Misc"), (3, "EEG")]));
    assert_eq!(loaded.bad[2], (ChannelId(2), true));
    // d is in µV in the recording.
    assert_eq!(loaded.units_differ, 1);
}

#[test]
fn units_are_compared_as_symbols() {
    // n/a is a unit not known: c's isn't compared. Units of one quantity are the same: b's
    // V is the recording's µV; d's ° isn't its %.
    let text = "name\ttype\tunits\na\tEEG\tuV\nb\tEEG\tV\nc\tMISC\tn/a\nd\tMISC\tdegrees\n";
    let loaded = load(
        text,
        &context(&["a", "b", "c", "d"], &["microvolts", "\u{b5}V", "V", "%"]),
    )
    .unwrap();
    assert_eq!(loaded.units_differ, 1);
}

#[test]
fn broken_files_give_errors() {
    let context = context(&["a"], &[""]);
    for (text, want) in [
        ("type\tunits\nEEG\tuV\n", "no column \"name\""),
        ("name\tunits\na\tuV\n", "no column \"type\""),
        (
            "name\ttype\na\tEEG\textra\n",
            "line 2: 3 fields where the header has 2",
        ),
        ("", "no column \"name\""),
    ] {
        let error = load(text, &context).unwrap_err().to_string();
        assert_eq!(error, want, "{text:?}");
    }
    let error = read(&b"name\ttype\na\t\xff\n"[..], &context).unwrap_err();
    assert_eq!(error.to_string(), "line 2: not UTF-8 text");
}
