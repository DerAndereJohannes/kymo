//! The tile cache: byte budget, LRU eviction, and use from several threads.

use std::sync::Arc;

use kymo_core::GridId;
use kymo_core::testing::tile;
use kymo_core::tile::{ChannelBlock, Level, SignalVersion, Tile, TileCache, TileData, TileKey};
use proptest::prelude::*;

const KEYS: u64 = 12;

fn key(time: u64) -> TileKey {
    TileKey {
        version: SignalVersion::RAW,
        level: Level::RAW,
        block: ChannelBlock {
            grid: GridId(0),
            index: 0,
        },
        time,
    }
}

/// A one-channel level-0 tile of `len` samples.
fn make(time: u64, len: usize) -> Arc<Tile> {
    let data = TileData::I16(vec![0; len].into());
    let valid = if len > 0 { vec![0..len as u32] } else { vec![] };
    Arc::new(tile(key(time), 1, len, data, valid))
}

/// The reference: a list of tiles, least recently used first.
struct Model {
    budget: usize,
    items: Vec<Arc<Tile>>,
}

impl Model {
    fn position(&self, time: u64) -> Option<usize> {
        self.items.iter().position(|t| t.key() == key(time))
    }

    fn get(&mut self, time: u64) -> Option<Arc<Tile>> {
        let tile = self.items.remove(self.position(time)?);
        self.items.push(Arc::clone(&tile));
        Some(tile)
    }

    fn insert(&mut self, tile: Arc<Tile>) -> bool {
        if tile.bytes() > self.budget {
            return false;
        }
        self.remove(tile.key().time);
        while self.bytes() + tile.bytes() > self.budget {
            self.items.remove(0);
        }
        self.items.push(tile);
        true
    }

    fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        while self.bytes() > budget {
            self.items.remove(0);
        }
    }

    fn remove(&mut self, time: u64) -> bool {
        self.position(time).map(|p| self.items.remove(p)).is_some()
    }

    fn bytes(&self) -> usize {
        self.items.iter().map(|t| t.bytes()).sum()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Insert(u64, usize),
    Get(u64),
    Remove(u64),
    Clear,
    Budget(usize),
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        8 => (0..KEYS, 0..3000usize).prop_map(|(t, len)| Op::Insert(t, len)),
        8 => (0..KEYS).prop_map(Op::Get),
        2 => (0..KEYS).prop_map(Op::Remove),
        1 => Just(Op::Clear),
        1 => (0..25_000usize).prop_map(Op::Budget),
    ];
    proptest::collection::vec(op, 0..200)
}

proptest! {
    /// Random operation sequences, budget changes included, behave exactly like the
    /// reference LRU list, and the cache never holds more than its budget.
    #[test]
    fn cache_matches_reference_lru(budget in 0..25_000usize, ops in ops()) {
        let cache = TileCache::new(budget);
        let mut model = Model { budget, items: Vec::new() };
        for op in ops {
            match op {
                Op::Insert(t, len) => {
                    let tile = make(t, len);
                    prop_assert_eq!(cache.insert(Arc::clone(&tile)), model.insert(tile));
                }
                Op::Get(t) => {
                    let (got, want) = (cache.get(&key(t)), model.get(t));
                    prop_assert_eq!(got.is_some(), want.is_some());
                    if let (Some(got), Some(want)) = (got, want) {
                        prop_assert!(Arc::ptr_eq(&got, &want), "get returned another tile");
                    }
                }
                Op::Remove(t) => prop_assert_eq!(cache.remove(&key(t)), model.remove(t)),
                Op::Clear => {
                    cache.clear();
                    model.items.clear();
                }
                Op::Budget(budget) => {
                    cache.set_budget(budget);
                    model.set_budget(budget);
                }
            }
            prop_assert_eq!(cache.budget(), model.budget);
            prop_assert!(cache.bytes() <= model.budget);
            prop_assert_eq!(cache.bytes(), model.bytes());
            prop_assert_eq!(cache.len(), model.items.len());
            for t in 0..KEYS {
                prop_assert_eq!(cache.contains(&key(t)), model.position(t).is_some(), "key {}", t);
            }
        }
    }
}

#[test]
fn least_recently_used_goes_first() {
    let size = make(0, 100).bytes();
    let cache = TileCache::new(3 * size);
    for t in 0..3 {
        assert!(cache.insert(make(t, 100)));
    }
    assert!(cache.get(&key(0)).is_some());
    // `contains` is not a use: 1 stays the oldest.
    assert!(cache.contains(&key(1)));
    cache.insert(make(3, 100));
    assert!(!cache.contains(&key(1)));
    cache.insert(make(4, 100));
    assert!(!cache.contains(&key(2)));
    assert!(cache.contains(&key(0)) && cache.contains(&key(3)) && cache.contains(&key(4)));
    assert_eq!(cache.bytes(), 3 * size);
}

#[test]
fn replacing_a_tile_updates_the_size() {
    let cache = TileCache::new(1 << 20);
    cache.insert(make(0, 100));
    cache.insert(make(0, 1000));
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.bytes(), make(0, 1000).bytes());
    assert_eq!(cache.get(&key(0)).unwrap().len(), 1000);
}

#[test]
fn oversized_tile_is_refused_and_changes_nothing() {
    let small = make(0, 10);
    let cache = TileCache::new(small.bytes() + 100);
    assert!(cache.insert(Arc::clone(&small)));
    assert!(!cache.insert(make(0, 1000)));
    assert!(!cache.insert(make(1, 1000)));
    assert!(Arc::ptr_eq(&cache.get(&key(0)).unwrap(), &small));
    assert_eq!(cache.len(), 1);
}

#[test]
fn cache_is_shareable() {
    fn shareable<T: Send + Sync>() {}
    shareable::<TileCache>();
    shareable::<Arc<Tile>>();
}

/// Several threads insert, get and remove at once: the budget holds at every step, gets
/// return the requested tile, and the byte count matches the tiles left at the end.
#[test]
fn concurrent_use_stays_consistent() {
    let size = make(0, 500).bytes();
    let cache = Arc::new(TileCache::new(10 * size));
    let threads: Vec<_> = (0..8u64)
        .map(|seed| {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || {
                let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                for _ in 0..5000 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let t = (x >> 8) % 40;
                    match x % 8 {
                        0..=3 => {
                            cache.insert(make(t, 300 + (x >> 20) as usize % 400));
                        }
                        4..=6 => {
                            if let Some(tile) = cache.get(&key(t)) {
                                assert_eq!(tile.key(), key(t));
                            }
                        }
                        _ => {
                            cache.remove(&key(t));
                        }
                    }
                    assert!(cache.bytes() <= cache.budget());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let held: usize = (0..40)
        .filter_map(|t| cache.get(&key(t)))
        .map(|t| t.bytes())
        .sum();
    assert_eq!(cache.bytes(), held);
    assert!(cache.len() <= 40);
}
