//! The annotation overlay: the legend (which descriptions are shown, in which color) and the
//! marks drawn for the annotations in view, thinned to what the screen can show.
//!
//! Marks are in physical pixel columns of the trace area and in rows, so this module needs
//! no GUI; `viewer` turns them into shapes.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use kymo_core::{Annotation, AnnotationData, AnnotationId, AnnotationStore, ChannelId, Origin};

use crate::view::ViewState;

/// Colors annotations cycle through (see `ui::palette::annotation_color`).
pub const COLORS: usize = 10;

/// A color as red, green and blue (sRGB).
pub type Rgb = [u8; 3];

/// Colors picked for descriptions, remembered across recordings.
pub type AnnotationColors = BTreeMap<String, Rgb>;

/// The color of a description's marks: a palette color (lighter in dark themes), or one
/// picked for it, used as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnnotationColor {
    /// Index into the annotation colors, `< COLORS`.
    Palette(usize),
    Custom(Rgb),
}

/// One description in the legend.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub description: String,
    /// Its palette color, by first appearance, `< COLORS`; kept while a picked color
    /// replaces it.
    pub slot: usize,
    /// The palette color, or the one picked for it (see [`Legend::apply_colors`]).
    pub color: AnnotationColor,
    /// Annotations with this description in the store.
    pub count: usize,
    pub shown: bool,
}

/// The descriptions of a store's annotations, each with a color that never changes, a
/// count, and whether it is shown.
#[derive(Clone, Debug, Default)]
pub struct Legend {
    /// In order of first appearance. Entries are never removed, so their colors (by
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
    /// at the end, by onset, shown, each with the next color; known ones keep their color
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
                        slot: i % COLORS,
                        color: AnnotationColor::Palette(i % COLORS),
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

    /// Colors each description as picked in `picked`, or with its palette color.
    pub fn apply_colors(&mut self, picked: &AnnotationColors) {
        for entry in &mut self.entries {
            entry.color = picked
                .get(&entry.description)
                .map_or(AnnotationColor::Palette(entry.slot), |&rgb| {
                    AnnotationColor::Custom(rgb)
                });
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

/// A band or line of one color over whole pixel columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Physical pixel columns of the trace area, never empty.
    pub columns: Range<u32>,
    pub span: Span,
    pub color: AnnotationColor,
}

/// A description at the top of the trace area.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// Physical pixels from the left edge of the trace area: the annotation's start, or 0
    /// if it starts before the view.
    pub x: f64,
    pub text: String,
    pub color: AnnotationColor,
}

/// What to draw for the annotations in view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Translucent: annotations at least a pixel long. Bands of the same color and span
    /// never overlap or touch; overlapping ones are merged. By color, span and first
    /// column, so bands of different colors that overlap blend the same in every frame.
    pub bands: Vec<Mark>,
    /// Solid, one column wide: annotations without or with less than a pixel's duration. At most one
    /// per column, color and span.
    pub lines: Vec<Mark>,
    /// From left to right, never overlapping.
    pub labels: Vec<Label>,
}

/// The marks for `store`'s annotations in `view`, on a trace area `width_px` physical
/// pixels wide, of the descriptions `legend` shows.
///
/// An annotation of all channels, or of a stim channel, spans the trace area's height (see
/// [`spans_all`]); one tied to channels spans the rows of those on screen, and draws nothing
/// if none is. Marks of the same color over
/// the same rows that would overlap are merged, so however many annotations there are,
/// there are at most a few marks per pixel column, and none is lost: every pixel an
/// annotation covers is covered by a mark of its color.
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
    // The band being extended and the last line, per color and span.
    let mut open: HashMap<(AnnotationColor, Span), Range<u32>> = HashMap::new();
    let mut lines: HashMap<(AnnotationColor, Span), u32> = HashMap::new();
    let mut next_label = f64::NEG_INFINITY;
    let mut spans = Vec::new();
    // By onset, so marks of each color and span come from left to right.
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
    // The bands still open come in the hash map's order, which changes with every map.
    marks
        .bands
        .sort_unstable_by_key(|m| (m.color, m.span, m.columns.start));
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

/// The annotation a click at pixel column `x` selects, as [`under`] finds them: the
/// shortest that can be edited (stim annotations can't), so events and short annotations
/// inside longer ones can be reached; of equally short ones, the first by onset.
pub fn pick(
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width_px: u32,
    x: f64,
    slack: u32,
    channel: Option<ChannelId>,
) -> Option<AnnotationId> {
    under(view, store, legend, width_px, x, slack, channel)
        .into_iter()
        .filter(|a| a.data.origin != Origin::Stim)
        .min_by(|a, b| a.data.duration.total_cmp(&b.data.duration))
        .map(|a| a.id)
}

/// The pixel columns `a`'s mark covers on a trace area `width_px` pixels wide, if it is in
/// `view`'s window.
pub fn extent(view: &ViewState, a: &AnnotationData, width_px: u32) -> Option<Range<u32>> {
    let time = view.time();
    (width_px > 0 && a.onset < time.end && a.end() >= time.start)
        .then(|| columns(view, a, width_px).0)
}

/// Whether `a`'s mark spans the trace area's height: if it applies to all channels, or is a
/// stim annotation. Those belong to their stim channel, but mark a moment for all channels, as
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
