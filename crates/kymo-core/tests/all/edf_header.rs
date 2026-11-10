use kymo_core::edf::{Block, EdfError, ParsedHeader, parse_header};
use std::ops::Range;

use kymo_core::{
    BlockReader, ChannelId, ChannelKind, GridId, Opened, ReadError, Recording, SampleType,
    SamplesMut,
};
use kymo_testgen::{ChannelSpec, Format, GapSpec, Spec, Tal};
use proptest::prelude::*;

const GENERAL: [usize; 10] = [8, 80, 80, 8, 8, 8, 44, 8, 8, 4];
const SIGNAL: [usize; 10] = [16, 80, 8, 8, 8, 8, 8, 80, 8, 32];

/// Five channels at three rates (100, 20 and 50 Hz with 0.5 s records), with a negative gain
/// and a huge offset among them.
fn spec(format: Format) -> Spec {
    let channels = vec![
        ChannelSpec::eeg("EEG Fz", 50, format),
        ChannelSpec {
            label: "EOG left".into(),
            physical_min: 500.0,
            physical_max: -500.0,
            ..ChannelSpec::eeg("", 10, format)
        },
        ChannelSpec::eeg("EEG Cz", 50, format),
        ChannelSpec {
            label: "ECG".into(),
            dimension: "mV".into(),
            physical_min: 999_000.0,
            physical_max: 1_001_000.0,
            ..ChannelSpec::eeg("", 25, format)
        },
        ChannelSpec {
            label: "EMG chin".into(),
            transducer: "".into(),
            prefilter: "".into(),
            ..ChannelSpec::eeg("", 10, format)
        },
    ];
    let mut spec = Spec::new(format, channels, 0.5, 9);
    if format.is_plus() {
        spec.annotations = vec![Tal {
            onset: 1.0,
            duration: None,
            texts: vec!["x".into()],
        }];
    }
    if format.is_discontinuous() {
        spec.gaps = vec![GapSpec {
            before_record: 4,
            seconds: 2.0,
        }];
    }
    spec
}

fn generated(spec: &Spec) -> Vec<u8> {
    let mut out = Vec::new();
    kymo_testgen::write_to(spec, &mut out).unwrap();
    out
}

fn parse(bytes: &[u8]) -> ParsedHeader {
    parse_header(bytes, bytes.len() as u64).unwrap()
}

fn error(bytes: &[u8]) -> EdfError {
    parse_header(bytes, bytes.len() as u64).unwrap_err()
}

/// Overwrites general header field `index` with `value`, space-padded.
fn set_general(bytes: &mut [u8], index: usize, value: &[u8]) {
    let start: usize = GENERAL[..index].iter().sum();
    put(&mut bytes[start..start + GENERAL[index]], value);
}

/// Overwrites per-signal field `column` of `signal` (of `ns` signals) with `value`.
fn set_signal(bytes: &mut [u8], ns: usize, column: usize, signal: usize, value: &[u8]) {
    let start = 256 + ns * SIGNAL[..column].iter().sum::<usize>() + signal * SIGNAL[column];
    put(&mut bytes[start..start + SIGNAL[column]], value);
}

fn put(field: &mut [u8], value: &[u8]) {
    field.fill(b' ');
    field[..value.len()].copy_from_slice(value);
}

#[test]
fn parses_generated_headers() {
    for format in Format::EDF {
        let spec = spec(format);
        let parsed = parse(&generated(&spec));
        let (h, layout) = (&parsed.header, &parsed.layout);
        assert_eq!(parsed.warnings, Vec::<String>::new(), "{format:?}");
        assert_eq!(h.format, format.name());
        assert_eq!(
            h.start_time,
            Some(jiff::civil::date(2024, 1, 2).at(3, 4, 5, 0))
        );
        assert_eq!(h.extra[0], ("patient".into(), "X X X X".into()));

        // Channels: the annotation signal is not one of them.
        assert_eq!(h.channels.len(), 5);
        let kinds: Vec<ChannelKind> = h.channels.iter().map(|c| c.kind).collect();
        use ChannelKind::*;
        assert_eq!(kinds, [Eeg, Eog, Eeg, Ecg, Emg]);
        for (c, s) in h.channels.iter().zip(&spec.channels) {
            assert_eq!(c.name, s.label);
            assert_eq!(c.unit.label, s.dimension);
            assert_eq!(c.transducer, s.transducer);
            assert_eq!(c.prefilter, s.prefilter);
            assert_eq!(c.gain, s.gain(), "{}", c.name);
            assert_eq!(c.offset, s.offset(), "{}", c.name);
            assert_eq!(c.physical_range, Some((s.physical_min, s.physical_max)));
            let digital = (f64::from(s.digital_min), f64::from(s.digital_max));
            assert_eq!(c.digital_range, Some(digital));
        }
        assert_eq!(h.channels[0].unit.to_si, Some(1e-6));
        assert_eq!(h.channels[3].unit.to_si, Some(1e-3));
        assert!(h.channels[1].gain < 0.0);

        // One grid per samples-per-record, in order of first appearance.
        let sample_type = if format.is_bdf() {
            SampleType::I32
        } else {
            SampleType::I16
        };
        let expected = [
            (100.0, 9 * 50, vec![0, 2]),
            (20.0, 9 * 10, vec![1, 4]),
            (50.0, 9 * 25, vec![3]),
        ];
        assert_eq!(h.grids.len(), 3);
        for (g, (grid, (rate, len, ids))) in h.grids.iter().zip(expected).enumerate() {
            assert_eq!(grid.rate(), rate);
            assert_eq!(grid.len(), len);
            assert_eq!(grid.segments(), [0..len]);
            assert_eq!(grid.sample_type(), sample_type);
            let ids: Vec<ChannelId> = ids.into_iter().map(ChannelId).collect();
            assert_eq!(grid.channels(), ids);
            for id in ids {
                assert_eq!(h.channels[id.0 as usize].grid, GridId(g as u32));
            }
        }

        // Record layout.
        let bps = format.bytes_per_sample() as u64;
        assert_eq!(layout.bytes_per_sample as u64, bps);
        assert_eq!(layout.discontinuous, format.is_discontinuous());
        assert_eq!(layout.header_bytes, spec.header_bytes());
        assert_eq!(layout.record_bytes, spec.record_bytes().unwrap());
        assert_eq!(layout.n_records, 9);
        assert_eq!(layout.record_duration, 0.5);
        let offsets: Vec<u64> = layout.channels.iter().map(|b| b.offset).collect();
        assert_eq!(offsets, [0, 50, 60, 110, 135].map(|o| o * bps));
        if format.is_plus() {
            let spr = spec.annotation_samples_per_record().unwrap().unwrap();
            let block = Block {
                offset: 145 * bps,
                samples: u64::from(spr),
            };
            assert_eq!(layout.annotations, [block]);
        } else {
            assert!(layout.annotations.is_empty());
        }
    }
}

#[test]
fn reads_from_a_header_prefix() {
    let bytes = generated(&spec(Format::BdfPlusC));
    let header_len = spec(Format::BdfPlusC).header_bytes() as usize;
    let parsed = parse_header(&bytes[..header_len], bytes.len() as u64).unwrap();
    assert_eq!(parsed, parse(&bytes));
}

#[test]
fn infers_kinds_and_units_from_labels() {
    let s = spec(Format::Edf);
    let mut bytes = generated(&s);
    let ns = s.signal_count();
    let labels: [&[u8]; 5] = [b"Status", b"TRIGGER", b"Fp1", b"ekg II", b"EOG-L"];
    let dimensions: [&[u8]; 5] = [b"mV", b"V", b"\xb5V", b"degC", b""];
    for i in 0..5 {
        set_signal(&mut bytes, ns, 0, i, labels[i]);
        set_signal(&mut bytes, ns, 2, i, dimensions[i]);
    }
    let h = parse(&bytes).header;
    let kinds: Vec<ChannelKind> = h.channels.iter().map(|c| c.kind).collect();
    use ChannelKind::*;
    // EDF+ separates the type from the name with a space; "EOG-L" has none, so it is EEG.
    assert_eq!(kinds, [Stim, Stim, Eeg, Ecg, Eeg]);
    let units: Vec<(&str, Option<f64>)> = h
        .channels
        .iter()
        .map(|c| (c.unit.label.as_str(), c.unit.to_si))
        .collect();
    assert_eq!(
        units,
        [
            ("mV", Some(1e-3)),
            ("V", Some(1.0)),
            ("\u{b5}V", Some(1e-6)),
            ("degC", None),
            ("", None)
        ]
    );
}

#[test]
fn unknown_record_count_is_taken_from_the_file_size() {
    let mut bytes = generated(&spec(Format::EdfPlusC));
    set_general(&mut bytes, 7, b"-1");
    let parsed = parse(&bytes);
    assert_eq!(parsed.layout.n_records, 9);
    assert_eq!(parsed.warnings.len(), 1);
    assert!(
        parsed.warnings[0].contains("unknown"),
        "{:?}",
        parsed.warnings
    );
}

#[test]
fn truncated_file_uses_the_complete_records() {
    let s = spec(Format::Bdf);
    let mut bytes = generated(&s);
    let record = s.record_bytes().unwrap() as usize;
    bytes.truncate(bytes.len() - record / 2);
    let parsed = parse(&bytes);
    assert_eq!(parsed.layout.n_records, 8);
    assert_eq!(parsed.header.grids[0].len(), 8 * 50);
    assert_eq!(parsed.warnings.len(), 1);
    assert!(parsed.warnings[0].contains("truncated"));
    assert!(parsed.warnings[0].contains("incomplete"));

    // A file cut at a record boundary is truncated too.
    bytes.truncate(s.header_bytes() as usize + 3 * record);
    assert_eq!(parse(&bytes).layout.n_records, 3);

    // No records at all.
    bytes.truncate(s.header_bytes() as usize);
    let parsed = parse(&bytes);
    assert_eq!(parsed.layout.n_records, 0);
    assert!(parsed.header.grids.iter().all(|g| g.is_empty()));
}

#[test]
fn trailing_bytes_are_ignored_with_a_warning() {
    let mut bytes = generated(&spec(Format::Edf));
    bytes.extend_from_slice(&[0; 7]);
    let parsed = parse(&bytes);
    assert_eq!(parsed.layout.n_records, 9);
    assert!(parsed.warnings[0].contains("7 bytes after"));
}

#[test]
fn parses_start_dates() {
    let mut bytes = generated(&spec(Format::Edf));
    set_general(&mut bytes, 3, b"01.02.85");
    set_general(&mut bytes, 4, b"23:59:58");
    let start = parse(&bytes).header.start_time;
    assert_eq!(start, Some(jiff::civil::date(1985, 2, 1).at(23, 59, 58, 0)));

    set_general(&mut bytes, 3, b"01.02.84");
    let start = parse(&bytes).header.start_time;
    assert_eq!(start.map(|s| s.year()), Some(2084));

    for bad in [&b"32.01.24"[..], b"01.13.24", b"1.2", b"aa.bb.cc"] {
        set_general(&mut bytes, 3, bad);
        let parsed = parse(&bytes);
        assert_eq!(parsed.header.start_time, None, "{bad:?}");
        assert!(parsed.warnings[0].contains("start date"));
    }

    // EDF+: the recording field's four-digit year covers years past 2084.
    let mut bytes = generated(&spec(Format::EdfPlusC));
    set_general(&mut bytes, 2, b"Startdate 02-JAN-2090 X X X");
    set_general(&mut bytes, 3, b"02.01.yy");
    let start = parse(&bytes).header.start_time;
    assert_eq!(start, Some(jiff::civil::date(2090, 1, 2).at(3, 4, 5, 0)));
}

#[test]
fn equal_physical_limits_warn_and_give_zero_gain() {
    let s = spec(Format::Edf);
    let mut bytes = generated(&s);
    set_signal(&mut bytes, s.signal_count(), 4, 2, b"-3276.8");
    let parsed = parse(&bytes);
    assert_eq!(parsed.header.channels[2].gain, 0.0);
    assert_eq!(parsed.header.channels[2].offset, -3276.8);
    assert!(parsed.warnings[0].contains("physical minimum equals maximum"));
}

#[test]
fn accepts_comma_decimal_separators() {
    let s = spec(Format::Edf);
    let mut bytes = generated(&s);
    set_general(&mut bytes, 8, b"0,5");
    set_signal(&mut bytes, s.signal_count(), 3, 0, b"-3276,8");
    let parsed = parse(&bytes);
    assert_eq!(parsed.layout.record_duration, 0.5);
    assert_eq!(parsed.header.channels[0].physical_range.unwrap().0, -3276.8);
}

#[test]
fn rejects_malformed_headers() {
    let s = spec(Format::EdfPlusC);
    let valid = generated(&s);
    let ns = s.signal_count();
    let header = s.header_bytes() as usize;

    let invalid_field = |e: &EdfError, name: &str| matches!(e, EdfError::InvalidField { field, .. } if field.contains(name));
    let invalid_signal =
        |e: &EdfError, i: usize| matches!(e, EdfError::InvalidSignal { index, .. } if *index == i);

    assert!(matches!(
        error(&valid[..200]),
        EdfError::Truncated {
            needed: 256,
            found: 200
        }
    ));
    assert!(matches!(
        error(&valid[..header - 1]),
        EdfError::Truncated { .. }
    ));
    let e = parse_header(&valid, (header - 1) as u64).unwrap_err();
    assert!(matches!(e, EdfError::Truncated { .. }));

    type Patch = Box<dyn Fn(&mut Vec<u8>)>;
    type Check = Box<dyn Fn(&EdfError) -> bool>;
    let cases: Vec<(Patch, Check)> = vec![
        (
            Box::new(|b| set_general(b, 0, b"1")),
            Box::new(|e| matches!(e, EdfError::NotEdf(_))),
        ),
        (
            Box::new(|b| set_general(b, 9, b"0")),
            Box::new(move |e| invalid_field(e, "number of signals")),
        ),
        (
            Box::new(|b| set_general(b, 9, b"abc")),
            Box::new(move |e| invalid_field(e, "number of signals")),
        ),
        (
            Box::new(|b| set_general(b, 5, b"256")),
            Box::new(move |e| invalid_field(e, "bytes in the header")),
        ),
        (
            Box::new(|b| set_general(b, 8, b"0")),
            Box::new(move |e| invalid_field(e, "record duration")),
        ),
        (
            Box::new(|b| set_general(b, 8, b"-1")),
            Box::new(move |e| invalid_field(e, "record duration")),
        ),
        (
            Box::new(|b| set_general(b, 7, b"-5")),
            Box::new(move |e| invalid_field(e, "number of records")),
        ),
        (
            Box::new(|b| set_general(b, 7, b"many")),
            Box::new(move |e| invalid_field(e, "number of records")),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 8, 1, b"x")),
            Box::new(move |e| invalid_field(e, "samples per record of signal 1")),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 8, 1, b"0")),
            Box::new(move |e| invalid_signal(e, 1)),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 5, 2, b"40000")),
            Box::new(move |e| invalid_signal(e, 2)),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 3, 0, b"nan")),
            Box::new(move |e| invalid_field(e, "physical minimum of signal 0")),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 4, 0, b"inf")),
            Box::new(move |e| invalid_field(e, "physical maximum of signal 0")),
        ),
        (
            Box::new(move |b| set_signal(b, ns, 6, 3, b"1.5e")),
            Box::new(move |e| invalid_field(e, "digital maximum of signal 3")),
        ),
        (
            // Every data signal relabeled as annotations.
            Box::new(move |b| {
                for i in 0..ns - 1 {
                    set_signal(b, ns, 0, i, b"EDF Annotations");
                }
            }),
            Box::new(|e| matches!(e, EdfError::NoDataSignals)),
        ),
    ];
    for (i, (patch, check)) in cases.iter().enumerate() {
        let mut bytes = valid.clone();
        patch(&mut bytes);
        let e = error(&bytes);
        assert!(check(&e), "case {i}: {e}");
    }
}

/// A reader that is never called: the header tests only build recordings.
struct NoReader;

impl BlockReader for NoReader {
    fn read(
        &self,
        _: GridId,
        _: &[ChannelId],
        _: Range<u64>,
        _: SamplesMut<'_>,
    ) -> Result<(), ReadError> {
        unreachable!("header tests never read samples")
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2000)) {
        let _ = parse_header(&bytes, bytes.len() as u64);
    }

    /// Mutations of valid headers reach the deeper checks that random bytes rarely get to.
    #[test]
    fn mutated_headers_never_panic(
        format in proptest::sample::select(Format::EDF.to_vec()),
        edits in proptest::collection::vec(
            (any::<prop::sample::Index>(), prop_oneof![
                proptest::sample::select(b"0123456789 -.,+eE".to_vec()),
                any::<u8>(),
            ]),
            1..12,
        ),
        cut in any::<prop::sample::Index>(),
        file_len_delta in -2000i64..2000,
    ) {
        let mut bytes = generated(&spec(format));
        let header = spec(format).header_bytes() as usize;
        for (index, byte) in edits {
            let i = index.index(header);
            bytes[i] = byte;
        }
        let len = cut.index(bytes.len() + 1);
        let file_len = (len as i64 + file_len_delta).max(0) as u64;
        if let Ok(parsed) = parse_header(&bytes[..len], file_len) {
            // Whatever parses must be consistent.
            let layout = &parsed.layout;
            prop_assert!(layout.n_records * layout.record_bytes + layout.header_bytes <= file_len);
            prop_assert_eq!(layout.channels.len(), parsed.header.channels.len());
            let opened = Opened {
                header: parsed.header,
                annotations: Vec::new(),
                warnings: parsed.warnings,
                sources: vec![],
                reader: Box::new(NoReader),
            };
            prop_assert!(Recording::new(opened).is_ok());
        }
    }
}
