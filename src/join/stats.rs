//! What tile-join collects about the features it writes, for the metadata:
//! each layer's zoom range, feature counts and tile-stats, and the bounds.
//! Each thread collects its own, and they are merged at the end, which gives
//! the same result in any order, as in tile-join, whose threads do the same.

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use tippecanoe::mbtiles::{LayermapEntry, Tilestat};
use tippecanoe::mvt::{MVT_DOUBLE, MVT_NULL};
use tippecanoe::projection::tile2lonlat;
use tippecanoe::serial::SerialVal;

/// `std::min` on doubles: `(b < a) ? b : a`.
pub fn std_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// `std::max` on doubles: `(a < b) ? b : a`.
pub fn std_max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

#[derive(Clone, Debug, Default)]
pub struct LayerStats {
    pub minzoom: i32,
    pub maxzoom: i32,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
    pub attributes: FxHashMap<String, Tilestat>,
}

impl LayerStats {
    pub fn zoom(&mut self, z: i32) {
        if z < self.minzoom {
            self.minzoom = z;
        }
        if z > self.maxzoom {
            self.maxzoom = z;
        }
    }

    /// `add_to_tilestats` with the type and text of the value's `serial_val`,
    /// keeping the `max_samples` smallest values.
    pub fn add(&mut self, key: &str, type_: i32, s: &str, max_samples: usize) {
        if type_ == MVT_NULL {
            return;
        }
        let tsa = match self.attributes.get_mut(key) {
            Some(t) => t,
            None => self.attributes.entry(key.to_string()).or_default(),
        };

        if type_ == MVT_DOUBLE {
            let d = tippecanoe::cstd::atof(s);
            if d < tsa.min {
                tsa.min = d;
            }
            if d > tsa.max {
                tsa.max = d;
            }
        }

        tsa.type_ |= 1 << type_;

        // serial_val's order: by text, then by type. A full list keeps the
        // smallest values, so a value past its last is left out, and most
        // values are once the list is full.
        if tsa.sample_values.len() >= max_samples
            && tsa
                .sample_values
                .last()
                .is_some_and(|last| (last.s.as_str(), last.type_) <= (s, type_))
        {
            return;
        }
        let less = |v: &SerialVal| v.s.as_str() < s || (v.s == s && v.type_ < type_);
        let pt = tsa.sample_values.partition_point(less);
        let found = pt < tsa.sample_values.len()
            && tsa.sample_values[pt].s == s
            && tsa.sample_values[pt].type_ == type_;
        if !found {
            if tsa.sample_values.len() >= max_samples {
                if pt != tsa.sample_values.len() {
                    // bump the former last value, insert this one
                    tsa.sample_values.insert(
                        pt,
                        SerialVal {
                            type_,
                            s: s.to_string(),
                        },
                    );
                    tsa.sample_values.pop();
                }
            } else {
                tsa.sample_values.insert(
                    pt,
                    SerialVal {
                        type_,
                        s: s.to_string(),
                    },
                );
            }
        }
    }
}

/// One thread's share of the metadata.
#[derive(Clone, Debug)]
pub struct Accum {
    pub max_samples: usize,
    names: FxHashMap<String, usize>,
    pub layers: Vec<(String, LayerStats)>,
    pub minlat: f64,
    pub minlon: f64,
    pub maxlat: f64,
    pub maxlon: f64,
    pub minlon2: f64,
    pub maxlon2: f64,
}

impl Accum {
    pub fn new(max_samples: usize) -> Accum {
        Accum {
            max_samples,
            names: FxHashMap::default(),
            layers: Vec::new(),
            minlat: i32::MAX as f64,
            minlon: i32::MAX as f64,
            maxlat: i32::MIN as f64,
            maxlon: i32::MIN as f64,
            minlon2: i32::MAX as f64,
            maxlon2: i32::MIN as f64,
        }
    }

    /// The number of layer `name`, which starts at zoom `z` when it is new.
    pub fn layer(&mut self, name: &str, z: i32) -> usize {
        if let Some(&i) = self.names.get(name) {
            return i;
        }
        let i = self.layers.len();
        self.layers.push((
            name.to_string(),
            LayerStats {
                minzoom: z,
                maxzoom: z,
                ..Default::default()
            },
        ));
        self.names.insert(name.to_string(), i);
        i
    }

    /// Takes in the world coordinates of what an input layer added to a tile.
    pub fn bounds(&mut self, minx: i64, miny: i64, maxx: i64, maxy: i64, minx2: i64, maxx2: i64) {
        let (lon1, lat1) = tile2lonlat(minx, maxy, 32);
        let (lon2, lat2) = tile2lonlat(maxx, miny, 32);

        self.minlat = std_min(self.minlat, std_min(lat1, lat2));
        self.minlon = std_min(self.minlon, std_min(lon1, lon2));
        self.maxlat = std_max(self.maxlat, std_max(lat1, lat2));
        self.maxlon = std_max(self.maxlon, std_max(lon1, lon2));

        let (lon1, _) = tile2lonlat(minx2, maxy, 32);
        let (lon2, _) = tile2lonlat(maxx2, miny, 32);

        self.minlon2 = std_min(self.minlon2, std_min(lon1, lon2));
        self.maxlon2 = std_max(self.maxlon2, std_max(lon1, lon2));
    }

    /// The layers as tippecanoe-rs's layer map, for `merge_layermaps`.
    pub fn layermap(&self) -> BTreeMap<String, LayermapEntry> {
        let mut m = BTreeMap::new();
        for (name, l) in &self.layers {
            let n = m.len();
            m.insert(
                name.clone(),
                LayermapEntry {
                    id: n,
                    tilestats: l
                        .attributes
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    minzoom: l.minzoom,
                    maxzoom: l.maxzoom,
                    description: String::new(),
                    points: l.points,
                    lines: l.lines,
                    polygons: l.polygons,
                    retain: 0,
                },
            );
        }
        m
    }

    /// The bounds of both, as tile-join combines its threads' bounds.
    pub fn merge_bounds(&mut self, o: &Accum) {
        self.minlat = std_min(self.minlat, o.minlat);
        self.minlon = std_min(self.minlon, o.minlon);
        self.maxlat = std_max(self.maxlat, o.maxlat);
        self.maxlon = std_max(self.maxlon, o.maxlon);
        self.minlon2 = std_min(self.minlon2, o.minlon2);
        self.maxlon2 = std_max(self.maxlon2, o.maxlon2);
    }
}
