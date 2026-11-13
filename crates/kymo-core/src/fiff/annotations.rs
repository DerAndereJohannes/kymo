//! MNE's annotations block: onsets, ends, descriptions, their origin and, per annotation,
//! the channels it applies to (as JSON).

use super::tag::{Node, kind, types};
use crate::{AnnotationData, ChannelId, Origin};

/// The annotations of block `node`, with onsets from the first sample: `first` is
/// `first_samp / sfreq`, `meas_date` the recording's date in seconds since 1970. `channel`
/// finds a channel by name.
///
/// MNE stores onsets from `orig_time`, the annotations' own date, if they have one, and
/// from the recording's sample 0 (`meas_date`) otherwise; without `meas_date`, from sample
/// 0 too. Annotations wholly before the first sample or after `end` (the data's end, from
/// the first sample) are left out, with a warning.
pub(super) fn read(
    node: &Node,
    map: &[u8],
    first: f64,
    end: f64,
    meas_date: Option<f64>,
    channel: &dyn Fn(&str) -> Option<ChannelId>,
    warnings: &mut Vec<String>,
) -> Vec<AnnotationData> {
    let (mut onsets, mut ends, mut descriptions) = (Vec::new(), Vec::new(), Vec::new());
    let (mut orig_time, mut channels) = (None, None);
    for tag in &node.tags {
        match tag.kind {
            kind::MNE_BASELINE_MIN => onsets = tag.numbers(map),
            kind::MNE_BASELINE_MAX => ends = tag.numbers(map),
            kind::DESCRIPTION => descriptions = tag.names(map),
            kind::MEAS_DATE => {
                // Seconds and microseconds (as doubles or integers), or seconds alone in
                // older files.
                let v = tag.numbers(map);
                orig_time = match (v.first(), v.get(1)) {
                    (Some(s), Some(us)) if tag.typ != types::FLOAT => Some(s + us * 1e-6),
                    (Some(s), _) => Some(*s),
                    _ => None,
                };
            }
            kind::MNE_EPOCHS_DROP_LOG => match string_lists(&tag.text(map)) {
                Some(lists) => channels = Some(lists),
                None => warnings.push(
                    "the annotations' channel lists are not valid JSON; the annotations apply \
                     to all channels"
                        .to_owned(),
                ),
            },
            _ => {}
        }
    }
    let n = onsets.len().min(ends.len()).min(descriptions.len());
    if [onsets.len(), ends.len(), descriptions.len()]
        .iter()
        .any(|&k| k != n)
    {
        warnings.push(format!(
            "the annotations block has {} onsets, {} ends and {} descriptions; the first {n} \
             are read",
            onsets.len(),
            ends.len(),
            descriptions.len()
        ));
    }
    let shift = match (orig_time, meas_date) {
        (Some(orig), Some(date)) => orig - date - first,
        // From the first sample already: MNE adds `first_samp / sfreq` when it reads them.
        (None, Some(_)) => 0.0,
        _ => -first,
    };
    let mut unknown = Vec::new();
    let mut outside = 0;
    let mut annotations = Vec::with_capacity(n);
    for k in 0..n {
        let onset = onsets[k] + shift;
        let duration = (ends[k] - onsets[k]).max(0.0);
        if !(onset.is_finite() && duration.is_finite()) || onset + duration < 0.0 || onset > end {
            outside += 1;
            continue;
        }
        let ids = channels
            .as_ref()
            .and_then(|lists: &Vec<Vec<String>>| lists.get(k))
            .map(|list| {
                let ids: Option<Vec<ChannelId>> = list
                    .iter()
                    .map(|name| {
                        let id = channel(name);
                        if id.is_none() && !unknown.contains(name) {
                            unknown.push(name.clone());
                        }
                        id
                    })
                    .collect();
                ids.unwrap_or_default()
            })
            .unwrap_or_default();
        annotations.push(AnnotationData {
            onset,
            duration,
            description: descriptions[k].clone(),
            channels: ids,
            origin: Origin::File,
            stream: None,
        });
    }
    if !unknown.is_empty() {
        warnings.push(format!(
            "annotations name channels the recording doesn't have ({}); they apply to all \
             channels",
            unknown.join(", ")
        ));
    }
    if outside > 0 {
        warnings.push(format!(
            "{outside} annotation(s) outside the data are left out"
        ));
    }
    annotations
}

/// A JSON array of arrays of strings, as MNE writes the annotations' channels
/// (`[["MEG 0111", "EEG 001"], []]`); `None` if the text is anything else.
pub(super) fn string_lists(text: &str) -> Option<Vec<Vec<String>>> {
    let mut p = Parser {
        chars: text.chars().peekable(),
    };
    let lists = p.array(|p| p.array(Parser::string))?;
    p.space();
    p.chars.next().is_none().then_some(lists)
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.chars.next_if(|c| c.is_whitespace()).is_some() {}
    }

    fn eat(&mut self, c: char) -> Option<()> {
        self.space();
        self.chars.next_if_eq(&c).map(|_| ())
    }

    /// `[item, item, ...]`.
    fn array<T>(&mut self, item: impl Fn(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        self.eat('[')?;
        let mut items = Vec::new();
        if self.eat(']').is_some() {
            return Some(items);
        }
        loop {
            items.push(item(self)?);
            if self.eat(']').is_some() {
                return Some(items);
            }
            self.eat(',')?;
        }
    }

    /// A string with JSON's escapes, surrogate pairs included.
    fn string(&mut self) -> Option<String> {
        self.eat('"')?;
        let mut s = String::new();
        loop {
            match self.chars.next()? {
                '"' => return Some(s),
                '\\' => match self.chars.next()? {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'u' => {
                        let high = self.hex()?;
                        let c = if (0xd800..0xdc00).contains(&high) {
                            self.chars.next_if_eq(&'\\')?;
                            self.chars.next_if_eq(&'u')?;
                            let low = self.hex()?;
                            char::from_u32(
                                0x10000 + ((high - 0xd800) << 10) + (low.checked_sub(0xdc00)?),
                            )
                        } else {
                            char::from_u32(high)
                        };
                        s.push(c?);
                    }
                    c => s.push(c),
                },
                c => s.push(c),
            }
        }
    }

    fn hex(&mut self) -> Option<u32> {
        let mut v = 0;
        for _ in 0..4 {
            v = v * 16 + self.chars.next()?.to_digit(16)?;
        }
        Some(v)
    }
}
