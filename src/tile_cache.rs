//! Port of tile-cache.hpp.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::dirtiles::Zxy; // for zxy
use crate::mvt::MvtTile;

#[derive(Clone, Default)]
pub struct MvtTileSeq {
    pub tile: MvtTile,
    pub seq: usize,
}

pub struct TileCache {
    pub overzoom_cache: BTreeMap<Zxy, MvtTileSeq>,
    pub seq: AtomicUsize,
    pub capacity: usize,
}

impl Default for TileCache {
    fn default() -> Self {
        TileCache {
            overzoom_cache: BTreeMap::new(),
            seq: AtomicUsize::new(0),
            capacity: 1000,
        }
    }
}

impl TileCache {
    pub fn get(&mut self, parent_tile: Zxy, getter: &mut dyn FnMut(Zxy) -> MvtTile) -> MvtTile {
        let source;
        match self.overzoom_cache.get_mut(&parent_tile) {
            None => {
                if self.overzoom_cache.len() >= self.capacity {
                    // evict the oldest tile to make room

                    if let Some(mut to_erase) = self.overzoom_cache.iter().next() {
                        for here in self.overzoom_cache.iter() {
                            if here.1.seq < to_erase.1.seq {
                                to_erase = here;
                            }
                        }

                        let k = to_erase.0.clone();
                        self.overzoom_cache.remove(&k);
                    }
                }

                source = getter(parent_tile.clone());

                let to_cache = MvtTileSeq {
                    tile: source.clone(),
                    seq: self.seq.fetch_add(1, Ordering::Relaxed),
                };

                self.overzoom_cache.entry(parent_tile).or_insert(to_cache);
            }
            Some(f) => {
                f.seq = self.seq.fetch_add(1, Ordering::Relaxed);
                source = f.tile.clone();
            }
        }

        source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile_named(name: &str) -> MvtTile {
        MvtTile {
            layers: vec![crate::mvt::MvtLayer {
                name: name.to_string(),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut cache = TileCache {
            capacity: 2,
            ..Default::default()
        };
        let mut calls = Vec::new();
        let mut getter = |t: Zxy| {
            calls.push(t.x);
            tile_named(&t.x.to_string())
        };

        assert_eq!(
            cache.get(Zxy::new(1, 0, 0), &mut getter).layers[0].name,
            "0"
        );
        assert_eq!(
            cache.get(Zxy::new(1, 1, 0), &mut getter).layers[0].name,
            "1"
        );
        // touch 0 so that 1 is the oldest
        assert_eq!(
            cache.get(Zxy::new(1, 0, 0), &mut getter).layers[0].name,
            "0"
        );
        assert_eq!(
            cache.get(Zxy::new(1, 2, 0), &mut getter).layers[0].name,
            "2"
        );
        assert!(cache.overzoom_cache.contains_key(&Zxy::new(1, 0, 0)));
        assert!(!cache.overzoom_cache.contains_key(&Zxy::new(1, 1, 0)));
        assert_eq!(calls, vec![0, 1, 2]);
    }
}
