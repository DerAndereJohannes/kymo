//! The annotation legend and the overlay's marks, checked against brute force over the
//! annotations.

use std::collections::{BTreeSet, HashMap};

use kymo::annotations::AnnotationColor::{self, Palette};
use kymo::annotations::{COLORS, Legend, Mark, Marks, Span, marks, span_y, under};
use kymo::view::ViewState;
use kymo_core::testing::{MemData, single_grid};
use kymo_core::{
    Annotation, AnnotationData, AnnotationOp, AnnotationStore, ChannelId, Origin, Recording,
};
use proptest::prelude::*;

/// 8 channels at 100 Hz over 600 s.
fn recording() -> Recording {
    let data = MemData::I16(vec![vec![0; 60_000]; 8]);
    Recording::new(single_grid(100.0, data)).unwrap().0
}

fn data(onset: f64, duration: f64, description: &str, channels: &[u32]) -> AnnotationData {
    AnnotationData {
        onset,
        duration,
        description: description.into(),
        channels: channels.iter().copied().map(ChannelId).collect(),
        origin: Origin::File,
    }
}

fn insert(store: &mut AnnotationStore, data: AnnotationData) {
    let id = store.new_id();
    store
        .apply(AnnotationOp::Insert(Annotation { id, data }))
        .unwrap();
}

fn colors(legend: &Legend) -> Vec<(String, usize, usize)> {
    legend
        .entries()
        .map(|e| (e.description.clone(), e.slot, e.count))
        .collect()
}

#[test]
fn colors_follow_first_appearance_and_never_change() {
    let mut store = AnnotationStore::from_data(vec![
        data(5.0, 0.0, "b", &[]),
        data(1.0, 2.0, "a", &[]),
        data(9.0, 0.0, "b", &[]),
    ]);
    let mut legend = Legend::new(&store);
    let own = |d: &str, c: usize, n: usize| (d.to_owned(), c, n);
    assert_eq!(colors(&legend), [own("a", 0, 1), own("b", 1, 2)]);

    // New descriptions take the next colors, even when they come earlier in time; the
    // known ones keep theirs, and their shown state.
    legend.set_shown("b", false);
    insert(&mut store, data(0.5, 0.0, "c", &[]));
    insert(&mut store, data(0.2, 0.0, "d", &[]));
    insert(&mut store, data(3.0, 0.0, "a", &[]));
    legend.sync(&store);
    assert_eq!(
        colors(&legend),
        [
            own("a", 0, 2),
            own("b", 1, 2),
            own("d", 2, 1),
            own("c", 3, 1)
        ]
    );
    assert!(!legend.entry("b").unwrap().shown);
    assert!(legend.entry("d").unwrap().shown);

    // A description whose annotations are all gone leaves the legend and keeps its color.
    let ids: Vec<_> = store
        .iter()
        .filter(|a| a.data.description == "a")
        .map(|a| a.id)
        .collect();
    for id in ids {
        store.apply(AnnotationOp::Remove(id)).unwrap();
    }
    legend.sync(&store);
    assert_eq!(
        colors(&legend),
        [own("b", 1, 2), own("d", 2, 1), own("c", 3, 1)]
    );
    insert(&mut store, data(7.0, 0.0, "a", &[]));
    legend.sync(&store);
    assert_eq!(legend.entry("a").unwrap().slot, 0);

    // Colors cycle: a, b, d and c take 0 to 3, x0 to x5 take 4 to 9, x6 takes 0 again.
    for k in 0..COLORS {
        insert(
            &mut store,
            data(20.0 + k as f64, 0.0, &format!("x{k}"), &[]),
        );
    }
    legend.sync(&store);
    assert_eq!(legend.entry("x6").unwrap().slot, 0);
}

#[test]
fn descriptions_show_and_hide() {
    let store = AnnotationStore::from_data(vec![
        data(1.0, 0.0, "a", &[]),
        data(2.0, 0.0, "b", &[]),
        data(3.0, 0.0, "c", &[]),
    ]);
    let mut legend = Legend::new(&store);
    assert_eq!(legend.shown_count(), (3, 3));
    legend.set_shown("b", false);
    assert_eq!(legend.shown_count(), (2, 3));
    legend.set_all_shown(false);
    assert_eq!(legend.shown_count(), (0, 3));
    legend.set_all_shown(true);
    assert_eq!(legend.shown_count(), (3, 3));
    assert!(!legend.is_empty());
    assert!(Legend::new(&AnnotationStore::default()).is_empty());
}

/// Columns of each color and span, from bands and lines separately.
type Coverage = HashMap<(AnnotationColor, Span), (BTreeSet<u32>, BTreeSet<u32>)>;

/// What `marks` must cover, from every annotation in turn.
fn brute_force(view: &ViewState, store: &AnnotationStore, legend: &Legend, width: u32) -> Coverage {
    let mut cover = Coverage::new();
    let t = view.time();
    let w = f64::from(width);
    for a in store.iter() {
        let d = &a.data;
        let entry = legend.entry(&d.description).unwrap();
        if !entry.shown || d.onset >= t.end || d.end() < t.start {
            continue;
        }
        // Stim annotations mark a moment for all channels.
        let spans: Vec<Span> = if d.channels.is_empty() || d.origin == Origin::Stim {
            vec![Span::All]
        } else {
            d.channels
                .iter()
                .filter_map(|&c| view.row_of(c))
                .filter(|r| view.visible_rows().contains(r))
                .map(Span::Row)
                .collect()
        };
        let x0 = view.x_at(d.onset, w);
        let x1 = view.x_at(d.end(), w);
        let first = x0.floor().clamp(0.0, w - 1.0) as u32;
        for span in spans {
            let (bands, lines) = cover.entry((entry.color, span)).or_default();
            if x1 - x0 < 1.0 {
                // Shorter than a pixel: the column of its start.
                lines.insert(first);
            } else {
                // Every column it touches.
                let end = (x1.ceil().min(w) as u32).max(first + 1);
                bands.extend(first..end);
            }
        }
    }
    cover
}

fn check(marks: &Marks, cover: &Coverage, width: u32) -> Result<(), TestCaseError> {
    let mut got = Coverage::new();
    let mut by_key: HashMap<(AnnotationColor, Span), Vec<&Mark>> = HashMap::new();
    for band in &marks.bands {
        prop_assert!(
            !band.columns.is_empty() && band.columns.end <= width,
            "{band:?}"
        );
        by_key
            .entry((band.color, band.span))
            .or_default()
            .push(band);
        got.entry((band.color, band.span))
            .or_default()
            .0
            .extend(band.columns.clone());
    }
    // Bands of a color and span neither overlap nor touch.
    for bands in by_key.values_mut() {
        bands.sort_by_key(|b| b.columns.start);
        for pair in bands.windows(2) {
            prop_assert!(pair[0].columns.end < pair[1].columns.start, "{pair:?}");
        }
    }
    for line in &marks.lines {
        prop_assert_eq!(line.columns.len(), 1);
        prop_assert!(line.columns.end <= width);
        let lines = &mut got.entry((line.color, line.span)).or_default().1;
        prop_assert!(lines.insert(line.columns.start), "two lines at {line:?}");
    }
    got.retain(|_, (b, l)| !b.is_empty() || !l.is_empty());
    let mut want = cover.clone();
    want.retain(|_, (b, l)| !b.is_empty() || !l.is_empty());
    prop_assert_eq!(got, want);
    Ok(())
}

fn label_width(text: &str) -> f64 {
    7.0 * text.chars().count() as f64 + 5.0
}

/// Labels don't overlap, come from shown annotations in view, and one is skipped only where
/// the label before it takes the room.
fn check_labels(
    marks: &Marks,
    view: &ViewState,
    store: &AnnotationStore,
    legend: &Legend,
    width: u32,
) -> Result<(), TestCaseError> {
    let w = f64::from(width);
    let t = view.time();
    // Shown annotations in view on a visible row, by onset.
    let candidates: Vec<(f64, &str, AnnotationColor)> = store
        .iter()
        .filter(|a| a.data.onset < t.end && a.data.end() >= t.start)
        .filter(|a| legend.entry(&a.data.description).unwrap().shown)
        .filter(|a| {
            a.data.channels.is_empty()
                || a.data.origin == Origin::Stim
                || a.data.channels.iter().any(|&c| {
                    view.row_of(c)
                        .is_some_and(|r| view.visible_rows().contains(&r))
                })
        })
        .map(|a| {
            let x = view.x_at(a.data.onset, w).max(0.0);
            let color = legend.entry(&a.data.description).unwrap().color;
            (x, a.data.description.as_str(), color)
        })
        .collect();
    let mut labels = marks.labels.iter().peekable();
    let mut room_until = f64::NEG_INFINITY;
    for (x, text, color) in candidates {
        match labels.peek() {
            Some(l) if l.x == x && l.text == text => {
                prop_assert!(x >= room_until, "{l:?} overlaps the label before it");
                prop_assert_eq!(l.color, color);
                room_until = x + label_width(text);
                labels.next();
            }
            _ => prop_assert!(x < room_until, "no label for {text} at {x}"),
        }
    }
    prop_assert!(labels.next().is_none(), "labels left over");
    Ok(())
}

/// A random annotation: onset in the recording, an event, a sliver or a band, one of 14
/// descriptions (so colors repeat), for all channels or a few, or a stim annotation of a
/// channel.
fn annotation() -> impl Strategy<Value = AnnotationData> {
    (
        0.0..600.0f64,
        prop_oneof![Just(0.0), 0.0..0.01f64, 0.0..30.0f64],
        0..14usize,
        prop_oneof![
            2 => Just(vec![]),
            1 => proptest::collection::vec(0..8u32, 1..3),
        ],
        0..8u32,
        prop_oneof![3 => Just(false), 1 => Just(true)],
    )
        .prop_map(|(onset, duration, d, channels, stim_channel, stim)| {
            if stim {
                AnnotationData {
                    origin: Origin::Stim,
                    ..data(onset, 0.0, &format!("d{d}"), &[stim_channel])
                }
            } else {
                data(onset, duration, &format!("d{d}"), &channels)
            }
        })
}

/// A view moved, zoomed and scrolled at random, with some channels hidden.
fn view(rec: &Recording) -> impl Strategy<Value = ViewState> + use<> {
    let rec_view = ViewState::new(rec);
    (
        0.0..600.0f64,
        prop_oneof![Just(1.0), 0.001..1.0f64, 1.0..60.0f64],
        0.0..8.0f64,
        1..8usize,
        proptest::collection::vec(any::<bool>(), 8),
    )
        .prop_map(move |(start, zoom, top, rows, hidden)| {
            let mut view = rec_view.clone();
            for (c, hide) in hidden.into_iter().enumerate() {
                if hide {
                    view.set_shown(ChannelId(c as u32), false);
                }
            }
            view.zoom(zoom, 0.0);
            view.pan(start - view.start());
            view.set_rows(rows);
            view.set_top(top);
            view
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every pixel column a shown annotation touches holds a mark of its color over its
    /// rows, and no other column does; marks of a color and span never overlap.
    #[test]
    fn marks_cover_exactly_the_annotations_in_view(
        view in view(&recording()),
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..700u32,
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store);
        for d in hidden {
            legend.set_shown(&format!("d{d}"), false);
        }
        let marks = marks(&view, &store, &legend, width, label_width);
        check(&marks, &brute_force(&view, &store, &legend, width), width)?;
        check_labels(&marks, &view, &store, &legend, width)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The annotations under the pointer are exactly the shown ones with a mark within
    /// `slack` columns of it, over the pointer's row or the whole height, by onset.
    #[test]
    fn the_pointer_finds_the_marks_under_it(
        view in view(&recording()),
        annotations in proptest::collection::vec(annotation(), 0..200),
        hidden in proptest::collection::vec(0..14usize, 0..4),
        width in 1..700u32,
        x in 0.0..1.0f64,
        slack in 0..4u32,
        channel in 0..9u32,
    ) {
        let store = AnnotationStore::from_data(annotations);
        let mut legend = Legend::new(&store);
        for d in hidden {
            legend.set_shown(&format!("d{d}"), false);
        }
        let x = x * f64::from(width);
        // A channel on screen, as under the pointer, or none.
        let channel = Some(ChannelId(channel)).filter(|&c| {
            view.row_of(c).is_some_and(|r| view.visible_rows().contains(&r))
        });
        let at = (x.floor() as u32).min(width - 1);
        let near = at.saturating_sub(slack)..at + 1 + slack;
        let want: Vec<_> = store
            .iter()
            .filter(|a| {
                let span = if a.data.channels.is_empty() || a.data.origin == Origin::Stim {
                    Span::All
                } else if let Some(c) = channel.filter(|c| a.data.channels.contains(c)) {
                    Span::Row(view.row_of(c).unwrap())
                } else {
                    return false;
                };
                let alone = AnnotationStore::from_data(vec![a.data.clone()]);
                let cover = brute_force(&view, &alone, &legend, width);
                cover.into_iter().any(|((_, s), (bands, lines))| {
                    s == span && bands.union(&lines).any(|c| near.contains(c))
                })
            })
            .map(|a| a.id)
            .collect();
        let got: Vec<_> = under(&view, &store, &legend, width, x, slack, channel)
            .into_iter()
            .map(|a| a.id)
            .collect();
        prop_assert_eq!(got, want);
    }
}

/// However many annotations there are, there are at most a few marks per pixel column.
#[test]
fn thousands_of_events_thin_to_the_screen() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.zoom(view.span() / 600.0, 0.0);
    assert_eq!(view.time(), 0.0..600.0);
    let annotations = (0..20_000)
        .map(|k| data(k as f64 * 0.03, 0.0, &format!("event {k}"), &[]))
        .collect();
    let store = AnnotationStore::from_data(annotations);
    let legend = Legend::new(&store);
    let marks = marks(&view, &store, &legend, 400, label_width);
    assert!(marks.bands.is_empty());
    assert!(marks.lines.len() <= 400 * COLORS, "{}", marks.lines.len());
    assert!(marks.lines.len() >= 400);
    assert!(marks.labels.len() <= 400);
}

/// Events tied to channels mark only those rows; one whose channels are all off screen draws
/// nothing.
#[test]
fn channel_annotations_cover_their_rows() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.set_rows(4);
    view.set_shown(ChannelId(1), false);
    // Rows: 0, 2, 3, 4 on screen; 5, 6, 7 below.
    let store = AnnotationStore::from_data(vec![
        data(1.0, 2.0, "a", &[2, 4]),
        data(1.0, 2.0, "b", &[1, 6]),
        data(5.0, 0.0, "c", &[0]),
    ]);
    let legend = Legend::new(&store);
    let marks = marks(&view, &store, &legend, 200, label_width);
    let spans = |marks: &[Mark]| -> Vec<(AnnotationColor, Span)> {
        let mut s: Vec<_> = marks.iter().map(|m| (m.color, m.span)).collect();
        s.sort();
        s
    };
    assert_eq!(
        spans(&marks.bands),
        [(Palette(0), Span::Row(1)), (Palette(0), Span::Row(3))]
    );
    assert_eq!(spans(&marks.lines), [(Palette(2), Span::Row(0))]);
    let texts: Vec<_> = marks.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["a", "c"]);

    // A row's span is that row, a quarter of the height; all channels span all of it.
    let height = 400.0;
    for (row, channel) in [(0, 0), (1, 2), (3, 4)] {
        let y = span_y(&view, Span::Row(row), height);
        assert!((y.end - y.start - 100.0).abs() < 1e-9, "{y:?}");
        for at in [y.start + 0.5, (y.start + y.end) / 2.0, y.end - 0.5] {
            assert_eq!(view.channel_at(at, height), Some(ChannelId(channel)));
        }
    }
    assert_eq!(span_y(&view, Span::All, height), 0.0..height);
}

/// A stim annotation belongs to its stim channel but marks all rows, even with that channel off
/// screen or hidden, and the pointer finds it on any of them.
#[test]
fn stim_annotations_mark_every_row() {
    let rec = recording();
    let mut view = ViewState::new(&rec);
    view.set_rows(4);
    view.set_shown(ChannelId(1), false);
    let stim = |onset: f64, channel: u32| AnnotationData {
        origin: Origin::Stim,
        ..data(onset, 0.0, "7", &[channel])
    };
    let store = AnnotationStore::from_data(vec![stim(5.0, 6), stim(10.0, 1)]);
    let legend = Legend::new(&store);
    let marks = marks(&view, &store, &legend, 200, label_width);
    let lines: Vec<_> = marks
        .lines
        .iter()
        .map(|m| (m.columns.start, m.span))
        .collect();
    assert_eq!(lines, [(50, Span::All), (100, Span::All)]);
    assert_eq!(marks.labels.len(), 2);
    for channel in [None, Some(ChannelId(0)), Some(ChannelId(4))] {
        let found = under(&view, &store, &legend, 200, 100.5, 0, channel);
        assert_eq!(found.len(), 1, "{channel:?}");
        assert_eq!(found[0].data.onset, 10.0);
    }
}
