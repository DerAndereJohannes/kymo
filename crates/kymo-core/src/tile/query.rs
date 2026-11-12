//! What to draw for a view, from the tile cache alone.

use std::ops::Range;
use std::sync::Arc;

use super::{ChannelBlock, Tile, TileCache, TileKey, View, max_level, placements_for_view};
use crate::Recording;

/// The answer to a view: what to draw now, what to compute, and what to show as loading.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Tiles to draw, each over part of its span. Sorted by grid, block and samples; the
    /// ranges of one block never overlap.
    pub draw: Vec<Placed>,
    /// Tiles the view wants that are not cached, each once, in the order of
    /// [`tiles_for_view`](super::tiles_for_view).
    pub missing: Vec<TileKey>,
    /// Parts of the view with no cached tile at all, not even a coarser one.
    pub loading: Vec<Loading>,
}

/// A cached tile and the samples of its grid to draw it over.
#[derive(Clone, Debug)]
pub struct Placed {
    pub tile: Arc<Tile>,
    /// Inside the tile's span and the grid. A coarser fallback tile covers only the span of
    /// the wanted tiles it stands in for.
    pub samples: Range<u64>,
    /// Whether the tile is at the level the view wants, not a coarser fallback.
    pub exact: bool,
}

/// Samples of one channel block that no cached tile covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loading {
    pub block: ChannelBlock,
    pub samples: Range<u64>,
}

/// Assembles `view` from the tiles in `cache`; never computes or reads anything else.
///
/// Every tile the view wants ([`placements_for_view`](super::placements_for_view)) is drawn
/// from the cache over its samples if present. Otherwise its nearest coarser ancestor in the
/// cache stands in, stretched over the same samples, and the tile is listed as missing.
/// Without any cached ancestor its samples are loading.
pub fn query(recording: &Recording, view: &View, cache: &TileCache) -> Query {
    let mut query = Query::default();
    for (key, samples) in placements_for_view(recording, view) {
        let grid = recording.grid(key.block.grid);
        let exact = cache.get(&key);
        if exact.is_none() && !query.missing.contains(&key) {
            query.missing.push(key);
        }
        let top = max_level(grid.len());
        let found = exact.map(|tile| (tile, true)).or_else(|| {
            let mut ancestor = key.parent();
            while let Some(k) = ancestor.filter(|k| k.level <= top) {
                if let Some(tile) = cache.get(&k) {
                    return Some((tile, false));
                }
                ancestor = k.parent();
            }
            None
        });

        match found {
            Some((tile, exact)) => {
                if let Some(last) = query.draw.last_mut()
                    && Arc::ptr_eq(&last.tile, &tile)
                    && last.samples.end == samples.start
                {
                    last.samples.end = samples.end;
                    continue;
                }
                query.draw.push(Placed {
                    tile,
                    samples,
                    exact,
                });
            }
            None => match query.loading.last_mut() {
                Some(last) if last.block == key.block && last.samples.end == samples.start => {
                    last.samples.end = samples.end;
                }
                _ => query.loading.push(Loading {
                    block: key.block,
                    samples,
                }),
            },
        }
    }
    query
}
