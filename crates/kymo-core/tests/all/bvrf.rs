//! BVRF recordings: samples against the truth function for every data type and scaling,
//! and headers and markers written by hand for each rule of the reader.

use std::ops::Range;
use std::path::PathBuf;

use crate::common::{TempDir, TempFile};
use kymo_core::bvrf::BvrfError;
use kymo_core::{
    AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Recording, SampleType, SamplesMut,
};
use kymo_testgen::{BvrfData, BvrfScaling, ChannelSpec, Format, Spec};
use proptest::prelude::*;

fn open(path: &std::path::Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

/// Native samples as i64: the stored integers, and float64 relative to each channel's
/// first sample (its reference).
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

/// The truth function's values, relative to each channel's first sample for float64.
fn truth(spec: &Spec, channels: &[ChannelId], range: Range<u64>) -> Vec<i64> {
    let relative = spec.format == Format::Bvrf(BvrfData::Double);
    channels
        .iter()
        .flat_map(|c| {
            let c = c.0 as usize;
            let reference = if relative { spec.digital(c, 0) } else { 0 };
            range
                .clone()
                .map(move |i| i64::from(spec.digital(c, i)) - i64::from(reference))
        })
        .collect()
}

/// Whether `got` is `expected` within float32 rounding of the larger of it and `scale`
/// (the channel's reference and offset, which the reader adds in float64; and the
/// resolution, written as the gain over a factor and multiplied back).
fn close(got: f32, expected: f64, scale: f64) -> bool {
    let size = expected.abs().max(scale.abs());
    (f64::from(got) - expected).abs() <= 2.0 * f64::from(f32::EPSILON) * size
}

#[derive(Debug, Clone)]
struct Case {
    spec: Spec,
    /// Channels (any order, repeats allowed) and a sample range per read.
    reads: Vec<(Vec<usize>, Range<u64>)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::sample::select(Format::BVRF.to_vec()),
        proptest::collection::vec((0usize..3, any::<bool>()), 1..7),
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
        .prop_map(|(format, kinds, spr, records, seed, picks)| {
            let n = kinds.len();
            let mut channels = Vec::new();
            let mut scaling = Vec::new();
            for (i, &(k, shifted)) in kinds.iter().enumerate() {
                let mut c = ChannelSpec::eeg(format!("E{i}"), spr, format);
                // An offset, written as linear coefficients.
                if shifted {
                    c.physical_min += 1000.0;
                    c.physical_max += 1000.0;
                }
                channels.push(c);
                scaling.push(match k {
                    0 => BvrfScaling::Resolution,
                    1 => BvrfScaling::Linear(0.001),
                    _ => BvrfScaling::Linear(50.0),
                });
            }
            let mut spec = Spec::new(format, channels, 1.0, records);
            spec.bvrf.scaling = scaling;
            spec.seed = seed;
            // Spikes often, so the digital limits are read too.
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

    /// Every data type reads back the truth function, natively and in physical units with
    /// any offset and linear coefficients, for any channels and range.
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
            let n = (range.end - range.start) as usize;
            let mut physical = vec![0.0f32; ids.len() * n];
            rec.read_physical(GridId(0), &ids, range.clone(), &mut physical).unwrap();
            for (k, c) in ids.iter().enumerate() {
                let channel = rec.channel(*c);
                for (j, i) in range.clone().enumerate() {
                    let expected = case.spec.physical(c.0 as usize, i);
                    prop_assert!(
                        close(physical[k * n + j], expected, channel.offset),
                        "channel {} sample {i}: {} for {expected}", c.0, physical[k * n + j]
                    );
                }
            }
            // Reading ahead never fails, whatever the range.
            rec.prefetch(GridId(0), range.clone());
        }
    }
}

#[test]
fn presets_open_with_their_channels_markers_and_start() {
    for preset in ["bvrf-int16", "bvrf-float64"] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let (rec, annotations) = open(file.path());
        assert_eq!(rec.format(), "BVRF");
        let rational = preset == "bvrf-float64";
        let expected_warnings: &[&str] = if rational {
            &[
                "channel \"GSR\": its coefficients aren't linear, so it shows the sensor output \
               in mV rather than \u{b5}S",
            ]
        } else {
            &[]
        };
        assert_eq!(rec.warnings(), expected_warnings, "{preset}");
        // Not the markers, so editing them keeps the pyramid.
        assert_eq!(
            rec.sources(),
            [file.path().to_owned(), file.companion("bvrd").to_owned()]
        );
        assert_eq!(rec.start_time(), Some(spec.start), "{preset}");
        let grid = rec.grid(GridId(0));
        assert_eq!(grid.len(), spec.len(0));
        assert_eq!(grid.rate(), spec.rate(0));
        // Whole channels: reads that span many of the reader's pieces.
        let ids = grid.channels().to_vec();
        assert_eq!(
            read_native(&rec, &ids, 0..grid.len()),
            truth(&spec, &ids, 0..grid.len()),
            "{preset}"
        );
        for (channel, c) in rec.channels().iter().zip(&spec.channels) {
            assert_eq!(channel.name, c.label);
            assert_eq!(channel.unit.label, c.dimension);
            assert!(close(channel.gain as f32, c.gain(), 0.0), "{}", c.label);
        }
        // Each text is one marker, its time on whole samples; start-end pairs are one
        // annotation with a duration; Recording Info markers aren't annotations.
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
                assert_eq!(a.stream, None);
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
        assert!(got.iter().any(|g| g.1 > 0.0), "{preset}: no start-end pair");
    }
    // Kinds from the types, the photo sensor in mV through its coefficient, the offset.
    let file = TempFile::preset("bvrf-int16");
    let rec = open(file.path()).0;
    use crate::kinds::*;
    let summary: Vec<(ChannelKind, Option<f64>, f64)> = rec
        .channels()
        .iter()
        .map(|c| (c.kind.clone(), c.unit.to_si, c.offset))
        .collect();
    let uv = Some(1e-6);
    assert_eq!(
        summary[..8],
        [
            (Eeg, uv, 0.0),
            (Eeg, uv, 0.0),
            (Eeg, uv, 0.0),
            (Eeg, uv, 0.0),
            (Eeg, uv, 0.0),
            (Eog, uv, 0.0),
            (Ecg, uv, 0.0),
            (ChannelKind::named("PHOTO"), Some(1e-3), 0.0),
        ]
    );
    assert_eq!(rec.channels()[8].kind.name(), "Temperature");
    assert!((rec.channels()[8].offset - 35.0).abs() < 1e-3);
    assert_eq!(rec.channels()[0].prefilter, "HP:0.1Hz LP:100Hz");
    assert_eq!(rec.channels()[7].prefilter, "");
}

/// A recording written by hand: `rec.bvrh`, `rec.bvrd` and, if given, `rec.bvrm`.
struct Written {
    dir: TempDir,
}

impl Written {
    fn new(bvrh: &str, bvrd: &[u8], bvrm: Option<&str>) -> Written {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("rec.bvrh"), bvrh).unwrap();
        std::fs::write(dir.path().join("rec.bvrd"), bvrd).unwrap();
        if let Some(bvrm) = bvrm {
            std::fs::write(dir.path().join("rec.bvrm"), bvrm).unwrap();
        }
        Written { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn open(&self) -> Result<(Recording, Vec<AnnotationData>), OpenError> {
        kymo_core::open(&self.path("rec.bvrh")).map(|o| Recording::new(o).unwrap())
    }

    fn ok(&self) -> (Recording, Vec<AnnotationData>) {
        self.open().unwrap()
    }

    fn error(&self) -> BvrfError {
        match self.open() {
            Err(OpenError::Bvrf { source, .. }) => source,
            other => panic!("expected a BVRF error, got {other:?}"),
        }
    }
}

/// A header with the given `EEGModality` fields besides the files and the rate (1000 Hz),
/// and the given top-level fields.
fn header_with(top: &str, eeg: &str, data_type: &str) -> String {
    format!(
        "\u{feff}{{\r\n\
         \"BVRF\": {{\"Version\": \"1.0.0\"}},\r\n\
         {top}\
         \"EEGModality\": {{\r\n\
         \"BVRFFiles\": {{\"DataFile\": {{\"Version\": \"1.0.0\", \"NumericDataType\": \
         \"{data_type}\"}}, \"MarkerFile\": {{\"Version\": \"1.0.0\"}}}},\r\n\
         \"DataSpecific\": {{\"SamplingFrequencyInHertz\": 1000}},\r\n\
         {eeg}\r\n\
         }}\r\n\
         }}\r\n"
    )
}

/// A header of single-precision data with these channels (a JSON list's items).
fn header(channels: &str) -> String {
    header_with("", &format!("\"Channels\": [{channels}]"), "Single")
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A marker file: the byte-order mark, the usual columns and these lines (tab-separated
/// fields as `|`), with CRLF line ends.
fn markers(lines: &[&str]) -> String {
    let mut text = String::from(
        "\u{feff}Sample\tType\tCode\tValue\tChannel\tDateTime\tStartEndId\tComment\r\n",
    );
    for line in lines {
        text.push_str(&line.replace('|', "\t"));
        text.push_str("\r\n");
    }
    text
}

const EEG: &str = r#"{"Name": "Fz", "Type": "EEG", "Unit": "µV", "ResolutionPerBit": 0.5}"#;

#[test]
fn channels_give_names_units_scaling_and_kinds() {
    let channels = [
        // Plain: the resolution as the gain.
        EEG,
        // Linear coefficients: offset and factor, divided by the denominator.
        r#"{"Name": "Photo", "Type": "PHOTO", "Unit": "mV", "ResolutionPerBit": 0.3,
            "Coefficients": {"Num": [4, 0.002], "Denom": [2]}}"#,
        // No resolution: 1, as the schema's default.
        r#"{"Name": "SpO2", "Type": "SPO2", "Unit": "%"}"#,
        // Not linear: the sensor output in the resolution's unit.
        r#"{"Name": "GSR", "Type": "GSR", "Unit": "µS", "ResolutionPerBit": 0.1,
            "ResolutionUnit": "mV", "Coefficients": {"Num": [0, 2], "Denom": [1, 0.5]}}"#,
        // Not linear, without a resolution unit.
        r#"{"Name": "Odd", "Type": "MISC", "Unit": "u", "Coefficients": {"Num": [0, 0, 1]}}"#,
        // MNE's types in any case and with spaces, a type MNE lacks, none at all.
        r#"{"Name": "VEOG", "Type": " eog ", "Unit": "µV"}"#,
        r#"{"Name": "EKG", "Type": "Ecg", "Unit": "mV"}"#,
        r#"{"Name": "Chin", "Type": "EMG", "Unit": "µV"}"#,
        r#"{"Name": "Trig", "Type": "STIM", "Unit": ""}"#,
        r#"{"Name": "Acc", "Type": "ACC", "Unit": "g"}"#,
        r#"{"Name": "Untyped", "Unit": "µV"}"#,
        // A magnetic unit makes a magnetometer, whatever the type.
        r#"{"Name": "MEG1", "Type": "MISC", "Unit": "fT"}"#,
        // No name: its number; names are trimmed.
        r#"{"Type": "EEG", "Unit": "µV"}"#,
        r#"{"Name": "  Cz ", "Type": "EEG", "Unit": "µV", "Coefficients": null}"#,
    ]
    .join(",");
    let n = 14;
    let w = Written::new(
        &header(&channels),
        &floats(&vec![2.0; n * 3]),
        Some(&markers(&[])),
    );
    let (rec, _) = w.ok();
    let summary: Vec<(&str, ChannelKind, &str, f64, f64)> = rec
        .channels()
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.kind.clone(),
                c.unit.label.as_str(),
                c.gain,
                c.offset,
            )
        })
        .collect();
    use crate::kinds::*;
    assert_eq!(
        summary,
        [
            ("Fz", Eeg, "µV", 0.5, 0.0),
            // Types as written.
            ("Photo", ChannelKind::named("PHOTO"), "mV", 0.3 * 0.001, 2.0),
            ("SpO2", ChannelKind::named("SPO2"), "%", 1.0, 0.0),
            ("GSR", ChannelKind::named("gsr"), "mV", 0.1, 0.0),
            ("Odd", Misc, "", 1.0, 0.0),
            ("VEOG", Eog, "µV", 1.0, 0.0),
            ("EKG", Ecg, "mV", 1.0, 0.0),
            ("Chin", Emg, "µV", 1.0, 0.0),
            ("Trig", Stim, "", 1.0, 0.0),
            ("Acc", ChannelKind::named("ACC"), "g", 1.0, 0.0),
            ("Untyped", Misc, "µV", 1.0, 0.0),
            ("MEG1", Mag, "fT", 1.0, 0.0),
            ("Ch13", Eeg, "µV", 1.0, 0.0),
            ("Cz", Eeg, "µV", 1.0, 0.0),
        ]
    );
    assert_eq!(rec.grid(GridId(0)).len(), 3);
    let mut physical = vec![0.0f32; 3];
    rec.read_physical(GridId(0), &[ChannelId(1)], 0..3, &mut physical)
        .unwrap();
    // 2 × 0.3 × 0.002 / 2 + 4 / 2.
    assert_eq!(physical, [(2.0 * (0.3 * 0.001) + 2.0) as f32; 3]);
    assert_eq!(
        rec.warnings(),
        [
            "channel \"GSR\": its coefficients aren't linear, so it shows the sensor output in \
             mV rather than µS",
            "channel \"Odd\": its coefficients aren't linear, so it shows the sensor output \
             rather than u",
            "1 channels have no name; they are named by their number (Ch1, ...)",
        ]
    );
}

#[test]
fn filters_become_prefilters_in_edf_notation() {
    let filters = r#""Filters": [
        {"Id": "1", "Type": "Lowpass", "FrequencyInHertz": 260, "Domain": "Digital"},
        {"Id": "2", "Type": "Bandpass", "FrequenciesInHertz": [0.5, 40], "Order": 32},
        {"Id": "hp", "Type": "HighPass", "FrequencyInHertz": 0},
        {"Id": "n", "Type": "Notch", "FrequencyInHertz": 50},
        {"Id": "bs", "Type": "Bandstop", "FrequenciesInHertz": [48, 52]},
        {"Id": "odd", "Type": "Comb", "FrequencyInHertz": 50}
    ],"#;
    let channels = [
        r#"{"Name": "A", "Type": "EEG", "Unit": "µV", "FilterIds": ["1"]}"#,
        r#"{"Name": "B", "Type": "EEG", "Unit": "µV", "FilterIds": ["2", "n"]}"#,
        r#"{"Name": "C", "Type": "EEG", "Unit": "µV", "FilterIds": ["hp", "bs", "odd"]}"#,
        r#"{"Name": "D", "Type": "EEG", "Unit": "µV", "FilterIds": ["missing", "1"]}"#,
        r#"{"Name": "E", "Type": "EEG", "Unit": "µV", "FilterIds": null}"#,
    ]
    .join(",");
    let header = header_with(
        "",
        &format!("{filters} \"Channels\": [{channels}]"),
        "Single",
    );
    let w = Written::new(&header, &floats(&[0.0; 5]), Some(&markers(&[])));
    let (rec, _) = w.ok();
    let prefilters: Vec<&str> = rec
        .channels()
        .iter()
        .map(|c| c.prefilter.as_str())
        .collect();
    assert_eq!(
        prefilters,
        [
            "LP:260Hz",
            "HP:0.5Hz LP:40Hz N:50Hz",
            "HP:DC N:48-52Hz",
            "LP:260Hz",
            ""
        ]
    );
    assert_eq!(
        rec.warnings(),
        ["channels name 1 filters the Filters section doesn't have"]
    );
}

#[test]
fn the_info_view_gets_the_headers_fields() {
    let top = r#""RecordingSoftware": {"Name": "BrainVision Recorder", "Version": "2.1.0"},
        "Task": {"Name": "Oddball", "Description": null, "Instructions": "Count"},
        "Institution": {"Name": "Uni", "Department": "Lab"},
        "Participants": [{"Id": "P1", "HeadCircumferenceInCentiMeter": 56.5},
                         {"Id": "P2", "ArtefactDescription": "DBS stimuli"}, {"Id": "P3"}],"#;
    let eeg = format!(
        r#""DataSpecific": {{"SamplingFrequencyInHertz": 1000, "PowerLineFrequencyInHertz": 50}},
        "Amplifiers": [{{"Id": "A1", "Model": "actiCHamp", "SerialNumber": "1"}},
                       {{"Id": "A2", "SerialNumber": "2"}}],
        "Caps": [{{"ParticipantId": "P1", "CapManufacturer": "Easycap",
                   "CapManufacturersModelName": "BC-32", "EEGPlacementScheme": "10-20"}}],
        "References": [{{"Name": "REF", "Composition": [{{"Name": "REF"}}]}},
                       {{"ParticipantId": "P2", "Name": "Ears",
                         "Composition": [{{"Name": "A1", "Factor": 0.5}},
                                         {{"Name": "A2", "Factor": 0.5}}]}}],
        "Grounds": [{{"Name": "GND", "Composition": [{{"Name": "Fpz"}}]}}],
        "Channels": [{EEG}]"#
    );
    // The rate twice: the second wins in serde_json, as in most JSON readers.
    let header = header_with(top, &eeg, "Int16");
    let marker_file = markers(&["0|Recording Info|Saving|||2025-06-30T13:13:28.5+02:00||"]);
    let w = Written::new(&header, &[0, 0], Some(&marker_file));
    let (rec, _) = w.ok();
    let extra: Vec<(&str, &str)> = rec
        .extra()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        extra,
        [
            ("version", "1.0.0"),
            ("numeric data type", "Int16"),
            ("recording software", "BrainVision Recorder 2.1.0"),
            ("task", "Oddball"),
            ("task instructions", "Count"),
            ("institution", "Uni, Lab"),
            ("amplifiers", "A1: actiCHamp (serial 1); A2: serial 2"),
            ("caps", "P1: Easycap BC-32, 10-20"),
            ("power line frequency", "50 Hz"),
            (
                "participants",
                "P1 (head circumference 56.5 cm), P2 (DBS stimuli), P3"
            ),
            ("references", "REF; P2: Ears = 0.5 × A1 + 0.5 × A2"),
            ("grounds", "GND = Fpz"),
            ("UTC offset", "+02:00"),
        ]
    );
    assert_eq!(
        rec.start_time(),
        Some(jiff::civil::date(2025, 6, 30).at(13, 13, 28, 500_000_000))
    );
}

#[test]
fn headers_kymo_cannot_read_are_errors() {
    let data = floats(&[0.0; 4]);
    let missing = |field: &str| {
        let header = header(EEG).replace(field, "\"Ignored\"");
        Written::new(&header, &data, None).error()
    };
    assert_eq!(
        missing("\"NumericDataType\""),
        BvrfError::Missing("NumericDataType")
    );
    assert_eq!(
        missing("\"SamplingFrequencyInHertz\""),
        BvrfError::Missing("SamplingFrequencyInHertz")
    );
    assert_eq!(
        missing("\"Channels\""),
        BvrfError::Missing("Channels section")
    );
    let error = |header: String| Written::new(&header, &data, None).error();
    assert!(matches!(
        error(header_with("", &format!("\"Channels\": [{EEG}]"), "Int8")),
        BvrfError::Unsupported(e) if e.contains("Int8")
    ));
    assert!(matches!(
        error(header_with("", &format!("\"Channels\": [{EEG}]"), "Single").replace("1000", "0")),
        BvrfError::Invalid {
            field: "SamplingFrequencyInHertz",
            ..
        }
    ));
    assert!(matches!(
        error(
            header_with("", &format!("\"Channels\": [{EEG}]"), "Single")
                .replace("1000", "\"1 kHz\"")
        ),
        BvrfError::Invalid {
            field: "SamplingFrequencyInHertz",
            ..
        }
    ));
    assert!(matches!(
        error(header_with("", "\"Channels\": []", "Single")),
        BvrfError::Invalid {
            field: "Channels",
            ..
        }
    ));
    assert!(matches!(
        error(header_with("", "\"Channels\": {}", "Single")),
        BvrfError::Invalid {
            field: "Channels",
            ..
        }
    ));
    for bad in ["0", "\"0.1\"", "1e999"] {
        let channel = EEG.replace("0.5", bad);
        assert!(
            matches!(
                error(header(&channel)),
                BvrfError::InvalidChannel { index: 1, ref name, .. } if name == "Fz"
            ) || bad == "1e999",
            "{bad}"
        );
    }
    let channel = r#"{"Name": "X", "Unit": "V", "Coefficients": {"Num": "two"}}"#;
    assert!(matches!(
        error(header(channel)),
        BvrfError::InvalidChannel { index: 1, .. }
    ));
    // Not JSON, though it starts like it.
    assert!(matches!(error("{ \"BVRF\": ".into()), BvrfError::Json(_)));
    // A BVRF section without an EEG modality.
    assert_eq!(
        error("{\"BVRF\": {\"Version\": \"1.0.0\"}}".into()),
        BvrfError::Missing("EEGModality section")
    );
    // No data file.
    let w = Written::new(&header(EEG), &data, None);
    std::fs::remove_file(w.path("rec.bvrd")).unwrap();
    assert_eq!(w.error(), BvrfError::NoDataFile(w.path("rec.bvrd")));
}

#[test]
fn versions_other_than_1_are_read_with_a_warning() {
    let data = floats(&[0.0]);
    for (version, warning) in [
        ("1.0.0", None),
        ("1.2.3", None),
        ("2.0.0", Some("BVRF version 2.0.0 is newer than 1.0.0")),
        ("1.0", Some("invalid BVRF version \"1.0\"")),
    ] {
        let header = header(EEG).replace("\"1.0.0\"}},\r\n{top}", "").replacen(
            "{\"Version\": \"1.0.0\"},\r\n",
            &format!("{{\"Version\": \"{version}\"}},\r\n"),
            1,
        );
        let w = Written::new(&header, &data, None);
        let (rec, _) = w.ok();
        let warnings: Vec<&String> = rec
            .warnings()
            .iter()
            .filter(|w| w.contains("version"))
            .collect();
        match warning {
            None => assert!(warnings.is_empty(), "{version}: {warnings:?}"),
            Some(w) => assert!(warnings[0].starts_with(w), "{version}: {warnings:?}"),
        }
    }
    let header = header(EEG).replacen("\"BVRF\": {\"Version\": \"1.0.0\"},", "", 1);
    let (rec, _) = Written::new(&header, &data, None).ok();
    assert!(
        rec.warnings()[0].contains("no BVRF version"),
        "{:?}",
        rec.warnings()
    );
}

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
    let channels = [EEG, r#"{"Name": "Cz", "Type": "EEG", "Unit": "µV"}"#].join(",");
    let marker_file = markers(&[
        "0|Recording Info|Saving|||2025-06-30T13:13:28.974479+02:00|{a}|",
        "0|Recording Info|Sync|||2025-06-30T13:13:28.974479+02:00||",
        "1000|Stimulus|S|128|||||",
        "1100|Response|R|255||||",
        "1200|Stimulus|S|0||||",
        // Annotations of one channel, of a channel the header doesn't have.
        "2000|Annotation|Eye blink||Cz|||",
        "2000|Annotation|Eye blink||Fp1|||",
        // A pair, and one whose end comes first; the end's code doesn't matter.
        "4000|Annotation|Eyes closed||||{p}|",
        "4600|Annotation|Eyes open||||{p}|",
        "5500|Bad|End||||{q}|",
        "5000|Bad|Interval||||{q}|",
        // A type alone, a value alone, no type.
        "8540|Reconnection||||||",
        "8600|Count||7|||||",
        "8700||Lonely||||",
        // Without a partner; without a sample.
        "9000|Stimulus|S|1|||{lonely}|",
        "x|Stimulus|S|1|||",
        "",
        "23999|Recording Info||||2025-06-30T13:14:05.198028+02:00|{a}|",
    ]);
    let w = Written::new(&header(&channels), &floats(&[0.0; 2]), Some(&marker_file));
    let (rec, annotations) = w.ok();
    assert_eq!(
        summary(&annotations),
        [
            (1.0, 0.0, "Stimulus/S128", vec![]),
            (1.1, 0.0, "Response/R255", vec![]),
            (1.2, 0.0, "Stimulus/S0", vec![]),
            (2.0, 0.0, "Annotation/Eye blink", vec![1]),
            (2.0, 0.0, "Annotation/Eye blink", vec![]),
            (4.0, 0.6, "Annotation/Eyes closed", vec![]),
            (5.0, 0.5, "Bad/End", vec![]),
            (8.54, 0.0, "Reconnection", vec![]),
            (8.6, 0.0, "Count/7", vec![]),
            (8.7, 0.0, "Lonely", vec![]),
            (9.0, 0.0, "Stimulus/S1", vec![]),
        ]
    );
    assert_eq!(
        rec.warnings(),
        [
            "1 markers have a StartEndId no other marker has; they are points in time",
            "1 markers without a valid sample are ignored",
            "1 markers name a channel the header doesn't have; they apply to all channels (of \
             their participant, where they have one)",
        ]
    );
    assert_eq!(
        rec.start_time(),
        Some(jiff::civil::date(2025, 6, 30).at(13, 13, 28, 974_479_000))
    );
}

#[test]
fn marker_columns_go_by_their_names() {
    // Another order, ParticipantId among them, LF line ends, no byte-order mark.
    let text = "Type\tParticipantId\tSample\tValue\tCode\n\
                Stimulus\t\t500\t3\tS\n\
                Comment\t\t250\t\tstart\n";
    let w = Written::new(&header(EEG), &floats(&[0.0]), Some(text));
    let (rec, annotations) = w.ok();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(
        summary(&annotations),
        [
            (0.5, 0.0, "Stimulus/S3", vec![]),
            (0.25, 0.0, "Comment/start", vec![])
        ]
    );
    // No date anywhere: no start time.
    assert_eq!(rec.start_time(), None);
}

#[test]
fn the_start_time_is_the_earliest_dated_markers() {
    let start = |lines: &[&str]| {
        let w = Written::new(&header(EEG), &floats(&[0.0]), Some(&markers(lines)));
        let (rec, _) = w.ok();
        (rec.start_time(), rec.warnings().to_vec())
    };
    // Moved back by its sample, whatever comes first in the file (whose date, a second
    // later than the earliest's, would give a start a second later).
    let (at, warnings) = start(&[
        "3000|Stimulus|S|1||2025-01-01T00:00:06+01:00||",
        "1500|Stimulus|S|1||2025-01-01T00:00:03.5+01:00||",
        "2000|Stimulus|S|1||||",
    ]);
    assert_eq!(at, Some(jiff::civil::date(2025, 1, 1).at(0, 0, 2, 0)));
    assert!(warnings.is_empty(), "{warnings:?}");
    let (at, warnings) = start(&["0|Stimulus|S|1||yesterday||"]);
    assert_eq!(at, None);
    assert_eq!(
        warnings,
        ["the earliest marker's DateTime \"yesterday\" is invalid; the start time is unknown"]
    );
}

#[test]
fn marker_files_kymo_cannot_read_leave_no_annotations() {
    let data = floats(&[0.0]);
    for (text, warning) in [
        ("", "the marker file is empty"),
        ("\u{feff}", "the marker file is empty"),
        ("Position\tType\n1\tStimulus\n", "no Sample or Type column"),
    ] {
        let w = Written::new(&header(EEG), &data, Some(text));
        let (rec, annotations) = w.ok();
        assert!(annotations.is_empty());
        assert!(rec.warnings()[0].contains(warning), "{:?}", rec.warnings());
    }
    let w = Written::new(&header(EEG), &data, None);
    let (rec, annotations) = w.ok();
    assert!(annotations.is_empty());
    assert_eq!(
        rec.warnings(),
        ["marker file \"rec.bvrm\" not found, so there are no annotations"]
    );
    // A header that isn't UTF-8 is read with a warning.
    // Latin-1, which has no byte-order mark.
    let latin: Vec<u8> = header(EEG)
        .trim_start_matches('\u{feff}')
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    let dir = TempDir::new();
    std::fs::write(dir.path().join("rec.bvrh"), latin).unwrap();
    std::fs::write(dir.path().join("rec.bvrd"), &data).unwrap();
    let rec = open(&dir.path().join("rec.bvrh")).0;
    assert_eq!(rec.channels()[0].unit.label, "\u{fffd}V");
    assert!(
        rec.warnings()[0].contains("not valid UTF-8"),
        "{:?}",
        rec.warnings()
    );
}

#[test]
fn data_file_sizes() {
    let channels = [EEG, EEG].join(",");
    // Whole frames of two channels; the rest ignored.
    for (bytes, samples, warned) in [(16, 2, false), (20, 2, true), (4, 0, true), (0, 0, false)] {
        let w = Written::new(&header(&channels), &vec![0; bytes], None);
        let (rec, _) = w.ok();
        assert_eq!(rec.grid(GridId(0)).len(), samples, "{bytes} bytes");
        let incomplete = rec
            .warnings()
            .iter()
            .any(|w| w.contains("incomplete sample"));
        assert_eq!(incomplete, warned, "{bytes} bytes: {:?}", rec.warnings());
    }
    // Every data type, each value where it belongs.
    let doubles: Vec<u8> = [1.5f64, -2.0, 1e6, 3.25]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    for (data_type, bytes, expected) in [
        (
            "Int16",
            [1i16, -2, 300, -4]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
            [1.0, 300.0, -2.0, -4.0],
        ),
        (
            "Int32",
            [1i32, -2, 70000, -4]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
            [1.0, 70000.0, -2.0, -4.0],
        ),
        (
            "Single",
            floats(&[1.5, -2.0, 0.25, 3.0]),
            [1.5, 0.25, -2.0, 3.0],
        ),
        ("Double", doubles, [1.5, 1e6, -2.0, 3.25]),
    ] {
        let header =
            header_with("", &format!("\"Channels\": [{channels}]"), data_type).replace("0.5", "1");
        let w = Written::new(&header, &bytes, None);
        let (rec, _) = w.ok();
        let mut physical = [0.0f32; 4];
        rec.read_physical(
            GridId(0),
            &[ChannelId(0), ChannelId(1)],
            0..2,
            &mut physical,
        )
        .unwrap();
        assert_eq!(physical, expected, "{data_type}");
    }
}

/// Where kymo puts each of `spec`'s channels: its streams (each participant's in their
/// order, then all participants'), in file order within each; and each channel's stream
/// (the last for all participants), without streams `None`.
fn arranged(spec: &Spec) -> (Vec<usize>, Vec<Option<usize>>) {
    let o = &spec.bvrf;
    let n = spec.channels.len();
    if o.participants.len() < 2 {
        return ((0..n).collect(), vec![None; n]);
    }
    let all = o.participants.len();
    let stream = |c: usize| {
        o.channel_participants
            .get(c)
            .copied()
            .flatten()
            .unwrap_or(all)
    };
    let order: Vec<usize> = (0..=all)
        .flat_map(|s| (0..n).filter(move |&c| stream(c) == s))
        .collect();
    let streams = order.iter().map(|&c| Some(stream(c))).collect();
    (order, streams)
}

fn participants_case() -> impl Strategy<Value = Spec> {
    (
        proptest::sample::select(Format::BVRF.to_vec()),
        0usize..4,
        proptest::collection::vec(any::<prop::sample::Index>(), 1..9),
        any::<u64>(),
    )
        .prop_map(|(format, participants, owners, seed)| {
            let n = owners.len();
            let channel_participants: Vec<Option<usize>> = owners
                .iter()
                .map(|o| {
                    let k = o.index(participants + 1);
                    (k < participants).then_some(k)
                })
                .collect();
            let channels = (0..n)
                .map(|i| ChannelSpec::eeg(format!("E{i}"), 10, format))
                .collect();
            let mut spec = Spec::new(format, channels, 1.0, 3);
            spec.seed = seed;
            spec.spike_every = 7;
            spec.bvrf.participants = (1..=participants).map(|p| format!("P{p}")).collect();
            spec.bvrf.channel_participants = channel_participants;
            spec
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// With several participants, each channel is in its participant's stream (or that of
    /// all), the streams in order and their channels consecutive, and reads the truth of its
    /// place in the file.
    #[test]
    fn participants_keep_their_channels(spec in participants_case()) {
        let file = TempFile::written(&spec);
        let (rec, _) = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        let (order, streams) = arranged(&spec);
        let ids: Vec<ChannelId> = (0..order.len() as u32).map(ChannelId).collect();
        let got = read_native(&rec, &ids, 0..spec.len(0));
        let n = spec.len(0) as usize;
        for (k, &c) in order.iter().enumerate() {
            let expected = truth(&spec, &[ChannelId(c as u32)], 0..spec.len(0));
            prop_assert_eq!(&got[k * n..][..n], &expected[..], "channel {}", k);
            let p = spec.bvrf.channel_participants.get(c).copied().flatten();
            let label = &spec.channels[c].label;
            let channel = rec.channel(ChannelId(k as u32));
            let several = spec.bvrf.participants.len() > 1;
            match p.filter(|_| several) {
                Some(p) => {
                    prop_assert_eq!(&channel.name, &format!("{label} (P{})", p + 1));
                    prop_assert_eq!(channel.in_stream.as_deref(), Some(label.as_str()));
                }
                None => {
                    prop_assert_eq!(&channel.name, label);
                    prop_assert_eq!(&channel.in_stream, &None);
                }
            }
            let stream = rec.stream_of(ChannelId(k as u32));
            let expected = streams[k].map(|s| {
                rec.streams().iter().position(|st| st.id == s as u32).unwrap()
            });
            prop_assert_eq!(stream, expected);
        }
    }
}

/// An annotation's onset, duration, description, channels and stream.
type Placed = (f64, f64, String, Vec<u32>, Option<usize>);

#[test]
fn participants_become_streams() {
    use kymo_core::StreamContent;
    let spec = Spec::preset("bvrf-participants").unwrap();
    let file = TempFile::written(&spec);
    let (rec, annotations) = open(file.path());
    assert_eq!(
        rec.warnings(),
        [
            "channel \"GSR\": its coefficients aren't linear, so it shows the sensor output in \
          mV rather than µS"
        ]
    );
    let streams: Vec<(u32, &str, &str, Vec<u32>)> = rec
        .streams()
        .iter()
        .map(|s| {
            let StreamContent::Channels(ids) = &s.content else {
                panic!("{s:?}");
            };
            (
                s.id,
                s.name.as_str(),
                s.kind.as_str(),
                ids.iter().map(|c| c.0).collect(),
            )
        })
        .collect();
    assert_eq!(
        streams,
        [
            (0, "P1", "Participant", vec![0, 1, 2, 3]),
            (1, "P2", "Participant", vec![4, 5, 6, 7]),
            (2, "P3", "Participant", vec![8, 9, 10, 11]),
            (3, "All participants", "", vec![12, 13]),
        ]
    );
    let names: Vec<(&str, Option<&str>)> = rec
        .channels()
        .iter()
        .map(|c| (c.name.as_str(), c.in_stream.as_deref()))
        .collect();
    assert_eq!(
        names[..5],
        [
            ("Fp1 (P1)", Some("Fp1")),
            ("Cz (P1)", Some("Cz")),
            ("Pz (P1)", Some("Pz")),
            ("EOG (P1)", Some("EOG")),
            ("Fp1 (P2)", Some("Fp1")),
        ]
    );
    assert_eq!(names[12..], [("Photo_AUX1", None), ("GSR", None)]);
    // Interleaved in the file (each participant's Fp1, then each one's Cz, ...), in order here.
    let (order, _) = arranged(&spec);
    assert_eq!(order[..5], [0, 3, 6, 9, 1]);
    let ids: Vec<ChannelId> = (0..14).map(ChannelId).collect();
    let got = read_native(&rec, &ids, 0..spec.len(0));
    let expected: Vec<i64> = order
        .iter()
        .flat_map(|&c| truth(&spec, &[ChannelId(c as u32)], 0..spec.len(0)))
        .collect();
    assert_eq!(got, expected);

    // Each marker in its participant's stream, on its channels; the others in the stream
    // of all participants.
    let id_of = |p: usize, label: &str| {
        let name = format!("{label} (P{})", p + 1);
        rec.channels().iter().position(|c| c.name == name).unwrap() as u32
    };
    let expected: Vec<Placed> = spec
        .annotations
        .iter()
        .enumerate()
        .map(|(k, a)| {
            let target = spec.bvrf.targets.get(k).cloned().unwrap_or_default();
            let (channels, stream) = match (target.participant, target.channel.as_deref()) {
                (Some(p), Some(label)) => (vec![id_of(p, label)], Some(p)),
                (Some(p), None) => ((4 * p as u32..4 * p as u32 + 4).collect(), Some(p)),
                (None, Some(label)) => ((0..3).map(|p| id_of(p, label)).collect(), Some(3)),
                (None, None) => (vec![], Some(3)),
            };
            let duration = a.duration.unwrap_or(0.0);
            (a.onset, duration, a.texts[0].clone(), channels, stream)
        })
        .collect();
    let mut got: Vec<Placed> = annotations
        .iter()
        .map(|a| {
            (
                a.onset,
                a.duration,
                a.description.clone(),
                a.channels.iter().map(|c| c.0).collect(),
                a.stream,
            )
        })
        .collect();
    let mut expected = expected;
    let key = |a: &(f64, f64, String, Vec<u32>, Option<usize>)| (a.0.to_bits(), a.2.clone(), a.4);
    got.sort_by_key(key);
    expected.sort_by_key(key);
    assert_eq!(got.len(), expected.len());
    for (g, e) in got.iter().zip(&expected) {
        assert!(
            (g.0 - e.0).abs() < 1e-9 && (g.1 - e.1).abs() < 1e-9,
            "{g:?} {e:?}"
        );
        assert_eq!((&g.2, &g.3, g.4), (&e.2, &e.3, e.4));
    }
}

/// A header of single-precision data with these participants and channels.
fn participants_header(participants: &[&str], channels: &str) -> String {
    let list: Vec<String> = participants
        .iter()
        .map(|p| format!("{{\"Id\": \"{p}\"}}"))
        .collect();
    header_with(
        &format!("\"Participants\": [{}],\r\n", list.join(", ")),
        &format!("\"Channels\": [{channels}]"),
        "Single",
    )
}

fn of(name: &str, participant: Option<&str>) -> String {
    match participant {
        Some(p) => {
            format!(r#"{{"Name": "{name}", "Type": "EEG", "Unit": "µV", "ParticipantId": "{p}"}}"#)
        }
        None => format!(r#"{{"Name": "{name}", "Type": "EEG", "Unit": "µV"}}"#),
    }
}

/// Each stream's id, name and what it holds.
fn streams(rec: &Recording) -> Vec<(u32, String, String)> {
    rec.streams()
        .iter()
        .map(|s| (s.id, s.name.clone(), format!("{:?}", s.content)))
        .collect()
}

#[test]
fn participants_and_their_markers_by_hand() {
    let data = floats(&[0.0; 3]);
    // One participant listed: no streams, names as written, a ParticipantId column ignored.
    let channels = [of("Fz", Some("P1")), of("Cz", Some("P1")), of("Ref", None)].join(",");
    let text = "Sample\tType\tParticipantId\tChannel\n1\tA\tP1\tCz\n2\tB\tP9\t\n";
    let w = Written::new(&participants_header(&["P1"], &channels), &data, Some(text));
    let (rec, annotations) = w.ok();
    assert!(rec.streams().is_empty());
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Fz", "Cz", "Ref"]);
    assert_eq!(
        summary(&annotations),
        [(0.001, 0.0, "A", vec![1]), (0.002, 0.0, "B", vec![])]
    );

    // A participant the section doesn't list is one of its own; one without channels holds
    // its markers alone; one with neither is listed, not shown.
    let channels = [
        of("Fz", Some("P1")),
        of("Fz", Some("X")),
        of("Fz", Some("P1")),
    ]
    .join(",");
    let text = "Sample\tType\tParticipantId\tChannel\n\
                1\tOwn\tP1\t\n\
                2\tTheirs\tP2\t\n\
                3\tSomeone\tP9\t\n\
                4\tFz of all\t\tFz\n\
                5\tMissing channel\tX\tCz\n";
    let listed = participants_header(&["P1", "P2", "P3"], &channels);
    let w = Written::new(&listed, &data, Some(text));
    let (rec, annotations) = w.ok();
    assert_eq!(
        streams(&rec),
        [
            (
                0,
                "P1".into(),
                "Channels([ChannelId(0), ChannelId(1)])".into()
            ),
            (1, "P2".into(), "Annotations(1)".into()),
            (
                2,
                "P3".into(),
                "NotShown(\"it has no channels or markers\")".into()
            ),
            (3, "X".into(), "Channels([ChannelId(2)])".into()),
            (4, "All participants".into(), "Annotations(2)".into()),
        ]
    );
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Fz (P1)", "Fz (P1)", "Fz (X)"]);
    let placed: Vec<(&str, Vec<u32>, Option<usize>)> = annotations
        .iter()
        .map(|a| {
            (
                a.description.as_str(),
                a.channels.iter().map(|c| c.0).collect(),
                a.stream,
            )
        })
        .collect();
    assert_eq!(
        placed,
        [
            ("Own", vec![0, 1], Some(0)),
            ("Theirs", vec![], Some(1)),
            ("Someone", vec![], Some(4)),
            ("Fz of all", vec![0, 1, 2], Some(4)),
            ("Missing channel", vec![2], Some(3)),
        ]
    );
    assert_eq!(
        rec.warnings(),
        [
            "channels belong to participants the Participants section doesn't list (X); each \
             is a participant of its own",
            "1 markers name a participant the header doesn't have; they apply to all \
             participants",
            "1 markers name a channel the header doesn't have; they apply to all channels (of \
             their participant, where they have one)",
        ]
    );

    // Without channels or markers of all participants, there is no stream for them.
    let channels = [of("Fz", Some("A")), of("Fz", Some("B"))].join(",");
    let text = "Sample\tType\tParticipantId\n1\tGo\tA\n";
    let w = Written::new(
        &participants_header(&["A", "B"], &channels),
        &data,
        Some(text),
    );
    let (rec, _) = w.ok();
    let names: Vec<String> = streams(&rec).into_iter().map(|s| s.1).collect();
    assert_eq!(names, ["A", "B"]);
    // Participants only on the channels, without a section: streams all the same.
    let w = Written::new(&header(&channels), &data, Some(text));
    let (rec, _) = w.ok();
    let names: Vec<String> = streams(&rec).into_iter().map(|s| s.1).collect();
    assert_eq!(names, ["A", "B"]);
}
