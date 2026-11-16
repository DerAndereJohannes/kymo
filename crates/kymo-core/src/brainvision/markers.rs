//! The marker file (`.vmrk`): annotations and the recording's start time.

use super::text::{Ini, Repairs, unescape};
use crate::{AnnotationData, ChannelId, Origin};

/// What a marker file holds.
#[derive(Debug, Default)]
pub(super) struct Markers {
    pub(super) annotations: Vec<AnnotationData>,
    /// The date of the first sample, from the first dated "New Segment" marker.
    pub(super) start: Option<jiff::civil::DateTime>,
}

/// Parses a marker file's text for a recording at `rate` Hz with `channels` channels.
///
/// Each marker `Mk<n>=<type>,<description>,<position>,<size>,<channel>[,<date>]` becomes an
/// annotation:
/// - description "type/description" (MNE's form), or whichever of the two is not empty;
/// - onset `(position − 1) / rate`, as positions count from 1 (a marker at a position below
///   1, which the spec doesn't allow, is left out with a warning);
/// - duration `size / rate`, except that a size of 0 or 1 marks a point in time (0 s);
/// - the channel it names, or all channels for 0.
///
/// A "New Segment" marker at position 1 marks the recording start, so it is not an
/// annotation; later ones are (the data stays continuous across them). The first dated New
/// Segment gives the start time, moved back by its position.
pub(super) fn parse(ini: &Ini, rate: f64, channels: usize, warnings: &mut Vec<String>) -> Markers {
    if !ini.first_line.contains("Marker File") {
        warnings.push(format!(
            "the marker file starts with {:?}, not a marker file's identification",
            ini.first_line
        ));
    }
    let mut markers = Markers::default();
    let (mut skipped, mut before, mut bad_channels) = (0usize, 0usize, 0usize);
    let mut repairs = Repairs::new("marker descriptions");
    for (key, value) in ini.section("Marker Infos") {
        if !key.get(..2).is_some_and(|k| k.eq_ignore_ascii_case("mk")) {
            continue;
        }
        let fields: Vec<&str> = value.split(',').collect();
        let Some(position) = fields.get(2).and_then(|p| p.trim().parse::<i64>().ok()) else {
            skipped += 1;
            continue;
        };
        if position < 1 {
            before += 1;
            continue;
        }
        let kind = unescape(fields[0]);
        let description = unescape(fields[1]);
        let size = fields
            .get(3)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let channel = fields
            .get(4)
            .and_then(|c| c.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let onset = (position - 1) as f64 / rate;
        if kind == "New Segment" {
            if markers.start.is_none()
                && let Some(date) = fields.get(5).map(|d| d.trim())
            {
                markers.start = start_date(date, onset, warnings);
            }
            if position == 1 {
                continue;
            }
        }
        let channels = match channel {
            0 => Vec::new(),
            c if c <= channels => vec![ChannelId(c as u32 - 1)],
            _ => {
                bad_channels += 1;
                Vec::new()
            }
        };
        let description = match (kind.is_empty(), description.is_empty()) {
            (false, false) => format!("{kind}/{description}"),
            (false, true) => kind,
            (true, _) => description,
        };
        let description = repairs.apply(description);
        markers.annotations.push(AnnotationData {
            onset,
            duration: if size > 1 { size as f64 / rate } else { 0.0 },
            description,
            channels,
            origin: Origin::File,
            stream: None,
        });
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} markers without a valid position are ignored"
        ));
    }
    if before > 0 {
        warnings.push(format!(
            "{before} markers at positions below 1 (before the first sample, as positions count \
             from 1) are ignored"
        ));
    }
    repairs.warn(warnings);
    if bad_channels > 0 {
        warnings.push(format!(
            "{bad_channels} markers name a channel the header doesn't have; they apply to all \
             channels"
        ));
    }
    markers
}

/// The date of the first sample, from a New Segment's date `YYYYMMDDhhmmssuuuuuu` at
/// `onset` seconds. `None` without a date (empty, or all zeros as some writers put).
fn start_date(date: &str, onset: f64, warnings: &mut Vec<String>) -> Option<jiff::civil::DateTime> {
    if date.bytes().all(|b| b == b'0') {
        return None;
    }
    let parsed = (|| {
        if date.len() != 20 || !date.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let part = |range: std::ops::Range<usize>| date[range].parse::<i32>().ok();
        let narrow = |x: i32| i8::try_from(x).ok();
        let at = jiff::civil::DateTime::new(
            i16::try_from(part(0..4)?).ok()?,
            narrow(part(4..6)?)?,
            narrow(part(6..8)?)?,
            narrow(part(8..10)?)?,
            narrow(part(10..12)?)?,
            narrow(part(12..14)?)?,
            part(14..20)? * 1000,
        )
        .ok()?;
        let offset = jiff::SignedDuration::try_from_secs_f64(onset).ok()?;
        at.checked_sub(offset).ok()
    })();
    if parsed.is_none() {
        warnings.push(format!(
            "invalid New Segment date {date:?}; the start time is unknown"
        ));
    }
    parsed
}
