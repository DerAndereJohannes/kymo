//! BrainVision recordings: samples against the truth function in every layout and sample
//! type, and headers and markers written by hand for each rule of the reader.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::common::{TempDir, TempFile};
use kymo_core::brainvision::BrainVisionError;
use kymo_core::pyramid::Sidecar;
use kymo_core::{
    AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType, SamplesMut,
};
use kymo_testgen::{ChannelSpec, Format, Spec};
use proptest::prelude::*;

fn open(path: &Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

/// Native samples as i64 (float32 files hold the truth function's integers exactly).
fn read_native(rec: &Recording, channels: &[ChannelId], range: Range<u64>) -> Vec<i64> {
    let n = channels.len() * (range.end - range.start) as usize;
    let grid = GridId(0);
    match rec.grid(grid).sample_type() {
        SampleType::I16 => {
            let mut out = vec![0i16; n];
            rec.read_native(grid, channels, range, SamplesMut::I16(&mut out))
                .unwrap();
            out.into_iter().map(i64::from).collect()
        }
        SampleType::I32 => {
            let mut out = vec![0i32; n];
            rec.read_native(grid, channels, range, SamplesMut::I32(&mut out))
                .unwrap();
            out.into_iter().map(i64::from).collect()
        }
        _ => {
            let mut out = vec![0f32; n];
            rec.read_native(grid, channels, range, SamplesMut::F32(&mut out))
                .unwrap();
            out.into_iter().map(|x| x as i64).collect()
        }
    }
}

fn truth(spec: &Spec, channels: &[ChannelId], range: Range<u64>) -> Vec<i64> {
    channels
        .iter()
        .flat_map(|c| {
            range
                .clone()
                .map(move |i| i64::from(spec.digital(c.0 as usize, i)))
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Case {
    spec: Spec,
    /// Channels (any order, repeats allowed) and a sample range per read.
    reads: Vec<(Vec<usize>, Range<u64>)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::select(Format::BRAINVISION.to_vec()),
        1usize..7,
        proptest::sample::select(vec![1u32, 3, 50, 256]),
        1u64..8,
        any::<u64>(),
        proptest::collection::vec(
            (
                proptest::collection::vec(any::<prop::sample::Index>(), 1..5),
                (any::<prop::sample::Index>(), any::<prop::sample::Index>()),
            ),
            1..6,
        ),
    )
        .prop_map(|(format, n, spr, records, seed, picks)| {
            let channels = (0..n)
                .map(|i| ChannelSpec::eeg(format!("E{i}"), spr, format))
                .collect();
            let mut spec = Spec::new(format, channels, 1.0, records);
            spec.seed = seed;
            // Spikes often, so the digital limits (uint16's 65535, say) are read too.
            spec.spike_every = 7;
            let len = records * u64::from(spr);
            let reads = picks
                .into_iter()
                .map(|(chans, (a, b))| {
                    let chans = chans.iter().map(|c| c.index(n)).collect();
                    let (a, b) = (
                        a.index(len as usize + 1) as u64,
                        b.index(len as usize + 1) as u64,
                    );
                    (chans, a.min(b)..a.max(b))
                })
                .collect();
            Case { spec, reads }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Every layout and sample type reads back the truth function, natively and in
    /// physical units, for any channels and range.
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let file = TempFile::written(&case.spec);
        let (rec, _) = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        for (chans, range) in &case.reads {
            let ids: Vec<ChannelId> = chans.iter().map(|&c| ChannelId(c as u32)).collect();
            prop_assert_eq!(
                read_native(&rec, &ids, range.clone()),
                truth(&case.spec, &ids, range.clone())
            );
            let mut physical = vec![0.0f32; ids.len() * (range.end - range.start) as usize];
            rec.read_physical(GridId(0), &ids, range.clone(), &mut physical).unwrap();
            let expected: Vec<f32> = ids
                .iter()
                .flat_map(|c| range.clone().map(|i| case.spec.physical(c.0 as usize, i) as f32))
                .collect();
            prop_assert_eq!(physical, expected);
            // Reading ahead never fails, whatever the range.
            rec.prefetch(GridId(0), range.clone());
        }
    }
}

#[test]
fn presets_open_with_their_channels_markers_and_start() {
    for preset in [
        "bv-int16-multiplexed",
        "bv-int32-vectorized",
        "bv-float32-multiplexed",
    ] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let (rec, annotations) = open(file.path());
        assert_eq!(rec.format(), "BrainVision");
        assert!(rec.warnings().is_empty(), "{preset}: {:?}", rec.warnings());
        assert_eq!(
            rec.sources(),
            [file.path().to_owned(), file.companion("eeg").to_owned()]
        );
        assert_eq!(rec.start_time(), Some(spec.start), "{preset}");
        let grid = rec.grid(GridId(0));
        assert_eq!(grid.len(), spec.len(0));
        // Whole channels: reads that span many of the reader's pieces.
        let ids = grid.channels().to_vec();
        assert_eq!(
            read_native(&rec, &ids, 0..grid.len()),
            truth(&spec, &ids, 0..grid.len()),
            "{preset}"
        );
        assert!((grid.rate() - spec.rate(0)).abs() < 1e-9 * spec.rate(0));
        for (channel, c) in rec.channels().iter().zip(&spec.channels) {
            assert_eq!(channel.name, c.label);
            assert_eq!(channel.unit.label, c.dimension);
            assert_eq!(channel.gain, c.gain());
            assert_eq!(channel.offset, 0.0);
        }
        // Each text is one marker, its time on whole samples.
        let expected: Vec<(f64, f64, String)> = spec
            .annotations
            .iter()
            .flat_map(|a| {
                let duration = a.duration.unwrap_or(0.0);
                a.texts.iter().map(move |t| (a.onset, duration, t.clone()))
            })
            .collect();
        let got: Vec<(f64, f64, String)> = annotations
            .iter()
            .map(|a| {
                assert!(a.channels.is_empty());
                (a.onset, a.duration, a.description.clone())
            })
            .collect();
        assert_eq!(got.len(), expected.len(), "{preset}");
        for (g, e) in got.iter().zip(&expected) {
            assert!(
                (g.0 - e.0).abs() < 1e-9 && (g.1 - e.1).abs() < 1e-9,
                "{g:?} {e:?}"
            );
            assert_eq!(g.2, e.2);
        }
    }
    // Kinds from names and units, and both spellings of µ.
    let kinds = |preset: &str| {
        let file = TempFile::preset(preset);
        let rec = open(file.path()).0;
        rec.channels()
            .iter()
            .map(|c| (c.kind, c.unit.to_si))
            .collect::<Vec<_>>()
    };
    use ChannelKind::*;
    let uv = Some(1e-6);
    assert_eq!(
        kinds("bv-int16-multiplexed"),
        [
            (Eeg, uv),
            (Eeg, uv),
            (Eeg, uv),
            (Eeg, uv),
            (Eeg, uv),
            (Eog, uv),
            (Ecg, uv),
            (Misc, None)
        ]
    );
    assert_eq!(
        kinds("bv-float32-multiplexed"),
        [(Eeg, uv), (Eeg, uv), (Eog, uv), (Emg, uv)]
    );
}

/// A recording written by hand: `rec.vhdr`, `rec.eeg` and, if given, `rec.vmrk`.
struct Written {
    dir: TempDir,
}

impl Written {
    fn new(vhdr: &[u8], eeg: &[u8], vmrk: Option<&[u8]>) -> Written {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("rec.vhdr"), vhdr).unwrap();
        std::fs::write(dir.path().join("rec.eeg"), eeg).unwrap();
        if let Some(vmrk) = vmrk {
            std::fs::write(dir.path().join("rec.vmrk"), vmrk).unwrap();
        }
        Written { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn open(&self) -> Result<(Recording, Vec<AnnotationData>), OpenError> {
        kymo_core::open(&self.path("rec.vhdr")).map(|o| Recording::new(o).unwrap())
    }

    fn error(&self) -> BrainVisionError {
        match self.open() {
            Err(OpenError::BrainVision { source, .. }) => source,
            other => panic!("expected a BrainVision error, got {other:?}"),
        }
    }
}

/// A header with `common` lines in `[Common Infos]` after the identification, and the
/// given binary format and channel lines.
fn header(common: &str, binary: &str, channels: &str) -> String {
    format!(
        "Brain Vision Data Exchange Header File Version 1.0\r\n\
         ; Data created by a test\r\n\
         \r\n\
         [Common Infos]\r\n\
         {common}\
         \r\n\
         [Binary Infos]\r\n\
         BinaryFormat={binary}\r\n\
         \r\n\
         [Channel Infos]\r\n\
         ; Each entry: Ch<Channel number>=<Name>,<Reference channel name>,\r\n\
         {channels}"
    )
}

const COMMON: &str = "Codepage=UTF-8\r\nDataFile=rec.eeg\r\n\
                      DataFormat=BINARY\r\nDataOrientation=MULTIPLEXED\r\n\
                      NumberOfChannels=2\r\nSamplingInterval=2000\r\n";

/// [`COMMON`] naming the marker file `rec.vmrk`.
fn with_markers() -> String {
    COMMON.to_owned() + "MarkerFile=rec.vmrk\r\n"
}

fn int16s(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn markers(lines: &str) -> String {
    format!(
        "Brain Vision Data Exchange Marker File, Version 1.0\r\n\r\n[Common Infos]\r\n\
         Codepage=UTF-8\r\nDataFile=rec.eeg\r\n\r\n[Marker Infos]\r\n{lines}"
    )
}

#[test]
fn channel_lines_give_names_units_scaling_and_kinds() {
    let channels = "Ch1=Fp1\\1a,REF,0.5,\u{b5}V\r\n\
                    Ch2=Cz\r\n\
                    Ch3= Dummy\t\t ,,,\r\n\
                    Ch4=Temp,,2,\u{b0}C\r\n\
                    Ch5=VEOGb,,1,uV\r\n\
                    Ch6=EKG,,1,mV\r\n\
                    Ch7=EMG1,,1,nV\r\n\
                    Ch8=EOG,,1,AU\r\n\
                    Ch9=B2A,,1.0,\u{3bc}V\r\n\
                    Ch10=,,1,V\r\n\
                    Ch11=Out,,1,V\r\n";
    let common = COMMON.replace("NumberOfChannels=2", "NumberOfChannels=11");
    let vhdr = header(&common, "INT_16", channels)
        + "\r\n[Coordinates]\r\nCh1=1,-90,-72\r\nCh11=0,0,0\r\n";
    let written = Written::new(vhdr.as_bytes(), &int16s(&[0; 22]), None);
    let (rec, _) = written.open().unwrap();
    let got: Vec<(&str, ChannelKind, &str, Option<f64>, f64)> = rec
        .channels()
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.kind,
                c.unit.label.as_str(),
                c.unit.to_si,
                c.gain,
            )
        })
        .collect();
    use ChannelKind::*;
    assert_eq!(
        got,
        [
            ("Fp1,a", Eeg, "\u{b5}V", Some(1e-6), 0.5),
            // No resolution and unit: 1 µV per step, as the spec defaults.
            ("Cz", Eeg, "\u{b5}V", Some(1e-6), 1.0),
            ("Dummy", Eeg, "\u{b5}V", Some(1e-6), 1.0),
            ("Temp", Misc, "\u{b0}C", None, 2.0),
            ("VEOGb", Eog, "uV", Some(1e-6), 1.0),
            ("EKG", Ecg, "mV", Some(1e-3), 1.0),
            ("EMG1", Emg, "nV", Some(1e-9), 1.0),
            // A name means nothing without a voltage.
            ("EOG", Misc, "AU", None, 1.0),
            // The Greek mu is a micro too.
            ("B2A", Eeg, "\u{3bc}V", Some(1e-6), 1.0),
            ("Ch10", Eeg, "V", Some(1.0), 1.0),
            // Placed at the origin by the montage: not on the scalp.
            ("Out", Misc, "V", Some(1.0), 1.0),
        ]
    );
    assert!(rec.channels().iter().all(|c| c.offset == 0.0));
    let extra = |key: &str| {
        rec.extra()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(extra("created").as_deref(), Some("Data created by a test"));
    assert_eq!(extra("orientation").as_deref(), Some("multiplexed"));
    assert_eq!(extra("binary format").as_deref(), Some("INT_16"));
}

#[test]
fn codepages_decode_names() {
    // "Pz€" with a micro sign unit, in each encoding.
    let latin_1_name = b"Ch1=P\xe4,,1,\xb5V\r\nCh2=Oz\r\n";
    let windows_1252_name = b"Ch1=P\x80,,1,\xb5V\r\nCh2=Oz\r\n";
    let cases: [(&str, &[u8], &str, &str, bool); 6] = [
        (
            "UTF-8",
            "Ch1=Pä,,1,µV\r\nCh2=Oz\r\n".as_bytes(),
            "Pä",
            "\u{b5}V",
            false,
        ),
        ("latin-1", latin_1_name, "P\u{e4}", "\u{b5}V", false),
        ("ANSI", windows_1252_name, "P\u{20ac}", "\u{b5}V", false),
        // No codepage and not UTF-8: Windows-1252, as older files are.
        ("", windows_1252_name, "P\u{20ac}", "\u{b5}V", false),
        // UTF-8 said, but not UTF-8: Windows-1252 with a warning.
        ("UTF-8", windows_1252_name, "P\u{20ac}", "\u{b5}V", true),
        // A UTF-8 micro sign read as Windows-1252 ("Âµ") is a micro sign.
        (
            "ANSI",
            b"Ch1=Pz,,1,\xc2\xb5V\r\nCh2=Oz\r\n",
            "Pz",
            "\u{b5}V",
            false,
        ),
    ];
    for (codepage, channels, name, unit, warned) in cases {
        let common = match codepage {
            "" => COMMON.replace("Codepage=UTF-8\r\n", ""),
            c => COMMON.replace("UTF-8", c),
        };
        let mut vhdr = header(&common, "INT_16", "").into_bytes();
        vhdr.extend_from_slice(channels);
        let written = Written::new(&vhdr, &int16s(&[0; 4]), None);
        let (rec, _) = written.open().unwrap();
        assert_eq!(rec.channels()[0].name, name, "{codepage:?}");
        assert_eq!(rec.channels()[0].unit.label, unit, "{codepage:?}");
        assert_eq!(rec.channels()[0].unit.to_si, Some(1e-6), "{codepage:?}");
        assert_eq!(
            !rec.warnings().is_empty(),
            warned,
            "{codepage:?}: {:?}",
            rec.warnings()
        );
    }
    // A byte-order mark means UTF-8.
    let vhdr = [
        b"\xef\xbb\xbf".as_slice(),
        header(
            &COMMON.replace("Codepage=UTF-8\r\n", ""),
            "INT_16",
            "Ch1=P\u{e4}\r\nCh2=Oz\r\n",
        )
        .as_bytes(),
    ]
    .concat();
    let (rec, _) = Written::new(&vhdr, &int16s(&[0; 4]), None).open().unwrap();
    assert_eq!(rec.channels()[0].name, "P\u{e4}");
}

#[test]
fn version_2_headers_and_their_spellings() {
    let vhdr = "BrainVision Data Exchange Header File Version 2.0\n\
                [Common infos]\n\
                codepage=UTF-8\n\
                datafile=$b.eeg\n\
                DATAFORMAT=binary\n\
                DataOrientation=Vectorized\n\
                DataType=TIMEDOMAIN\n\
                NumberOfChannels=1\n\
                DataPoints=3\n\
                SamplingInterval=1999.9740292197562\n\
                [binary infos]\n\
                BinaryFormat=UINT_16\n\
                [channel infos]\n\
                ch1=A,,0.5,\u{b5}V\n";
    let written = Written::new(vhdr.as_bytes(), &[1, 0, 0xff, 0xff, 0, 0x80], None);
    let (rec, annotations) = written.open().unwrap();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert!(annotations.is_empty());
    let grid = rec.grid(GridId(0));
    assert_eq!(grid.rate(), 1e6 / 1999.9740292197562);
    assert_eq!(grid.sample_type(), SampleType::I32);
    assert_eq!(read_native(&rec, &[ChannelId(0)], 0..3), [1, 65535, 32768]);
    let version = rec.extra().iter().find(|(k, _)| k == "version").unwrap();
    assert_eq!(version.1, "2.0");
}

#[test]
fn headers_kymo_cannot_read_are_errors() {
    use BrainVisionError::*;
    let channels = "Ch1=A\r\nCh2=B\r\n";
    let common = |from: &str, to: &str| COMMON.replace(from, to);
    type Check = fn(&BrainVisionError) -> bool;
    let cases: Vec<(String, Check)> = vec![
        (
            header(&common("BINARY", "ASCII"), "INT_16", channels),
            |e| matches!(e, Unsupported(m) if m.contains("ASCII")),
        ),
        (
            header(COMMON, "IEEE_FLOAT_64", channels),
            |e| matches!(e, Unsupported(m) if m.contains("IEEE_FLOAT_64")),
        ),
        (
            header(
                &(COMMON.to_owned() + "DataType=FREQUENCYDOMAIN\r\n"),
                "INT_16",
                channels,
            ),
            |e| matches!(e, Unsupported(m) if m.contains("FREQUENCYDOMAIN")),
        ),
        (
            header(&common("MULTIPLEXED", "SIDEWAYS"), "INT_16", channels),
            |e| {
                matches!(
                    e,
                    Invalid {
                        key: "DataOrientation",
                        ..
                    }
                )
            },
        ),
        (
            header(
                &common("SamplingInterval=2000", "SamplingInterval=0"),
                "INT_16",
                channels,
            ),
            |e| {
                matches!(
                    e,
                    Invalid {
                        key: "SamplingInterval",
                        ..
                    }
                )
            },
        ),
        (
            header(
                &common("NumberOfChannels=2", "NumberOfChannels=0"),
                "INT_16",
                channels,
            ),
            |e| {
                matches!(
                    e,
                    Invalid {
                        key: "NumberOfChannels",
                        ..
                    }
                )
            },
        ),
        (
            header(&common("DataFile=rec.eeg\r\n", ""), "INT_16", channels),
            |e| {
                matches!(
                    e,
                    Missing {
                        key: "DataFile",
                        ..
                    }
                )
            },
        ),
        (
            header(&common("SamplingInterval=2000\r\n", ""), "INT_16", channels),
            |e| {
                matches!(
                    e,
                    Missing {
                        key: "SamplingInterval",
                        ..
                    }
                )
            },
        ),
        (header(COMMON, "INT_16", "Ch1=A\r\nCh3=C\r\n"), |e| {
            matches!(e, MissingChannel(2))
        }),
        (header(COMMON, "INT_16", "Ch1=A,,huge\r\nCh2=B\r\n"), |e| {
            matches!(e, InvalidChannel { index: 1, .. })
        }),
        (
            header(
                &common("DataFile=rec.eeg", "DataFile=nowhere.dat"),
                "INT_16",
                channels,
            )
            .replace("rec.eeg", "nowhere.dat"),
            |e| matches!(e, NoDataFile(_)),
        ),
    ];
    for (vhdr, expected) in cases {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("rec.vhdr"), &vhdr).unwrap();
        if !vhdr.contains("nowhere") {
            std::fs::write(dir.path().join("rec.eeg"), int16s(&[0; 4])).unwrap();
        } else {
            // Neither the named file nor one under the header's name.
            std::fs::write(dir.path().join("other.eeg"), int16s(&[0; 4])).unwrap();
        }
        match kymo_core::open(&dir.path().join("rec.vhdr")) {
            Err(OpenError::BrainVision { source, .. }) => {
                assert!(expected(&source), "{source:?} for\n{vhdr}");
            }
            other => panic!("expected an error, got {other:?} for\n{vhdr}"),
        }
    }
}

#[test]
fn filters_come_from_the_amplifier_tables() {
    let channels = "Ch1=Fp1,,0.0488281,\u{b5}V\r\nCh2=AUX 1,,0.0488281,\u{b5}V\r\n";
    let comment = "\r\n[Comment]\r\n\
                   A m p l i f i e r  S e t u p\r\n\
                   ============================\r\n\
                   Number of channels: 2\r\n\r\n\
                   Channels\r\n\
                   --------\r\n\
                   #     Name      Phys. Chn.    Resolution / Unit   Low Cutoff [s]   \
                   High Cutoff [Hz]   Notch [Hz]\r\n\
                   1     Fp1         1          0.0488281 \u{b5}V             DC              \
                   140              Off\r\n\
                   2     AUX 1       2          0.0488281 \u{b5}V             10              \
                   1000             50\r\n\
                   \r\n\
                   S o f t w a r e  F i l t e r s\r\n\
                   ==============================\r\n\
                   #     Low Cutoff [s]   High Cutoff [Hz]   Notch [Hz]\r\n\
                   1      0.1592          70              Off\r\n\
                   \r\n";
    let vhdr = header(COMMON, "INT_16", channels) + comment;
    let (rec, _) = Written::new(vhdr.as_bytes(), &int16s(&[0; 4]), None)
        .open()
        .unwrap();
    let filters: Vec<&str> = rec
        .channels()
        .iter()
        .map(|c| c.prefilter.as_str())
        .collect();
    assert_eq!(
        filters,
        [
            "HP:DC LP:140Hz; software HP:1Hz LP:70Hz",
            // A time constant of 10 s: 1 / (2π · 10 s).
            "HP:0.0159Hz LP:1000Hz N:50Hz"
        ]
    );
    // A "Channels" heading over a table of another shape (MNE fails on it) is ignored, as
    // is a section heading inside the comment.
    let comment = "\r\n[Comment]\r\nChannels\r\n--------\r\n# Name\r\n1 Fp1\r\n[Notes]\r\nx=y\r\n";
    let vhdr = header(COMMON, "INT_16", channels) + comment;
    let (rec, _) = Written::new(vhdr.as_bytes(), &int16s(&[0; 4]), None)
        .open()
        .unwrap();
    assert!(rec.channels().iter().all(|c| c.prefilter.is_empty()));
}

/// Annotations as (onset, duration, description, channels).
fn summary(annotations: &[AnnotationData]) -> Vec<(f64, f64, &str, Vec<u32>)> {
    annotations
        .iter()
        .map(|a| {
            (
                a.onset,
                a.duration,
                a.description.as_str(),
                a.channels.iter().map(|c| c.0).collect(),
            )
        })
        .collect()
}

#[test]
fn markers_become_annotations() {
    let vhdr = header(&with_markers(), "INT_16", "Ch1=A\r\nCh2=B\r\n");
    let vmrk = markers(
        "Mk1=New Segment,,1,1,0,20240208115841181247\r\n\
         Mk2=Stimulus,S  1,11,1,0\r\n\
         Mk3=Response,R\\1 2,21,0,0\r\n\
         Mk4=Bad Interval,Bad Min-Max,31,10,2\r\n\
         Mk5=Comment,,41,1,0\r\n\
         Mk6=,lonely,51,1,0\r\n\
         Mk7=New Segment,,61,1,0,20240208115842181247\r\n\
         Mk8=Stimulus,S  2,not a position,1,0\r\n\
         Mk9=Stimulus,S  3,71,1,9\r\n",
    );
    let written = Written::new(vhdr.as_bytes(), &int16s(&[0; 200]), Some(vmrk.as_bytes()));
    let (rec, annotations) = written.open().unwrap();
    // 500 Hz: position p is (p − 1) / 500 s.
    assert_eq!(
        summary(&annotations),
        [
            (0.02, 0.0, "Stimulus/S  1", vec![]),
            (0.04, 0.0, "Response/R, 2", vec![]),
            (0.06, 0.02, "Bad Interval/Bad Min-Max", vec![1]),
            (0.08, 0.0, "Comment", vec![]),
            (0.1, 0.0, "lonely", vec![]),
            // Later segments are kept; the data stays continuous across them.
            (0.12, 0.0, "New Segment", vec![]),
            // A channel the header doesn't have means all of them.
            (0.14, 0.0, "Stimulus/S  3", vec![]),
        ]
    );
    assert_eq!(
        rec.start_time(),
        Some(jiff::civil::date(2024, 2, 8).at(11, 58, 41, 181_247_000))
    );
    let warnings = rec.warnings().join("\n");
    assert!(
        warnings.contains("1 markers without a valid position"),
        "{warnings}"
    );
    assert!(warnings.contains("name a channel"), "{warnings}");
}

#[test]
fn the_start_time_is_the_first_samples() {
    let vhdr = header(&with_markers(), "INT_16", "Ch1=A\r\nCh2=B\r\n");
    let cases = [
        // Dated at position 501, 1 s in: the first sample is a second earlier.
        (
            "Mk1=New Segment,,501,1,0,20240208115842000000\r\n",
            Some(jiff::civil::date(2024, 2, 8).at(11, 58, 41, 0)),
        ),
        // No date, or a date of zeros.
        ("Mk1=New Segment,,1,1,0\r\n", None),
        ("Mk1=New Segment,,1,1,0,00000000000000000000\r\n", None),
        ("Mk1=New Segment,,1,1,0,0\r\n", None),
        // The first dated segment counts.
        (
            "Mk1=New Segment,,1,1,0,0\r\nMk2=New Segment,,1,1,0,20200101000000000000\r\n",
            Some(jiff::civil::date(2020, 1, 1).at(0, 0, 0, 0)),
        ),
    ];
    for (lines, start) in cases {
        let vmrk = markers(lines);
        let written = Written::new(vhdr.as_bytes(), &int16s(&[0; 2000]), Some(vmrk.as_bytes()));
        let (rec, _) = written.open().unwrap();
        assert_eq!(rec.start_time(), start, "{lines}");
        assert!(rec.warnings().is_empty(), "{lines}: {:?}", rec.warnings());
    }
    let vmrk = markers("Mk1=New Segment,,1,1,0,20241399000000000000\r\n");
    let written = Written::new(vhdr.as_bytes(), &int16s(&[0; 4]), Some(vmrk.as_bytes()));
    let (rec, _) = written.open().unwrap();
    assert_eq!(rec.start_time(), None);
    assert!(rec.warnings()[0].contains("invalid New Segment date"));
}

#[test]
fn companion_files_are_found_after_renames() {
    let vmrk = markers("Mk1=Stimulus,S  1,2,1,0\r\n");
    // Names from before a rename: the files under the header's own name are used.
    let renamed = header(
        &with_markers()
            .replace("rec.eeg", "old.eeg")
            .replace("rec.vmrk", "old.vmrk"),
        "INT_16",
        "Ch1=A\r\nCh2=B\r\n",
    );
    let written = Written::new(renamed.as_bytes(), &int16s(&[0; 4]), Some(vmrk.as_bytes()));
    let (rec, annotations) = written.open().unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(rec.sources()[1], written.path("rec.eeg"));
    let warnings = rec.warnings().join("\n");
    assert!(warnings.contains("\"old.eeg\" not found"), "{warnings}");
    assert!(warnings.contains("\"old.vmrk\" not found"), "{warnings}");

    // A Windows path: its last component, beside the header.
    let windows = header(
        &with_markers().replace("DataFile=rec.eeg", "DataFile=C:\\Data\\rec.eeg"),
        "INT_16",
        "Ch1=A\r\nCh2=B\r\n",
    );
    let written = Written::new(windows.as_bytes(), &int16s(&[0; 4]), Some(vmrk.as_bytes()));
    let (rec, _) = written.open().unwrap();
    assert_eq!(rec.sources()[1], written.path("rec.eeg"));

    // A missing marker file means no annotations, with a warning; none named, no warning.
    let vhdr = header(&with_markers(), "INT_16", "Ch1=A\r\nCh2=B\r\n");
    let (rec, annotations) = Written::new(vhdr.as_bytes(), &int16s(&[0; 4]), None)
        .open()
        .unwrap();
    assert!(annotations.is_empty());
    assert!(
        rec.warnings()[0].contains("no annotations"),
        "{:?}",
        rec.warnings()
    );
    let unnamed = header(COMMON, "INT_16", "Ch1=A\r\nCh2=B\r\n");
    let (rec, _) = Written::new(unnamed.as_bytes(), &int16s(&[0; 4]), None)
        .open()
        .unwrap();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
}

#[test]
fn data_file_sizes() {
    let vectorized = |points: &str| {
        let common = COMMON.replace("MULTIPLEXED", "VECTORIZED") + points;
        header(&common, "INT_16", "Ch1=A\r\nCh2=B\r\n")
    };
    let multiplexed = |points: &str| {
        header(
            &(COMMON.to_owned() + points),
            "INT_16",
            "Ch1=A\r\nCh2=B\r\n",
        )
    };
    let samples = int16s(&[1, 2, 3, 4, 5, 6]);
    let mut odd = samples.clone();
    odd.push(7);
    /// Header, data, samples per channel, channel 1's samples, a warning containing.
    type Case<'a> = (String, Vec<u8>, u64, Vec<i64>, Option<&'a str>);
    let cases: Vec<Case> = vec![
        (multiplexed(""), samples.clone(), 3, vec![2, 4, 6], None),
        (
            multiplexed(""),
            odd.clone(),
            3,
            vec![2, 4, 6],
            Some("incomplete sample"),
        ),
        (
            multiplexed("DataPoints=2\r\n"),
            samples.clone(),
            2,
            vec![2, 4],
            Some("more than"),
        ),
        (
            multiplexed("DataPoints=4\r\n"),
            samples.clone(),
            3,
            vec![2, 4, 6],
            Some("fewer than"),
        ),
        (
            vectorized("DataPoints=3\r\n"),
            samples.clone(),
            3,
            vec![4, 5, 6],
            None,
        ),
        (
            vectorized("DataPoints=2\r\n"),
            samples.clone(),
            2,
            vec![3, 4],
            Some("ignored"),
        ),
        (
            vectorized(""),
            samples.clone(),
            3,
            vec![4, 5, 6],
            Some("no DataPoints"),
        ),
    ];
    for (vhdr, data, len, second, warning) in cases {
        let (rec, _) = Written::new(vhdr.as_bytes(), &data, None).open().unwrap();
        let warnings: Vec<&String> = rec
            .warnings()
            .iter()
            .filter(|w| !w.contains("no annotations"))
            .collect();
        assert_eq!(rec.grid(GridId(0)).len(), len, "{vhdr}");
        assert_eq!(read_native(&rec, &[ChannelId(1)], 0..len), second, "{vhdr}");
        match warning {
            Some(w) => assert!(warnings.iter().any(|x| x.contains(w)), "{warnings:?}"),
            None => assert!(warnings.is_empty(), "{warnings:?}"),
        }
    }
    // Too short for DataPoints: the last channel would be cut off.
    let written = Written::new(vectorized("DataPoints=4\r\n").as_bytes(), &samples, None);
    assert!(matches!(
        written.error(),
        BrainVisionError::Truncated {
            needed: 16,
            found: 12
        }
    ));
}

#[test]
fn the_sidecar_follows_the_header_and_samples_but_not_the_markers() {
    let file = TempFile::preset("bv-int16-multiplexed");
    let dir = TempDir::new();
    let (rec, _) = open(file.path());
    let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
    assert!(sidecar.build(&rec).unwrap());
    drop((sidecar, rec));
    let complete = || {
        let (rec, _) = open(file.path());
        Sidecar::open_or_create(dir.path(), &rec)
            .unwrap()
            .is_complete()
    };
    assert!(complete());
    // Editing markers keeps the pyramid.
    let vmrk = file.companion("vmrk");
    let text = std::fs::read_to_string(vmrk).unwrap();
    std::fs::write(vmrk, text.replace("S  1", "S 99")).unwrap();
    assert!(complete());
    // Changed samples don't.
    let eeg = file.companion("eeg");
    let mut bytes = std::fs::read(eeg).unwrap();
    bytes[0] ^= 1;
    std::fs::write(eeg, bytes).unwrap();
    assert!(!complete());
}
