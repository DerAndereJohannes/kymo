//! The marker file (`.bvrm`, section 4 of the specification): annotations and the
//! recording's start time.

use std::collections::HashMap;

use super::participants::Participants;
use crate::{AnnotationData, Origin};

/// What a marker file holds.
#[derive(Debug, Default)]
pub(super) struct Markers {
    pub(super) annotations: Vec<AnnotationData>,
    /// The date and time of the first sample, from the earliest dated marker.
    pub(super) start: Option<jiff::civil::DateTime>,
    /// That marker's offset from UTC, as written ("+01:00").
    pub(super) utc_offset: Option<String>,
}

/// Markers of this type record when saving started and stopped and where the clocks were
/// synchronized, not events of the recording, so they aren't annotations (Analyzer leaves
/// them out too).
const RECORDING_INFO: &str = "Recording Info";

/// Parses a marker file's text (UTF-8, byte-order mark removed) for a recording at `rate`
/// Hz with `participants`.
///
/// A line's columns go by the names in the first line, in any order (`ParticipantId`, for
/// one, is left out where there is one participant). Each marker becomes an annotation:
/// - onset `Sample / rate`, as samples count from 0;
/// - description "Type/CodeValue" ("Stimulus/S1"), or the type alone without a code or
///   value;
/// - the channel `Channel` names, or all channels where it is empty;
/// - two markers sharing a `StartEndId` are one annotation from the first to the second.
///
/// With several participants, a marker with a `ParticipantId` is in that participant's
/// stream and applies to its channels (its channel of that name, with a `Channel`); one
/// without is in the stream of all participants and applies to all channels (each
/// participant's channel of that name, with a `Channel`).
///
/// "Recording Info" markers aren't annotations. The start time is the date and time of the
/// earliest marker with a `DateTime`, moved back by its sample, in local time.
pub(super) fn parse(
    text: &str,
    rate: f64,
    participants: &Participants,
    warnings: &mut Vec<String>,
) -> Markers {
    let mut markers = Markers::default();
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        warnings.push("the marker file is empty, so there are no annotations".into());
        return markers;
    };
    let columns: Vec<&str> = first.split('\t').map(str::trim).collect();
    let column = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(sample), Some(kind)) = (column("Sample"), column("Type")) else {
        warnings.push(
            "the marker file has no Sample or Type column, so there are no annotations".into(),
        );
        return markers;
    };
    let (code, value, participant, channel, date, pair) = (
        column("Code"),
        column("Value"),
        column("ParticipantId"),
        column("Channel"),
        column("DateTime"),
        column("StartEndId"),
    );

    // Markers whose pair hasn't ended yet, by `StartEndId`: the annotation's index, `None`
    // for one that isn't an annotation.
    let mut open: HashMap<&str, (u64, Option<usize>)> = HashMap::new();
    let (mut skipped, mut unknown_channels, mut unknown_participants) = (0usize, 0, 0);
    let mut earliest: Option<(u64, &str)> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let field = |c: Option<usize>| c.and_then(|c| fields.get(c)).map_or("", |f| f.trim());
        let Ok(at) = field(Some(sample)).parse::<u64>() else {
            skipped += 1;
            continue;
        };
        let dated = field(date);
        if !dated.is_empty() && earliest.is_none_or(|(s, _)| at < s) {
            earliest = Some((at, dated));
        }
        let kind = field(Some(kind));
        let id = field(pair);
        if !id.is_empty()
            && let Some((start, index)) = open.remove(id)
        {
            if let Some(index) = index {
                let a: &mut AnnotationData = &mut markers.annotations[index];
                a.onset = start.min(at) as f64 / rate;
                a.duration = start.abs_diff(at) as f64 / rate;
            }
            continue;
        }
        let index = (kind != RECORDING_INFO).then_some(markers.annotations.len());
        if !id.is_empty() {
            open.insert(id, (at, index));
        }
        if index.is_none() {
            continue;
        }
        let name = format!("{}{}", field(code), field(value));
        let description = match (kind.is_empty(), name.is_empty()) {
            (false, false) => format!("{kind}/{name}"),
            (false, true) => kind.to_owned(),
            (true, _) => name,
        };
        let id = field(participant);
        let own = if participants.several() && !id.is_empty() {
            let own = participants.of(id);
            unknown_participants += usize::from(own.is_none());
            own
        } else {
            None
        };
        let channels = match (own, field(channel)) {
            (Some(s), "") => participants.channels_of(s),
            (None, "") => Vec::new(),
            (own, name) => match participants.named(name, own) {
                c if c.is_empty() => {
                    unknown_channels += 1;
                    own.map(|s| participants.channels_of(s)).unwrap_or_default()
                }
                c => c,
            },
        };
        markers.annotations.push(AnnotationData {
            onset: at as f64 / rate,
            duration: 0.0,
            description,
            channels,
            origin: Origin::File,
            stream: own.or(participants.all()),
        });
    }
    let unpaired = open.values().filter(|(_, index)| index.is_some()).count();
    if unpaired > 0 {
        warnings.push(format!(
            "{unpaired} markers have a StartEndId no other marker has; they are points in time"
        ));
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} markers without a valid sample are ignored"
        ));
    }
    if unknown_participants > 0 {
        warnings.push(format!(
            "{unknown_participants} markers name a participant the header doesn't have; they \
             apply to all participants"
        ));
    }
    if unknown_channels > 0 {
        warnings.push(format!(
            "{unknown_channels} markers name a channel the header doesn't have; they apply to \
             all channels (of their participant, where they have one)"
        ));
    }
    if let Some((at, dated)) = earliest {
        match date_time(dated, at as f64 / rate) {
            Some((start, offset)) => {
                markers.start = Some(start);
                markers.utc_offset = offset;
            }
            None => warnings.push(format!(
                "the earliest marker's DateTime {dated:?} is invalid; the start time is unknown"
            )),
        }
    }
    markers
}

/// The local date and time `seconds` before an ISO 8601 date and time with its offset from
/// UTC ("2025-06-30T13:13:28.974479+02:00"), and that offset as written ("+02:00"; `None`
/// without one).
fn date_time(text: &str, seconds: f64) -> Option<(jiff::civil::DateTime, Option<String>)> {
    let (date, time) = text.split_once('T')?;
    let split = time.find(['+', '-', 'Z', 'z']).unwrap_or(time.len());
    let (time, offset) = time.split_at(split);
    let local: jiff::civil::DateTime = format!("{date}T{time}").parse().ok()?;
    let back = jiff::SignedDuration::try_from_secs_f64(seconds).ok()?;
    let offset = (!offset.is_empty()).then(|| match offset {
        "Z" | "z" => "+00:00".to_owned(),
        o => o.to_owned(),
    });
    Some((local.checked_sub(back).ok()?, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_keep_local_time_and_their_offset() {
        let (at, offset) = date_time("2025-06-30T13:13:28.974479+02:00", 0.0).unwrap();
        assert_eq!(
            at,
            jiff::civil::date(2025, 6, 30).at(13, 13, 28, 974_479_000)
        );
        assert_eq!(offset.as_deref(), Some("+02:00"));
        let (at, offset) = date_time("2025-06-30T00:00:01Z", 2.5).unwrap();
        assert_eq!(
            at,
            jiff::civil::date(2025, 6, 29).at(23, 59, 58, 500_000_000)
        );
        assert_eq!(offset.as_deref(), Some("+00:00"));
        let (_, offset) = date_time("2025-06-30T10:00:00-05:30", 0.0).unwrap();
        assert_eq!(offset.as_deref(), Some("-05:30"));
        assert_eq!(date_time("2025-06-30T10:00:00", 0.0).unwrap().1, None);
        assert!(date_time("2025-06-30", 0.0).is_none());
        assert!(date_time("2025-13-30T10:00:00+01:00", 0.0).is_none());
    }
}
