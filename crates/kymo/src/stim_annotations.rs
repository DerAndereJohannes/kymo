//! Stim channel annotations in the annotation store, kept up with the pyramid build and the
//! rule. GUI-free: the viewer hands in the build's progress, the time and the changes found.

use kymo_core::stim::{self, EventOptions, StimChanges};
use kymo_core::{AnnotationStore, Origin, Recording};

/// Shortest time between two updates while the build runs, in seconds: each one rebuilds
/// every stim annotation.
pub const INTERVAL: f64 = 0.25;

/// What [`StimAnnotations::update`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Update {
    /// Nothing new.
    Unchanged,
    /// The store's stim annotations were replaced.
    Changed,
    /// The build moved on, but the last update was too recent: try again in this many
    /// seconds.
    Wait(f64),
}

/// Keeps the store's [`Origin::Stim`] annotations at the codes the rule picks from the changes
/// found so far.
#[derive(Clone, Debug)]
pub struct StimAnnotations {
    options: EventOptions,
    /// The build's samples done and the time at the last update; `None` before the first,
    /// and after a rule change.
    last: Option<(u64, f64)>,
}

impl StimAnnotations {
    pub fn new(options: EventOptions) -> StimAnnotations {
        StimAnnotations {
            options,
            last: None,
        }
    }

    pub fn options(&self) -> EventOptions {
        self.options
    }

    /// Changes the rule; the next update rebuilds the annotations at once.
    pub fn set_options(&mut self, options: EventOptions) {
        if options != self.options {
            self.options = options;
            self.last = None;
        }
    }

    /// Replaces `store`'s stim annotations with the annotations of `changes()` if the build has
    /// moved on (`done` samples, read before the changes) and the last update is at least
    /// [`INTERVAL`] before `now` (seconds); at once for the first update and after a rule
    /// change. Other annotations stay as they are.
    pub fn update(
        &mut self,
        store: &mut AnnotationStore,
        recording: &Recording,
        done: u64,
        now: f64,
        changes: impl FnOnce() -> Vec<StimChanges>,
    ) -> Update {
        if let Some((last_done, at)) = self.last {
            if last_done == done {
                return Update::Unchanged;
            }
            let wait = at + INTERVAL - now;
            if wait > 0.0 {
                return Update::Wait(wait);
            }
        }
        let annotations = stim::annotations(recording, &changes(), self.options);
        store
            .replace_origin(Origin::Stim, annotations)
            .expect("stim annotations have finite onsets and the stim origin");
        self.last = Some((done, now));
        Update::Changed
    }
}
