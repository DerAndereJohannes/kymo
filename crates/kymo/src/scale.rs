//! Automatic amplitude scale: per channel type, the amplitude at which what the window
//! draws fits the rows.
//!
//! What a channel draws in a pixel column spans the lowest to the highest of its samples
//! there (or of its buckets, when zoomed out), each counted in the column it starts in.
//! The channel's center is the median of its columns' midpoints, so a DC offset doesn't
//! matter, DC removal on or off. Every column deviates from it by its farther end, above
//! or below alike. A type's amplitude is the [`PERCENTILE`] of the deviations over its
//! good channels on screen: as ±amplitude spans a row, that much of what they draw stays
//! in their rows, and a single artifact doesn't shrink the rest. Working per column keeps
//! the cost to the screen's rows times its width, however many samples a column holds.

use std::collections::HashMap;

use kymo_core::tile::{Level, Query, Tile, TileData, samples_in};
use kymo_core::{ChannelId, ChannelKind, Recording};

use crate::view::{KINDS, ViewState, kind_index};

/// The share of a type's deviations that stays within the rows.
pub const PERCENTILE: f64 = 0.99;

/// Per type in [`KINDS`] order, the amplitude per channel spacing (SI units) that fits what
/// `query` draws of the window of `view` in a trace area `width_px` pixels wide; `None`
/// for a type without a good, non-flat channel on screen, and always for stim channels,
/// whose codes keep their scale.
pub fn fit(
    recording: &Recording,
    view: &ViewState,
    query: &Query,
    width_px: u32,
) -> [Option<f64>; KINDS.len()] {
    let wanted: HashMap<ChannelId, usize> = view
        .visible_channels()
        .iter()
        .filter(|&&c| !view.is_bad(c) && view.kind_of(c) != ChannelKind::Stim)
        .map(|&c| (c, kind_index(view.kind_of(c))))
        .collect();
    let time = view.time();
    let width = width_px.max(1) as usize;
    let column = |t: f64| {
        let x = (t - time.start) / (time.end - time.start) * width as f64;
        (x.floor().max(0.0) as usize).min(width - 1)
    };

    // Per channel and pixel column, the lowest and highest value drawn there, in SI.
    let mut drawn: HashMap<ChannelId, Vec<(f64, f64)>> = HashMap::new();
    for placed in &query.draw {
        let tile = &placed.tile;
        let key = tile.key();
        let grid = recording.grid(key.block.grid);
        let visible = samples_in(grid, &time);
        let start = placed.samples.start.max(visible.start);
        let end = placed.samples.end.min(visible.end);
        if start >= end {
            continue;
        }
        let first_sample = key.sample_range().start;
        let bucket = key.level.bucket_len();
        let items = (start - first_sample) / bucket..(end - first_sample).div_ceil(bucket);
        for (row, &channel) in key.block.channels(grid).iter().enumerate() {
            if !wanted.contains_key(&channel) {
                continue;
            }
            let (gain, offset) = recording.channel(channel).tile_scaling();
            let to_si = view.to_si(channel);
            let si = |native: f64| (native * gain + offset) * to_si;
            let columns = drawn
                .entry(channel)
                .or_insert_with(|| vec![(f64::INFINITY, f64::NEG_INFINITY); width]);
            for valid in tile.valid() {
                let lo = u64::from(valid.start).max(items.start);
                let hi = u64::from(valid.end).min(items.end);
                for item in lo..hi {
                    // NaN drops out here: `min` and `max` return the other value.
                    let (a, b) = extremes(tile, row, item as usize);
                    let (a, b) = (si(a), si(b));
                    let x = column(grid.time_of(first_sample + item * bucket));
                    let (low, high) = &mut columns[x];
                    *low = low.min(a.min(b));
                    *high = high.max(a.max(b));
                }
            }
        }
    }

    let mut deviations: [Vec<f64>; KINDS.len()] = Default::default();
    for (channel, mut items) in drawn {
        items.retain(|&(lo, hi)| lo <= hi);
        let mut midpoints: Vec<f64> = items.iter().map(|&(lo, hi)| (lo + hi) / 2.0).collect();
        let Some(center) = quantile(&mut midpoints, 0.5) else {
            continue;
        };
        let own = items.iter().map(|&(lo, hi)| (hi - center).max(center - lo));
        let start = deviations[wanted[&channel]].len();
        let all = &mut deviations[wanted[&channel]];
        all.extend(own);
        // A flat channel says nothing about the scale.
        if all[start..].iter().all(|&d| d == 0.0) {
            all.truncate(start);
        }
    }
    deviations.map(|mut d| quantile(&mut d, PERCENTILE).filter(|a| a.is_finite() && *a > 0.0))
}

/// The low and high end of item `item` of row `row` in native units: a sample twice at
/// level 0, a bucket's minimum and maximum above.
fn extremes(tile: &Tile, row: usize, item: usize) -> (f64, f64) {
    let at = |index: usize| -> f64 {
        match tile.data() {
            TileData::I16(d) => f64::from(d[index]),
            TileData::I32(d) => f64::from(d[index]),
            TileData::F32(d) => f64::from(d[index]),
        }
    };
    let index = row * tile.len() + item;
    if tile.key().level == Level::RAW {
        let v = at(index);
        (v, v)
    } else {
        (at(2 * index), at(2 * index + 1))
    }
}

/// The value at quantile `q` (0 to 1) of `values`, nearest rank; reorders them.
fn quantile(values: &mut [f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let rank = ((values.len() - 1) as f64 * q).round() as usize;
    Some(*values.select_nth_unstable_by(rank, f64::total_cmp).1)
}
