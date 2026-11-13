//! Events from stim (trigger) channels.
//!
//! The pyramid build records every change of each stim channel's masked raw value (see
//! [`Channel::trigger_mask`](crate::Channel::trigger_mask)) as a [`Change`]; the sidecar keeps
//! them, so a reopened recording has them at once. Events are derived from the changes on
//! demand, by an [`EventRule`], so changing the rule needs no rescan.
//!
//! Gaps are skipped: the first sample after a gap is compared with the last one before it,
//! as if the data were contiguous (which is also how MNE reads EDF+D).

use crate::{AnnotationData, ChannelId, Origin, Recording};

/// The masked value of a stim channel changes at `sample` (of its grid) to `value`.
///
/// The first present sample of a channel is always a change, even to 0, since there is
/// nothing before it to compare with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub sample: u64,
    pub value: i32,
}

/// An event: the stim channel takes the code `value` at `sample`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub sample: u64,
    pub value: i32,
}

/// The changes of one stim channel found so far, in sample order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StimChanges {
    pub channel: ChannelId,
    pub changes: Vec<Change>,
}

/// Which changes are events. Changes to 0 never are. The names follow the `consecutive`
/// argument of MNE's `find_events`, which agrees for codes ≥ 0 (all masked BDF codes);
/// for negative codes, MNE's `consecutive=True` counts only changes to positive values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventRule {
    /// Every change to a non-zero value, also from another non-zero value
    /// (`consecutive=True`).
    #[default]
    EveryChange,
    /// Only changes from 0 (`consecutive=False`).
    FromZero,
    /// Changes to a larger value, from 0 or not (`consecutive="increasing"`).
    Increasing,
}

/// How events are derived from changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventOptions {
    pub rule: EventRule,
    /// Whether a non-zero value at the channel's first present sample is an event. Off by
    /// default, as in MNE (`initial_event=False`): the recording may start inside a trigger.
    pub initial_event: bool,
}

/// The events among `changes` (sorted by sample) under `options`.
pub fn events(changes: &[Change], options: EventOptions) -> Vec<Event> {
    let mut events = Vec::new();
    let mut previous: Option<i32> = None;
    for change in changes {
        let value = change.value;
        let event = match previous {
            None => options.initial_event && value != 0,
            Some(before) => {
                value != 0
                    && match options.rule {
                        EventRule::EveryChange => true,
                        EventRule::FromZero => before == 0,
                        EventRule::Increasing => value > before,
                    }
            }
        };
        if event {
            events.push(Event {
                sample: change.sample,
                value,
            });
        }
        previous = Some(value);
    }
    events
}

/// The events of `stim` as zero-duration annotations, ordered by onset: the code as the
/// description, the stim channel as the only channel, [`Origin::Stim`] as the origin.
pub fn annotations(
    recording: &Recording,
    stim: &[StimChanges],
    options: EventOptions,
) -> Vec<AnnotationData> {
    let mut annotations: Vec<AnnotationData> = stim
        .iter()
        .flat_map(|s| {
            let grid = recording.grid(recording.channel(s.channel).grid);
            let stream = recording.stream_of(s.channel);
            events(&s.changes, options)
                .into_iter()
                .map(move |e| AnnotationData {
                    onset: grid.time_of(e.sample),
                    duration: 0.0,
                    description: e.value.to_string(),
                    channels: vec![s.channel],
                    origin: Origin::Stim,
                    stream,
                })
        })
        .collect();
    annotations.sort_by(|a, b| a.onset.total_cmp(&b.onset));
    annotations
}

/// Records the changes of one channel from its masked values, chunk by chunk.
#[derive(Clone, Debug)]
pub(crate) struct Scanner {
    mask: i32,
    last: Option<i32>,
    found: Vec<Change>,
}

impl Scanner {
    pub(crate) fn new(mask: Option<u32>) -> Self {
        Scanner {
            // Reinterpreting the bits: the mask applies to the raw two's-complement value.
            mask: mask.map_or(-1, |m| m as i32),
            last: None,
            found: Vec::new(),
        }
    }

    /// Scans `codes`, the raw values of the present samples from `start` on.
    pub(crate) fn scan(&mut self, start: u64, codes: impl IntoIterator<Item = i32>) {
        for (sample, code) in (start..).zip(codes) {
            let value = code & self.mask;
            if self.last != Some(value) {
                self.found.push(Change { sample, value });
                self.last = Some(value);
            }
        }
    }

    /// The changes found since the last call.
    pub(crate) fn take(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.found)
    }
}
