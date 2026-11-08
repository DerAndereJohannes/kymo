//! Undo and redo of annotation edits.
//!
//! An edit is recorded as the states of the annotations it changed, before and after, so
//! one step can hold any number of changes (a drag that ends in a merge, say) and undoing
//! or redoing it is setting each annotation to one side's state. Stim annotations are never
//! edited, so they never appear here.

use kymo_core::{AnnotationData, AnnotationError, AnnotationId, AnnotationOp, AnnotationStore};

/// One annotation's content before and after an edit; `None` where it doesn't exist.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub id: AnnotationId,
    pub before: Option<AnnotationData>,
    pub after: Option<AnnotationData>,
}

/// One undoable edit: the annotations it changed. [`History::push`] keeps each once.
pub type Step = Vec<Change>;

/// The edits that can be undone and redone, most recent last.
#[derive(Clone, Debug)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// How many steps were done at the saved state (at first, the opened one); `None` once
    /// no undo or redo can return to it.
    saved: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            saved: Some(0),
        }
    }
}

impl History {
    /// Whether the annotations differ from the saved state, the opened one until
    /// [`mark_saved`](Self::mark_saved).
    pub fn edited(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    /// Makes the current state the saved one.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Records an edit already made to the store. Changes of the same annotation become
    /// one, from its first state to its last; those that leave it as it was are dropped,
    /// and so is a step without any other. A recorded step clears what could be redone.
    /// Returns whether it was recorded.
    pub fn push(&mut self, changes: Step) -> bool {
        let mut step = Step::new();
        for change in changes {
            match step.iter_mut().find(|c| c.id == change.id) {
                Some(c) => c.after = change.after,
                None => step.push(change),
            }
        }
        step.retain(|c| c.before != c.after);
        if step.is_empty() {
            return false;
        }
        // The saved state was undone: with redo gone, it can't come back.
        if self.saved.is_some_and(|saved| saved > self.undo.len()) {
            self.saved = None;
        }
        self.undo.push(step);
        self.redo.clear();
        true
    }

    /// Sets each annotation of `changes` to its content (`None` removes it) and records
    /// them as one step. On error, the store is left as it was and nothing is recorded.
    pub fn edit(
        &mut self,
        store: &mut AnnotationStore,
        changes: Vec<(AnnotationId, Option<AnnotationData>)>,
    ) -> Result<(), AnnotationError> {
        let mut step = Step::new();
        for (id, after) in changes {
            let before = store.get(id).map(|a| a.data.clone());
            if let Err(error) = set(store, id, after.clone()) {
                restore(store, &step, |c| &c.before);
                return Err(error);
            }
            step.push(Change { id, before, after });
        }
        self.push(step);
        Ok(())
    }

    /// Undoes the last edit; returns the annotations it changed, or `None` if there was
    /// nothing to undo.
    pub fn undo(&mut self, store: &mut AnnotationStore) -> Option<Vec<AnnotationId>> {
        let step = self.undo.pop()?;
        restore(store, &step, |c| &c.before);
        let ids = step.iter().map(|c| c.id).collect();
        self.redo.push(step);
        Some(ids)
    }

    /// Redoes the last undone edit; returns the annotations it changed, or `None` if there
    /// was nothing to redo.
    pub fn redo(&mut self, store: &mut AnnotationStore) -> Option<Vec<AnnotationId>> {
        let step = self.redo.pop()?;
        restore(store, &step, |c| &c.after);
        let ids = step.iter().map(|c| c.id).collect();
        self.undo.push(step);
        Some(ids)
    }
}

/// Sets annotation `id` in `store` to `data`: inserts, replaces or removes it.
pub fn set(
    store: &mut AnnotationStore,
    id: AnnotationId,
    data: Option<AnnotationData>,
) -> Result<(), AnnotationError> {
    let op = match (store.get(id).is_some(), data) {
        (true, Some(data)) => AnnotationOp::Replace(kymo_core::Annotation { id, data }),
        (false, Some(data)) => AnnotationOp::Insert(kymo_core::Annotation { id, data }),
        (true, None) => AnnotationOp::Remove(id),
        (false, None) => return Ok(()),
    };
    store.apply(op).map(|_| ())
}

/// Sets each annotation of `step`, last first, to the side `side` picks: an edit being
/// rolled back may hold an annotation more than once. Those states were in the store
/// before, so they are valid.
fn restore(
    store: &mut AnnotationStore,
    step: &Step,
    side: impl Fn(&Change) -> &Option<AnnotationData>,
) {
    for change in step.iter().rev() {
        set(store, change.id, side(change).clone())
            .expect("the history holds states the store had");
    }
}
