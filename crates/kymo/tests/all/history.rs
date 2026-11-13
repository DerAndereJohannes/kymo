//! Undo and redo against a model that keeps every state: after any sequence of edits,
//! undos, redos and stim updates, the store holds exactly the state the model is at.

use kymo::history::{Change, History, set};
use kymo_core::{Annotation, AnnotationData, AnnotationId, AnnotationStore, Origin};
use proptest::prelude::*;

fn data(onset: f64, duration: f64, description: &str, origin: Origin) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: description.to_owned(),
        channels: Vec::new(),
        origin,
        stream: None,
    }
}

/// Few distinct values, so edits often tie in onset or leave an annotation as it was.
fn any_data() -> impl Strategy<Value = AnnotationData> {
    (
        0u8..8,
        prop_oneof![Just(0.0), Just(1.0), Just(2.5)],
        prop_oneof![Just("BAD"), Just("spike"), Just("a,b")],
    )
        .prop_map(|(onset, duration, description)| {
            data(f64::from(onset), duration, description, Origin::Viewer)
        })
}

#[derive(Clone, Debug)]
enum Op {
    /// Adds an annotation with a new id.
    Insert(AnnotationData),
    /// Changes the `n`th edited annotation (modulo their number) to the data.
    Replace(usize, AnnotationData),
    Remove(usize),
    /// Two changes in one step: one annotation changed, another added.
    Both(usize, AnnotationData, AnnotationData),
    /// Valid changes, one annotation's twice, and an invalid one: nothing happens.
    Invalid(usize, AnnotationData),
    /// The same annotation changed twice in one step.
    Twice(usize, AnnotationData, AnnotationData),
    /// A step recorded after the fact, as a drag records it.
    Pushed(usize, AnnotationData),
    Undo,
    Redo,
    /// Marks the current state saved.
    Save,
    /// New stim annotations, which no undo touches.
    Stim(u8),
}

fn any_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => any_data().prop_map(Op::Insert),
        2 => (any::<usize>(), any_data()).prop_map(|(n, d)| Op::Replace(n, d)),
        1 => any::<usize>().prop_map(Op::Remove),
        1 => (any::<usize>(), any_data(), any_data()).prop_map(|(n, a, b)| Op::Both(n, a, b)),
        1 => (any::<usize>(), any_data()).prop_map(|(n, d)| Op::Invalid(n, d)),
        1 => (any::<usize>(), any_data()).prop_map(|(n, d)| Op::Pushed(n, d)),
        1 => (any::<usize>(), any_data(), any_data()).prop_map(|(n, a, b)| Op::Twice(n, a, b)),
        3 => Just(Op::Undo),
        2 => Just(Op::Redo),
        1 => Just(Op::Save),
        1 => (0u8..4).prop_map(Op::Stim),
    ]
}

/// The store without its stim annotations, which the history never holds.
fn edited(store: &AnnotationStore) -> Vec<Annotation> {
    store
        .iter()
        .filter(|a| a.data.origin != Origin::Stim)
        .cloned()
        .collect()
}

fn stim(store: &AnnotationStore) -> Vec<(f64, String)> {
    store
        .iter()
        .filter(|a| a.data.origin == Origin::Stim)
        .map(|a| (a.data.onset, a.data.description.clone()))
        .collect()
}

/// The ids whose annotations differ between two states.
fn differing(a: &[Annotation], b: &[Annotation]) -> Vec<AnnotationId> {
    let mut ids: Vec<AnnotationId> = a.iter().chain(b).map(|x| x.id).collect();
    ids.sort();
    ids.dedup();
    ids.retain(|&id| {
        let find = |s: &[Annotation]| s.iter().find(|x| x.id == id).cloned();
        find(a) != find(b)
    });
    ids
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn undo_and_redo_restore_every_state(
        initial in proptest::collection::vec(any_data(), 0..4),
        ops in proptest::collection::vec(any_op(), 1..40),
    ) {
        let initial: Vec<AnnotationData> = initial
            .into_iter()
            .map(|d| AnnotationData { origin: Origin::File, ..d })
            .collect();
        let mut store = AnnotationStore::from_data(initial);
        let mut history = History::default();
        // Every state the edits went through, and the one the store is at.
        let mut states = vec![edited(&store)];
        let mut at: usize = 0;
        // The state saved, the opened one at first, while it can be reached.
        let mut saved: Option<usize> = Some(0);
        for op in ops {
            let before = edited(&store);
            let ids: Vec<AnnotationId> = before.iter().map(|a| a.id).collect();
            let pick = |n: usize| (!ids.is_empty()).then(|| ids[n % ids.len()]);
            let mut changed: Option<Vec<AnnotationId>> = None;
            match op {
                Op::Insert(d) => {
                    let id = store.new_id();
                    history.edit(&mut store, vec![(id, Some(d))]).unwrap();
                }
                Op::Replace(n, d) => {
                    if let Some(id) = pick(n) {
                        history.edit(&mut store, vec![(id, Some(d))]).unwrap();
                    }
                }
                Op::Remove(n) => {
                    if let Some(id) = pick(n) {
                        history.edit(&mut store, vec![(id, None)]).unwrap();
                    }
                }
                Op::Both(n, a, b) => {
                    let new = store.new_id();
                    let mut changes = vec![(new, Some(b))];
                    if let Some(id) = pick(n) {
                        changes.insert(0, (id, Some(a)));
                    }
                    history.edit(&mut store, changes).unwrap();
                }
                Op::Invalid(n, d) => {
                    let new = store.new_id();
                    let mut changes = vec![(new, Some(d.clone()))];
                    if let Some(id) = pick(n) {
                        // Twice, so rolling back must go last first.
                        changes.push((id, Some(d.clone())));
                        changes.push((id, None));
                    }
                    changes.push((store.new_id(), Some(AnnotationData { onset: f64::NAN, ..d })));
                    prop_assert!(history.edit(&mut store, changes).is_err());
                }
                Op::Twice(n, a, b) => {
                    if let Some(id) = pick(n) {
                        history.edit(&mut store, vec![(id, Some(a)), (id, Some(b))]).unwrap();
                    }
                }
                Op::Pushed(n, d) => {
                    if let Some(id) = pick(n) {
                        let old = store.get(id).map(|a| a.data.clone());
                        set(&mut store, id, Some(d.clone())).unwrap();
                        history.push(vec![Change { id, before: old, after: Some(d) }]);
                    }
                }
                Op::Undo => {
                    prop_assert_eq!(history.can_undo(), at > 0);
                    changed = history.undo(&mut store);
                    prop_assert_eq!(changed.is_some(), at > 0);
                    at = at.saturating_sub(1);
                }
                Op::Redo => {
                    prop_assert_eq!(history.can_redo(), at + 1 < states.len());
                    changed = history.redo(&mut store);
                    prop_assert_eq!(changed.is_some(), at + 1 < states.len());
                    at = (at + 1).min(states.len() - 1);
                }
                Op::Save => {
                    history.mark_saved();
                    saved = Some(at);
                }
                Op::Stim(k) => {
                    let events = (0..k)
                        .map(|i| data(f64::from(i), 0.0, &format!("{k}"), Origin::Stim))
                        .collect();
                    store.replace_origin(Origin::Stim, events).unwrap();
                }
            }
            let now = edited(&store);
            match changed.is_some() {
                // An undo or redo: the store is at the model's state, and it names exactly
                // the annotations that changed.
                true => {
                    prop_assert_eq!(&now, &states[at]);
                    let mut changed = changed.unwrap();
                    changed.sort();
                    prop_assert_eq!(changed, differing(&before, &now));
                }
                // An edit: a new state, unless it changed nothing; redo is gone.
                false if now != before => {
                    states.truncate(at + 1);
                    if saved.is_some_and(|s| s > at) {
                        saved = None;
                    }
                    states.push(now);
                    at += 1;
                    prop_assert!(!history.can_redo());
                }
                false => prop_assert_eq!(&now, &states[at]),
            }
            prop_assert_eq!(history.edited(), saved != Some(at));
        }
        let stim_now = stim(&store);
        // Undoing everything restores the start, redoing everything the end; stim
        // annotations stay as they are.
        let last = states.len() - 1;
        while history.undo(&mut store).is_some() {}
        prop_assert_eq!(edited(&store), states[0].clone());
        while history.redo(&mut store).is_some() {}
        prop_assert_eq!(edited(&store), states[last].clone());
        prop_assert_eq!(stim(&store), stim_now);
    }
}

#[test]
fn a_step_that_changes_nothing_is_not_recorded() {
    let mut store = AnnotationStore::from_data(vec![data(1.0, 2.0, "BAD", Origin::File)]);
    let mut history = History::default();
    let id = store.iter().next().unwrap().id;
    let same = store.get(id).unwrap().data.clone();
    history.edit(&mut store, vec![(id, Some(same))]).unwrap();
    assert!(!history.can_undo());
    // Removing what isn't there changes nothing either.
    let gone = store.new_id();
    history.edit(&mut store, vec![(gone, None)]).unwrap();
    assert!(!history.can_undo());
    assert!(!history.push(Vec::new()));
}
