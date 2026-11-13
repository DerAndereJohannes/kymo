//! XDF files: samples against the truth function for every value format, timestamp layout
//! and chunking, the presets, and files put together chunk by chunk for each rule of the
//! scan, the stream modes, labels, kinds, units, start times and annotations.

use std::ops::Range;
use std::path::PathBuf;

use crate::common::{TempDir, TempFile};
use kymo_core::xdf::XdfError;
use kymo_core::{
    AnnotationData, ChannelId, ChannelKind, GridId, OpenError, Quantity, Recording, SampleType,
    Stream, StreamContent,
};
use kymo_testgen::xdf::{
    Chunks, channel_order, clock_offset_at, header_xml, local_time, offset_times, stream_of,
    streams, tag, written_stamps,
};
use kymo_testgen::{
    ChannelSpec, Format, MarkerValues, Spec, XdfBreak, XdfFormat, XdfMarkers, XdfStamps, XdfStream,
};
use proptest::prelude::*;

/// Channels as a stream header describes them: label, type, unit.
type Described = Vec<(String, String, String)>;

fn open(path: &std::path::Path) -> Recording {
    open_with(path).0
}

fn open_with(path: &std::path::Path) -> (Recording, Vec<AnnotationData>) {
    Recording::new(kymo_core::open(path).unwrap()).unwrap()
}

fn physical(rec: &Recording, grid: GridId, channels: &[ChannelId], range: Range<u64>) -> Vec<f32> {
    let mut out = vec![0.0f32; channels.len() * (range.end - range.start) as usize];
    rec.read_physical(grid, channels, range, &mut out).unwrap();
    out
}

/// One f32 step at `x`.
fn step(x: f64) -> f64 {
    let x = (x as f32).abs();
    f64::from(f32::from_bits(x.to_bits() + 1) - x)
}

/// Whether `got` (channel-major, one column per stream sample in `samples`) matches the
/// truth: exactly, or within the steps of both roundings for formats read relative to each
/// channel's first sample (int64, double64).
fn matches(spec: &Spec, channels: &[ChannelId], samples: Range<u64>, got: &[f32]) -> bool {
    let order = channel_order(spec);
    let n = (samples.end - samples.start) as usize;
    got.len() == channels.len() * n
        && got.iter().enumerate().all(|(k, &g)| {
            let c = order[channels[k / n.max(1)].0 as usize];
            let (stream, _) = stream_of(spec, c).unwrap();
            let i = samples.start + (k % n.max(1)) as u64;
            let e = spec.physical(c, i);
            let relative = matches!(stream.format, XdfFormat::I64 | XdfFormat::F64)
                && !spec.channels[c].trigger;
            g == e as f32
                || relative
                    && (f64::from(g) - e).abs() <= step(e - spec.physical(c, 0)) + step(e) * 2.0
        })
}

/// What kymo should make of a stream's time stamps, worked out by brute force as pyxdf
/// does: each sample's time (relative to `t = 0`), the stream's effective rate, its
/// segments' lengths.
struct Expected {
    times: Vec<f64>,
    rate: f64,
    segments: Vec<u64>,
}

/// The expected time stamps of every stream of `spec`, by stream id, with `t = 0` the
/// earliest first time; and that time on the recording's clock.
fn timeline(spec: &Spec) -> (Vec<Expected>, f64) {
    let mut streams = streams(spec);
    streams.sort_by_key(|s| s.id);
    // The clock's ranges between resets: each its offset line over the stream's clock
    // (offsets lie on it exactly), and its first and last clock time.
    let offsets: Vec<(f64, f64, bool)> = offset_times(spec)
        .into_iter()
        .map(|t| {
            let reset = spec
                .xdf
                .clock_reset
                .is_some_and(|(at, _)| t >= spec.xdf.clock + at);
            (local_time(spec, t), clock_offset_at(spec, t), reset)
        })
        .collect();
    let ranges: Vec<(f64, f64, f64, f64)> = [false, true]
        .iter()
        .filter_map(|&after| {
            let part: Vec<(f64, f64)> = offsets
                .iter()
                .filter(|o| o.2 == after)
                .map(|o| (o.0, o.1))
                .collect();
            let (first, last) = (*part.first()?, *part.last()?);
            let slope = if part.len() > 1 {
                (last.1 - first.1) / (last.0 - first.0)
            } else {
                0.0
            };
            Some((first.1 - slope * first.0, slope, first.0, last.0))
        })
        .collect();
    let mut all: Vec<Expected> = streams
        .iter()
        .map(|s| {
            let tdiff = 1.0 / spec.rate(s.channels[0]);
            // pyxdf deduces an irregular stream's missing time stamps as the previous one.
            let deduce = if s.irregular { 0.0 } else { tdiff };
            let stamps = written_stamps(spec, s);
            let (q, first) = stamps
                .iter()
                .enumerate()
                .find_map(|(i, t)| t.map(|t| (i, t)))
                .unwrap();
            let mut raw: Vec<f64> = Vec::with_capacity(stamps.len());
            for (i, stamp) in stamps.iter().enumerate() {
                let t = match (stamp, raw.last()) {
                    (Some(t), _) => *t,
                    (None, Some(p)) => p + deduce,
                    (None, None) => first - (q - i) as f64 * deduce,
                };
                raw.push(t);
            }
            // Synchronized, each in the range pyxdf's rule puts it in.
            let mut range = 0;
            let synced: Vec<f64> = raw
                .iter()
                .map(|&t| {
                    if ranges.is_empty() {
                        return t;
                    }
                    while range + 1 < ranges.len()
                        && (t - ranges[range].3).abs() >= (t - ranges[range + 1].2).abs()
                    {
                        range += 1;
                    }
                    let (a, b, _, _) = ranges[range];
                    t + (a + b * t)
                })
                .collect();
            if s.irregular || s.can_drop {
                return own_times(synced, s.irregular, tdiff);
            }
            // Segments between breaks, each replaced by its least-squares line.
            let threshold = 1f64.max(2.0 * tdiff);
            let mut times = synced.clone();
            let mut segments = Vec::new();
            let (mut samples, mut seconds) = (0.0, 0.0);
            let mut start = 0;
            for i in 1..=synced.len() {
                if i < synced.len() && (synced[i] - synced[i - 1]).abs() <= threshold {
                    continue;
                }
                let y = &synced[start..i];
                let n = y.len() as f64;
                if y.len() > 1 {
                    let mean = y.iter().sum::<f64>() / n;
                    let idx = |k: usize| k as f64 - (n - 1.0) / 2.0;
                    let slope = (0..y.len()).map(|k| idx(k) * (y[k] - mean)).sum::<f64>()
                        / (n * (n * n - 1.0) / 12.0);
                    for k in 0..y.len() {
                        times[start + k] = mean + slope * idx(k);
                    }
                    samples += n - 1.0;
                    seconds += slope * (n - 1.0);
                }
                segments.push(y.len() as u64);
                start = i;
            }
            let rate = if samples > 0.0 {
                samples / seconds
            } else {
                1.0 / tdiff
            };
            Expected {
                times,
                rate,
                segments,
            }
        })
        .collect();
    let t0 = all.iter().map(|e| e.times[0]).fold(f64::INFINITY, f64::min);
    for e in &mut all {
        for t in &mut e.times {
            *t -= t0;
        }
    }
    (all, t0)
}

/// What kymo should make of the synchronized time stamps of a stream on its own time
/// stamps (irregular, or with a nominal rate that can drop samples): never before the
/// previous one, gaps where a step is longer than 1 s and 500 median steps (the lower
/// median; if 0, the average step), or for a nominal rate 1 s and 2 periods; the average
/// rate within the segments.
fn own_times(mut times: Vec<f64>, irregular: bool, tdiff: f64) -> Expected {
    for i in 1..times.len() {
        times[i] = times[i].max(times[i - 1]);
    }
    let mut steps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    steps.sort_by(f64::total_cmp);
    let n = times.len();
    let span = times[n - 1] - times[0];
    let usual = if !irregular {
        tdiff
    } else {
        match steps.get(steps.len().saturating_sub(1) / 2) {
            Some(&m) if m > 0.0 => m,
            _ if span > 0.0 => span / (n - 1) as f64,
            _ => 1.0,
        }
    };
    let threshold = if irregular {
        1f64.max(500.0 * usual)
    } else {
        1f64.max(2.0 * tdiff)
    };
    let mut segments = vec![1u64];
    let (mut samples, mut seconds) = (0u64, 0.0);
    for i in 1..n {
        let step = times[i] - times[i - 1];
        if step > threshold {
            segments.push(1);
        } else {
            *segments.last_mut().unwrap() += 1;
            samples += 1;
            seconds += step;
        }
    }
    let rate = if samples > 0 && seconds > 0.0 {
        samples as f64 / seconds
    } else {
        1.0 / usual
    };
    Expected {
        times,
        rate,
        segments,
    }
}

/// The grid slot of each of a grid's samples, its segments holding them in order.
fn slots(grid: &kymo_core::Grid) -> Vec<u64> {
    grid.segments().iter().flat_map(|s| s.clone()).collect()
}

/// The annotations kymo should read from `spec`'s marker streams, as (onset, description),
/// sorted: string texts as they are, numeric ones without NaN values, irregular numeric
/// streams not of an event type left out.
/// The annotations of `spec`'s marker streams read as annotations: onset, text and the
/// stream's id, by onset.
fn expected_annotations(spec: &Spec) -> Vec<(f64, String, u32)> {
    // Markers are written at their true times, which synchronization restores.
    let first = timeline(spec).1 - spec.xdf.clock;
    let mut expected: Vec<(f64, String, u32)> = spec
        .xdf
        .markers
        .iter()
        .filter(|m| !marker_grids(spec).contains(m))
        .flat_map(|m| {
            marker_annotations(spec, m)
                .into_iter()
                .map(move |(onset, text)| (onset - first, text, m.id))
        })
        .collect();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    expected
}

/// The annotations marker stream `m` holds, as kymo reads them if it reads the stream as
/// annotations: true onset and text, one per non-empty text.
fn marker_annotations(spec: &Spec, m: &XdfMarkers) -> Vec<(f64, String)> {
    let mut annotations = Vec::new();
    for a in &spec.annotations[m.annotations.clone()] {
        for text in &a.texts {
            let text = match m.values {
                MarkerValues::Strings => text.clone(),
                MarkerValues::Numeric(..) => text
                    .split(',')
                    .filter(|v| *v != "nan")
                    .collect::<Vec<_>>()
                    .join(","),
            };
            if !text.is_empty() {
                annotations.push((a.onset, text));
            }
        }
    }
    annotations
}

/// Checks `rec`'s streams against `spec`'s: every stream in id order with its name and
/// type, the channels of each stream shown as channels (consecutive, in id order), and the
/// number of annotations of each other marker stream.
fn check_streams(spec: &Spec, rec: &Recording) {
    let shown = marker_grids(spec);
    let mut expected: Vec<(u32, String, String, usize, Option<usize>)> = streams(spec)
        .iter()
        .map(|s| (s.id, s.name.clone(), s.kind.clone(), s.channels.len(), None))
        .chain(spec.xdf.markers.iter().map(|m| {
            let (channels, annotations) = match m.values {
                MarkerValues::Numeric(_, n) if shown.contains(&m) => (n, None),
                _ => (0, Some(marker_annotations(spec, m).len())),
            };
            (m.id, m.name.clone(), m.kind.clone(), channels, annotations)
        }))
        .collect();
    expected.sort_by_key(|e| e.0);
    let mut next = 0;
    let expected: Vec<Stream> = expected
        .into_iter()
        .map(|(id, name, kind, channels, annotations)| {
            let content = match annotations {
                Some(n) => StreamContent::Annotations(n),
                None => {
                    next += channels as u32;
                    StreamContent::Channels((next - channels as u32..next).map(ChannelId).collect())
                }
            };
            Stream {
                id,
                name,
                kind,
                content,
            }
        })
        .collect();
    assert_eq!(rec.streams(), expected);
}

/// Whether `rec`'s `annotations` are `spec`'s, each in its stream.
fn same_annotations(spec: &Spec, rec: &Recording, annotations: &[AnnotationData]) -> bool {
    let Some(mut got) = annotations
        .iter()
        .map(|a| {
            let stream = rec.streams().get(a.stream?)?;
            Some((a.onset, a.description.clone(), stream.id))
        })
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    got.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let expected = expected_annotations(spec);
    let tolerance = 1e-9 * spec.xdf.clock.abs().max(1.0);
    annotations
        .iter()
        .all(|a| a.duration == 0.0 && a.channels.is_empty())
        && got.len() == expected.len()
        && got
            .iter()
            .zip(&expected)
            .all(|(g, e)| (g.0 - e.0).abs() <= tolerance && g.1 == e.1 && g.2 == e.2)
}

/// Marker streams of `spec` that kymo shows as channels: irregular, numeric, not of an
/// event type.
fn marker_grids(spec: &Spec) -> Vec<&XdfMarkers> {
    spec.xdf
        .markers
        .iter()
        .filter(|m| {
            matches!(m.values, MarkerValues::Numeric(..))
                && m.rate == 0.0
                && ![
                    "marker", "markers", "event", "events", "trigger", "triggers", "stim",
                ]
                .contains(&m.kind.to_lowercase().as_str())
        })
        .collect()
}

/// Checks every grid of `rec` against `spec`'s streams: rate, segments, every sample's
/// time, sample type, and the samples over `reads` (grid slots; or everything). Grids of
/// marker streams shown as channels are left to `check_marker_grids`.
fn check_grids(spec: &Spec, rec: &Recording, reads: Option<&[(usize, Range<u64>)]>) {
    let mut streams = streams(spec);
    streams.sort_by_key(|s| s.id);
    let markers: Vec<u32> = marker_grids(spec).iter().map(|m| m.id).collect();
    let mut ids: Vec<u32> = streams
        .iter()
        .map(|s| s.id)
        .chain(markers.iter().copied())
        .collect();
    ids.sort_unstable();
    assert_eq!(rec.grids().len(), ids.len());
    check_streams(spec, rec);
    let (expected, _) = timeline(spec);
    let grids = rec
        .grids()
        .iter()
        .enumerate()
        .filter(|&(g, _)| !markers.contains(&ids[g]));
    for ((g, grid), stream) in grids.zip(&streams) {
        let e = &expected[streams.iter().position(|s| s.id == stream.id).unwrap()];
        let c = stream.channels[0];
        assert!(
            (grid.rate() - e.rate).abs() <= 1e-9 * e.rate,
            "stream {}: rate {} vs {}",
            stream.id,
            grid.rate(),
            e.rate
        );
        let slots = slots(grid);
        assert_eq!(slots.len() as u64, spec.len(c), "stream {}", stream.id);
        assert_eq!(
            grid.segments().len(),
            e.segments.len(),
            "stream {}",
            stream.id
        );
        for (i, &slot) in slots.iter().enumerate() {
            let t = grid.time_of(slot);
            assert!(
                (t - e.times[i]).abs() < 1e-6,
                "stream {} sample {i}: {t} vs {}",
                stream.id,
                e.times[i]
            );
        }
        let native = match stream.format {
            XdfFormat::I8 | XdfFormat::I16 => SampleType::I16,
            XdfFormat::I32 => SampleType::I32,
            _ => SampleType::F32,
        };
        assert_eq!(grid.sample_type(), native);
        let ids = grid.channels().to_vec();
        let all = [(g, 0..grid.len())];
        for (_, range) in reads.unwrap_or(&all).iter().filter(|(k, _)| *k == g) {
            for piece in grid.present_in(range.clone()).collect::<Vec<_>>() {
                let got = physical(rec, GridId(g as u32), &ids, piece.clone());
                let first = slots.partition_point(|&s| s < piece.start) as u64;
                let samples = first..first + (piece.end - piece.start);
                assert!(matches(spec, &ids, samples, &got), "{piece:?}: {got:?}");
            }
            rec.prefetch(GridId(g as u32), range.clone());
        }
    }
}

#[derive(Clone, Debug)]
struct Case {
    spec: Spec,
    reads: Vec<(
        prop::sample::Index,
        prop::sample::Index,
        prop::sample::Index,
    )>,
}

fn stamps() -> impl Strategy<Value = XdfStamps> {
    prop_oneof![
        Just(XdfStamps::All),
        Just(XdfStamps::FirstOfChunk),
        (1u64..6).prop_map(XdfStamps::Every),
    ]
}

/// A stream's time: how far its rate is off nominal, its jitter (in sample periods), and
/// maybe a break (where, how much longer than the threshold, the rate error after it).
type Timing = (f64, f64, Option<(prop::sample::Index, f64, f64)>);

/// One stream: its format, rate, channel kinds (EEG or trigger), start, timestamps,
/// chunking, whether it describes its channels, and its time.
type StreamCase = (
    XdfFormat,
    u32,
    Vec<bool>,
    f64,
    XdfStamps,
    u64,
    bool,
    bool,
    Timing,
    u8,
);

fn stream() -> impl Strategy<Value = StreamCase> {
    (
        proptest::sample::select(XdfFormat::ALL.to_vec()),
        proptest::sample::select(vec![1u32, 3, 50, 256]),
        proptest::collection::vec(any::<bool>(), 1..4),
        proptest::sample::select(vec![0.0, 0.25, 1.3]),
        stamps(),
        1u64..60,
        any::<bool>(),
        any::<bool>(),
        (
            proptest::sample::select(vec![0.0, 1e-4, -5e-3]),
            proptest::sample::select(vec![0.0, 0.2]),
            proptest::option::of((
                any::<prop::sample::Index>(),
                0.5f64..20.0,
                proptest::sample::select(vec![0.0, 3e-3]),
            )),
        ),
        // Regular, irregular, or with a nominal rate but able to drop samples.
        proptest::sample::select(vec![0u8, 0, 1, 2]),
    )
}

fn case() -> impl Strategy<Value = Case> {
    (
        proptest::collection::vec(stream(), 1..4),
        1u64..7,
        proptest::sample::select(vec![1.0, 0.5]),
        proptest::sample::select(vec![0.0, 12_345.678]),
        (any::<bool>(), any::<bool>(), any::<bool>()),
        any::<u64>(),
        proptest::collection::vec(
            any::<(
                prop::sample::Index,
                prop::sample::Index,
                prop::sample::Index,
            )>(),
            1..5,
        ),
    )
        .prop_map(
            |(defs, records, duration, clock, (wide, offsets, boundaries), seed, reads)| {
                let length = records;
                let ids = [42u32, 7, 0xDEAD_BEEF];
                let mut channels = Vec::new();
                let mut xdf_streams = Vec::new();
                for (
                    k,
                    (format, spr, triggers, start, stamps, chunk, varying, described, timing, how),
                ) in defs.into_iter().enumerate()
                {
                    let (rate_error, jitter, gap) = timing;
                    let tdiff = duration / f64::from(spr);
                    let len = length * u64::from(spr);
                    let irregular = how == 1;
                    // A break longer than the threshold: 1 s and 2 samples, or for an
                    // irregular stream 1 s and 500 median steps, which deduced time stamps
                    // (each the previous one) can make up to 5 periods.
                    let threshold = if irregular {
                        3000.0 * tdiff
                    } else {
                        2.0 * tdiff
                    };
                    let breaks = gap
                        .filter(|_| len > 1)
                        .map(|(at, extra, rate_error)| XdfBreak {
                            before: 1 + at.index(len as usize - 1) as u64,
                            seconds: 1f64.max(threshold) + extra,
                            rate_error,
                        })
                        .into_iter()
                        .collect();
                    let mut members = Vec::new();
                    for (i, trigger) in triggers.into_iter().enumerate() {
                        let label = format!("S{k}C{i}");
                        // Codes up to 65535 need a format that holds them.
                        let holds_codes = format.limits().1 >= 65_535;
                        let channel = if trigger && holds_codes {
                            ChannelSpec {
                                label,
                                kind: "Trigger".into(),
                                ..ChannelSpec::trigger(spr, Format::Xdf)
                            }
                        } else {
                            ChannelSpec::xdf(label, "EEG", "microvolts", spr, format)
                        };
                        members.push(channels.len());
                        channels.push(channel);
                    }
                    xdf_streams.push(XdfStream {
                        format,
                        start,
                        stamps,
                        chunk,
                        varying_chunks: varying,
                        described,
                        rate_error,
                        jitter: jitter * tdiff,
                        breaks,
                        irregular,
                        can_drop: how == 2,
                        ..XdfStream::new(ids[k], &format!("stream {k}"), "EEG", members)
                    });
                }
                let mut spec = Spec::new(Format::Xdf, channels, duration, records);
                spec.seed = seed;
                // Spikes often, so the digital limits are read too.
                spec.spike_every = 7;
                spec.xdf.streams = xdf_streams;
                spec.xdf.clock = clock;
                spec.xdf.wide_lengths = wide;
                spec.xdf.clock_offsets = offsets;
                // Drifting clocks, which the offsets correct (without them, kymo reads the
                // time stamps as they are).
                spec.xdf.clock_offset = -0.75;
                spec.xdf.clock_drift = 3e-6;
                spec.xdf.boundaries = boundaries;
                Case { spec, reads }
            },
        )
}

proptest! {
    /// Every value format reads back the truth function for any range, with timestamps on
    /// all, some or the first sample of each chunk, chunks of any (and varying) length,
    /// streams starting at different times, and lengths of every width; every sample is at
    /// the time pyxdf's synchronization and dejittering give it (drifting clocks, rates off
    /// nominal, jitter, breaks as gaps with the rate changing after them), or for irregular
    /// streams and those that can drop samples, its synchronized time stamp.
    #[test]
    fn random_reads_match_the_truth(case in case()) {
        let spec = &case.spec;
        let file = TempFile::written(spec);
        prop_assert_eq!(std::fs::metadata(file.path()).unwrap().len(), spec.file_size().unwrap());
        let rec = open(file.path());
        prop_assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        prop_assert_eq!(rec.format(), "XDF");
        prop_assert_eq!(rec.sources(), &[file.path().to_owned()][..]);
        let order = channel_order(spec);
        for (channel, &c) in rec.channels().iter().zip(&order) {
            let (stream, i) = stream_of(spec, c).unwrap();
            let name = if stream.described {
                spec.channels[c].label.clone()
            } else {
                format!("{}_{i}", stream.name)
            };
            prop_assert_eq!(&channel.name, &name);
            let kind = if spec.channels[c].trigger { ChannelKind::Stim } else { ChannelKind::Eeg };
            prop_assert_eq!(channel.kind, if stream.described { kind } else { ChannelKind::Misc });
        }
        let reads: Vec<(usize, Range<u64>)> = case
            .reads
            .iter()
            .map(|(g, a, b)| {
                let g = g.index(rec.grids().len());
                let len = rec.grids()[g].len() as usize + 1;
                let (a, b) = (a.index(len) as u64, b.index(len) as u64);
                (g, a.min(b)..a.max(b))
            })
            .collect();
        check_grids(spec, &rec, Some(&reads));
    }
}

#[test]
fn presets_open_with_their_channels() {
    use ChannelKind::*;
    for (preset, warnings) in [
        ("xdf-float32", 0),
        ("xdf-mixed", 1),
        ("xdf-events", 0),
        ("xdf-clocks", 0),
        ("xdf-irregular", 0),
    ] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len(),
            spec.file_size().unwrap()
        );
        let (rec, annotations) = open_with(file.path());
        assert_eq!(
            rec.warnings().len(),
            warnings,
            "{preset}: {:?}",
            rec.warnings()
        );
        assert!(
            same_annotations(&spec, &rec, &annotations),
            "{preset}: {:?}",
            annotations
                .iter()
                .map(|a| (a.onset, &a.description))
                .collect::<Vec<_>>()
        );
        check_grids(&spec, &rec, None);
    }

    let rec = open(TempFile::preset("xdf-float32").path());
    assert_eq!(
        rec.start_time(),
        Some(jiff::civil::date(2024, 1, 2).at(3, 4, 5, 0))
    );
    let extra = |rec: &Recording, key: &str| {
        rec.extra()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(extra(&rec, "time zone").as_deref(), Some("UTC+01:00"));
    assert_eq!(extra(&rec, "XDF version").as_deref(), Some("1.0"));
    assert_eq!(
        extra(&rec, "stream 1").as_deref(),
        Some(
            "\"BrainAmp\" (EEG), 8 × float32, 500 Hz, 30000 samples: channels; effective \
             rate 500.000000 Hz, 1 segment"
        )
    );
    assert_eq!(
        extra(&rec, "stream 2").as_deref(),
        Some("\"BrainAmp Markers\" (Markers), 1 × string, irregular, 36 samples: annotations")
    );
    for channel in rec.channels() {
        assert_eq!((channel.kind, channel.unit.to_si), (Eeg, Some(1e-6)));
        assert_eq!(channel.unit.quantity(), Some(Quantity::Voltage));
    }

    // Drifting clocks with a reset, and a break in each stream.
    let rec = open(TempFile::preset("xdf-clocks").path());
    for id in [1, 2] {
        let field = extra(&rec, &format!("stream {id}")).unwrap();
        assert!(field.ends_with("Hz, 2 segments, 1 clock reset"), "{field}");
    }
    assert!(matches!(
        rec.grids()[1].timebase(),
        kymo_core::Timebase::Segmented { pieces, .. } if pieces.len() == 2
    ));

    let rec = open(TempFile::preset("xdf-mixed").path());
    assert_eq!(rec.start_time(), None);
    let channels: Vec<(&str, ChannelKind, &str)> = rec
        .channels()
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.unit.label.as_str()))
        .collect();
    assert_eq!(
        channels,
        [
            // Labels two streams of one name share get the stream type.
            ("ACC X", Misc, "g"),
            ("ACC Y", Misc, "g"),
            ("GYRO X", Misc, "dps"),
            ("GYRO Y", Misc, "dps"),
            ("Fz", Eeg, "microvolts"),
            ("Cz", Eeg, "microvolts"),
            ("Pz", Eeg, "\u{b5}V"),
            ("EOG", Eog, "mV"),
            ("EMG", Emg, "uV"),
            ("DC", Misc, "nV"),
            ("Temp", Misc, "degrees celsius"),
            // An undescribed stream.
            ("Aux_0", Misc, ""),
            ("Aux_1", Misc, ""),
        ]
    );
    // Names within their streams, where shorter: the labels without the qualifier, the
    // numbers of unlabeled channels.
    let in_stream: Vec<Option<&str>> = rec
        .channels()
        .iter()
        .map(|c| c.in_stream.as_deref())
        .collect();
    let mut want = vec![Some("X"), Some("Y"), Some("X"), Some("Y")];
    want.extend([None; 7]);
    want.extend([Some("0"), Some("1")]);
    assert_eq!(in_stream, want);
    assert!(rec.warnings()[0].contains("g, dps"), "{:?}", rec.warnings());
    // The int64 DC channel is read relative to its first sample, folded into the offset.
    let dc = &rec.channels()[9];
    assert!((dc.offset - 1e9).abs() < 1e3, "{}", dc.offset);
    assert_eq!(rec.channels()[10].offset, spec_first(&rec, 10));

    let rec = open(TempFile::preset("xdf-events").path());
    let kinds: Vec<ChannelKind> = rec.channels().iter().map(|c| c.kind).collect();
    // A Trigger channel, the channel of a regular Markers stream, and the irregular
    // "Ints" and "Mouse" pairs, of types MNE doesn't have.
    assert_eq!(kinds, [Eeg, Eeg, Eeg, Stim, Stim, Misc, Misc, Misc]);
    assert!(
        rec.channels()
            .iter()
            .all(|c| c.events && c.trigger_mask.is_none())
    );
    assert_eq!(rec.channels()[3].offset, 0.0);
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert!(
        extra(&rec, "stream 5")
            .unwrap()
            .contains("channels; own time stamps, average rate")
    );
}

/// The offset of channel `c` of `rec`, which for an int64 or double64 channel is its first
/// sample's value.
fn spec_first(rec: &Recording, c: u32) -> f64 {
    let channel = rec.channel(ChannelId(c));
    let mut out = [0.0f32];
    rec.read_physical(channel.grid, &[ChannelId(c)], 0..1, &mut out)
        .unwrap();
    assert_eq!(out[0], channel.offset as f32);
    channel.offset
}

/// An XDF file put together chunk by chunk, as `rec.xdf` in its own directory.
struct Built {
    _dir: TempDir,
    path: PathBuf,
}

impl Built {
    fn new(bytes: &[u8]) -> Built {
        let dir = TempDir::new();
        let path = dir.path().join("rec.xdf");
        std::fs::write(&path, bytes).unwrap();
        Built { _dir: dir, path }
    }

    fn open(&self) -> Recording {
        open(&self.path)
    }

    fn open_with(&self) -> (Recording, Vec<AnnotationData>) {
        open_with(&self.path)
    }

    fn error(&self) -> XdfError {
        match kymo_core::open(&self.path) {
            Err(OpenError::Xdf { source, .. }) => source,
            other => panic!("expected an XDF error, got {other:?}"),
        }
    }
}

/// Three labeled int16 EEG channels at 10 Hz.
fn eeg_header(c: &mut Chunks, id: u32) {
    let channels: Vec<(String, String, String)> = ["A", "B", "C"]
        .iter()
        .map(|l| ((*l).to_owned(), "EEG".to_owned(), "microvolts".to_owned()))
        .collect();
    c.stream_header(
        id,
        &header_xml("Amp", "EEG", 3, 10.0, "int16", Some(&channels)),
    );
}

/// Samples `from..to` of [`eeg_header`]'s stream: channel `k` holds `100 k + i`, the first
/// stamped at `5 + i / 10`.
fn eeg_samples(range: Range<u64>) -> Vec<(Option<f64>, Vec<f64>)> {
    let first = range.start;
    range
        .map(|i| {
            let stamp = (i == first).then_some(5.0 + i as f64 / 10.0);
            (stamp, (0..3).map(|k| (100 * k) as f64 + i as f64).collect())
        })
        .collect()
}

fn values(rec: &Recording, c: u32) -> Vec<f32> {
    let channel = rec.channel(ChannelId(c));
    let len = rec.grid(channel.grid).len();
    if len == 0 {
        return Vec::new();
    }
    physical(rec, channel.grid, &[ChannelId(c)], 0..len)
}

fn has_warning(rec: &Recording, text: &str) -> bool {
    rec.warnings().iter().any(|w| w.contains(text))
}

/// A complete file of [`eeg_header`]'s stream (id 1) with two chunks and a footer, and
/// whatever `more` adds before the footer.
fn eeg_file(more: impl FnOnce(&mut Chunks)) -> Vec<u8> {
    let mut c = Chunks::new(None);
    eeg_header(&mut c, 1);
    c.numeric(1, XdfFormat::I16, &eeg_samples(0..4));
    c.numeric(1, XdfFormat::I16, &eeg_samples(4..10));
    more(&mut c);
    c.footer(1, "<info/>");
    c.bytes
}

#[test]
fn chunks_of_every_length_width_and_sample_count() {
    for wide in [false, true] {
        let mut c = Chunks::new(None);
        c.wide = wide;
        eeg_header(&mut c, 1);
        c.numeric(1, XdfFormat::I16, &eeg_samples(0..300));
        c.footer(1, "<info/>");
        let rec = Built::new(&c.bytes).open();
        assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
        let expected: Vec<f32> = (0..300).map(|i| 200.0 + i as f32).collect();
        assert_eq!(values(&rec, 2), expected);
    }
}

#[test]
fn damaged_chunks_keep_their_whole_samples() {
    let reference: Vec<f32> = (0..10).map(|i| 100.0 + i as f32).collect();
    let footer = 1 + 1 + 2 + 4 + "<info/>".len();

    // Cut inside the last Samples chunk: its whole samples are read.
    let bytes = eeg_file(|_| {});
    let rec = Built::new(&bytes[..bytes.len() - footer - 5]).open();
    assert!(has_warning(
        &rec,
        "ends inside a Samples chunk of stream 1 (Amp)"
    ));
    assert!(has_warning(&rec, "no footer"));
    assert_eq!(values(&rec, 1), reference[..9]);

    // A damaged timestamp flag after the chunk's timestamps (its length says how many it
    // has) is never looked at: the samples read as the index has them.
    let mut bytes = eeg_file(|_| {});
    let first = first_sample(&bytes);
    bytes[first + 15 + 7] = 3;
    let rec = Built::new(&bytes).open();
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    assert_eq!(values(&rec, 1), reference);

    // An invalid flag before them ends its chunk there; the next chunk is read.
    let mut c = Chunks::new(None);
    eeg_header(&mut c, 1);
    let mut samples = eeg_samples(0..4);
    samples[0].0 = None;
    samples[2].0 = Some(5.2);
    c.numeric(1, XdfFormat::I16, &samples);
    c.numeric(1, XdfFormat::I16, &eeg_samples(4..10));
    c.footer(1, "<info/>");
    let mut bytes = c.bytes;
    let first = first_sample(&bytes);
    bytes[first + 7] = 3;
    let rec = Built::new(&bytes).open();
    assert!(has_warning(
        &rec,
        "sample 1 has an invalid timestamp flag (3)"
    ));
    assert_eq!(
        values(&rec, 1),
        [100.0, 104.0, 105.0, 106.0, 107.0, 108.0, 109.0]
    );

    // A chunk whose count says more samples than it holds, and one with bytes after them.
    let samples = |count: u8, extra: &[u8]| {
        let mut content = 1u32.to_le_bytes().to_vec();
        content.extend_from_slice(&[1, count]);
        for (stamp, v) in eeg_samples(0..4) {
            match stamp {
                Some(t) => {
                    content.push(8);
                    content.extend_from_slice(&t.to_le_bytes());
                }
                None => content.push(0),
            }
            for x in v {
                content.extend_from_slice(&(x as i16).to_le_bytes());
            }
        }
        content.extend_from_slice(extra);
        content
    };
    let mut c = Chunks::new(None);
    eeg_header(&mut c, 1);
    c.raw(tag::SAMPLES, &samples(6, &[]));
    c.raw(tag::SAMPLES, &samples(4, &[0, 0, 0]));
    c.footer(1, "<info/>");
    let rec = Built::new(&c.bytes).open();
    assert!(has_warning(&rec, "fewer samples than its count (6)"));
    assert!(has_warning(
        &rec,
        "3 bytes after its last sample are ignored"
    ));
    assert_eq!(values(&rec, 0).len(), 8);

    // An invalid chunk length ends the scan.
    let mut bytes = eeg_file(|_| {});
    let at = bytes.len() - footer;
    bytes[at] = 3;
    let rec = Built::new(&bytes).open();
    assert!(has_warning(
        &rec,
        &format!("the chunk at byte {at} has an invalid length")
    ));
    assert_eq!(values(&rec, 1), reference);
}

/// The byte of the first sample (its timestamp flag) in the first Samples chunk of `bytes`
/// (with a one-byte sample count).
fn first_sample(bytes: &[u8]) -> usize {
    let mut pos = 4;
    loop {
        let width = bytes[pos] as usize;
        let mut len = [0u8; 8];
        len[..width].copy_from_slice(&bytes[pos + 1..pos + 1 + width]);
        let len = u64::from_le_bytes(len) as usize;
        let content = pos + 1 + width;
        if u16::from_le_bytes([bytes[content], bytes[content + 1]]) == tag::SAMPLES {
            // The tag, the stream id and the count (its width and one byte).
            return content + 2 + 4 + 2;
        }
        pos = content + len;
    }
}

#[test]
fn chunks_out_of_place_and_unknown() {
    // Samples before their stream's header; a tag XDF doesn't have, after which the walk
    // goes on after the next boundary chunk (skipping the clock offset before it).
    let mut c = Chunks::new(None);
    c.numeric(1, XdfFormat::I16, &eeg_samples(0..2));
    eeg_header(&mut c, 1);
    let at = c.bytes.len();
    c.raw(77, b"whatever");
    c.clock_offset(1, 10.0, -0.5);
    c.boundary();
    c.numeric(1, XdfFormat::I16, &eeg_samples(2..5));
    c.footer(1, "<info/>");
    let rec = Built::new(&c.bytes).open();
    assert!(has_warning(
        &rec,
        "samples of stream 1 come before its header"
    ));
    let skipped = (1 + 1 + 2 + 8) + (1 + 1 + 2 + 20) + (1 + 1 + 2 + 16);
    assert!(
        has_warning(
            &rec,
            &format!(
                "the chunk at byte {at} has the tag 77, which XDF doesn't have; the \
                 {skipped} bytes up to and including the next boundary chunk are skipped"
            )
        ),
        "{:?}",
        rec.warnings()
    );
    assert_eq!(rec.warnings().len(), 2, "{:?}", rec.warnings());
    assert_eq!(values(&rec, 0), [2.0, 3.0, 4.0]);
    // The first sample is at its own timestamp.
    assert_eq!(rec.grids()[0].time_of(0), 0.0);
}

#[test]
fn streams_kymo_doesnt_show() {
    // Only a string marker stream and an irregular numeric one of an event type:
    // annotations.
    let mut c = Chunks::new(None);
    c.stream_header(3, &header_xml("Markers", "Markers", 1, 0.0, "string", None));
    c.stream_header(4, &header_xml("Speed", "Events", 1, 0.0, "float32", None));
    c.strings(3, &[(Some(1.0), vec!["x".into()])]);
    c.numeric(4, XdfFormat::F32, &[(Some(1.0), vec![1.0])]);
    match Built::new(&c.bytes).error() {
        XdfError::NoChannels(listed) => {
            assert!(listed.contains("stream 3 (Markers)"), "{listed}");
            assert!(listed.contains("stream 4 (Speed)"), "{listed}");
            assert!(!listed.contains("channels"), "{listed}");
        }
        other => panic!("{other:?}"),
    }

    // Headers kymo can't read, and an empty stream.
    let bytes = eeg_file(|c| {
        c.stream_header(5, "<info><name>Broken</name>");
        c.stream_header(6, &header_xml("Wide", "EEG", 2, 10.0, "float128", None));
        c.stream_header(7, &header_xml("Odd", "EEG", 2, -1.0, "float32", None));
        c.stream_header(8, &header_xml("Empty", "EEG", 2, 10.0, "float32", None));
        c.numeric(8, XdfFormat::F32, &[]);
        c.stream_header(9, &header_xml("Quiet", "Markers", 1, 0.0, "string", None));
        for id in [5, 6, 7, 8, 9] {
            c.footer(id, "<info/>");
        }
    });
    let rec = Built::new(&bytes).open();
    assert_eq!(rec.grids().len(), 1);
    assert!(has_warning(
        &rec,
        "stream 6 (Wide) is not read: its channel format \"float128\""
    ));
    assert!(has_warning(
        &rec,
        "stream 7 (Odd) is not read: its channel count (\"2\") or rate"
    ));
    let extra = |key: &str| {
        rec.extra()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert!(extra("stream 5").contains("its header can't be read"));
    assert!(has_warning(
        &rec,
        "stream 5 is not read: its header can't be read"
    ));
    assert!(extra("stream 8").ends_with("0 samples: no samples"));
    assert_eq!(rec.warnings().len(), 3, "{:?}", rec.warnings());
    // Every stream is listed, with why it isn't shown.
    let listed: Vec<(u32, &str, &str, &StreamContent)> = rec
        .streams()
        .iter()
        .map(|s| (s.id, s.name.as_str(), s.kind.as_str(), &s.content))
        .collect();
    let not_shown = |reason: &str| StreamContent::NotShown(reason.to_owned());
    let reason = |id: u32| match &rec.streams()[id as usize - 4].content {
        StreamContent::NotShown(reason) => reason.clone(),
        other => panic!("{other:?}"),
    };
    assert!(reason(5).starts_with("its header can't be read"));
    assert!(reason(6).contains("\"float128\""));
    assert!(reason(7).contains("rate"));
    assert_eq!(
        listed,
        [
            (
                1,
                "Amp",
                "EEG",
                &StreamContent::Channels((0..3).map(ChannelId).collect())
            ),
            (5, "", "", &not_shown(&reason(5))),
            (6, "Wide", "EEG", &not_shown(&reason(6))),
            (7, "Odd", "EEG", &not_shown(&reason(7))),
            (8, "Empty", "EEG", &not_shown("it has no samples")),
            (9, "Quiet", "Markers", &not_shown("it has no samples")),
        ]
    );
    // Annotation files name only streams that can hold annotations.
    assert_eq!(
        kymo_core::annotation_file::Context::of(&rec).streams,
        [Some(1), None, None, None, None, None]
    );

    // Not an XDF file at all, given to the reader.
    let built = Built::new(b"EDF+ or so");
    assert!(matches!(
        kymo_core::xdf::open(&built.path),
        Err(OpenError::Xdf {
            source: XdfError::NotXdf,
            ..
        })
    ));
}

#[test]
fn streams_start_at_their_first_timestamp() {
    // Stream 2 starts 1.5 s after stream 1; its first sample has no timestamp, its third
    // has one. Stream 3 has none at all.
    let mut c = Chunks::new(None);
    eeg_header(&mut c, 1);
    c.stream_header(2, &header_xml("Late", "EEG", 1, 4.0, "float32", None));
    c.stream_header(3, &header_xml("Lost", "EEG", 1, 4.0, "float32", None));
    c.numeric(1, XdfFormat::I16, &eeg_samples(0..10));
    c.numeric(
        2,
        XdfFormat::F32,
        &[
            (None, vec![1.0]),
            (None, vec![2.0]),
            (Some(5.0 + 1.5 + 0.5), vec![3.0]),
        ],
    );
    c.numeric(3, XdfFormat::F32, &[(None, vec![1.0])]);
    for id in 1..=3 {
        c.footer(id, "<info/>");
    }
    let rec = Built::new(&c.bytes).open();
    let starts: Vec<f64> = rec.grids().iter().map(|g| g.time_of(0)).collect();
    assert_eq!(starts, [0.0, 1.5, 0.0]);
    assert!(has_warning(&rec, "stream 3 (Lost) has no timestamps"));
    assert_eq!(values(&rec, 3), [1.0, 2.0, 3.0]);
}

#[test]
fn labels_kinds_and_units() {
    use ChannelKind::*;
    let described = |labels: &[(&str, &str, &str)]| -> Described {
        labels
            .iter()
            .map(|(l, k, u)| ((*l).to_owned(), (*k).to_owned(), (*u).to_owned()))
            .collect()
    };
    let mut c = Chunks::new(None);
    let streams: [(u32, &str, &str, Described); 6] = [
        (
            1,
            "Muse",
            "ACC",
            described(&[("X", "accelerometer", "g"), ("", "EEG", "microvolts")]),
        ),
        (
            2,
            "Muse",
            "GYRO",
            described(&[("X", "", "dps"), ("Y", "ECG", "mV")]),
        ),
        // Same name and type as another: the id tells them apart.
        (
            3,
            "Muse",
            "GYRO",
            described(&[("X", "trigger", "NA"), ("Z", "Stim", "")]),
        ),
        (
            4,
            "Glove",
            "Hand",
            described(&[("Y", "Triggers", ""), ("F", "seeg", "kiloohm")]),
        ),
        // A regular stream of an event type is stim, whatever its channels' types.
        (
            5,
            "Codes",
            "Markers",
            described(&[("C1", "EEG", "uV"), ("C2", "", "")]),
        ),
        (
            6,
            "Pos",
            "Mocap",
            described(&[("mm", "misc", "mm"), ("deg", "bogus", "deg")]),
        ),
    ];
    for (id, name, kind, channels) in &streams {
        c.stream_header(
            *id,
            &header_xml(name, kind, 2, 10.0, "float32", Some(channels)),
        );
        c.numeric(*id, XdfFormat::F32, &[(Some(1.0), vec![f64::NAN, 1.0])]);
        c.footer(*id, "<info/>");
    }
    // A header describing fewer channels than it has.
    let one = described(&[("only", "EEG", "V")]);
    c.stream_header(7, &header_xml("Few", "EEG", 2, 10.0, "float32", Some(&one)));
    c.numeric(7, XdfFormat::F32, &[(Some(1.0), vec![1.0, 2.0])]);
    c.footer(7, "<info/>");
    let rec = Built::new(&c.bytes).open();
    let got: Vec<(&str, ChannelKind, &str, Option<f64>)> = rec
        .channels()
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.unit.label.as_str(), c.unit.to_si))
        .collect();
    assert_eq!(
        got,
        [
            // Streams 2 and 3 share its name, but not its type.
            ("ACC X", Misc, "g", None),
            ("Muse_1", Eeg, "microvolts", Some(1e-6)),
            // Streams 2 and 3 share name and type.
            ("stream 2 X", Misc, "dps", None),
            ("stream 2 Y", Ecg, "mV", Some(1e-3)),
            ("stream 3 X", Stim, "", None),
            ("Z", Stim, "", None),
            ("Glove Y", Stim, "", None),
            ("F", Seeg, "kiloohm", None),
            ("C1", Stim, "uV", Some(1e-6)),
            ("C2", Stim, "", None),
            ("mm", Misc, "mm", None),
            ("deg", Misc, "deg", None),
            ("Few_0", Misc, "", None),
            ("Few_1", Misc, "", None),
        ]
    );
    let in_stream: Vec<Option<&str>> = rec
        .channels()
        .iter()
        .map(|c| c.in_stream.as_deref())
        .collect();
    assert_eq!(
        in_stream,
        [
            Some("X"),
            Some("1"),
            Some("X"),
            Some("Y"),
            Some("X"),
            None,
            Some("Y"),
            None,
            None,
            None,
            None,
            None,
            Some("0"),
            Some("1"),
        ]
    );
    assert!(has_warning(
        &rec,
        "units not recognized, so their channels are not scaled: g, dps, kiloohm"
    ));
    assert!(has_warning(
        &rec,
        "describes 1 channels, but the stream has 2"
    ));
    assert_eq!(rec.warnings().len(), 2, "{:?}", rec.warnings());
    // Stim channels' NaN is "no event", 0; other channels keep it.
    assert_eq!(values(&rec, 4), [0.0]);
    assert!(values(&rec, 2)[0].is_nan());
}

#[test]
fn start_times() {
    for (datetime, start, zone, warns) in [
        (
            Some("2021-12-15T10:46:02+0100"),
            Some((10, 46, 2, 0)),
            Some("UTC+01:00"),
            false,
        ),
        (
            Some("2021-12-15T10:46:02.25-05:30"),
            Some((10, 46, 2, 250_000_000)),
            Some("UTC-05:30"),
            false,
        ),
        (
            Some("2021-12-15T10:46:02Z"),
            Some((10, 46, 2, 0)),
            Some("UTC"),
            false,
        ),
        (
            Some("2021-12-15T10:46:02"),
            Some((10, 46, 2, 0)),
            None,
            false,
        ),
        (Some("2021-12-15T10:46:02+1"), None, None, true),
        (Some("yesterday"), None, None, true),
        (None, None, None, false),
    ] {
        let mut c = Chunks::new(datetime);
        eeg_header(&mut c, 1);
        c.numeric(1, XdfFormat::I16, &eeg_samples(0..2));
        c.footer(1, "<info/>");
        let rec = Built::new(&c.bytes).open();
        let expected = start.map(|(h, m, s, ns)| jiff::civil::date(2021, 12, 15).at(h, m, s, ns));
        assert_eq!(rec.start_time(), expected, "{datetime:?}");
        let got_zone = rec
            .extra()
            .iter()
            .find(|(k, _)| k == "time zone")
            .map(|(_, v)| v.as_str());
        assert_eq!(got_zone, zone, "{datetime:?}");
        assert_eq!(
            has_warning(&rec, "can't be read; it has no start time"),
            warns,
            "{datetime:?}"
        );
    }
}

#[test]
fn annotations_from_strings_and_numbers() {
    let bytes = eeg_file(|c| {
        // Two string channels, at a nominal 10 Hz: samples without a timestamp follow the
        // previous one by a sample period; those before the first are left out.
        c.stream_header(2, &header_xml("Words", "Markers", 2, 10.0, "string", None));
        c.strings(
            2,
            &[
                (None, vec!["lost".into(), "".into()]),
                (Some(5.5), vec!["a".into(), "".into()]),
                (None, vec!["b".into(), "c & d".into()]),
            ],
        );
        // Numbers at an irregular rate: NaN left out, all-NaN samples none.
        c.stream_header(3, &header_xml("Pairs", "Events", 2, 0.0, "double64", None));
        c.numeric(
            3,
            XdfFormat::F64,
            &[
                (Some(6.0), vec![1.0, f64::NAN]),
                (Some(6.25), vec![f64::NAN, f64::NAN]),
                (Some(6.5), vec![0.1, -2.5e-7]),
            ],
        );
        // One channel of int64, irregular, of an event type.
        c.stream_header(4, &header_xml("Big", "Trigger", 1, 0.0, "int64", None));
        let mut content = 4u32.to_le_bytes().to_vec();
        content.extend_from_slice(&[1, 1, 8]);
        content.extend_from_slice(&7.0f64.to_le_bytes());
        content.extend_from_slice(&(i64::MAX - 1).to_le_bytes());
        c.raw(tag::SAMPLES, &content);
        c.stream_header(5, &header_xml("F", "Markers", 1, 0.0, "float32", None));
        // Markers at a nominal rate keep their own (jittered) times: not dejittered.
        c.stream_header(6, &header_xml("Ticks", "Markers", 1, 10.0, "string", None));
        c.strings(
            6,
            &[
                (Some(9.0), vec!["t0".into()]),
                (Some(9.13), vec!["t1".into()]),
                (Some(9.2), vec!["t2".into()]),
            ],
        );
        c.footer(6, "<info/>");
        // A float32 marker stream: numpy prints float32 positionally only from 1e-4 to 1e6.
        c.numeric(
            5,
            XdfFormat::F32,
            &[
                (Some(7.5), vec![0.1]),
                (Some(7.75), vec![1e6]),
                (Some(7.75), vec![999_999.0]),
                (Some(8.0), vec![1e-4]),
            ],
        );
        for id in 2..=5 {
            c.footer(id, "<info/>");
        }
    });
    let (rec, annotations) = Built::new(&bytes).open_with();
    let mut got: Vec<(f64, &str)> = annotations
        .iter()
        .map(|a| (a.onset, a.description.as_str()))
        .collect();
    got.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(b.1)));
    let expected = [
        (0.5, "a"),
        (0.6, "b"),
        (0.6, "c & d"),
        (1.0, "1.0"),
        (1.5, "0.1,-2.5e-07"),
        (2.0, "9223372036854775806"),
        (2.5, "0.1"),
        (2.75, "1e+06"),
        (2.75, "999999.0"),
        (3.0, "1e-04"),
        (4.0, "t0"),
        (4.13, "t1"),
        (4.2, "t2"),
    ];
    assert_eq!(got.len(), expected.len(), "{got:?}");
    for (g, e) in got.iter().zip(&expected) {
        assert!((g.0 - e.0).abs() < 1e-9 && g.1 == e.1, "{got:?}");
    }
    assert!(has_warning(
        &rec,
        "stream 2 (Words) has 1 samples before its first timestamp"
    ));
    assert_eq!(rec.warnings().len(), 1, "{:?}", rec.warnings());
}

fn corruptible() -> Vec<Vec<u8>> {
    ["xdf-mixed", "xdf-events", "xdf-irregular"]
        .into_iter()
        .map(|p| {
            let mut spec = Spec::preset(p).unwrap();
            spec.n_records = 3;
            for s in &mut spec.xdf.streams {
                s.breaks.clear();
            }
            spec.annotations.retain(|a| a.onset < 3.0);
            for m in &mut spec.xdf.markers {
                m.annotations = 0..0;
            }
            std::fs::read(TempFile::written(&spec).path()).unwrap()
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Damaged files never panic: opening fails, or the recording reads (or fails to).
    #[test]
    fn damaged_files_never_panic(
        which in 0usize..3,
        changes in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<prop::sample::Index>()),
    ) {
        thread_local! {
            static FILES: Vec<Vec<u8>> = corruptible();
        }
        let mut bytes = FILES.with(|f| f[which].clone());
        for (at, value) in changes {
            let at = at.index(bytes.len());
            bytes[at] = value;
        }
        if let Some(cut) = cut {
            bytes.truncate(cut.index(bytes.len() + 1));
        }
        let built = Built::new(&bytes);
        if let Ok(opened) = kymo_core::open(&built.path)
            && let Ok((rec, _)) = Recording::new(opened)
        {
            for (g, grid) in rec.grids().iter().enumerate() {
                let g = GridId(g as u32);
                let ids = grid.channels().to_vec();
                let _ = grid.sample_at(grid.time_of(grid.len() / 2));
                for s in grid.segments().iter().take(4) {
                    let s = s.start..s.end.min(s.start + (1 << 14));
                    let mut out = vec![0.0f32; ids.len() * (s.end - s.start) as usize];
                    let _ = rec.read_physical(g, &ids, s.clone(), &mut out);
                    rec.prefetch(g, s);
                }
            }
        }
    }
}

/// Checks the grids of `spec`'s marker streams shown as channels: one sample per text at its
/// annotation's onset, holding its values ("nan" as NaN).
fn check_marker_grids(spec: &Spec, rec: &Recording) {
    let first = timeline(spec).1 - spec.xdf.clock;
    let markers = marker_grids(spec);
    let mut ids: Vec<u32> = streams(spec).iter().map(|s| s.id).collect();
    ids.extend(markers.iter().map(|m| m.id));
    ids.sort_unstable();
    for m in markers {
        let g = ids.iter().position(|&id| id == m.id).unwrap();
        let grid = &rec.grids()[g];
        assert!(grid.holds_values());
        let samples: Vec<(f64, &str)> = spec.annotations[m.annotations.clone()]
            .iter()
            .flat_map(|a| a.texts.iter().map(move |t| (a.onset - first, t.as_str())))
            .collect();
        assert_eq!(grid.segments(), [0..samples.len() as u64]);
        let ids = grid.channels().to_vec();
        let got = physical(rec, GridId(g as u32), &ids, 0..samples.len() as u64);
        for (i, (onset, text)) in samples.iter().enumerate() {
            let t = grid.time_of(i as u64);
            assert!(
                (t - onset).abs() < 1e-6,
                "{} sample {i}: {t} vs {onset}",
                m.name
            );
            for (c, value) in text.split(',').enumerate() {
                let want: f32 = if value == "nan" {
                    f32::NAN
                } else {
                    value.parse().unwrap()
                };
                let v = got[c * samples.len() + i];
                assert!(
                    v == want || (v.is_nan() && want.is_nan()),
                    "{} {i} {c}: {v}",
                    m.name
                );
            }
        }
    }
}

#[test]
fn marker_streams_of_the_generator() {
    // Pairs of numbers at an irregular rate are annotations in a stream of an event type.
    let mut spec = Spec::preset("xdf-events").unwrap();
    let pairs = spec.xdf.markers[2].annotations.clone();
    spec.xdf.markers.push(XdfMarkers {
        id: 9,
        name: "More".into(),
        kind: "Trigger".into(),
        values: MarkerValues::Numeric(XdfFormat::F64, 2),
        rate: 0.0,
        annotations: pairs,
    });
    let (rec, annotations) = open_with(TempFile::written(&spec).path());
    assert!(same_annotations(&spec, &rec, &annotations));
    // The EEG, the codes, and the irregular "Ints" and "Mouse" pairs on their own time
    // stamps.
    assert_eq!(rec.grids().len(), 4);
    check_grids(&spec, &rec, None);
    check_marker_grids(&spec, &rec);
}

/// A spec of one 100 Hz stream (time stamps on every sample) with `breaks`.
fn broken(breaks: Vec<XdfBreak>) -> Spec {
    let channels = vec![ChannelSpec::xdf(
        "A",
        "EEG",
        "microvolts",
        100,
        XdfFormat::F32,
    )];
    let mut spec = Spec::new(Format::Xdf, channels, 1.0, 30);
    spec.xdf.streams = vec![XdfStream {
        stamps: XdfStamps::All,
        breaks,
        ..XdfStream::new(1, "Amp", "EEG", vec![0])
    }];
    spec.xdf.clock = 100.0;
    spec
}

#[test]
fn breaks_backwards_and_far_away() {
    // Time jumps back by 7 s (more than the 5 s threshold at 100 Hz): the segment after
    // it follows the first directly.
    let spec = broken(vec![XdfBreak {
        before: 1000,
        seconds: -17.0,
        rate_error: 0.0,
    }]);
    let rec = open(TempFile::written(&spec).path());
    assert!(
        has_warning(&rec, "1 of its segments start before the previous one ends"),
        "{:?}",
        rec.warnings()
    );
    let grid = &rec.grids()[0];
    // Touching segments merge, but each keeps its own piece of the time base.
    assert_eq!(grid.segments(), [0..3000]);
    assert!((grid.time_of(999) - 9.99).abs() < 1e-9);
    assert!((grid.time_of(1000) - 10.0).abs() < 1e-9);
    assert_eq!(
        grid.regular_run(1000).expect("segmented").samples.start,
        1000
    );
    assert_eq!(values(&rec, 0).len(), 3000);

    // Time jumps 40 days ahead: taken for damage, the segment follows directly too.
    let spec = broken(vec![XdfBreak {
        before: 1000,
        seconds: 40.0 * 86_400.0,
        rate_error: 0.0,
    }]);
    let rec = open(TempFile::written(&spec).path());
    assert!(has_warning(
        &rec,
        "more than 30 days after the previous one"
    ));
    assert_eq!(rec.grids()[0].len(), 3000);
    assert!((rec.duration() - 30.0).abs() < 1e-6);

    // 3 s ahead: a gap of 2.99 s after the next sample, 299 whole slots.
    let spec = broken(vec![XdfBreak {
        before: 1000,
        seconds: 7.0,
        rate_error: 0.0,
    }]);
    let rec = open(TempFile::written(&spec).path());
    assert!(rec.warnings().is_empty(), "{:?}", rec.warnings());
    check_grids(&spec, &rec, None);
    assert_eq!(rec.grids()[0].segments(), [0..1000, 1700..3700]);
}

#[test]
fn garbage_is_skipped_to_the_next_boundary_chunk() {
    let mut spec = Spec::preset("xdf-float32").unwrap();
    spec.xdf.garbage = true;
    let (rec, annotations) = open_with(TempFile::written(&spec).path());
    assert!(
        has_warning(
            &rec,
            "has an invalid length; the 57 bytes up to and including the next boundary chunk"
        ),
        "{:?}",
        rec.warnings()
    );
    assert_eq!(rec.warnings().len(), 1);
    check_grids(&spec, &rec, None);
    assert!(same_annotations(&spec, &rec, &annotations));
}

#[test]
fn irregular_streams_on_their_own_time_stamps() {
    // 20 samples 10 ms apart (the eighth stamped before the seventh), a pause of 10 s,
    // then 10 more.
    let mut c = Chunks::new(None);
    c.stream_header(1, &header_xml("Tracker", "Gaze", 2, 0.0, "float32", None));
    let mut samples: Vec<(Option<f64>, Vec<f64>)> = (0..20)
        .map(|i| {
            (
                Some(5.0 + 0.01 * f64::from(i)),
                vec![f64::from(i), -f64::from(i)],
            )
        })
        .collect();
    samples[7].0 = Some(5.05);
    samples.extend((0..10).map(|k| (Some(15.2 + 0.01 * f64::from(k)), vec![100.0, 0.0])));
    c.numeric(1, XdfFormat::F32, &samples);
    c.footer(1, "<info/>");
    let rec = Built::new(&c.bytes).open();
    assert!(
        has_warning(&rec, "1 of its time stamps come before the previous one's"),
        "{:?}",
        rec.warnings()
    );
    let grid = &rec.grids()[0];
    assert!(grid.holds_values());
    // A slot of its own for the pause, a usual step (the median step) after the sample
    // before it; the last sample ends a usual step after its time.
    assert_eq!(grid.segments(), [0..20, 21..31]);
    assert_eq!(grid.time_of(7), grid.time_of(6));
    assert!((grid.time_of(6) - 0.06).abs() < 1e-9);
    let step = |a: u64, b: u64| grid.time_of(b) - grid.time_of(a);
    assert!((step(19, 20) / 0.01 - 1.0).abs() < 0.01, "{}", step(19, 20));
    assert!((grid.time_of(21) - 10.2).abs() < 1e-9);
    assert!((step(30, 31) / 0.01 - 1.0).abs() < 0.01, "{}", step(30, 31));
    // The average rate within the segments: 28 steps of (mostly) 10 ms.
    let seconds = 0.19 + 0.09;
    assert!(
        (grid.rate() - 28.0 / seconds).abs() < 1e-6,
        "{}",
        grid.rate()
    );
    let mut out = vec![0.0f32; 2];
    rec.read_physical(GridId(0), grid.channels(), 21..22, &mut out)
        .unwrap();
    assert_eq!(out, [100.0, 0.0]);
}

#[test]
fn streams_that_can_drop_samples_and_stamps_that_are_not_numbers() {
    let mut c = Chunks::new(None);
    let xml = header_xml("Glasses", "Gaze", 1, 10.0, "float32", None).replace(
        "<desc>",
        "<desc><synchronization><can_drop_samples>true</can_drop_samples>\
         </synchronization>",
    );
    c.stream_header(1, &xml);
    // The third time stamp isn't a number: deduced from the second.
    let samples: Vec<(Option<f64>, Vec<f64>)> = (0..6)
        .map(|i| {
            let t = if i == 2 {
                f64::NAN
            } else {
                3.0 + i as f64 / 10.0
            };
            (Some(t), vec![i as f64])
        })
        .collect();
    c.numeric(1, XdfFormat::F32, &samples);
    c.clock_offset(1, 3.0, f64::NAN);
    c.footer(1, "<info/>");
    let rec = Built::new(&c.bytes).open();
    assert!(has_warning(
        &rec,
        "1 clock offsets that aren't numbers are ignored"
    ));
    assert_eq!(rec.warnings().len(), 1, "{:?}", rec.warnings());
    // On its own time stamps, as pyxdf leaves such a stream, joined by lines.
    let grid = &rec.grids()[0];
    assert!(grid.times().is_some());
    assert!(!grid.holds_values());
    assert_eq!(grid.segments(), [0..6]);
    assert!((grid.rate() - 10.0).abs() < 1e-9);
    for i in 0..6 {
        assert!((grid.time_of(i) - i as f64 / 10.0).abs() < 1e-9);
    }
}
