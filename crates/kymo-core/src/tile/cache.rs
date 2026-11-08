//! The in-memory tile cache: least recently used tiles go first once the byte budget is full.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Tile, TileKey};

/// Default byte budget.
pub const DEFAULT_BUDGET: usize = 256 << 20;

/// Tiles shared between workers (which insert) and the UI thread (which gets).
///
/// Its size never exceeds the budget, counted by [`Tile::bytes`]. Every call holds a short
/// internal lock and never does I/O.
#[derive(Debug)]
pub struct TileCache {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    budget: usize,
    entries: HashMap<TileKey, Entry>,
    /// Keys by last use, oldest first.
    order: BTreeMap<u64, TileKey>,
    tick: u64,
    bytes: usize,
}

#[derive(Debug)]
struct Entry {
    tile: Arc<Tile>,
    bytes: usize,
    used: u64,
}

impl TileCache {
    pub fn new(budget_bytes: usize) -> Self {
        TileCache {
            inner: Mutex::new(Inner {
                budget: budget_bytes,
                ..Inner::default()
            }),
        }
    }

    pub fn budget(&self) -> usize {
        self.lock().budget
    }

    /// Changes the budget, evicting the least recently used tiles until the rest fit.
    pub fn set_budget(&self, budget_bytes: usize) {
        let mut inner = self.lock();
        inner.budget = budget_bytes;
        inner.evict(0);
    }

    /// The cached tile `key`, now the most recently used.
    pub fn get(&self, key: &TileKey) -> Option<Arc<Tile>> {
        let mut inner = self.lock();
        let Inner {
            entries,
            order,
            tick,
            ..
        } = &mut *inner;
        let entry = entries.get_mut(key)?;
        order.remove(&entry.used);
        *tick += 1;
        entry.used = *tick;
        order.insert(*tick, *key);
        Some(Arc::clone(&entry.tile))
    }

    /// Whether `key` is cached, without counting as a use.
    pub fn contains(&self, key: &TileKey) -> bool {
        self.lock().entries.contains_key(key)
    }

    /// Caches `tile` under its key as the most recently used, replacing any tile with the
    /// same key and evicting the least recently used ones until it fits.
    ///
    /// Returns `false`, and leaves the cache unchanged, if the tile alone exceeds the budget.
    pub fn insert(&self, tile: Arc<Tile>) -> bool {
        let bytes = tile.bytes();
        let key = tile.key();
        let mut inner = self.lock();
        if bytes > inner.budget {
            return false;
        }
        inner.remove(&key);
        inner.evict(bytes);
        inner.tick += 1;
        let used = inner.tick;
        inner.order.insert(used, key);
        inner.entries.insert(key, Entry { tile, bytes, used });
        inner.bytes += bytes;
        true
    }

    /// Removes tile `key`; returns whether it was cached.
    pub fn remove(&self, key: &TileKey) -> bool {
        self.lock().remove(key)
    }

    /// Removes every tile.
    pub fn clear(&self) {
        let mut inner = self.lock();
        *inner = Inner {
            budget: inner.budget,
            ..Inner::default()
        };
    }

    /// Bytes held by the cached tiles, by [`Tile::bytes`]; never above the budget.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Number of cached tiles.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        crate::lock(&self.inner)
    }
}

impl Default for TileCache {
    fn default() -> Self {
        TileCache::new(DEFAULT_BUDGET)
    }
}

impl Inner {
    /// Evicts the least recently used tiles until `room` more bytes fit in the budget.
    fn evict(&mut self, room: usize) {
        while self.bytes + room > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
    }

    fn remove(&mut self, key: &TileKey) -> bool {
        match self.entries.remove(key) {
            Some(entry) => {
                self.order.remove(&entry.used);
                self.bytes -= entry.bytes;
                true
            }
            None => false,
        }
    }
}
