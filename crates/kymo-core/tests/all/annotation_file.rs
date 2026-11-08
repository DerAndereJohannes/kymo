//! Annotation files: kymo's events table round-trips, MNE's CSV reads as MNE wrote it, and
//! broken files give errors, never panics.

use kymo_core::annotation_file::{Context, FileError, Format, Loaded, read, write_table};
use kymo_core::{AnnotationData, ChannelId, Origin, Recording};
use proptest::prelude::*;

fn context(first_sample: f64) -> Context {
    Context {
        start_time: Some(jiff::civil::date(2024, 1, 2).at(3, 4, 5, 0)),
        first_sample,
        end: 600.0,
        channels: ["Fz", "C:z", "a;b", "back\\slash", " spaced "]
            .map(String::from)
            .into(),
    }
}

fn table(context: &Context, annotations: &[AnnotationData]) -> String {
    let mut out = Vec::new();
    write_table(&mut out, context, annotations).unwrap();
    String::from_utf8(out).unwrap()
}

fn load(text: &str, format: Format, context: &Context) -> Result<Loaded, FileError> {
    read(text.as_bytes(), format, context)
}

fn data(onset: f64, duration: f64, description: &str, channels: &[u32]) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: description.into(),
        channels: channels.iter().copied().map(ChannelId).collect(),
        origin: Origin::Viewer,
    }
}

/// Descriptions of characters that need quoting or escaping, or that readers trim.
fn any_description() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('B'),
            Just(' '),
            Just('\t'),
            Just('"'),
            Just('\n'),
            Just('\r'),
            Just(','),
            Just(';'),
            Just(':'),
            Just('\\'),
            Just('ü'),
            Just('𝄞'),
        ],
        0..8,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn any_annotation() -> impl Strategy<Value = AnnotationData> {
    (
        0.25f64..600.0,
        prop_oneof![Just(0.0), 0.0f64..5.0],
        any_description(),
        proptest::collection::btree_set(0u32..5, 0..3),
    )
        .prop_map(|(onset, duration, description, channels)| AnnotationData {
            onset,
            duration,
            description,
            channels: channels.into_iter().map(ChannelId).collect(),
            origin: Origin::Viewer,
        })
}

proptest! {
    #[test]
    fn a_table_round_trips(
        annotations in proptest::collection::vec(any_annotation(), 0..20),
        first_sample in prop_oneof![Just(0.0), Just(0.25)],
    ) {
        let context = context(first_sample);
        let text = table(&context, &annotations);
        let loaded = load(&text, Format::Table, &context).unwrap();
        prop_assert_eq!(loaded.outside + loaded.unknown_channels, 0);
        prop_assert_eq!(loaded.annotations.len(), annotations.len());
        for (got, want) in loaded.annotations.iter().zip(&annotations) {
            // Exact from the first sample; kymo's onset then adds its time back.
            if first_sample == 0.0 {
                prop_assert_eq!(got, want);
            } else {
                prop_assert!((got.onset - want.onset).abs() <= 1e-12 * want.onset.max(1.0));
                prop_assert_eq!(
                    &AnnotationData { onset: want.onset, ..got.clone() },
                    want
                );
            }
        }
        // The channels column only when some annotation has channels.
        let header = text.lines().next().unwrap();
        prop_assert_eq!(
            header.ends_with("\tchannels"),
            annotations.iter().any(|a| !a.channels.is_empty())
        );
    }

    #[test]
    fn broken_input_is_an_error_not_a_panic(
        text in proptest::collection::vec(
            prop_oneof![
                Just("onset".to_owned()),
                Just("duration".to_owned()),
                Just("trial_type".to_owned()),
                Just("description".to_owned()),
                Just("ch_names".to_owned()),
                Just("channels".to_owned()),
                Just("\t".to_owned()),
                Just(",".to_owned()),
                Just("\n".to_owned()),
                Just("\"".to_owned()),
                Just("\\".to_owned()),
                Just(";".to_owned()),
                Just("n/a".to_owned()),
                Just("1.5".to_owned()),
                Just("-1".to_owned()),
                Just("1e308".to_owned()),
                Just("NaN".to_owned()),
                Just("inf".to_owned()),
                Just("2024-01-02 03:04:05.250".to_owned()),
                Just("Fz".to_owned()),
                any::<char>().prop_map(String::from),
            ],
            0..40,
        ),
        bytes in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let context = context(0.25);
        for format in [Format::Table, Format::MneCsv] {
            for input in [text.concat().into_bytes(), bytes.clone()] {
                if let Ok(loaded) = read(&input[..], format, &context) {
                    for a in loaded.annotations {
                        prop_assert!(a.onset.is_finite() && a.end().is_finite());
                        prop_assert!(a.duration >= 0.0);
                    }
                }
            }
        }
    }
}

#[test]
fn a_table_reads_as_bids_writes_it() {
    let context = context(0.0);
    // Written by hand, as other tools do: `description` or `value` for the kind, `n/a`
    // durations, extra columns, a byte order mark, CRLF line ends.
    let text =
        "\u{feff}onset\tduration\tvalue\tsample\r\n1.5\tn/a\tstim\t384\r\n2\t0.5\tx\t512\r\n";
    let loaded = load(text, Format::Table, &context).unwrap();
    assert_eq!(
        loaded.annotations,
        [data(1.5, 0.0, "stim", &[]), data(2.0, 0.5, "x", &[])]
    );
    let text = "onset\tduration\tdescription\ttrial_type\n1\t0\tno\tyes\n";
    assert_eq!(
        load(text, Format::Table, &context).unwrap().annotations[0].description,
        "yes"
    );
    // Channels by name; unknown ones, and annotations wholly outside the recording, are
    // skipped and counted; one reaching into it is kept.
    let text = "onset\tduration\ttrial_type\tchannels\n\
                1\t0\ta\tFz;C:z\n\
                2\t0\tb\tnope\n\
                -5\t1\tc\tn/a\n\
                -5\t5\td\t\n\
                601\t0\te\t\n\
                3\t0\tf\ta\\;b;back\\\\slash\n";
    let loaded = load(text, Format::Table, &context).unwrap();
    let got: Vec<(&str, &[ChannelId])> = loaded
        .annotations
        .iter()
        .map(|a| (a.description.as_str(), &a.channels[..]))
        .collect();
    assert_eq!(
        got,
        [
            ("a", &[ChannelId(0), ChannelId(1)][..]),
            ("d", &[][..]),
            ("f", &[ChannelId(2), ChannelId(3)][..]),
        ]
    );
    assert_eq!((loaded.unknown_channels, loaded.outside), (1, 2));
}

#[test]
fn mne_csv_reads_as_mne_wrote_it() {
    // As MNE 1.13's `Annotations.save` wrote them: meas_date 2024-01-02 03:04:05, and one
    // without a measurement date.
    let with_channels = "onset,duration,description,ch_names\n\
        2024-01-02 03:04:05.000,0.0,BAD,\n\
        2024-01-02 03:04:06.250,2.5,\"a,b \"\"q\"\"\",Fz:C{COLON}z\n\
        2024-01-02 03:04:08.000,0.3333333333333333,\"ü\nx\",\n";
    let whole_seconds = "onset,duration,description\n\
        2024-01-02 03:04:07,1.0,x\n\
        2024-01-02 03:04:10,0.0,y\n";
    let undated = "onset,duration,description\n1970-01-01 00:00:02,1.0,x\n";

    // Times count from the first sample: kymo adds its time.
    let context = context(0.25);
    let loaded = load(with_channels, Format::MneCsv, &context).unwrap();
    assert_eq!(
        loaded.annotations,
        [
            data(0.25, 0.0, "BAD", &[]),
            data(1.5, 2.5, "a,b \"q\"", &[0, 1]),
            data(3.25, 1.0 / 3.0, "ü\nx", &[]),
        ]
    );
    let loaded = load(whole_seconds, Format::MneCsv, &context).unwrap();
    assert_eq!(
        loaded.annotations,
        [data(2.25, 1.0, "x", &[]), data(5.25, 0.0, "y", &[])]
    );
    let undated_context = Context {
        start_time: None,
        ..context.clone()
    };
    let loaded = load(undated, Format::MneCsv, &undated_context).unwrap();
    assert_eq!(loaded.annotations, [data(2.25, 1.0, "x", &[])]);
    // Another day's: outside the recording.
    let loaded = load(undated, Format::MneCsv, &context).unwrap();
    assert_eq!((loaded.annotations.len(), loaded.outside), (0, 1));
}

#[test]
fn broken_files_say_where() {
    let context = context(0.0);
    let line = |text: &str, format| match load(text, format, &context) {
        Err(FileError::Line { line, message }) => (line, message),
        other => panic!("{other:?}"),
    };
    let (n, message) = line(
        "onset\tduration\ttrial_type\n1\t0\ta\nx\t0\tb\n",
        Format::Table,
    );
    assert_eq!(n, 3);
    assert!(message.contains("\"x\" is not a number"), "{message}");
    for (bad, says) in [
        ("-1", "is negative"),
        ("NaN", "is not a number"),
        ("inf", "is not a number"),
        ("1e309", "is not a number"),
    ] {
        let text = format!("onset\tduration\ttrial_type\n1\t{bad}\ta\n");
        let (_, message) = line(&text, Format::Table);
        assert!(message.contains(says), "{bad}: {message}");
    }
    let (n, message) = line("onset\tduration\ttrial_type\n1\t0\n", Format::Table);
    assert_eq!(n, 2);
    assert!(
        message.contains("2 fields where the header has 3"),
        "{message}"
    );
    let (_, message) = line(
        "onset\tduration\ttrial_type\tchannels\n1\t0\ta\tF\\z\n",
        Format::Table,
    );
    assert!(message.contains("stray backslash"), "{message}");
    // Seconds in a CSV: MNE would read them as milliseconds.
    let (n, message) = line("onset,duration,description\n1.5,0,a\n", Format::MneCsv);
    assert_eq!(n, 2);
    assert!(message.contains("is a number"), "{message}");
    let (_, message) = line(
        "onset,duration,description\nyesterday,0,a\n",
        Format::MneCsv,
    );
    assert!(message.contains("not a date and time"), "{message}");
    // A table read as MNE's CSV: no columns it knows.
    assert!(matches!(
        load(
            "onset\tduration\ttrial_type\n1\t0\ta\n",
            Format::MneCsv,
            &context
        ),
        Err(FileError::Header(_))
    ));
    assert!(matches!(
        load("", Format::Table, &context),
        Err(FileError::Header(_))
    ));
    assert!(matches!(
        read(
            &b"onset\tduration\ttrial_type\n1\t0\t\xff\n"[..],
            Format::Table,
            &context
        ),
        Err(FileError::Line { line: 2, .. })
    ));
}

#[test]
fn formats_go_by_extension() {
    let of = |name: &str| Format::of(std::path::Path::new(name));
    assert_eq!(of("a/run-1_events.tsv"), Some(Format::Table));
    assert_eq!(of("x.TSV"), Some(Format::Table));
    assert_eq!(of("x-annot.csv"), Some(Format::MneCsv));
    assert_eq!(of("x.txt"), None);
    assert_eq!(of("tsv"), None);
}

/// The time base: a file's onsets count from the first sample, at the first record's
/// onset in `edf+d-mixed` (0.25 s after the header's start time).
#[test]
fn files_count_from_the_first_sample() {
    let file = kymo_testgen::TempFile::preset("edf+d-mixed");
    let (recording, annotations) = Recording::new(kymo_core::open(file.path()).unwrap()).unwrap();
    let context = Context::of(&recording);
    assert_eq!(context.first_sample, 0.25);
    assert_eq!(context.start_time, recording.start_time());
    assert_eq!(context.end, recording.duration());
    let text = table(&context, &annotations);
    let first = text.lines().nth(1).unwrap();
    let onset: f64 = first.split('\t').next().unwrap().parse().unwrap();
    assert_eq!(onset, annotations[0].onset - 0.25);
    let loaded = load(&text, Format::Table, &context).unwrap();
    assert_eq!(loaded.annotations.len(), annotations.len());
    for (got, want) in loaded.annotations.iter().zip(&annotations) {
        assert_eq!(
            (got.onset, got.duration, &got.description),
            (want.onset, want.duration, &want.description)
        );
    }
}
