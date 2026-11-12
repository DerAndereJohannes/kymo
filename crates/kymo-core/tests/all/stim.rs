//! Stim channel changes and events, checked against brute force over the samples.

use std::ops::Range;

use crate::common::{TempDir, TempFile, len_and_segments, sample_types};
use std::sync::Arc;

use kymo_core::pyramid::{MemStorage, Pyramid, PyramidBuilder, PyramidStorage, Sidecar};
use kymo_core::stim::{Change, Event, EventOptions, EventRule, StimChanges, annotations, events};
use kymo_core::testing::{MemData, opened};
use kymo_core::tile::{Level, TileData};
use kymo_core::{
    Channel, ChannelId, ChannelKind, Grid, GridId, Origin, Recording, SampleType, Timebase,
};
use kymo_testgen::Spec;
use proptest::prelude::*;

const RULES: [EventRule; 3] = [
    EventRule::EveryChange,
    EventRule::FromZero,
    EventRule::Increasing,
];

/// All six combinations of rule and initial event.
fn all_options() -> impl Iterator<Item = EventOptions> {
    RULES.into_iter().flat_map(|rule| {
        [false, true].map(|initial_event| EventOptions {
            rule,
            initial_event,
        })
    })
}

/// The masked codes of the present samples, in order, as (sample, value).
fn present_codes(segments: &[Range<u64>], codes: &[i32], mask: Option<u32>) -> Vec<(u64, i32)> {
    let mask = mask.map_or(-1, |m| m as i32);
    segments
        .iter()
        .flat_map(|s| s.clone())
        .map(|i| (i, codes[i as usize] & mask))
        .collect()
}

/// Every sample whose value differs from the present sample before it, plus the first.
fn brute_changes(present: &[(u64, i32)]) -> Vec<Change> {
    present
        .iter()
        .enumerate()
        .filter(|&(k, &(_, v))| k == 0 || present[k - 1].1 != v)
        .map(|(_, &(sample, value))| Change { sample, value })
        .collect()
}

/// MNE's `find_events` over the present samples: a step from the previous present value.
fn brute_events(present: &[(u64, i32)], options: EventOptions) -> Vec<Event> {
    let mut found = Vec::new();
    for (k, &(sample, value)) in present.iter().enumerate() {
        let event = match k.checked_sub(1).map(|j| present[j].1) {
            None => options.initial_event && value != 0,
            Some(before) => {
                value != before
                    && value != 0
                    && match options.rule {
                        EventRule::EveryChange => true,
                        EventRule::FromZero => before == 0,
                        EventRule::Increasing => value > before,
                        _ => unreachable!(),
                    }
            }
        };
        if event {
            found.push(Event { sample, value });
        }
    }
    found
}

/// Codes in runs: mostly 0 and small codes, some arbitrary values and some with bits above
/// the 16 of a BDF status channel.
fn rows(n: usize, len: u64) -> impl Strategy<Value = Vec<Vec<i32>>> {
    let code = prop_oneof![
        3 => Just(0),
        3 => 1..4i32,
        1 => any::<i32>(),
        1 => Just(0x10003),
        1 => Just(0x10000),
    ];
    let runs = proptest::collection::vec((code, 1..60usize), 1..40);
    proptest::collection::vec(runs, n).prop_map(move |rows| {
        rows.into_iter()
            .map(|runs| {
                let mut row = Vec::new();
                for &(code, run) in runs.iter().cycle() {
                    if row.len() >= len as usize {
                        break;
                    }
                    row.extend(std::iter::repeat_n(code, run));
                }
                row.truncate(len as usize);
                row
            })
            .collect()
    })
}

/// The rows as stored in `sample_type`, and the codes a reader gives back for them.
fn stored(sample_type: SampleType, rows: &[Vec<i32>]) -> (MemData, Vec<Vec<i32>>) {
    match sample_type {
        SampleType::I16 => {
            let typed: Vec<Vec<i16>> = rows
                .iter()
                .map(|r| r.iter().map(|&v| v.clamp(-32768, 32767) as i16).collect())
                .collect();
            let codes = typed
                .iter()
                .map(|r| r.iter().map(|&v| i32::from(v)).collect());
            let codes = codes.collect();
            (MemData::I16(typed), codes)
        }
        SampleType::I32 => (MemData::I32(rows.to_vec()), rows.to_vec()),
        _ => {
            let typed: Vec<Vec<f32>> = rows
                .iter()
                .map(|r| r.iter().map(|&v| v as f32).collect())
                .collect();
            let codes = typed.iter().map(|r| r.iter().map(|&v| v as i32).collect());
            let codes = codes.collect();
            (MemData::F32(typed), codes)
        }
    }
}

fn recording(
    sample_type: SampleType,
    len: u64,
    segments: Vec<Range<u64>>,
    data: MemData,
    kinds: &[(ChannelKind, Option<u32>)],
) -> Recording {
    let ids: Vec<ChannelId> = (0..kinds.len() as u32).map(ChannelId).collect();
    let channels = kinds
        .iter()
        .enumerate()
        .map(|(i, &(kind, trigger_mask))| Channel {
            kind,
            trigger_mask,
            ..Channel::new(format!("ch{i}"), GridId(0))
        })
        .collect();
    let timebase = Timebase::Regular {
        rate: 250.0,
        start: 0.5,
    };
    let grid = Grid::new(timebase, len, segments, sample_type, ids).unwrap();
    Recording::new(opened(channels, vec![grid], vec![data]))
        .unwrap()
        .0
}

fn kinds() -> impl Strategy<Value = Vec<(ChannelKind, Option<u32>)>> {
    let mask = prop_oneof![Just(None), Just(Some(0xffff)), any::<u32>().prop_map(Some)];
    let kind = prop_oneof![Just(ChannelKind::Stim), Just(ChannelKind::Eeg)];
    proptest::collection::vec((kind, mask), 1..=3)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// The build records exactly the brute-force changes of every stim channel, whatever
    /// the chunk size and thread count; while it runs it serves only changes below the
    /// watermark. Events and annotations under every rule match MNE's algorithm.
    #[test]
    fn changes_and_events_match_brute_force(
        sample_type in sample_types(),
        kinds in kinds(),
        ((len, segments), rows) in len_and_segments(20_000).prop_flat_map(|(len, s)| {
            (Just((len, s)), rows(3, len))
        }),
        chunk in 1..=5_000u64,
        threads in 1..=3usize,
    ) {
        let rows = &rows[..kinds.len()];
        let (data, codes) = stored(sample_type, rows);
        let rec = recording(sample_type, len, segments.clone(), data, &kinds);
        let stim: Vec<usize> = (0..kinds.len()).filter(|&c| kinds[c].0 == ChannelKind::Stim).collect();
        let want: Vec<Vec<(u64, i32)>> = stim
            .iter()
            .map(|&c| present_codes(&segments, &codes[c], kinds[c].1))
            .collect();

        let mut builder = PyramidBuilder::new(&rec, GridId(0))
            .unwrap()
            .with_chunk_len(chunk)
            .with_threads(threads);
        let pyramid = builder.pyramid();
        loop {
            let complete = builder.step().unwrap();
            let done = builder.done();
            let got = pyramid.stim_changes(&rec);
            prop_assert_eq!(got.len(), stim.len());
            for (s, present) in got.iter().zip(&want) {
                let upto: Vec<(u64, i32)> =
                    present.iter().copied().filter(|&(i, _)| i < done).collect();
                prop_assert_eq!(&s.changes, &brute_changes(&upto), "done {}", done);
            }
            if complete {
                break;
            }
        }

        let got = pyramid.stim_changes(&rec);
        for ((s, &c), present) in got.iter().zip(&stim).zip(&want) {
            prop_assert_eq!(s.channel, ChannelId(c as u32));
            for options in all_options() {
                prop_assert_eq!(
                    events(&s.changes, options),
                    brute_events(present, options),
                    "{:?}",
                    options
                );
            }
        }

        let options = EventOptions::default();
        let grid = rec.grid(GridId(0));
        let mut want: Vec<(f64, String, ChannelId)> = got
            .iter()
            .zip(&want)
            .flat_map(|(s, present)| {
                brute_events(present, options)
                    .into_iter()
                    .map(move |e| (grid.time_of(e.sample), e.value.to_string(), s.channel))
            })
            .collect();
        want.sort_by(|a, b| a.0.total_cmp(&b.0));
        let annotations = annotations(&rec, &got, options);
        for a in &annotations {
            prop_assert_eq!(a.duration, 0.0);
            prop_assert_eq!(a.origin, Origin::Stim);
            prop_assert_eq!(a.channels.len(), 1);
        }
        let mut got: Vec<(f64, String, ChannelId)> = annotations
            .into_iter()
            .map(|a| (a.onset, a.description, a.channels[0]))
            .collect();
        prop_assert!(got.windows(2).all(|w| w[0].0 <= w[1].0));
        // Equal onsets of different channels may come in either order.
        got.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        want.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        prop_assert_eq!(got, want);
    }
}

/// Publishes only half of each watermark, as if readers looked while a step was running:
/// changes are stored ahead of what is published.
struct HalfPublished(MemStorage);

impl PyramidStorage for HalfPublished {
    fn put_tile(&self, level: Level, time: u64, data: TileData) {
        self.0.put_tile(level, time, data);
    }
    fn put_sums(&self, block: u64, sums: &[f64]) {
        self.0.put_sums(block, sums);
    }
    fn publish(&self, done: u64) {
        self.0.publish(done / 2);
    }
    fn done(&self) -> u64 {
        self.0.done()
    }
    fn tile_rows(
        &self,
        level: Level,
        time: u64,
        rows: Range<usize>,
        len: usize,
    ) -> Option<TileData> {
        self.0.tile_rows(level, time, rows, len)
    }
    fn sum(&self, block: u64, row: usize) -> Option<f64> {
        self.0.sum(block, row)
    }
    fn put_changes(&self, row: usize, changes: &[Change]) {
        self.0.put_changes(row, changes);
    }
    fn changes(&self, row: usize) -> Vec<Change> {
        self.0.changes(row)
    }
}

#[test]
fn changes_past_the_watermark_are_not_served() {
    let len = 10_000u64;
    let row: Vec<i32> = (0..len as i32).map(|i| (i / 7) % 3).collect();
    let rec = recording(
        SampleType::I32,
        len,
        vec![0..len],
        MemData::I32(vec![row.clone()]),
        &[(ChannelKind::Stim, None)],
    );
    let pyramid = PyramidBuilder::new(&rec, GridId(0))
        .unwrap()
        .with_storage(Arc::new(HalfPublished(MemStorage::new())))
        .finish()
        .unwrap();
    assert_eq!(pyramid.done(), len / 2);
    let present: Vec<(u64, i32)> = (0..len / 2).map(|i| (i, row[i as usize])).collect();
    assert_eq!(
        pyramid.stim_changes(&rec)[0].changes,
        brute_changes(&present)
    );
}

#[test]
fn events_follow_each_rule() {
    let changes: Vec<Change> = [
        (0, 5),
        (10, 0),
        (20, 3),
        (25, 7),
        (30, 2),
        (35, 0),
        (40, -1),
    ]
    .into_iter()
    .map(|(sample, value)| Change { sample, value })
    .collect();
    let samples = |rule, initial_event| -> Vec<u64> {
        let options = EventOptions {
            rule,
            initial_event,
        };
        events(&changes, options).iter().map(|e| e.sample).collect()
    };
    assert_eq!(samples(EventRule::EveryChange, false), [20, 25, 30, 40]);
    assert_eq!(samples(EventRule::EveryChange, true), [0, 20, 25, 30, 40]);
    assert_eq!(samples(EventRule::FromZero, false), [20, 40]);
    assert_eq!(samples(EventRule::Increasing, false), [20, 25]);
    assert!(events(&[], EventOptions::default()).is_empty());
}

/// The trigger codes of a generated file's channel `c` over its present samples.
fn spec_codes(spec: &Spec, c: usize) -> Vec<(u64, i32)> {
    spec.segments(c)
        .into_iter()
        .flatten()
        .map(|i| (i, spec.trigger_code(c, i).unwrap()))
        .collect()
}

/// Generated BDF+C (masked Status with toggling high bits), EDF+D (gaps) and XDF (a float64
/// stream's Trigger channel) files: the changes match the codes the generator wrote, and a
/// reopened sidecar has the same ones.
#[test]
fn generated_files_and_sidecars_have_the_written_codes() {
    for preset in ["bdf+c-small", "edf+d-mixed", "xdf-events"] {
        let spec = Spec::preset(preset).unwrap();
        let file = TempFile::written(&spec);
        let rec = Recording::new(kymo_core::open(file.path()).unwrap())
            .unwrap()
            .0;
        let c = spec.channels.iter().position(|ch| ch.trigger).unwrap();
        let channel = rec.channel(ChannelId(c as u32));
        assert_eq!(channel.kind, ChannelKind::Stim);
        assert_eq!(channel.trigger_mask, spec.format.is_bdf().then_some(0xffff));

        let present = spec_codes(&spec, c);
        let want = vec![StimChanges {
            channel: ChannelId(c as u32),
            changes: brute_changes(&present),
        }];
        let in_memory = Pyramid::build(&rec, channel.grid).unwrap();
        assert_eq!(in_memory.stim_changes(&rec), want, "{preset}");
        // The file holds events of every kind the rules tell apart.
        let every = events(&want[0].changes, EventOptions::default()).len();
        let from_zero = EventOptions {
            rule: EventRule::FromZero,
            ..Default::default()
        };
        assert!(
            every > events(&want[0].changes, from_zero).len(),
            "{preset}"
        );
        assert!(every > 50, "{preset}: {every} events");

        let dir = TempDir::new();
        let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
        assert!(sidecar.build(&rec).unwrap());
        assert_eq!(
            sidecar.pyramid(channel.grid).unwrap().stim_changes(&rec),
            want
        );
        drop(sidecar);
        let reopened = Sidecar::open_or_create(dir.path(), &rec).unwrap();
        assert!(reopened.is_complete(), "{preset}: not reused");
        assert_eq!(
            reopened.pyramid(channel.grid).unwrap().stim_changes(&rec),
            want
        );
        for g in 0..rec.grids().len() as u32 {
            // Other grids without stim channels have no changes (xdf-events has a stim
            // stream of its own).
            let stim = rec
                .grid(GridId(g))
                .channels()
                .iter()
                .any(|&c| rec.channel(c).kind == ChannelKind::Stim);
            if GridId(g) != channel.grid && !stim {
                assert!(
                    reopened
                        .pyramid(GridId(g))
                        .unwrap()
                        .stim_changes(&rec)
                        .is_empty()
                );
            }
        }
    }
}

/// A broken trailer (the stim changes) makes the sidecar rebuild, not misreport events.
#[test]
fn broken_stim_trailer_is_rebuilt() {
    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
    sidecar.build(&rec).unwrap();
    let path = sidecar.path();
    let want = sidecar.pyramid(GridId(0)).unwrap().stim_changes(&rec);
    drop(sidecar);
    let good = std::fs::read(&path).unwrap();
    let trailer = good.windows(8).rposition(|w| w == b"KYMOSTIM").unwrap();
    let mut swapped = good.clone();
    // The first two changes' samples, swapped: out of order.
    let first = trailer + 32;
    let (a, b) = (
        good[first..first + 8].to_vec(),
        good[first + 16..first + 24].to_vec(),
    );
    swapped[first..first + 8].copy_from_slice(&b);
    swapped[first + 16..first + 24].copy_from_slice(&a);
    let cases = [
        ("no trailer", good[..trailer].to_vec()),
        ("cut trailer", good[..good.len() - 3].to_vec()),
        ("extra byte", [good.as_slice(), &[0]].concat()),
        ("out of order", swapped),
    ];
    for (name, bytes) in cases {
        std::fs::write(&path, &bytes).unwrap();
        let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
        assert!(!sidecar.is_complete(), "{name}: accepted");
        sidecar.build(&rec).unwrap();
        assert_eq!(
            sidecar.pyramid(GridId(0)).unwrap().stim_changes(&rec),
            want,
            "{name}"
        );
    }
}

/// A BDF Status channel's tiles hold its trigger codes, not its raw values with the
/// amplifier's status bits: raw tiles, every level of the pyramid, and a reopened sidecar.
/// Its block sums, and so its window means, stay those of the raw values.
#[test]
fn stim_tiles_hold_the_codes() {
    use kymo_core::tile::{CHANNEL_BLOCK, ChannelBlock, SignalVersion, TILE_LEN, TileKey};
    use kymo_core::tile::{Tile, compute_raw};

    let spec = Spec::preset("bdf+c-small").unwrap();
    let file = TempFile::written(&spec);
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    let c = spec.channels.iter().position(|ch| ch.trigger).unwrap();
    let id = ChannelId(c as u32);
    let channel = rec.channel(id);
    assert_eq!(channel.tile_scaling(), (1.0, 0.0));
    let eeg = rec.channel(ChannelId(0));
    assert_eq!(eeg.kind, ChannelKind::Eeg);
    assert_eq!(eeg.tile_scaling(), (eeg.gain, eeg.offset));

    let grid = rec.grid(channel.grid);
    let row = grid.channels().iter().position(|&x| x == id).unwrap();
    let block = ChannelBlock {
        grid: channel.grid,
        index: (row / CHANNEL_BLOCK) as u32,
    };
    let r = row % CHANNEL_BLOCK;
    let codes: Vec<i32> = (0..grid.len())
        .map(|i| spec.trigger_code(c, i).unwrap())
        .collect();
    assert!(codes.iter().any(|&x| x > 0) && codes.iter().all(|&x| (0..=255).contains(&x)));

    let values = |tile: &Tile| -> Vec<i32> {
        let TileData::I32(d) = tile.data() else {
            panic!("BDF tiles are int32");
        };
        let per = d.len() / tile.channels();
        d[r * per..(r + 1) * per].to_vec()
    };
    let key = |level: Level, time: u64| TileKey {
        version: SignalVersion::RAW,
        level,
        block,
        time,
    };
    for time in 0..grid.len().div_ceil(TILE_LEN) {
        let tile = compute_raw(&rec, key(Level::RAW, time)).unwrap();
        let start = (time * TILE_LEN) as usize;
        assert_eq!(
            values(&tile),
            codes[start..start + tile.len()],
            "raw tile {time}"
        );
    }

    let check = |pyramid: &Pyramid| {
        let mut level = Level::RAW;
        while let Some(up) = level.up().filter(|_| level < pyramid.top()) {
            level = up;
            let bucket = level.bucket_len();
            let buckets = grid.len().div_ceil(bucket);
            for time in 0..buckets.div_ceil(TILE_LEN) {
                let tile = pyramid.tile(key(level, time)).unwrap();
                let pairs = values(&tile);
                for (j, pair) in pairs.as_chunks::<2>().0.iter().enumerate() {
                    let k = time * TILE_LEN + j as u64;
                    let s =
                        &codes[(k * bucket) as usize..((k + 1) * bucket).min(grid.len()) as usize];
                    let want = [*s.iter().min().unwrap(), *s.iter().max().unwrap()];
                    assert_eq!(*pair, want, "level {level:?}, bucket {k}");
                }
            }
        }
    };
    let pyramid = Pyramid::build(&rec, channel.grid).unwrap();
    assert!(pyramid.top() >= Level::new(1).unwrap());
    check(&pyramid);
    let dir = TempDir::new();
    Sidecar::open_or_create(dir.path(), &rec)
        .unwrap()
        .build(&rec)
        .unwrap();
    let reopened = Sidecar::open_or_create(dir.path(), &rec).unwrap();
    assert!(reopened.is_complete());
    check(&reopened.pyramid(channel.grid).unwrap());

    let len = grid.len();
    let mut physical = vec![0f32; len as usize];
    rec.read_physical(channel.grid, &[id], 0..len, &mut physical)
        .unwrap();
    let want = physical.iter().map(|&x| f64::from(x)).sum::<f64>() / len as f64;
    let mean = pyramid.window_mean(&rec, id, 0..len).unwrap().unwrap();
    assert!(want > 1e6, "raw values with status bits: {want}");
    assert!(
        (mean - want).abs() <= want.abs() * 1e-6,
        "mean {mean}, want {want}"
    );
}

/// An EDF stim channel shows its digital values as codes, with no unit, as MNE does,
/// whatever its physical range: its tiles hold the codes and draw at gain 1 and offset 0.
#[test]
fn edf_stim_channels_show_their_digital_codes() {
    use kymo_core::tile::compute_raw;
    use kymo_core::tile::{CHANNEL_BLOCK, ChannelBlock, SignalVersion, TILE_LEN, TileKey};

    let mut spec = Spec::preset("edf+d-mixed").unwrap();
    // Without gaps, sample indices in the grid are the spec's.
    spec.gaps.clear();
    let c = spec.channels.iter().position(|ch| ch.trigger).unwrap();
    let trigger = &mut spec.channels[c];
    trigger.physical_min = 2.0 * f64::from(trigger.digital_min) + 100.0;
    trigger.physical_max = 2.0 * f64::from(trigger.digital_max) + 100.0;
    let file = TempFile::written(&spec);
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    let id = ChannelId(c as u32);
    let channel = rec.channel(id);
    assert_eq!(channel.kind, ChannelKind::Stim);
    assert_eq!(channel.trigger_mask, None);
    assert!((channel.gain - 2.0).abs() < 1e-9 && (channel.offset - 100.0).abs() < 1e-6);
    assert_eq!(channel.tile_scaling(), (1.0, 0.0));
    assert_eq!(channel.tile_unit(), "");
    assert_eq!(channel.unit.label, "Boolean");
    let eeg = rec.channel(ChannelId(0));
    assert_eq!(eeg.kind, ChannelKind::Eeg);
    assert_eq!(eeg.tile_scaling(), (eeg.gain, eeg.offset));
    assert_eq!(eeg.tile_unit(), eeg.unit.label);
    assert!(!eeg.tile_unit().is_empty());

    let grid = rec.grid(channel.grid);
    let row = grid.channels().iter().position(|&x| x == id).unwrap();
    let block = ChannelBlock {
        grid: channel.grid,
        index: (row / CHANNEL_BLOCK) as u32,
    };
    let r = row % CHANNEL_BLOCK;
    let (gain, offset) = channel.tile_scaling();
    let mut checked = 0;
    for time in 0..grid.len().div_ceil(TILE_LEN) {
        let key = TileKey {
            version: SignalVersion::RAW,
            level: Level::RAW,
            block,
            time,
        };
        let tile = compute_raw(&rec, key).unwrap();
        let TileData::I16(d) = tile.data() else {
            panic!("EDF tiles are int16");
        };
        let per = d.len() / tile.channels();
        for (j, &x) in d[r * per..r * per + tile.len()].iter().enumerate() {
            let i = time * TILE_LEN + j as u64;
            if let Some(code) = spec.trigger_code(c, i) {
                assert_eq!(f64::from(x) * gain + offset, f64::from(code), "sample {i}");
                checked += usize::from(code != 0);
            }
        }
    }
    assert!(checked > 0);
}
