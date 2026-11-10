// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::Command;

use kymo_testgen::{
    Binary, ChannelSpec, Error, Format, GapSpec, Orientation, PRESETS, Spec, Tal, TempFile,
};

/// A small spec of `format` with mixed rates, scaling edge cases, and (where the format allows)
/// gaps, a sub-second first onset and annotations.
fn small(format: Format) -> Spec {
    let channels = vec![
        ChannelSpec::eeg("EEG Fz", 50, format),
        ChannelSpec {
            label: "Resp".into(),
            physical_min: 100.0,
            physical_max: -100.0,
            ..ChannelSpec::eeg("", 10, format)
        },
        ChannelSpec {
            label: "DC".into(),
            physical_min: 999_000.0,
            physical_max: 1_001_000.0,
            ..ChannelSpec::eeg("", 25, format)
        },
    ];
    let mut spec = Spec::new(format, channels, 0.5, 9);
    spec.seed = 11;
    if format.is_plus() {
        spec.first_onset = 0.125;
        spec.annotations = vec![
            Tal {
                onset: 0.2,
                duration: None,
                texts: vec!["start".into()],
            },
            Tal {
                onset: 1.3,
                duration: Some(0.75),
                texts: vec!["two".into(), "texts µV".into()],
            },
            Tal {
                onset: 1.3,
                duration: Some(0.0),
                texts: vec!["same onset".into()],
            },
            Tal {
                onset: -1.0,
                duration: None,
                texts: vec!["before start".into()],
            },
            Tal {
                onset: 100.0,
                duration: Some(2.0),
                texts: vec!["after end".into()],
            },
        ];
    }
    if format.is_discontinuous() {
        spec.gaps = vec![
            GapSpec {
                before_record: 3,
                seconds: 1.0,
            },
            GapSpec {
                before_record: 4,
                seconds: 0.3,
            },
        ];
    }
    spec
}

fn bytes(spec: &Spec) -> Vec<u8> {
    let mut out = Vec::new();
    kymo_testgen::write_to(spec, &mut out).unwrap();
    out
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kymo-testgen-{}-{name}", std::process::id()))
}

/// The header split into its fields: 10 general ones, then the 10 per-signal columns.
struct Header {
    general: Vec<String>,
    signals: Vec<[String; 10]>,
}

fn field(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn parse_header(bytes: &[u8]) -> Header {
    let widths = [8, 80, 80, 8, 8, 8, 44, 8, 8, 4];
    let mut pos = 0;
    let mut general = Vec::new();
    for w in widths {
        let raw = &bytes[pos..pos + w];
        // The BDF version field starts with 0xFF.
        general.push(if pos == 0 && raw[0] == 0xff {
            format!("\u{ff}{}", field(&raw[1..]))
        } else {
            field(raw)
        });
        pos += w;
    }
    let ns: usize = general[9].trim().parse().unwrap();
    let mut signals = vec![<[String; 10]>::default(); ns];
    for (column, w) in [16, 80, 8, 8, 8, 8, 8, 80, 8, 32].into_iter().enumerate() {
        for s in &mut signals {
            s[column] = field(&bytes[pos..pos + w]);
            pos += w;
        }
    }
    assert_eq!(pos, 256 * (ns + 1));
    Header { general, signals }
}

fn decode(bytes: &[u8], bdf: bool) -> Vec<i32> {
    if bdf {
        bytes
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8)
            .collect()
    } else {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i32::from(i16::from_le_bytes(*b)))
            .collect()
    }
}

/// A parsed TAL: onset, duration, texts.
type ParsedTal = (f64, Option<f64>, Vec<String>);

/// Splits one record's annotation signal into TALs.
fn parse_tals(bytes: &[u8]) -> Vec<ParsedTal> {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    assert!(end < bytes.len(), "TAL list must end with a zero byte");
    bytes[..=end]
        .split(|&b| b == 0)
        .filter(|tal| !tal.is_empty())
        .map(|tal| {
            let text = std::str::from_utf8(tal).unwrap();
            let mut parts = text.split('\x14');
            let time = parts.next().unwrap();
            let (onset, duration) = match time.split_once('\x15') {
                Some((o, d)) => (o, Some(d.parse().unwrap())),
                None => (time, None),
            };
            assert!(
                onset.starts_with(['+', '-']),
                "onset {onset:?} needs a sign"
            );
            let texts: Vec<String> = parts.map(str::to_owned).collect();
            assert_eq!(
                texts.last().map(String::as_str),
                Some(""),
                "TAL ends with \\x14"
            );
            let texts = texts[..texts.len() - 1].to_vec();
            (onset.parse().unwrap(), duration, texts)
        })
        .collect()
}

#[test]
fn file_size_is_header_plus_records() {
    for format in Format::EDF {
        let spec = small(format);
        let bps = format.bytes_per_sample() as u64;
        let annotation = spec.annotation_samples_per_record().unwrap();
        assert_eq!(annotation.is_some(), format.is_plus());
        let record = (50 + 10 + 25 + u64::from(annotation.unwrap_or(0))) * bps;
        assert_eq!(spec.record_bytes().unwrap(), record);
        let header = 256 * (4 + u64::from(format.is_plus()));
        assert_eq!(spec.header_bytes(), header);
        assert_eq!(spec.file_size().unwrap(), header + 9 * record);
        assert_eq!(bytes(&spec).len() as u64, header + 9 * record, "{format:?}");
    }
}

#[test]
fn writes_the_same_bytes_to_a_file() {
    let spec = small(Format::BdfPlusD);
    let path = temp_path("same.bdf");
    kymo_testgen::write(&spec, &path).unwrap();
    let written = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(written, bytes(&spec));
}

#[test]
fn header_fields_are_padded_ascii() {
    for format in Format::EDF {
        let spec = small(format);
        let out = bytes(&spec);
        let header_len = spec.header_bytes() as usize;
        let start = usize::from(format.is_bdf());
        assert!(
            out[start..header_len]
                .iter()
                .all(|b| (0x20..=0x7e).contains(b)),
            "{format:?}: header must be printable ASCII"
        );

        let h = parse_header(&out);
        let g: Vec<&str> = h.general.iter().map(|s| s.trim_end()).collect();
        let version = if format.is_bdf() {
            "\u{ff}BIOSEMI"
        } else {
            "0"
        };
        assert_eq!(g[0], version);
        assert_eq!(g[1], "X X X X");
        if format.is_plus() {
            assert_eq!(g[2], "Startdate 02-JAN-2024 X X X");
        } else {
            assert_eq!(g[2], "kymo-testgen");
        }
        assert_eq!(g[3], "02.01.24");
        assert_eq!(g[4], "03.04.05");
        assert_eq!(g[5], header_len.to_string());
        let reserved = match format {
            Format::Edf => "",
            Format::Bdf => "24BIT",
            other => other.name(),
        };
        assert_eq!(g[6], reserved);
        assert_eq!(g[7], "9");
        assert_eq!(g[8], "0.5");
        assert_eq!(g[9], spec.signal_count().to_string());

        let rows: Vec<Vec<&str>> = h
            .signals
            .iter()
            .map(|s| s.iter().map(|f| f.trim_end()).collect())
            .collect();
        let (lo, hi) = format.digital_limits();
        assert_eq!(
            rows[1],
            [
                "Resp",
                "AgAgCl electrode",
                "uV",
                "100",
                "-100",
                &lo.to_string(),
                &hi.to_string(),
                "HP:0.1Hz LP:100Hz",
                "10",
                ""
            ]
        );
        assert_eq!(rows[2][3..5], ["999000", "1001000"]);
        if format.is_plus() {
            let label = if format.is_bdf() {
                "BDF Annotations"
            } else {
                "EDF Annotations"
            };
            assert_eq!(rows[3][0], label);
            let spr = spec.annotation_samples_per_record().unwrap().unwrap();
            assert_eq!(rows[3][8], spr.to_string());
        }
        // Left-aligned: a non-empty field never starts with a space.
        for f in h.general.iter().chain(h.signals.iter().flatten()) {
            assert!(!f.starts_with(' ') || f.trim().is_empty(), "{f:?}");
        }
    }
}

#[test]
fn samples_decode_to_the_truth_function() {
    for format in Format::EDF {
        let spec = small(format);
        let out = bytes(&spec);
        let record_bytes = spec.record_bytes().unwrap() as usize;
        let bps = format.bytes_per_sample();
        let data = &out[spec.header_bytes() as usize..];
        for (r, record) in data.chunks_exact(record_bytes).enumerate() {
            let mut pos = 0;
            for (c, channel) in spec.channels.iter().enumerate() {
                let n = channel.samples_per_record as usize;
                let values = decode(&record[pos..pos + n * bps], format.is_bdf());
                let start = spec.record_start(c, r as u64);
                let expected: Vec<i32> =
                    (0..n as u64).map(|j| spec.digital(c, start + j)).collect();
                assert_eq!(values, expected, "{format:?}, record {r}, channel {c}");
                pos += n * bps;
            }
        }
    }
}

#[test]
fn records_carry_time_keeping_and_annotations() {
    for format in Format::EDF.into_iter().filter(|f| f.is_plus()) {
        let spec = small(format);
        let out = bytes(&spec);
        let record_bytes = spec.record_bytes().unwrap() as usize;
        let bps = format.bytes_per_sample();
        let annotation_bytes =
            spec.annotation_samples_per_record().unwrap().unwrap() as usize * bps;
        let data = &out[spec.header_bytes() as usize..];

        let mut found = Vec::new();
        for (r, record) in data.chunks_exact(record_bytes).enumerate() {
            let tals = parse_tals(&record[record_bytes - annotation_bytes..]);
            let (onset, duration, texts) = &tals[0];
            assert_eq!(
                *onset,
                spec.record_onset(r as u64),
                "{format:?}, record {r}"
            );
            assert_eq!(
                (duration, texts.as_slice()),
                (&None, [String::new()].as_slice())
            );
            for tal in &tals[1..] {
                found.push((r, tal.clone()));
            }
        }

        // In the record that holds the onset (see below), in input order within a record.
        let record_of = |onset: f64| {
            (0..spec.n_records)
                .rev()
                .find(|&r| spec.record_onset(r) <= onset)
                .unwrap_or(0)
        };
        let mut expected: Vec<(u64, ParsedTal)> = spec
            .annotations
            .iter()
            .map(|a| (record_of(a.onset), (a.onset, a.duration, a.texts.clone())))
            .collect();
        expected.sort_by_key(|(r, _)| *r);
        let sorted_expected: Vec<_> = expected.into_iter().map(|(_, t)| t).collect();
        let tals: Vec<_> = found.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(tals, sorted_expected, "{format:?}");

        // Each annotation sits in the record whose span holds it (or the nearest one).
        for (r, (onset, ..)) in &found {
            let r = *r as u64;
            let last = spec.n_records - 1;
            assert!(
                r == 0 || spec.record_onset(r) <= *onset,
                "{onset} in record {r}"
            );
            assert!(
                r == last || *onset < spec.record_onset(r + 1),
                "{onset} in record {r}"
            );
        }
    }
}

#[test]
fn record_starts_follow_gaps() {
    let spec = small(Format::EdfPlusD);
    // Onsets: 0.125 + 0.5 r, plus 1.0 from record 3 and 0.3 more from record 4.
    assert_eq!(spec.record_onset(0), 0.125);
    assert_eq!(spec.record_onset(3), 0.125 + 1.5 + 1.0);
    assert_eq!(spec.record_onset(4), 0.125 + 2.0 + 1.0 + 0.3);
    // Channel 0 runs at 100 Hz: record 4 starts 2.0 + 1.3 = 3.3 s = 330 samples in.
    assert_eq!(spec.record_start(0, 4), 330);
    // Channel 1 runs at 20 Hz: 3.3 s is 66 samples.
    assert_eq!(spec.record_start(1, 4), 66);
    assert_eq!(spec.segments(0), [0..150, 250..300, 330..580]);
    assert_eq!(spec.len(0), 580);
    // Channel 2 runs at 50 Hz: 0.3 s is 15 samples.
    assert_eq!(spec.segments(2), [0..75, 125..150, 165..290]);

    let continuous = small(Format::EdfPlusC);
    assert_eq!(continuous.segments(0), [0..450]);
    assert_eq!(continuous.record_start(1, 4), 40);
}

#[test]
fn short_gaps_round_to_touching_segments_and_merge() {
    let mut spec = small(Format::EdfPlusD);
    spec.gaps = vec![GapSpec {
        before_record: 2,
        seconds: 0.001,
    }];
    // 0.1 samples at 100 Hz rounds away; the segments merge.
    assert_eq!(spec.segments(0), [0..450]);
}

#[test]
fn truth_function_spikes_are_the_only_extremes() {
    for format in [Format::Edf, Format::Bdf] {
        let mut spec = small(format);
        spec.annotations.clear();
        spec.spike_every = 997;
        let (lo, hi) = format.digital_limits();
        let (mut low_spikes, mut high_spikes) = (0, 0);
        for i in 0..200_000 {
            let v = spec.digital(0, i);
            assert_eq!(v == lo || v == hi, spec.is_spike(0, i), "sample {i}: {v}");
            low_spikes += usize::from(v == lo);
            high_spikes += usize::from(v == hi);
        }
        // About one in 997 samples, half of each sign.
        assert!((50..150).contains(&low_spikes), "{low_spikes}");
        assert!((50..150).contains(&high_spikes), "{high_spikes}");
        spec.spike_every = 0;
        assert!((0..200_000).all(|i| !spec.is_spike(0, i)));
    }
}

/// Every preset's signal stays within its channel's amplitude of the middle of the digital
/// range (15 uV of zero for EEG, so traces fit MNE's default scale), and spikes are rare.
#[test]
fn presets_stay_within_their_amplitude() {
    for &(name, _) in kymo_testgen::PRESETS {
        let spec = Spec::preset(name).unwrap();
        // The first 8 channels and the last of each preset (its channels are built alike).
        for (c, channel) in spec.channels.iter().enumerate() {
            if channel.trigger || (c >= 8 && c + 1 < spec.channels.len()) {
                continue;
            }
            // EEG within MNE's EEG scale; edf+c-kinds scales each type to its own.
            if channel.dimension == "uV" && name != "edf+c-kinds" {
                assert_eq!(channel.amplitude, kymo_testgen::EEG_AMPLITUDE, "{name} {c}");
            }
            let mid = (f64::from(channel.digital_min) + f64::from(channel.digital_max)) / 2.0;
            let physical = |d: f64| d * channel.gain() + channel.offset();
            let center = physical(mid);
            let n = spec.len(c).min(100_000);
            let mut spikes = 0;
            for i in 0..n {
                if spec.is_spike(c, i) {
                    spikes += 1;
                    continue;
                }
                let v = physical(f64::from(spec.digital(c, i)));
                assert!(
                    (v - center).abs() <= channel.amplitude + channel.gain().abs(),
                    "{name} channel {c} sample {i}: {v}"
                );
            }
            assert!(
                spikes <= n / 20_000 + 3,
                "{name} channel {c}: {spikes} spikes in {n}"
            );
        }
    }
}

#[test]
fn truth_function_is_deterministic_and_seeded() {
    let a = small(Format::Bdf);
    let mut b = small(Format::Bdf);
    let values = |s: &Spec| (0..1000).map(|i| s.digital(1, i)).collect::<Vec<_>>();
    assert_eq!(values(&a), values(&b));
    b.seed += 1;
    assert_ne!(values(&a), values(&b));
    // Channels differ too.
    let other: Vec<i32> = (0..1000).map(|i| a.digital(2, i)).collect();
    assert_ne!(values(&a), other);
}

#[test]
fn physical_uses_edf_scaling() {
    let spec = small(Format::Edf);
    // Resp: physical 100..-100 over digital -32768..32767, so the gain is negative.
    let resp = &spec.channels[1];
    assert!(resp.gain() < 0.0);
    assert_eq!(
        f64::from(resp.digital_min) * resp.gain() + resp.offset(),
        100.0
    );
    let i = 17;
    let expected = f64::from(spec.digital(1, i)) * resp.gain() + resp.offset();
    assert_eq!(spec.physical(1, i), expected);
    let dc = &spec.channels[2];
    assert!((dc.offset() - 1_000_000.0).abs() < 1.0);
}

#[test]
fn rejects_invalid_specs() {
    let cases: Vec<(Spec, &str)> = vec![
        (
            {
                let mut s = small(Format::Edf);
                s.annotations = small(Format::EdfPlusC).annotations;
                s
            },
            "annotations need",
        ),
        (
            {
                let mut s = small(Format::EdfPlusC);
                s.gaps = small(Format::EdfPlusD).gaps;
                s
            },
            "gaps need",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.channels[0].digital_max = 40_000;
                s
            },
            "digital range",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.channels[0].physical_min = 0.123_456_789;
                s
            },
            "longer than 8",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.channels[0].label = "a label longer than 16".into();
                s
            },
            "longer than 16",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.patient = "Jürgen".into();
                s
            },
            "printable ASCII",
        ),
        (
            {
                let mut s = small(Format::EdfPlusD);
                s.gaps.reverse();
                s
            },
            "sorted",
        ),
        (
            {
                let mut s = small(Format::EdfPlusC);
                s.annotations[0].texts[0] = "bad\x14text".into();
                s
            },
            "separator",
        ),
        (
            {
                let mut s = small(Format::EdfPlusC);
                s.annotations[1].duration = Some(-1.0);
                s
            },
            "duration",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.start = jiff::civil::date(2090, 1, 1).at(0, 0, 0, 0);
                s
            },
            "start year",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.first_onset = 0.5;
                s
            },
            "first onset",
        ),
        (
            {
                let mut s = small(Format::Bdf);
                s.channels[0].physical_max = s.channels[0].physical_min;
                s
            },
            "physical minimum equals",
        ),
        (
            {
                let mut s = small(Format::EdfPlusC);
                s.channels[1].label = "EDF Annotations".into();
                s
            },
            "annotation label",
        ),
        (
            {
                let mut s = small(Format::Edf);
                s.record_duration = 0.0;
                s
            },
            "record duration",
        ),
    ];
    for (spec, message) in cases {
        let error = kymo_testgen::write_to(&spec, &mut Vec::new()).unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains(message)),
            "expected {message:?}, got {error}"
        );
    }
}

#[test]
fn presets_are_valid_with_the_described_size() {
    for (name, description) in PRESETS {
        let spec = Spec::preset(name).unwrap();
        spec.validate().unwrap();
        let size = spec.file_size().unwrap();
        let mb = size as f64 / 1e6;
        let described = if mb >= 1000.0 {
            format!("({:.0} GB)", mb / 1000.0)
        } else {
            format!("({mb:.0} MB)")
        };
        if description.contains("MB)") || description.contains("GB)") {
            assert!(
                description.ends_with(&described),
                "{name}: {description} vs {described}"
            );
        }
    }
    assert!(Spec::preset("nope").is_none());

    // The small presets are written in full.
    for name in ["edf+d-mixed", "bdf+c-small"] {
        let spec = Spec::preset(name).unwrap();
        assert_eq!(bytes(&spec).len() as u64, spec.file_size().unwrap());
    }
}

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kymo-testgen"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_writes_a_preset() {
    let help = cli(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage"));

    let list = cli(&["--list"]);
    let listed = String::from_utf8_lossy(&list.stdout).into_owned();
    for (name, _) in PRESETS {
        assert!(listed.contains(name));
    }

    let path = temp_path("cli.bdf");
    let out = cli(&[
        "bdf+c-small",
        path.to_str().unwrap(),
        "--duration",
        "10",
        "--seed",
        "5",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    let mut spec = Spec::preset("bdf+c-small").unwrap();
    spec.n_records = 10;
    spec.annotations.retain(|a| a.onset < 10.0);
    spec.seed = 5;
    assert_eq!(written, bytes(&spec));

    for bad in [
        &["nope", "x.edf"][..],
        &["bdf+c-small"],
        &["bdf+c-small", "x.bdf", "--duration", "-3"],
        &["bdf+c-small", "x.bdf", "--frobnicate"],
    ] {
        let out = cli(bad);
        assert!(!out.status.success(), "{bad:?} should fail");
    }
}

/// A BrainVision spec of `format`: three channels, one with a comma in its name.
fn brainvision(format: Format) -> Spec {
    let channels = vec![
        ChannelSpec::eeg("Fz", 50, format),
        ChannelSpec::eeg("Cz, left", 50, format),
        ChannelSpec {
            dimension: "%".into(),
            ..ChannelSpec::eeg("SpO2", 50, format)
        },
    ];
    let mut spec = Spec::new(format, channels, 0.5, 9);
    spec.seed = 13;
    spec.spike_every = 11;
    spec.annotations = vec![
        Tal {
            onset: 0.2,
            duration: None,
            texts: vec!["Stimulus/S  1".into(), "a, b".into()],
        },
        Tal {
            onset: 1.0,
            duration: Some(0.5),
            texts: vec!["Bad Interval/Bad Min-Max".into()],
        },
    ];
    spec
}

/// The stored values of a BrainVision data file, as i64.
fn brainvision_values(bytes: &[u8], binary: Binary) -> Vec<i64> {
    match binary {
        Binary::Int16 => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i64::from(i16::from_le_bytes(*b)))
            .collect(),
        Binary::UInt16 => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i64::from(u16::from_le_bytes(*b)))
            .collect(),
        Binary::Int32 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i64::from(i32::from_le_bytes(*b)))
            .collect(),
        Binary::Float32 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b) as i64)
            .collect(),
    }
}

#[test]
fn brainvision_writes_three_files() {
    for format in Format::BRAINVISION {
        let Format::BrainVision(orientation, binary) = format else {
            unreachable!()
        };
        let spec = brainvision(format);
        let file = TempFile::written(&spec);
        let vhdr = std::fs::read_to_string(file.path()).unwrap();
        let eeg = std::fs::read(file.companion("eeg")).unwrap();
        let vmrk = std::fs::read_to_string(file.companion("vmrk")).unwrap();
        assert_eq!(eeg.len() as u64, spec.file_size().unwrap());

        let lines: Vec<&str> = vhdr.lines().collect();
        assert_eq!(
            lines[0],
            "Brain Vision Data Exchange Header File Version 1.0"
        );
        let orientation_line = match orientation {
            Orientation::Multiplexed => "DataOrientation=MULTIPLEXED",
            Orientation::Vectorized => "DataOrientation=VECTORIZED",
        };
        for expected in [
            orientation_line,
            &format!("BinaryFormat={}", binary.name()),
            "NumberOfChannels=3",
            "DataPoints=450",
            "SamplingInterval=10000",
            "Ch2=Cz\\1 left,,0.1,\u{b5}V",
            "Ch3=SpO2,,0.1,%",
        ] {
            assert!(
                lines.contains(&expected),
                "{format:?}: no {expected:?} in\n{vhdr}"
            );
        }

        // Samples in the file's order, against the truth function.
        let values = brainvision_values(&eeg, binary);
        for (k, v) in values.into_iter().enumerate() {
            let (c, i) = match orientation {
                Orientation::Multiplexed => (k % 3, k / 3),
                Orientation::Vectorized => (k / 450, k % 450),
            };
            assert_eq!(
                v,
                i64::from(spec.digital(c, i as u64)),
                "{format:?} {c} {i}"
            );
        }

        // A New Segment with the start date, then a marker per text, positions from 1.
        let markers: Vec<&str> = vmrk.lines().filter(|l| l.starts_with("Mk")).collect();
        assert_eq!(
            markers,
            [
                "Mk1=New Segment,,1,1,0,20240102030405000000",
                "Mk2=Stimulus,S  1,21,1,0",
                "Mk3=,a\\1 b,21,1,0",
                "Mk4=Bad Interval,Bad Min-Max,101,50,0",
            ]
        );
    }
}

#[test]
fn brainvision_specs_are_checked() {
    let format = Format::BrainVision(Orientation::Vectorized, Binary::Int32);
    let cases: Vec<(Spec, &str)> = vec![
        (
            {
                let mut s = brainvision(format);
                s.channels[1].samples_per_record = 25;
                s
            },
            "one rate",
        ),
        (
            {
                let mut s = brainvision(format);
                s.channels[0].physical_min += 1000.0;
                s
            },
            "no offsets",
        ),
        (
            {
                let mut s = brainvision(format);
                s.annotations[0].onset = 0.205;
                s
            },
            "on a sample",
        ),
        (
            {
                let mut s = brainvision(format);
                s.annotations[1].duration = Some(0.01);
                s
            },
            "at least 2 whole samples",
        ),
        (
            {
                let mut s = brainvision(format);
                s.channels[0].dimension = "µV,".into();
                s
            },
            "comma",
        ),
    ];
    for (spec, message) in cases {
        let path = temp_path("checked.vhdr");
        let error = kymo_testgen::write(&spec, &path).unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains(message)),
            "expected {message:?}, got {error}"
        );
    }
    // One stream can't hold three files, and there are no EDF records.
    let spec = brainvision(format);
    assert!(kymo_testgen::write_to(&spec, &mut Vec::new()).is_err());
    assert!(spec.record_bytes().is_err());
}
