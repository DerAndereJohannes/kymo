use std::collections::BTreeMap;
use std::ops::Range;

use kymo_core::{
    Annotation, AnnotationData, AnnotationError, AnnotationId, AnnotationOp, AnnotationStore,
    ChannelId, Origin,
};
use proptest::prelude::*;

fn data(onset: f64, duration: f64, description: &str) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: description.into(),
        channels: vec![],
        origin: Origin::File,
    }
}

fn snapshot(store: &AnnotationStore) -> Vec<Annotation> {
    store.iter().cloned().collect()
}

fn descriptions<'a>(it: impl Iterator<Item = &'a Annotation>) -> Vec<&'a str> {
    it.map(|a| a.data.description.as_str()).collect()
}

#[test]
fn from_data_numbers_in_input_order_and_sorts_by_onset() {
    let store = AnnotationStore::from_data(vec![
        data(5.0, 0.0, "c"),
        data(1.0, 2.0, "a"),
        data(5.0, 1.0, "d"),
        data(3.0, 0.0, "b"),
    ]);
    assert_eq!(store.len(), 4);
    assert_eq!(descriptions(store.iter()), ["a", "b", "c", "d"]);
    let ids: Vec<u64> = store.iter().map(|a| a.id.0).collect();
    assert_eq!(ids, [1, 3, 0, 2]);
    assert_eq!(store.get(AnnotationId(2)).unwrap().data.description, "d");
    assert!(store.get(AnnotationId(4)).is_none());
}

#[test]
fn query_includes_touching_edges_as_documented() {
    let store = AnnotationStore::from_data(vec![
        data(1.0, 0.0, "event at start"),
        data(0.0, 1.0, "ends at start"),
        data(0.0, 0.5, "ends before"),
        data(2.0, 0.0, "event at end"),
        data(1.5, 10.0, "inside, long"),
        data(-5.0, 20.0, "spans all"),
    ]);
    assert_eq!(
        descriptions(store.query(1.0..2.0)),
        [
            "spans all",
            "ends at start",
            "event at start",
            "inside, long"
        ]
    );
    assert!(store.query(f64::NAN..2.0).next().is_none());
    assert!(store.query(1.0..f64::NAN).next().is_none());
    assert!(AnnotationStore::default().query(0.0..10.0).next().is_none());
}

#[test]
fn ids_stay_stable_and_are_never_reused() {
    let mut store = AnnotationStore::from_data(vec![
        data(0.0, 0.0, "a"),
        data(1.0, 0.0, "b"),
        data(2.0, 0.0, "c"),
    ]);
    store.apply(AnnotationOp::Remove(AnnotationId(2))).unwrap();
    store.apply(AnnotationOp::Remove(AnnotationId(0))).unwrap();
    assert_eq!(store.get(AnnotationId(1)).unwrap().data.description, "b");
    assert_eq!(store.new_id(), AnnotationId(3));
    assert_eq!(store.new_id(), AnnotationId(4));

    // An id inserted from outside (e.g. an undo) is never handed out again either.
    let outside = Annotation {
        id: AnnotationId(100),
        data: data(0.5, 0.0, "x"),
    };
    store.apply(AnnotationOp::Insert(outside)).unwrap();
    assert_eq!(store.new_id(), AnnotationId(101));
}

#[test]
fn failed_operations_leave_the_store_unchanged() {
    let mut store = AnnotationStore::from_data(vec![data(0.0, 1.0, "a"), data(2.0, 0.0, "b")]);
    let before = snapshot(&store);
    let existing = store.get(AnnotationId(0)).unwrap().clone();
    let missing = AnnotationId(9);

    let cases = [
        (
            AnnotationOp::Remove(missing),
            AnnotationError::UnknownId(missing),
        ),
        (
            AnnotationOp::Replace(Annotation {
                id: missing,
                data: data(0.0, 0.0, "z"),
            }),
            AnnotationError::UnknownId(missing),
        ),
        (
            AnnotationOp::Insert(existing.clone()),
            AnnotationError::DuplicateId(AnnotationId(0)),
        ),
    ];
    for (op, expected) in cases {
        assert_eq!(store.apply(op), Err(expected));
        assert_eq!(snapshot(&store), before);
    }

    for (onset, duration) in [
        (f64::NAN, 0.0),
        (f64::INFINITY, 0.0),
        (0.0, -1.0),
        (0.0, f64::NAN),
        (f64::MAX, f64::MAX),
    ] {
        let bad = Annotation {
            id: AnnotationId(0),
            data: data(onset, duration, "bad"),
        };
        let replace = store.apply(AnnotationOp::Replace(bad.clone()));
        assert!(matches!(replace, Err(AnnotationError::Invalid { .. })));
        let fresh = Annotation {
            id: AnnotationId(50),
            ..bad
        };
        let insert = store.apply(AnnotationOp::Insert(fresh));
        assert!(matches!(insert, Err(AnnotationError::Invalid { .. })));
        assert_eq!(snapshot(&store), before);
    }
    assert_eq!(
        store.new_id(),
        AnnotationId(2),
        "failed inserts reserve no ids"
    );
}

#[test]
fn replace_origin_swaps_one_origin_and_fails_without_a_trace() {
    let stim = |onset: f64, code: &str| AnnotationData {
        origin: Origin::Stim,
        ..data(onset, 0.0, code)
    };
    let mut store = AnnotationStore::from_data(vec![data(1.0, 2.0, "file"), stim(0.5, "old")]);
    store
        .replace_origin(Origin::Stim, vec![stim(3.0, "b"), stim(0.0, "a")])
        .unwrap();
    assert_eq!(descriptions(store.iter()), ["a", "file", "b"]);
    let ids: Vec<u64> = store.iter().map(|a| a.id.0).collect();
    assert_eq!(ids, [3, 0, 2], "fresh ids in the given order");
    assert!(store.get(AnnotationId(1)).is_none());
    assert_eq!(descriptions(store.query(2.5..4.0)), ["file", "b"]);

    let before = snapshot(&store);
    let wrong = vec![stim(1.0, "fine"), data(2.0, 0.0, "from the file")];
    assert!(matches!(
        store.replace_origin(Origin::Stim, wrong),
        Err(AnnotationError::Invalid { .. })
    ));
    let invalid = vec![stim(f64::NAN, "bad")];
    assert!(matches!(
        store.replace_origin(Origin::Stim, invalid),
        Err(AnnotationError::Invalid { .. })
    ));
    assert_eq!(snapshot(&store), before);
    assert_eq!(
        store.new_id(),
        AnnotationId(4),
        "failed calls use up no ids"
    );

    store.replace_origin(Origin::Stim, vec![]).unwrap();
    assert_eq!(descriptions(store.iter()), ["file"]);
}

// ---------- property tests against a reference model ----------

/// Onsets and durations on a coarse grid, so ties and touching edges are common.
fn time() -> impl Strategy<Value = f64> {
    prop_oneof![
        (0u32..24).prop_map(|x| f64::from(x) * 0.5 - 1.0),
        -6.0f64..16.0
    ]
}

fn annotation_data() -> impl Strategy<Value = AnnotationData> {
    let duration = prop_oneof![
        Just(0.0),
        (0u32..8).prop_map(|x| f64::from(x) * 0.5),
        0.0f64..5.0
    ];
    (
        time(),
        duration,
        "[a-z]{0,3}",
        proptest::collection::vec(0u32..4, 0..3),
        origin(),
    )
        .prop_map(
            |(onset, duration, description, channels, origin)| AnnotationData {
                onset,
                duration,
                description,
                channels: channels.into_iter().map(ChannelId).collect(),
                origin,
            },
        )
}

fn origin() -> impl Strategy<Value = Origin> {
    prop_oneof![Just(Origin::File), Just(Origin::Viewer), Just(Origin::Stim)]
}

/// An edit, resolved against the current state when applied.
#[derive(Clone, Debug)]
enum Intent {
    Insert(AnnotationData),
    /// Removes the n-th annotation (modulo the count).
    Remove(usize),
    Replace(usize, AnnotationData),
    RemoveMissing,
    InsertExisting(usize),
}

fn intent() -> impl Strategy<Value = Intent> {
    prop_oneof![
        3 => annotation_data().prop_map(Intent::Insert),
        2 => any::<usize>().prop_map(Intent::Remove),
        2 => (any::<usize>(), annotation_data()).prop_map(|(i, d)| Intent::Replace(i, d)),
        1 => Just(Intent::RemoveMissing),
        1 => any::<usize>().prop_map(Intent::InsertExisting),
    ]
}

type Model = BTreeMap<AnnotationId, AnnotationData>;

/// The model's annotations in the documented order: by onset, then id.
fn model_sorted(model: &Model) -> Vec<Annotation> {
    let mut items: Vec<Annotation> = model
        .iter()
        .map(|(&id, data)| Annotation {
            id,
            data: data.clone(),
        })
        .collect();
    items.sort_by(|a, b| a.data.onset.total_cmp(&b.data.onset).then(a.id.cmp(&b.id)));
    items
}

fn brute_query(sorted: &[Annotation], t: &Range<f64>) -> Vec<Annotation> {
    sorted
        .iter()
        .filter(|a| a.data.onset < t.end && a.data.onset + a.data.duration >= t.start)
        .cloned()
        .collect()
}

fn check_against_model(
    store: &AnnotationStore,
    model: &Model,
    queries: &[Range<f64>],
) -> Result<(), TestCaseError> {
    let sorted = model_sorted(model);
    prop_assert_eq!(store.len(), model.len());
    prop_assert_eq!(snapshot(store), sorted.clone());
    for (id, data) in model {
        prop_assert_eq!(store.get(*id).map(|a| &a.data), Some(data));
    }
    for t in queries {
        let found: Vec<Annotation> = store.query(t.clone()).cloned().collect();
        prop_assert_eq!(found, brute_query(&sorted, t), "query {:?}", t);
    }
    Ok(())
}

/// Picks the n-th annotation in iteration order.
fn pick(store: &AnnotationStore, n: usize) -> Option<Annotation> {
    let len = store.len();
    (len > 0)
        .then(|| store.iter().nth(n % len).cloned())
        .flatten()
}

proptest! {
    #[test]
    fn edits_match_model_and_undo_restores(
        initial in proptest::collection::vec(annotation_data(), 0..40),
        intents in proptest::collection::vec(intent(), 0..40),
        queries in proptest::collection::vec((time(), time()).prop_map(|(a, b)| a..b), 1..8),
    ) {
        let mut store = AnnotationStore::from_data(initial.clone());
        let mut model: Model = initial
            .into_iter()
            .zip(0..)
            .map(|(d, i)| (AnnotationId(i), d))
            .collect();
        check_against_model(&store, &model, &queries)?;
        let start = snapshot(&store);

        let mut inverses = Vec::new();
        for intent in intents {
            let before = snapshot(&store);
            let (op, expect_ok) = match intent {
                Intent::Insert(data) => {
                    let id = store.new_id();
                    prop_assert!(!model.contains_key(&id), "new_id reused {:?}", id);
                    (AnnotationOp::Insert(Annotation { id, data }), true)
                }
                Intent::Remove(n) => match pick(&store, n) {
                    Some(a) => (AnnotationOp::Remove(a.id), true),
                    None => (AnnotationOp::Remove(AnnotationId(u64::MAX)), false),
                },
                Intent::Replace(n, data) => match pick(&store, n) {
                    Some(a) => (AnnotationOp::Replace(Annotation { id: a.id, data }), true),
                    None => continue,
                },
                Intent::RemoveMissing => (AnnotationOp::Remove(AnnotationId(u64::MAX - 1)), false),
                Intent::InsertExisting(n) => match pick(&store, n) {
                    Some(a) => (AnnotationOp::Insert(a), false),
                    None => continue,
                },
            };

            // Update the model the way the operation is documented to.
            let expected_model = {
                let mut m = model.clone();
                match &op {
                    AnnotationOp::Insert(a) | AnnotationOp::Replace(a) if expect_ok => {
                        m.insert(a.id, a.data.clone());
                    }
                    AnnotationOp::Remove(id) if expect_ok => {
                        m.remove(id);
                    }
                    _ => {}
                }
                m
            };

            match store.apply(op) {
                Ok(inverse) => {
                    prop_assert!(expect_ok, "operation should have failed");
                    inverses.push(inverse);
                    model = expected_model;
                }
                Err(_) => {
                    prop_assert!(!expect_ok, "operation should have succeeded");
                    prop_assert_eq!(snapshot(&store), before);
                }
            }
            check_against_model(&store, &model, &queries)?;
        }
        let end = snapshot(&store);

        // Undo everything, then redo everything.
        let mut redos = Vec::new();
        for inverse in inverses.into_iter().rev() {
            redos.push(store.apply(inverse).unwrap());
        }
        prop_assert_eq!(snapshot(&store), start);
        for redo in redos.into_iter().rev() {
            store.apply(redo).unwrap();
        }
        prop_assert_eq!(snapshot(&store), end);
    }
}

proptest! {
    #[test]
    fn replace_origin_matches_model(
        initial in proptest::collection::vec(annotation_data(), 0..30),
        rounds in proptest::collection::vec(
            (origin(), proptest::collection::vec(annotation_data(), 0..20)),
            1..5,
        ),
        queries in proptest::collection::vec((time(), time()).prop_map(|(a, b)| a..b), 1..8),
    ) {
        let mut store = AnnotationStore::from_data(initial.clone());
        let mut model: Model = initial
            .into_iter()
            .zip(0..)
            .map(|(d, i)| (AnnotationId(i), d))
            .collect();
        let mut seen: Vec<AnnotationId> = model.keys().copied().collect();
        for (origin, items) in rounds {
            let items: Vec<AnnotationData> = items
                .into_iter()
                .map(|d| AnnotationData { origin, ..d })
                .collect();
            store.replace_origin(origin, items.clone()).unwrap();
            model.retain(|_, d| d.origin != origin);
            let mut fresh: Vec<&Annotation> =
                store.iter().filter(|a| a.data.origin == origin).collect();
            fresh.sort_by_key(|a| a.id);
            let data: Vec<AnnotationData> = fresh.iter().map(|a| a.data.clone()).collect();
            prop_assert_eq!(data, items, "the new items, ids in the given order");
            for a in fresh {
                prop_assert!(!seen.contains(&a.id), "id {:?} used before", a.id);
                seen.push(a.id);
                model.insert(a.id, a.data.clone());
            }
            check_against_model(&store, &model, &queries)?;
        }
    }
}
