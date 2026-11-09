//! Annotations and the editable annotation store.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;

use crate::{AnnotationError, ChannelId};

/// Identifies an annotation for the whole session, across edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

/// Where an annotation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Origin {
    /// Read from the file (e.g. EDF+ TAL).
    File,
    /// Added in the viewer.
    Viewer,
    /// An event of a stim channel (see [`stim`](crate::stim)).
    Stim,
}

/// The content of an annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationData {
    /// Seconds since the recording start.
    pub onset: f64,
    /// Seconds, `>= 0`. Zero marks an event.
    pub duration: f64,
    pub description: String,
    /// Channels the annotation applies to; empty means all channels.
    pub channels: Vec<ChannelId>,
    pub origin: Origin,
}

impl AnnotationData {
    /// End time, `onset + duration`.
    pub fn end(&self) -> f64 {
        self.onset + self.duration
    }

    /// Why the timing is invalid, if it is.
    pub(crate) fn timing_error(&self) -> Option<String> {
        if !self.onset.is_finite() {
            Some(format!("onset {} is not finite", self.onset))
        } else if !(self.duration.is_finite() && self.duration >= 0.0) {
            Some(format!(
                "duration {} is not a finite number >= 0",
                self.duration
            ))
        } else if !self.end().is_finite() {
            Some(format!("end {} is not finite", self.end()))
        } else {
            None
        }
    }
}

/// An annotation with its session id.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: AnnotationId,
    pub data: AnnotationData,
}

/// One change to an [`AnnotationStore`]. Every edit is an operation, which drives undo and
/// the change messages sent to Python.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnotationOp {
    /// Adds an annotation under an id that is not in the store.
    Insert(Annotation),
    Remove(AnnotationId),
    /// Replaces the data of the annotation with the same id.
    Replace(Annotation),
}

/// Annotations of a recording, editable, with an interval index for overlap queries.
///
/// Edits happen at human speed, so the index is rebuilt on every change (about 0.4 ms for
/// 10,000 annotations); queries are `O(log n + k)`.
#[derive(Clone, Debug, Default)]
pub struct AnnotationStore {
    /// Sorted by onset, then id.
    items: Vec<Annotation>,
    /// Position of each id in `items`.
    positions: HashMap<AnnotationId, usize>,
    /// Implicit binary tree over `items` (root at 1, leaf `i` at `leaves + i`): the maximum end
    /// time in each subtree, `-inf` for padding.
    max_end: Vec<f64>,
    leaves: usize,
    next_id: u64,
    /// Counts the changes, for [`revision`](Self::revision).
    revision: u64,
}

impl AnnotationStore {
    /// A store holding `items` with ids `0, 1, ...` in the given order.
    ///
    /// The items should come from [`Recording::new`](crate::Recording::new), which validates them.
    pub fn from_data(items: Vec<AnnotationData>) -> Self {
        let items: Vec<Annotation> = items
            .into_iter()
            .zip(0..)
            .map(|(data, id)| Annotation {
                id: AnnotationId(id),
                data,
            })
            .collect();
        let mut store = AnnotationStore {
            next_id: items.len() as u64,
            items,
            ..AnnotationStore::default()
        };
        store.items.sort_by(order);
        store.reindex();
        store
    }

    /// A fresh id, never handed out or used in this store before.
    pub fn new_id(&mut self) -> AnnotationId {
        let id = AnnotationId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        self.positions.get(&id).map(|&i| &self.items[i])
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// A number that grows with every change (a successful [`apply`](Self::apply) or
    /// [`replace_origin`](Self::replace_origin)) and with nothing else, so something
    /// derived from the store can tell whether it is out of date.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// All annotations, by onset, then id.
    pub fn iter(&self) -> impl Iterator<Item = &Annotation> + '_ {
        self.items.iter()
    }

    /// Annotations that overlap the time range `t`, in [`iter`](Self::iter) order.
    ///
    /// An annotation covers the closed interval `[onset, onset + duration]`, so it overlaps
    /// `t` if `onset < t.end && onset + duration >= t.start`. An event at `t.start` is included,
    /// one at `t.end` is not.
    pub fn query(&self, t: Range<f64>) -> impl Iterator<Item = &Annotation> + '_ {
        let limit = if t.start.is_nan() || t.end.is_nan() {
            0
        } else {
            self.items.partition_point(|a| a.data.onset < t.end)
        };
        let t0 = t.start;
        // Depth-first, left before right, so results come out sorted.
        let mut stack: Vec<(usize, usize, usize)> = Vec::new();
        if limit > 0 {
            stack.push((1, 0, self.leaves));
        }
        std::iter::from_fn(move || {
            while let Some((node, lo, hi)) = stack.pop() {
                if lo >= limit || self.max_end[node] < t0 {
                    continue;
                }
                if hi - lo == 1 {
                    return Some(&self.items[lo]);
                }
                let mid = lo + (hi - lo) / 2;
                stack.push((2 * node + 1, mid, hi));
                stack.push((2 * node, lo, mid));
            }
            None
        })
    }

    /// Applies `op` and returns its inverse. On error the store is unchanged.
    ///
    /// Applying the inverses of a sequence of operations in reverse order restores the state
    /// before the sequence (undo); applying the inverse of an inverse redoes.
    pub fn apply(&mut self, op: AnnotationOp) -> Result<AnnotationOp, AnnotationError> {
        let inverse = match op {
            AnnotationOp::Insert(a) => {
                check(&a)?;
                if self.positions.contains_key(&a.id) {
                    return Err(AnnotationError::DuplicateId(a.id));
                }
                let id = a.id;
                self.next_id = self.next_id.max(id.0.saturating_add(1));
                self.insert_sorted(a);
                AnnotationOp::Remove(id)
            }
            AnnotationOp::Remove(id) => {
                let i = self.position(id)?;
                AnnotationOp::Insert(self.items.remove(i))
            }
            AnnotationOp::Replace(a) => {
                check(&a)?;
                let i = self.position(a.id)?;
                let old = self.items.remove(i);
                self.insert_sorted(a);
                AnnotationOp::Replace(old)
            }
        };
        self.reindex();
        Ok(inverse)
    }

    /// Replaces every annotation of `origin` with `items`, which get fresh ids in the given
    /// order; the others keep theirs. Not an edit: there is no inverse, as it is meant for
    /// annotations derived from something else, such as stim channel events as the pyramid
    /// build finds them. On error (an item of another origin, or with invalid timing) the
    /// store is unchanged and no ids are used up.
    pub fn replace_origin(
        &mut self,
        origin: Origin,
        items: Vec<AnnotationData>,
    ) -> Result<(), AnnotationError> {
        let first = self.next_id;
        let items: Vec<Annotation> = items
            .into_iter()
            .zip(first..)
            .map(|(data, id)| Annotation {
                id: AnnotationId(id),
                data,
            })
            .collect();
        for a in &items {
            if a.data.origin != origin {
                return Err(AnnotationError::Invalid {
                    id: a.id,
                    reason: format!("origin {:?} instead of {origin:?}", a.data.origin),
                });
            }
            check(a)?;
        }
        self.next_id = first + items.len() as u64;
        self.items.retain(|a| a.data.origin != origin);
        self.items.extend(items);
        // Two sorted runs, which the sort merges.
        self.items.sort_by(order);
        self.reindex();
        Ok(())
    }

    fn position(&self, id: AnnotationId) -> Result<usize, AnnotationError> {
        self.positions
            .get(&id)
            .copied()
            .ok_or(AnnotationError::UnknownId(id))
    }

    fn insert_sorted(&mut self, a: Annotation) {
        let i = self
            .items
            .partition_point(|x| order(x, &a) == Ordering::Less);
        self.items.insert(i, a);
    }

    /// Rebuilds the index after a change, which counts as a revision.
    fn reindex(&mut self) {
        self.revision += 1;
        self.positions = self
            .items
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id, i))
            .collect();
        self.leaves = self.items.len().next_power_of_two();
        self.max_end = vec![f64::NEG_INFINITY; 2 * self.leaves];
        for (i, a) in self.items.iter().enumerate() {
            self.max_end[self.leaves + i] = a.data.end();
        }
        for node in (1..self.leaves).rev() {
            self.max_end[node] = self.max_end[2 * node].max(self.max_end[2 * node + 1]);
        }
    }
}

fn order(a: &Annotation, b: &Annotation) -> Ordering {
    a.data.onset.total_cmp(&b.data.onset).then(a.id.cmp(&b.id))
}

fn check(a: &Annotation) -> Result<(), AnnotationError> {
    match a.data.timing_error() {
        Some(reason) => Err(AnnotationError::Invalid { id: a.id, reason }),
        None => Ok(()),
    }
}
