use crate::common;

use crate::common::TempFile;
use kymo_core::edf::EdfError;
use kymo_core::{AnnotationData, ChannelId, OpenError, Origin, Recording};
use kymo_testgen::{ChannelSpec, Format, GapSpec, Spec, Tal};
use proptest::prelude::*;

fn open(path: &std::path::Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

/// The annotations a reader should report for `spec`: one per TAL text.
fn expected_annotations(spec: &Spec) -> Vec<AnnotationData> {
    let mut expected: Vec<AnnotationData> = spec
        .annotations
        .iter()
        .flat_map(|t| {
            t.texts.iter().map(move |text| AnnotationData {
                onset: t.onset,
                duration: t.duration.unwrap_or(0.0),
                description: text.clone(),
                channels: vec![],
                origin: Origin::File,
                stream: None,
            })
        })
        .collect();
    sort(&mut expected);
    expected
}

fn sort(annotations: &mut [AnnotationData]) {
    annotations.sort_by(|a, b| {
        a.onset
            .total_cmp(&b.onset)
            .then(a.description.cmp(&b.description))
            .then(a.duration.total_cmp(&b.duration))
    });
}

/// Checks every channel's grid against the spec: time base, segments, and a physical read
/// over the whole grid (NaN exactly in the gaps, the truth elsewhere).
fn check_against_spec(rec: &Recording, spec: &Spec) -> Result<(), TestCaseError> {
    for c in 0..spec.channels.len() {
        let channel = rec.channel(ChannelId(c as u32));
        let grid = rec.grid(channel.grid);
        prop_assert_eq!(grid.rate(), spec.rate(c));
        prop_assert_eq!(grid.time_of(0), spec.first_onset);
        prop_assert_eq!(grid.len(), spec.len(c));
        let segments = spec.segments(c);
        prop_assert_eq!(grid.segments(), segments.as_slice(), "channel {}", c);

        let mut out = vec![0.0f32; grid.len() as usize];
        rec.read_physical(
            channel.grid,
            &[ChannelId(c as u32)],
            0..grid.len(),
            &mut out,
        )
        .unwrap();
        for (i, &x) in out.iter().enumerate() {
            let i = i as u64;
            if common::is_present(&segments, i) {
                prop_assert_eq!(x, spec.physical(c, i) as f32, "channel {}, sample {}", c, i);
            } else {
                prop_assert!(
                    x.is_nan(),
                    "channel {}, sample {} is in a gap but reads {}",
                    c,
                    i,
                    x
                );
            }
        }
    }
    Ok(())
}

#[test]
fn mixed_edf_d_preset_has_gaps_in_every_grid() {
    let spec = Spec::preset("edf+d-mixed").unwrap();
    let file = TempFile::written(&spec);
    let (rec, mut annotations) = open(file.path());
    assert_eq!(rec.format(), "EDF+D");
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(rec.grids().len(), 3);
    for grid in rec.grids() {
        assert_eq!(grid.segments().len(), 4, "{} Hz", grid.rate());
    }
    // 1200 records of 0.5 s after a 0.25 s start, plus 11.75 s of gaps.
    assert_eq!(rec.duration(), 0.25 + 600.0 + 11.75);
    check_against_spec(&rec, &spec).unwrap();
    sort(&mut annotations);
    assert_eq!(annotations.len(), 100);
    assert_eq!(annotations, expected_annotations(&spec));
}

/// Prefetching only warms the cache: any range of any grid, gaps and the end included, is
/// accepted without a panic and leaves the samples unchanged.
#[test]
fn prefetch_accepts_any_range_and_changes_nothing() {
    let spec = Spec::preset("edf+d-mixed").unwrap();
    let file = TempFile::written(&spec);
    let (rec, _) = open(file.path());
    for g in 0..=rec.grids().len() as u32 {
        let len = rec.grids().get(g as usize).map_or(10, |grid| grid.len());
        for (a, b) in [
            (0, len),
            (0, 1),
            (len / 3, len / 2),
            (len - 1, len + 5),
            (len, len + 1),
        ] {
            rec.prefetch(kymo_core::GridId(g), a..b);
        }
        #[allow(clippy::reversed_empty_ranges)]
        rec.prefetch(kymo_core::GridId(g), 5..1);
        rec.prefetch(kymo_core::GridId(g), u64::MAX - 1..u64::MAX);
    }
    check_against_spec(&rec, &spec).unwrap();

    // A truncated last record. Never change a mapped file (Windows refuses).
    drop(rec);
    let full = std::fs::read(file.path()).unwrap();
    std::fs::write(file.path(), &full[..full.len() - 100]).unwrap();
    let (rec, _) = open(file.path());
    for grid in 0..rec.grids().len() as u32 {
        rec.prefetch(kymo_core::GridId(grid), 0..u64::MAX);
    }
}

#[test]
fn contiguous_plus_file_has_one_segment_and_a_sub_second_start() {
    let mut spec = Spec::preset("bdf+c-small").unwrap();
    spec.first_onset = 0.125;
    let file = TempFile::written(&spec);
    let (rec, mut annotations) = open(file.path());
    let grid = rec.grid(kymo_core::GridId(0));
    assert_eq!(grid.segments(), [0..60 * 512]);
    assert_eq!(grid.time_of(0), 0.125);
    assert_eq!(rec.duration(), 60.125);
    sort(&mut annotations);
    assert_eq!(annotations, expected_annotations(&spec));
    // Annotations without a duration field get duration 0.
    assert!(spec.annotations.iter().any(|a| a.duration.is_none()));
}

#[test]
fn plain_edf_has_no_annotations_and_starts_at_zero() {
    let spec = Spec::new(
        Format::Edf,
        vec![ChannelSpec::eeg("EEG", 10, Format::Edf)],
        1.0,
        3,
    );
    let file = TempFile::written(&spec);
    let (rec, annotations) = open(file.path());
    assert!(annotations.is_empty());
    assert_eq!(rec.grid(kymo_core::GridId(0)).time_of(0), 0.0);
}

/// Offset of record `r`'s annotation signal in a testgen file (it is the last signal).
fn annotation_block(spec: &Spec, r: u64) -> std::ops::Range<usize> {
    let record = spec.record_bytes().unwrap();
    let block = u64::from(spec.annotation_samples_per_record().unwrap().unwrap())
        * spec.format.bytes_per_sample() as u64;
    let end = spec.header_bytes() + (r + 1) * record;
    (end - block) as usize..end as usize
}

/// Writes `spec`, replaces record `r`'s annotation block with `tals`, and opens the result.
fn open_patched(spec: &Spec, r: u64, tals: &[u8]) -> Result<kymo_core::Opened, OpenError> {
    let file = TempFile::written(spec);
    let mut bytes = std::fs::read(file.path()).unwrap();
    let block = &mut bytes[annotation_block(spec, r)];
    block.fill(0);
    block[..tals.len()].copy_from_slice(tals);
    std::fs::write(file.path(), &bytes).unwrap();
    kymo_core::open(file.path())
}

fn d_spec() -> Spec {
    let f = Format::EdfPlusD;
    let mut spec = Spec::new(f, vec![ChannelSpec::eeg("EEG", 20, f)], 1.0, 6);
    spec.gaps = vec![GapSpec {
        before_record: 3,
        seconds: 2.0,
    }];
    spec.annotations = vec![
        Tal {
            // Long, so every record's annotation block has room for the patched TALs.
            onset: 0.5,
            duration: None,
            texts: vec!["a long annotation that makes the blocks large enough".into()],
        },
        Tal {
            // In the gap, so it goes into record 2.
            onset: 4.5,
            duration: None,
            texts: vec!["kept".into()],
        },
    ];
    spec
}

fn record_error(result: Result<kymo_core::Opened, OpenError>) -> (u64, String) {
    match result {
        Err(OpenError::Edf {
            source: EdfError::InvalidRecord { record, reason },
            ..
        }) => (record, reason),
        other => panic!("expected an invalid record, got {other:?}"),
    }
}

#[test]
fn discontinuous_record_without_time_keeping_is_an_error() {
    let (record, reason) = record_error(open_patched(&d_spec(), 2, b""));
    assert_eq!(record, 2);
    assert!(reason.contains("time-keeping"), "{reason}");
}

#[test]
fn out_of_order_or_overlapping_records_are_errors() {
    // Record 4 would start at 6 s; 0 s is out of order, 4.5 s overlaps record 3 (5..6 s).
    for onset in [&b"+0"[..], b"+4.5"] {
        let mut tal = onset.to_vec();
        tal.extend_from_slice(b"\x14\x14\0");
        let (record, reason) = record_error(open_patched(&d_spec(), 4, &tal));
        assert_eq!(record, 4);
        assert!(
            reason.contains("before the previous record ends"),
            "{reason}"
        );
    }
}

#[test]
fn malformed_tals_warn_and_the_rest_is_read() {
    // Record 4 starts at 6 s (records at 0, 1, 2, then 5, 6, 7 after the 2 s gap).
    let tals = b"+6\x14\x14\0+6.5\x14bad\0+6.25\x14good\x14\0";
    let opened = open_patched(&d_spec(), 4, tals).unwrap();
    assert_eq!(opened.warnings.len(), 1, "{:?}", opened.warnings);
    assert!(opened.warnings[0].contains("record 4"));
    let descriptions: Vec<&str> = opened
        .annotations
        .iter()
        .map(|a| a.description.as_str())
        .collect();
    assert_eq!(
        descriptions,
        [
            "a long annotation that makes the blocks large enough",
            "kept",
            "good"
        ]
    );
}

/// Only a TAL at the annotation signal's first byte with an empty first annotation keeps
/// the record's time (EDF+): an annotation's TAL in its place, or after a damaged one,
/// doesn't move the record.
#[test]
fn only_the_first_tal_with_an_empty_text_keeps_time() {
    let f = Format::EdfPlusC;
    let mut spec = Spec::new(f, vec![ChannelSpec::eeg("EEG", 20, f)], 1.0, 6);
    // Long, so every record's annotation block has room for the patched TALs; in record 4,
    // which isn't patched.
    spec.annotations = vec![Tal {
        onset: 4.5,
        duration: None,
        texts: vec!["a long annotation that makes the blocks large enough".into()],
    }];
    let texts = |o: &kymo_core::Opened| -> Vec<(f64, String)> {
        o.annotations
            .iter()
            .map(|a| (a.onset, a.description.clone()))
            .filter(|(_, d)| !d.starts_with("a long"))
            .collect()
    };
    // Record 0 without a time-keeping TAL, only an annotation at 2.5 s: it starts at 0, as
    // contiguity implies for the first record, and the annotation stays at 2.5 s.
    let opened = open_patched(&spec, 0, b"+2.5\x14lights off\x14\0").unwrap();
    assert_eq!(opened.header.grids[0].time_of(0), 0.0);
    assert_eq!(texts(&opened), [(2.5, "lights off".to_owned())]);
    assert!(
        opened.warnings.iter().any(|w| w.contains("time-keeping")),
        "{:?}",
        opened.warnings
    );
    // A damaged TAL first, then an annotation: still no time-keeping.
    let opened = open_patched(&spec, 0, b"+x\x14\x14\0+0.25\x14\x14\0").unwrap();
    assert_eq!(opened.header.grids[0].time_of(0), 0.0);
    // The time-keeping TAL may carry annotations after its empty one.
    let opened = open_patched(&spec, 0, b"+0.125\x14\x14start\x14\0").unwrap();
    assert_eq!(opened.header.grids[0].time_of(0), 0.125);
    assert_eq!(texts(&opened), [(0.125, "start".to_owned())]);
    // EDF+D needs one in every record.
    let (record, reason) = record_error(open_patched(&d_spec(), 2, b"+2\x14lights off\x14\0"));
    assert_eq!(record, 2);
    assert!(reason.contains("time-keeping"), "{reason}");
}

#[test]
fn non_contiguous_continuous_file_warns_and_stays_continuous() {
    let f = Format::EdfPlusC;
    let spec = Spec::new(f, vec![ChannelSpec::eeg("EEG", 20, f)], 1.0, 6);
    // Record 3 claims to start at 7 s instead of 3 s.
    let opened = open_patched(&spec, 3, b"+7\x14\x14\0").unwrap();
    assert_eq!(opened.warnings.len(), 1);
    assert!(
        opened.warnings[0].contains("marked continuous"),
        "{:?}",
        opened.warnings
    );
    assert_eq!(opened.header.grids[0].segments(), [0..120]);
}

/// A random EDF+/BDF+ spec: mixed rates, sub-second start, gaps (EDF+D/BDF+D) of lengths that
/// may round away or land between samples, and annotations.
fn plus_spec() -> impl Strategy<Value = Spec> {
    let format = proptest::sample::select(vec![
        Format::EdfPlusC,
        Format::EdfPlusD,
        Format::BdfPlusC,
        Format::BdfPlusD,
    ]);
    let spr = proptest::sample::select(vec![1u32, 4, 7, 10, 32]);
    let gap = (
        any::<prop::sample::Index>(),
        proptest::sample::select(vec![0.001, 0.1, 0.25, 0.5, 1.0, 2.75, 10.0]),
    );
    let text = "[a-zA-Z0-9 µäö]{1,8}";
    let tal = (
        -2.0f64..20.0,
        proptest::option::of(proptest::sample::select(vec![0.0, 0.5, 1.25, 3.0])),
        proptest::collection::vec(text, 1..3),
    );
    (
        format,
        proptest::collection::vec(spr, 1..5),
        proptest::sample::select(vec![0.25, 0.5, 1.0]),
        1u64..12,
        proptest::sample::select(vec![0.0, 0.125, 0.5]),
        proptest::collection::vec(gap, 0..4),
        proptest::collection::vec(tal, 0..8),
        any::<u64>(),
    )
        .prop_map(
            |(format, sprs, record_duration, n_records, first_onset, gaps, tals, seed)| {
                let channels = sprs
                    .iter()
                    .enumerate()
                    .map(|(i, &s)| ChannelSpec::eeg(format!("EEG {i}"), s, format))
                    .collect();
                let mut spec = Spec::new(format, channels, record_duration, n_records);
                spec.first_onset = first_onset;
                spec.seed = seed;
                if format.is_discontinuous() && n_records > 1 {
                    let mut gaps: Vec<GapSpec> = gaps
                        .into_iter()
                        .map(|(i, seconds)| GapSpec {
                            before_record: 1 + i.index(n_records as usize - 1) as u64,
                            seconds,
                        })
                        .collect();
                    gaps.sort_by_key(|g| g.before_record);
                    gaps.dedup_by_key(|g| g.before_record);
                    spec.gaps = gaps;
                }
                spec.annotations = tals
                    .into_iter()
                    .map(|(onset, duration, texts)| Tal {
                        // Onsets on a 1/64 s grid, so they print exactly.
                        onset: (onset * 64.0).round() / 64.0,
                        duration,
                        texts,
                    })
                    .collect();
                spec
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn plus_files_match_the_spec(spec in plus_spec()) {
        let file = TempFile::written(&spec);
        let (rec, mut annotations) = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        check_against_spec(&rec, &spec)?;
        sort(&mut annotations);
        prop_assert_eq!(annotations, expected_annotations(&spec));
    }
}
