//! The tiler against tippecanoe-rs, on generated features and options.

use std::collections::{BTreeMap, HashMap};

use tippecanoe::attribute::AttributeOp;
use tippecanoe::geometry::{Draw, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON};
use tippecanoe::globals::OrderField;
use tippecanoe::mbtiles::LayermapEntry;
use tippecanoe::mvt::{MVT_DOUBLE, MVT_NULL, MVT_STRING};
use tippecanoe::options::*;
use tippecanoe::tile::Strategy;

use super::oracle::{
    GLOBALS, MemorySink, input_layermap, reference, serialized_layout, set_globals,
};
use super::util::shift_right;
use super::{TileOptions, tile_with};
use crate::store::{Attr, Feature, FeatureStore, Layer};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Builds a store the way ingest would, from world coordinates.
struct Builder {
    store: FeatureStore,
    gs: i32,
    keys: HashMap<String, u32>,
    values: HashMap<(i32, String), u32>,
    layers: HashMap<String, u32>,
}

impl Builder {
    fn new(gs: i32) -> Builder {
        Builder {
            store: FeatureStore {
                geometry_scale: gs,
                ..FeatureStore::default()
            },
            gs,
            keys: HashMap::new(),
            values: HashMap::new(),
            layers: HashMap::new(),
        }
    }

    fn q(&self, v: i64) -> i64 {
        shift_right(v, self.gs) * (1i64 << self.gs)
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        layer: &str,
        t: i32,
        geom: &[Draw],
        attrs: &[(&str, i32, &str)],
        feature_minzoom: i8,
        id: Option<u64>,
        extent: i64,
    ) {
        let li = match self.layers.get(layer) {
            Some(&i) => i,
            None => {
                let i = self.store.layers.len() as u32;
                self.store.layers.push(Layer {
                    name: layer.to_string(),
                    entry: LayermapEntry::new(i as usize),
                });
                self.layers.insert(layer.to_string(), i);
                i
            }
        };
        {
            let e = &mut self.store.layers[li as usize].entry;
            match t {
                VT_POINT => e.points += 1,
                VT_LINE => e.lines += 1,
                _ => e.polygons += 1,
            }
        }

        let mut bbox = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        let mut quantized = Vec::with_capacity(geom.len());
        for d in geom {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                let x = self.q(d.x);
                let y = self.q(d.y);
                bbox = [
                    bbox[0].min(x),
                    bbox[1].min(y),
                    bbox[2].max(x),
                    bbox[3].max(y),
                ];
                quantized.push(Draw::new(d.op, x, y));
            } else {
                quantized.push(Draw::new(d.op, 0, 0));
            }
        }
        let geom_start = self.store.push_geometry(&quantized);

        let attr_start = self.store.attrs.len() as u64;
        for &(k, ty, v) in attrs {
            let key = match self.keys.get(k) {
                Some(&i) => i,
                None => {
                    let i = self.store.keys.len() as u32;
                    self.store.keys.push(k.to_string());
                    self.keys.insert(k.to_string(), i);
                    i
                }
            };
            let value = match self.values.get(&(ty, v.to_string())) {
                Some(&i) => i,
                None => {
                    let i = self.store.values.len() as u32;
                    self.store.values.push(ty, v);
                    self.values.insert((ty, v.to_string()), i);
                    i
                }
            };
            self.store.attrs.push(Attr { key, value });
        }

        let cx = ((bbox[0] + bbox[2]) / 2).clamp(0, u32::MAX as i64) as u32;
        let cy = ((bbox[1] + bbox[3]) / 2).clamp(0, u32::MAX as i64) as u32;
        let index = tippecanoe::projection::encode_index(cx, cy);

        let seq = self.store.features.len() as u64;
        self.store.features.push(Feature {
            keep_index: true,
            ix: index,
            seq,
            label_point: index,
            extent,
            id: id.unwrap_or(0),
            geom_start,
            geom_len: geom.len() as u32,
            attr_start,
            attr_len: attrs.len() as u32,
            layer: li,
            t: t as i8,
            has_id: id.is_some(),
            feature_minzoom,
            tippecanoe_minzoom: -1,
            tippecanoe_maxzoom: -1,
            segment: 0,
        });
    }

    fn finish(mut self) -> FeatureStore {
        self.store.features.sort_by_key(|f| (f.index(), f.seq));
        self.store
    }
}

fn world(lon: f64, lat: f64) -> (i64, i64) {
    tippecanoe::projection::lonlat2tile(lon, lat, 32)
}

/// A minimum zoom from a drop rate of about 2.5, as `calc_feature_minzoom` would give.
fn minzoom(rng: &mut Rng, maxzoom: i32) -> i8 {
    let mut z = 0;
    while z < maxzoom && rng.f() > 0.4 {
        z += 1;
    }
    z as i8
}

const KINDS: [&str; 5] = ["a", "b", "c", "d", "e"];

fn points(n: usize, seed: u64, gs: i32, maxzoom: i32) -> FeatureStore {
    let mut rng = Rng(seed);
    let mut b = Builder::new(gs);
    let centers: Vec<(f64, f64)> = (0..6)
        .map(|_| (rng.f() * 300.0 - 150.0, rng.f() * 120.0 - 60.0))
        .collect();
    for i in 0..n {
        let (clon, clat) = centers[i % centers.len()];
        let lon = clon + (rng.f() - 0.5) * 20.0 * rng.f();
        let lat = clat + (rng.f() - 0.5) * 10.0 * rng.f();
        let (x, y) = world(lon, lat);
        let value = format!("{:.3}", rng.f() * 1000.0);
        let id = i.to_string();
        let mut attrs = vec![
            ("id", MVT_DOUBLE, id.as_str()),
            ("kind", MVT_STRING, KINDS[i % 5]),
            ("value", MVT_DOUBLE, value.as_str()),
        ];
        if i % 7 == 0 {
            attrs.push(("note", MVT_NULL, "null"));
        }
        let mz = minzoom(&mut rng, maxzoom);
        let layer = if i % 11 == 0 { "other" } else { "points" };
        b.add(
            layer,
            VT_POINT,
            &[Draw::new(VT_MOVETO, x, y)],
            &attrs,
            mz,
            None,
            0,
        );
    }
    b.finish()
}

fn lines(n: usize, seed: u64, gs: i32, maxzoom: i32) -> FeatureStore {
    let mut rng = Rng(seed);
    let mut b = Builder::new(gs);
    for i in 0..n {
        let mut lon = rng.f() * 60.0 - 120.0;
        let mut lat = rng.f() * 30.0 + 20.0;
        let steps = 2 + (rng.next() % 40) as usize;
        let mut geom = Vec::new();
        let mut len = 0i64;
        for s in 0..steps {
            let (x, y) = world(lon, lat);
            geom.push(Draw::new(if s == 0 { VT_MOVETO } else { VT_LINETO }, x, y));
            let dlon = (rng.f() - 0.5) * 0.8;
            let dlat = (rng.f() - 0.5) * 0.6;
            lon += dlon;
            lat += dlat;
            len += ((dlon.abs() + dlat.abs()) * 1e6) as i64;
        }
        let name = format!("road {}", i % 50);
        let attrs = vec![
            ("name", MVT_STRING, name.as_str()),
            ("kind", MVT_STRING, KINDS[i % 3]),
        ];
        let mz = minzoom(&mut rng, maxzoom);
        b.add("roads", VT_LINE, &geom, &attrs, mz, Some(i as u64 + 1), len);
    }
    b.finish()
}

fn polygons(n: usize, seed: u64, gs: i32, maxzoom: i32) -> FeatureStore {
    let mut rng = Rng(seed);
    let mut b = Builder::new(gs);
    for i in 0..n {
        let clon = rng.f() * 50.0 - 110.0;
        let clat = rng.f() * 20.0 + 25.0;
        let r = 0.02 + rng.f() * rng.f() * 3.0;
        let k = 5 + (rng.next() % 30) as usize;
        let mut geom = Vec::new();
        for s in 0..k {
            let a = std::f64::consts::TAU * s as f64 / k as f64;
            let rr = r * (0.6 + 0.4 * rng.f());
            let (x, y) = world(clon + rr * a.cos(), clat - rr * a.sin() * 0.7);
            geom.push(Draw::new(if s == 0 { VT_MOVETO } else { VT_LINETO }, x, y));
        }
        let first = geom[0];
        geom.push(Draw::new(VT_LINETO, first.x, first.y));
        let pop = (rng.next() % 100000).to_string();
        let attrs = vec![
            ("pop", MVT_DOUBLE, pop.as_str()),
            ("state", MVT_STRING, KINDS[i % 4]),
        ];
        let mz = minzoom(&mut rng, maxzoom);
        let area = (r * r * 1e12) as i64;
        b.add("tracts", VT_POLYGON, &geom, &attrs, mz, None, area);
    }
    b.finish()
}

fn options(maxzoom: i32) -> TileOptions {
    TileOptions {
        maxzoom,
        basezoom: maxzoom,
        geometry_scale: (32 - (12 + maxzoom)).max(0),
        cpus: 4,
        temp_files: 16,
        ..TileOptions::default()
    }
}

fn normalize(m: &BTreeMap<String, LayermapEntry>) -> String {
    let mut out = String::new();
    for (name, e) in m {
        out += &format!(
            "{} {} {} {} {} {:?}\n",
            name, e.points, e.lines, e.polygons, e.retain, e.tilestats
        );
    }
    out
}

/// Tiles `store` with tippecanoe-rs and with manby on 1, 2 and 18 threads, and
/// checks that everything matches. Returns the number of tiles.
fn check(store: &FeatureStore, o: &TileOptions) -> usize {
    check_stats(store, o).0
}

fn check_stats(store: &FeatureStore, o: &TileOptions) -> (usize, Vec<Strategy>) {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    set_globals(o);
    let r = reference(store, o);

    // zlib for every tile makes tippecanoe's bytes; libdeflate where the size
    // limit allows it makes the same tiles once decompressed
    for (threads, zlib_tiles) in [(1, true), (2, true), (18, true), (1, false), (18, false)] {
        set_globals(o);
        let sink = MemorySink::default();
        let topts = TileOptions {
            threads,
            // with 2 threads, all waiting tile data goes through temporary files
            rest_memory: if threads == 2 { 0 } else { o.rest_memory },
            zlib_tiles,
            ..o.clone()
        };
        let stats = tile_with(store, &topts, &sink).expect("tiling failed");
        let tiles = sink.tiles.into_inner().unwrap();

        let rk: Vec<_> = r.tiles.keys().collect();
        let mk: Vec<_> = tiles.keys().collect();
        assert_eq!(rk, mk, "tile sets differ with {} threads", threads);
        for (k, v) in &r.tiles {
            assert!(
                if zlib_tiles {
                    tiles[k] == *v
                } else {
                    decoded(&tiles[k]) == decoded(v)
                },
                "tile {:?} differs with {} threads, zlib {}",
                k,
                threads,
                zlib_tiles
            );
        }
        assert_eq!(
            format!("{:?}", r.strategies),
            format!("{:?}", stats.strategies),
            "strategies differ with {} threads",
            threads
        );
        assert_eq!(r.ret, stats.maxzoom, "maxzoom differs");
        assert_eq!((r.midx, r.midy), (stats.midx, stats.midy), "center differs");
        let merged =
            tippecanoe::mbtiles::merge_layermaps(&[input_layermap(store), stats.layermap.clone()]);
        assert_eq!(
            normalize(&r.layermap),
            normalize(&merged),
            "tilestats differ"
        );
    }
    (r.tiles.len(), r.strategies)
}

fn decoded(tile: &[u8]) -> Vec<u8> {
    if !tile.starts_with(&[0x1f, 0x8b]) {
        return tile.to_vec();
    }
    let mut out = Vec::new();
    assert_eq!(tippecanoe::mvt::decompress(tile, &mut out), 1);
    out
}

fn flag(o: &mut TileOptions, c: usize) {
    o.additional[c] = true;
}

#[test]
fn points_default() {
    let store = points(3000, 7, 18, 8);
    assert!(check(&store, &options(8)) > 50);
}

#[test]
fn points_drop_densest_with_retries() {
    let store = points(20000, 11, 18, 10);
    let mut o = options(10);
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    assert!(check(&store, &o) > 50);
}

#[test]
fn points_drop_fraction_and_feature_limit() {
    let store = points(20000, 13, 18, 9);
    let mut o = options(9);
    flag(&mut o, A_DROP_FRACTION_AS_NEEDED);
    o.max_tile_features = 2000;
    check(&store, &o);
}

#[test]
fn points_cluster_and_accumulate() {
    let store = points(8000, 17, 18, 8);
    let mut o = options(8);
    o.cluster_distance = 20;
    o.attribute_accum
        .insert("value".to_string(), AttributeOp::Sum);
    o.attribute_accum
        .insert("kind".to_string(), AttributeOp::Comma);
    check(&store, &o);
}

/// With --cluster-distance and --cluster-densest-as-needed, a tile too big
/// for its concatenated strings retries with no gaps sampled, and
/// choose_mingap indexes an empty list. tippecanoe-rs panics there; manby
/// fails the run rather than waiting for the task forever.
#[test]
fn cluster_distance_concat_too_big_fails() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let store = points(8000, 17, 18, 8);
    let mut o = options(8);
    o.cluster_distance = 20;
    flag(&mut o, A_CLUSTER_DENSEST_AS_NEEDED);
    o.attribute_accum
        .insert("kind".to_string(), AttributeOp::Concat);
    o.max_tile_size = 2000;
    set_globals(&o);
    let sink = MemorySink::default();
    let r = tile_with(&store, &TileOptions { threads: 4, ..o }, &sink);
    assert!(r.is_err());
}

#[test]
fn points_cluster_densest_and_gamma() {
    let store = points(20000, 19, 18, 9);
    let mut o = options(9);
    flag(&mut o, A_CLUSTER_DENSEST_AS_NEEDED);
    o.max_tile_size = 30000;
    o.gamma = 1.5;
    check(&store, &o);
}

#[test]
fn points_retain_multiplier_and_order() {
    let store = points(10000, 23, 18, 8);
    let mut o = options(8);
    o.retain_points_multiplier = 3;
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 30000;
    o.order_by = vec![OrderField::new("value".to_string(), true)];
    check(&store, &o);
}

#[test]
fn points_minzoom_and_extend() {
    let store = points(20000, 29, 18, 6);
    let mut o = options(6);
    o.minzoom = 3;
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    flag(&mut o, A_EXTEND_ZOOMS);
    o.max_tile_size = 15000;
    check(&store, &o);
}

#[test]
fn lines_default_and_reorder() {
    let store = lines(1500, 31, 18, 9);
    let mut o = options(9);
    check(&store, &o);
    flag(&mut o, A_REORDER);
    flag(&mut o, A_COALESCE);
    check(&store, &o);
}

#[test]
fn lines_drop_densest_small_tiles() {
    let store = lines(3000, 37, 18, 10);
    let mut o = options(10);
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    check(&store, &o);
}

#[test]
fn polygons_coalesce_densest() {
    let store = polygons(3000, 41, 18, 10);
    let mut o = options(10);
    flag(&mut o, A_COALESCE_DENSEST_AS_NEEDED);
    o.max_tile_size = 30000;
    assert!(check(&store, &o) > 20);
}

#[test]
fn polygons_smallest_and_detail() {
    let store = polygons(3000, 43, 18, 9);
    let mut o = options(9);
    flag(&mut o, A_COALESCE_SMALLEST_AS_NEEDED);
    o.max_tile_size = 20000;
    assert!(check(&store, &o) > 20);
    let mut o = options(9);
    o.max_tile_size = 15000;
    check(&store, &o);
}

#[test]
fn polygons_label_points_and_density() {
    let store = polygons(2000, 47, 18, 8);
    let mut o = options(8);
    flag(&mut o, A_GENERATE_POLYGON_LABEL_POINTS);
    flag(&mut o, A_CALCULATE_FEATURE_DENSITY);
    assert!(check(&store, &o) > 20);
}

#[test]
fn polygons_no_clipping_and_tiny() {
    let store = polygons(2000, 53, 18, 8);
    let mut o = options(8);
    o.prevent[P_CLIPPING] = true;
    o.prevent[P_TINY_POLYGON_REDUCTION] = true;
    check(&store, &o);
    let mut o = options(8);
    o.prevent[P_DUPLICATION] = true;
    o.extra_detail = 14;
    check(&store, &o);
}

#[test]
fn first_tile_below_zoom_zero() {
    let mut rng = Rng(59);
    let mut b = Builder::new(14);
    for i in 0..3000 {
        let (x, y) = world(-122.4 + rng.f() * 0.5, 37.6 + rng.f() * 0.4);
        let v = i.to_string();
        b.add(
            "p",
            VT_POINT,
            &[Draw::new(VT_MOVETO, x, y)],
            &[("i", MVT_DOUBLE, &v)],
            minzoom(&mut rng, 12),
            None,
            0,
        );
    }
    let store = b.finish();
    let (x, y) = world(-122.15, 37.8);
    let mut o = options(12);
    o.geometry_scale = 14;
    o.minzoom = 8;
    o.iz = 8;
    o.ix = (x >> 24) as u32;
    o.iy = (y >> 24) as u32;
    check(&store, &o);
}

/// Squares in a grid, so that neighbors share edges.
fn grid(n: usize, seed: u64, gs: i32, maxzoom: i32) -> FeatureStore {
    let mut rng = Rng(seed);
    let mut b = Builder::new(gs);
    let side = 0.05;
    for i in 0..n {
        let col = (i % 60) as f64;
        let row = (i / 60) as f64;
        let lon = -100.0 + col * side;
        let lat = 40.0 - row * side;
        let corners = [
            world(lon, lat),
            world(lon + side, lat),
            world(lon + side, lat - side),
            world(lon, lat - side),
            world(lon, lat),
        ];
        let geom: Vec<Draw> = corners
            .iter()
            .enumerate()
            .map(|(k, &(x, y))| Draw::new(if k == 0 { VT_MOVETO } else { VT_LINETO }, x, y))
            .collect();
        let v = (rng.next() % 1000).to_string();
        let mz = minzoom(&mut rng, maxzoom);
        b.add(
            "grid",
            VT_POLYGON,
            &geom,
            &[("v", MVT_DOUBLE, &v)],
            mz,
            None,
            1000,
        );
    }
    b.finish()
}

#[test]
fn polygons_shared_borders() {
    let store = grid(1800, 61, 18, 9);
    let mut o = options(9);
    flag(&mut o, A_DETECT_SHARED_BORDERS);
    assert!(check(&store, &o) > 10);
    flag(&mut o, A_COALESCE_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    check(&store, &o);
}

#[test]
fn filters() {
    let store = points(3000, 67, 18, 6);
    let mut o = options(6);
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    o.prefilter = Some("cat".to_string());
    assert!(check(&store, &o) > 10);
    o.prefilter = None;
    o.postfilter = Some("cat".to_string());
    check(&store, &o);
    let store = polygons(1000, 71, 18, 6);
    let mut o = options(6);
    o.prefilter = Some("cat".to_string());
    o.postfilter = Some("cat".to_string());
    o.attribute_accum
        .insert("pop".to_string(), AttributeOp::Sum);
    flag(&mut o, A_COALESCE_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    check(&store, &o);
}

/// `-at`, with the store laid out as the reference's temporary files are.
/// Returns how many tiles stopped early.
fn check_variable_depth(store: &mut FeatureStore, o: &TileOptions) -> usize {
    let mut o = o.clone();
    flag(&mut o, A_VARIABLE_DEPTH_PYRAMID);
    serialized_layout(store, &mut o);
    let (_, strategies) = check_stats(store, &o);
    strategies.iter().map(|s| s.truncated_zooms).sum()
}

fn all_minzoom_zero(mut store: FeatureStore) -> FeatureStore {
    for f in &mut store.features {
        f.feature_minzoom = 0;
    }
    store
}

#[test]
fn variable_depth_points() {
    let mut store = points(20000, 73, 18, 10);
    let mut o = options(10);
    assert!(check_variable_depth(&mut store, &o) > 10);

    // dense tiles make their zooms drop features, which revives the tiles
    // skipped below those that stopped early
    let mut store = all_minzoom_zero(points(20000, 73, 18, 10));
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    assert!(check_variable_depth(&mut store, &o) > 0);
}

#[test]
fn variable_depth_drop_rate_and_minzoom() {
    let mut store = points(4000, 79, 18, 9);
    for (i, f) in store.features.iter_mut().enumerate() {
        if i % 3 == 0 {
            f.feature_minzoom = 0;
        }
        if i % 97 == 0 {
            f.tippecanoe_minzoom = 7;
        }
    }
    check_variable_depth(&mut store, &options(9));
    let mut store = lines(1500, 83, 18, 9);
    check_variable_depth(&mut store, &options(9));
}

#[test]
fn variable_depth_polygons() {
    let mut store = polygons(6000, 89, 18, 10);
    let mut o = options(10);
    o.max_tile_size = 50000;
    assert!(check_variable_depth(&mut store, &o) > 10);
    flag(&mut o, A_COALESCE_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    check_variable_depth(&mut store, &o);
}

/// Sparse points next to a dense block that only appears at zoom 4: the
/// sparse tiles stop early at zoom 3, and the dense block makes zoom 4 drop
/// features, so their skipped children are made after all.
#[test]
fn variable_depth_revived() {
    let mut rng = Rng(101);
    let mut b = Builder::new(18);
    for i in 0..20300 {
        let (lon, lat) = if i < 300 {
            (-80.0 + rng.f() * 30.0, 10.0 + rng.f() * 25.0)
        } else {
            (-101.0 + rng.f() * 2.0, 29.0 + rng.f() * 2.0)
        };
        let (x, y) = world(lon, lat);
        let v = i.to_string();
        b.add(
            "p",
            VT_POINT,
            &[Draw::new(VT_MOVETO, x, y)],
            &[("i", MVT_DOUBLE, &v)],
            0,
            None,
            0,
        );
    }
    let mut store = b.finish();
    for f in &mut store.features {
        if f.seq >= 300 {
            f.tippecanoe_minzoom = 4;
        }
    }
    let mut o = options(8);
    flag(&mut o, A_DROP_DENSEST_AS_NEEDED);
    o.max_tile_size = 20000;
    assert!(check_variable_depth(&mut store, &o) > 0);
}
