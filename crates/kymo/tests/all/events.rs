//! Stim events kept up with the build and the rule: when the store is updated, what it
//! holds, and that the file's annotations stay as they are.

use kymo::events::{INTERVAL, StimEvents, Update};
use kymo_core::stim::{self, Change, EventOptions, EventRule, StimChanges};
use kymo_core::testing::{MemData, single_grid};
use kymo_core::{Annotation, AnnotationData, AnnotationStore, ChannelId, Origin, Recording};
use proptest::prelude::*;

/// 4 channels at 100 Hz over 100 s.
fn recording() -> Recording {
    let data = MemData::I16(vec![vec![0; 10_000]; 4]);
    Recording::new(single_grid(100.0, data)).unwrap().0
}

fn file_annotations() -> AnnotationStore {
    let data = |onset: f64, description: &str| AnnotationData {
        onset,
        duration: 1.0,
        description: description.into(),
        channels: vec![],
        origin: Origin::File,
    };
    AnnotationStore::from_data(vec![data(2.0, "a"), data(50.0, "b")])
}

fn of(store: &AnnotationStore, origin: Origin) -> Vec<Annotation> {
    store
        .iter()
        .filter(|a| a.data.origin == origin)
        .cloned()
        .collect()
}

fn stim_data(store: &AnnotationStore) -> Vec<AnnotationData> {
    of(store, Origin::Stim)
        .into_iter()
        .map(|a| a.data)
        .collect()
}

/// The changes of channels 2 and 3 in the first `done` samples, from fixed codes: 0, 5, 7,
/// 0, 3 on channel 2 and 9, 0, 9 on channel 3, every 10 samples.
fn changes(done: u64) -> Vec<StimChanges> {
    let found = |codes: &[i32]| -> Vec<Change> {
        codes
            .iter()
            .zip(0..)
            .map(|(&value, i)| Change {
                sample: i * 10,
                value,
            })
            .filter(|c| c.sample < done)
            .collect()
    };
    vec![
        StimChanges {
            channel: ChannelId(2),
            changes: found(&[0, 5, 7, 0, 3]),
        },
        StimChanges {
            channel: ChannelId(3),
            changes: found(&[9, 0, 9]),
        },
    ]
}

fn options(rule: EventRule, initial_event: bool) -> EventOptions {
    EventOptions {
        rule,
        initial_event,
    }
}

#[test]
fn updates_follow_the_build_at_most_every_interval() {
    let rec = recording();
    let mut store = file_annotations();
    let files = of(&store, Origin::File);
    let mut events = StimEvents::new(EventOptions::default());
    let never = || -> Vec<StimChanges> { panic!("changes read without an update") };

    // The first update is at once, whatever the time.
    let update = events.update(&mut store, &rec, 25, 100.0, || changes(25));
    assert_eq!(update, Update::Changed);
    let descriptions = |store: &AnnotationStore| -> Vec<(f64, String, u32)> {
        stim_data(store)
            .into_iter()
            .map(|a| (a.onset, a.description, a.channels[0].0))
            .collect()
    };
    // Channel 2: 5 at 0.1 s, 7 at 0.2 s; channel 3 starts at 9, not an event, and is 9
    // again at 0.2 s.
    assert_eq!(
        descriptions(&store),
        [
            (0.1, "5".into(), 2),
            (0.2, "7".into(), 2),
            (0.2, "9".into(), 3)
        ]
    );
    assert!(stim_data(&store).iter().all(|a| a.duration == 0.0));

    // No progress: nothing to do, however long it has been.
    assert_eq!(
        events.update(&mut store, &rec, 25, 200.0, never),
        Update::Unchanged
    );
    // Progress, but too soon: wait for the rest of the interval.
    let Update::Wait(wait) = events.update(&mut store, &rec, 45, 100.1, never) else {
        panic!("no wait");
    };
    assert!((wait - (INTERVAL - 0.1)).abs() < 1e-9, "{wait}");
    let update = events.update(&mut store, &rec, 45, 100.0 + INTERVAL, || changes(45));
    assert_eq!(update, Update::Changed);
    // And 3 at 0.4 s on channel 2.
    assert_eq!(
        descriptions(&store),
        [
            (0.1, "5".into(), 2),
            (0.2, "7".into(), 2),
            (0.2, "9".into(), 3),
            (0.4, "3".into(), 2)
        ]
    );
    assert_eq!(of(&store, Origin::File), files);
}

#[test]
fn a_rule_change_rebuilds_at_once_and_keeps_the_file_annotations() {
    let rec = recording();
    let mut store = file_annotations();
    let files = of(&store, Origin::File);
    let mut events = StimEvents::new(EventOptions::default());
    events.update(&mut store, &rec, 100, 0.0, || changes(100));
    assert_eq!(stim_data(&store).len(), 4);

    let from_zero = options(EventRule::FromZero, false);
    events.set_options(from_zero);
    assert_eq!(events.options(), from_zero);
    // Same progress, same instant: still rebuilt.
    assert_eq!(
        events.update(&mut store, &rec, 100, 0.0, || changes(100)),
        Update::Changed
    );
    let codes: Vec<String> = stim_data(&store)
        .into_iter()
        .map(|a| a.description)
        .collect();
    // 7 follows 5 without a 0 in between.
    assert_eq!(codes, ["5", "9", "3"]);
    assert_eq!(of(&store, Origin::File), files);

    // Setting the same options again changes nothing.
    events.set_options(from_zero);
    assert_eq!(
        events.update(&mut store, &rec, 100, 0.0, || changes(100)),
        Update::Unchanged
    );
}

#[derive(Clone, Debug)]
enum Step {
    /// The build moves on by this many samples (maybe none), this much later.
    Frame {
        samples: u64,
        later: f64,
    },
    Rule(EventOptions),
}

fn step() -> impl Strategy<Value = Step> {
    let rule = prop_oneof![
        Just(EventRule::EveryChange),
        Just(EventRule::FromZero),
        Just(EventRule::Increasing)
    ];
    prop_oneof![
        4 => (prop_oneof![Just(0u64), 1..20u64], 0.0..0.3f64)
            .prop_map(|(samples, later)| Step::Frame { samples, later }),
        1 => (rule, any::<bool>()).prop_map(|(rule, initial)| Step::Rule(options(rule, initial))),
    ]
}

proptest! {
    /// Over random frames and rule changes, an update happens exactly when the model says,
    /// and leaves the events of the changes found so far under the current rule.
    #[test]
    fn updates_match_the_model(steps in proptest::collection::vec(step(), 1..40)) {
        let rec = recording();
        let mut store = file_annotations();
        let files = of(&store, Origin::File);
        let mut events = StimEvents::new(EventOptions::default());
        let mut current = EventOptions::default();
        let (mut done, mut now) = (0u64, 0.0f64);
        // The progress and time of the last update, if it still holds for the rule.
        let mut last: Option<(u64, f64)> = None;
        for step in steps {
            match step {
                Step::Rule(options) => {
                    events.set_options(options);
                    if options != current {
                        last = None;
                    }
                    current = options;
                    continue;
                }
                Step::Frame { samples, later } => {
                    done += samples;
                    now += later;
                }
            }
            let expected = match last {
                None => Update::Changed,
                Some((d, _)) if d == done => Update::Unchanged,
                Some((_, at)) if now - at < INTERVAL => Update::Wait(at + INTERVAL - now),
                Some(_) => Update::Changed,
            };
            let before = stim_data(&store);
            let update = events.update(&mut store, &rec, done, now, || changes(done));
            match (update, expected) {
                (Update::Wait(a), Update::Wait(b)) => prop_assert!((a - b).abs() < 1e-9),
                _ => prop_assert_eq!(update, expected),
            }
            if update == Update::Changed {
                last = Some((done, now));
                prop_assert_eq!(stim_data(&store), stim::annotations(&rec, &changes(done), current));
            } else {
                prop_assert_eq!(stim_data(&store), before);
            }
            prop_assert_eq!(of(&store, Origin::File), files.clone());
        }
    }
}
