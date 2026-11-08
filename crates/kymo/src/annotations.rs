//! The annotation overlay: the legend (which descriptions are shown, in which colour) and the
//! marks drawn for the annotations in view, thinned to what the screen can show.
//!
//! Marks are in physical pixel columns of the trace area and in rows, so this module needs
//! no GUI; `viewer` turns them into shapes.

use std::collections::HashMap;
use std::ops::Range;

use kymo_core::{Annotation, AnnotationData, AnnotationStore, ChannelId, Origin};

use crate::view::ViewState;

/// Colours annotations cycle through (see `ui::palette::annotation_color`).
pub const COLORS: usize = 10;

/// One description in the legend.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub description: String,
    /// Index into the annotation colours, `< COLORS`.
    pub color: usize,
    /// Annotations with this description in the store.
    pub count: usize,
    pub shown: bool,
}

/// The descriptions of a store's annotations, each with a colour that never changes, a
/// count, and whether it is shown.
#[derive(Clone, Debug, Default)]
pub struct Legend {
    /// In order of first appearance. Entries are never removed, so their colours (by
    /// position) never change.
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
}

impl Legend {
    /// The legend of `store`, everything shown.
    pub fn new(store: &AnnotationStore) -> Legend {
        let mut legend = Legend::default();
        legend.sync(store);
        legend
    }

    /// Counts `store`'s annotations per description. Descriptions not seen before are added
    /// at the end, by onset, shown, each with the next colour; known ones keep their colour
    /// and shown state, even while none of their annotations are left.
    pub fn sync(&mut self, store: &AnnotationStore) {
        for entry in &mut self.entries {
            entry.count = 0;
        }
        for a in store.iter() {
            let description = &a.data.description;
            let i = match self.index.get(description) {
                Some(&i) => i,
                None => {
                    let i = self.entries.len();
                    self.entries.push(Entry {
                        description: description.clone(),
                        color: i % COLORS,
                        count: 0,
                        shown: true,
                    });
                    self.index.insert(description.clone(), i);
                    i
                }
            };
            self.entries[i].count += 1;
        }
    }

    /// Descriptions with at least one annotation, in order of first appearance.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> + '_ {
        self.entries.iter().filter(|e| e.count > 0)
    }

    pub fn entry(&self, description: &str) -> Option<&Entry> {
        self.index.get(description).map(|&i| &self.entries[i])
    }

    /// Whether there is no annotation to show.
    pub fn is_empty(&self) -> bool {
        self.entries().next().is_none()
    }

    pub fn set_shown(&mut self, description: &str, shown: bool) {
        if let Some(&i) = self.index.get(description) {
            self.entries[i].shown = shown;
        }
    }

    pub fn set_all_shown(&mut self, shown: bool) {
        for entry in &mut self.entries {
            entry.shown = shown;
        }
    }

    /// How many descriptions with annotations are shown, and how many there are.
    pub fn shown_count(&self) -> (usize, usize) {
        self.entries().fold((0, 0), |(shown, all), e| {
            (shown + usize::from(e.shown), all + 1)
        })
    }

    /// How many annotations there are, of every description.
    pub fn total(&self) -> usize {
        self.entries().map(|e| e.count).sum()
    }
}

/// The rows a mark covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Span {
    /// The whole height of the trace area: an annotation of all channels.
    All,
    /// One row (an index into the view's rows), for an annotation tied to its channel.
    Row(usize),
}

/// A band or line of one colour over whole pixel columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Physical pixel columns of the trace area, never empty.
    pub columns: Range<u32>,
    pub span: Span,
    pub color: usize,
}

/// A description at the top of the trace area.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// Physical pixels from the left edge of the trace area: the annotation's start, or 0
    /// if it starts before the view.
    pub x: f64,
    pub text: String,
    pub color: usize,
}

/// What to draw for the annotations in view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Translucent: annotations at least a pixel long. Bands of the same colour and span
    /// never overlap or touch; overlapping ones are merged.
    pub bands: Vec<Mark>,
    /// Solid, one column wide: events, and annotations shorter than a pixel. At most one
    /// per column, colour and span.
    pub lines: Vec<Mark>,
    /// From left to right, never overlapping.
    pub labels: Vec<Label>,
}

/// The marks for `store`'s annotations in `view`, on a trace area `width_px` physical
/// pixels wide, of the descriptions `legend` shows.
///
/// An annotation of all channels, or a stim event, spans the trace area's height (see
/// [`spans_all`]); one tied to channels spans the rows of those on screen, and draws nothing
/// if none is. Marks of the same colour over
/// the same rows that would overlap are merged, so however many annotations there are,
/// there are at most a few marks per pixel column, and none is lost: every pixel an
/// annotation covers is covered by a mark of its colour.
///
/// Each annotation drawn gets a label unless it would overlap the previous one;
/// `label_width` gives the room a label takes, in pixels, the gap to the next included.
pub fn marks(
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width_px: u32,
    mut label_width: impl FnMut(&str) -> f64,
) -> Marks {
    let mut marks = Marks::default();
    if width_px == 0 {
        return marks;
    }
    let rows = view.visible_rows();
    // The band being extended and the last line, per colour and span.
    let mut open: HashMap<(usize, Span), Range<u32>> = HashMap::new();
    let mut lines: HashMap<(usize, Span), u32> = HashMap::new();
    let mut next_label = f64::NEG_INFINITY;
    let mut spans = Vec::new();
    // By onset, so marks of each colour and span come from left to right.
    for a in store.query(view.time()) {
        let Some(entry) = legend.entry(&a.data.description).filter(|e| e.shown) else {
            continue;
        };
        spans.clear();
        if spans_all(&a.data) {
            spans.push(Span::All);
        } else {
            spans.extend(
                a.data
                    .channels
                    .iter()
                    .filter_map(|&c| view.row_of(c))
                    .filter(|r| rows.contains(r))
                    .map(Span::Row),
            );
            spans.sort();
            spans.dedup();
        }
        if spans.is_empty() {
            continue;
        }

        let (columns, line) = columns(view, &a.data, width_px);
        for &span in &spans {
            let key = (entry.color, span);
            if line {
                if lines.insert(key, columns.start) != Some(columns.start) {
                    marks.lines.push(Mark {
                        columns: columns.clone(),
                        span,
                        color: entry.color,
                    });
                }
            } else {
                let columns = columns.clone();
                match open.get_mut(&key) {
                    Some(band) if columns.start <= band.end => band.end = band.end.max(columns.end),
                    Some(band) => marks.bands.push(Mark {
                        columns: std::mem::replace(band, columns),
                        span,
                        color: entry.color,
                    }),
                    None => {
                        open.insert(key, columns);
                    }
                }
            }
        }

        let x = view.x_at(a.data.onset, f64::from(width_px)).max(0.0);
        if x >= next_label {
            next_label = x + label_width(&a.data.description);
            marks.labels.push(Label {
                x,
                text: a.data.description.clone(),
                color: entry.color,
            });
        }
    }
    marks
        .bands
        .extend(open.into_iter().map(|((color, span), columns)| Mark {
            columns,
            span,
            color,
        }));
    marks
}

/// The annotations under pixel column `x` of a trace area `width_px` physical pixels wide,
/// by onset: those of the descriptions `legend` shows whose mark is within `slack` columns
/// of `x`, and that span all rows or apply to `channel`, the one under the pointer.
pub fn under<'s>(
    view: &ViewState,
    store: &'s AnnotationStore,
    legend: &Legend,
    width_px: u32,
    x: f64,
    slack: u32,
    channel: Option<ChannelId>,
) -> Vec<&'s Annotation> {
    if width_px == 0 {
        return Vec::new();
    }
    let at = column(x, width_px - 1);
    let width = f64::from(width_px);
    // Within the trace area: annotations beyond it are not drawn.
    let near = at.saturating_sub(slack)..(at + 1 + slack).min(width_px);
    let time = view.time_at(f64::from(near.start), width)..view.time_at(f64::from(near.end), width);
    store
        .query(time)
        .filter(|a| legend.entry(&a.data.description).is_some_and(|e| e.shown))
        .filter(|a| spans_all(&a.data) || channel.is_some_and(|c| a.data.channels.contains(&c)))
        .filter(|a| {
            let (columns, _) = columns(view, &a.data, width_px);
            columns.start < near.end && near.start < columns.end
        })
        .collect()
}

/// Whether `a`'s mark spans the trace area's height: if it applies to all channels, or is a
/// stim event. Events belong to their stim channel, but mark a moment for all of them, as
/// MNE draws them; and the stim row may well be off screen.
pub fn spans_all(a: &AnnotationData) -> bool {
    a.channels.is_empty() || a.origin == Origin::Stim
}

/// The pixel columns `a`'s mark covers on a trace area `width_px` (`> 0`) pixels wide, and
/// whether it is a line: one column, for annotations shorter than a pixel.
fn columns(view: &ViewState, a: &AnnotationData, width_px: u32) -> (Range<u32>, bool) {
    let width = f64::from(width_px);
    let x0 = view.x_at(a.onset, width);
    let x1 = view.x_at(a.end(), width);
    let first = column(x0, width_px - 1);
    if x1 - x0 < 1.0 {
        (first..first + 1, true)
    } else {
        (first..(x1.ceil().min(width) as u32).max(first + 1), false)
    }
}

/// The y range `span` covers in a trace area `height` tall, from its top, in the units of
/// `height`.
pub fn span_y(view: &ViewState, span: Span, height: f64) -> Range<f64> {
    match span {
        Span::All => 0.0..height,
        Span::Row(row) => {
            view.y_of_row(row as f64, height)..view.y_of_row(row as f64 + 1.0, height)
        }
    }
}

/// The pixel column holding `x`, within `0..=last`.
fn column(x: f64, last: u32) -> u32 {
    x.floor().clamp(0.0, f64::from(last)) as u32
}
