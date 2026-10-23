//! manby join's tile work against tile-join's own: random tiles, odd ones
//! included, decoded both ways, and joined both ways, by `join_tile` and by
//! tile-join's `append_tile` and `mvt_tile::encode` as tippecanoe-rs has them,
//! copied here with its rules as arguments.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tippecanoe::mbtiles::{LayermapEntry, add_to_tilestats, merge_layermaps};
use tippecanoe::mvt::{
    MVT_DOUBLE, MVT_LINESTRING, MVT_LINETO, MVT_MOVETO, MVT_NULL, MVT_POINT, MVT_POLYGON,
    MVT_STRING, MvtFeature, MvtLayer, MvtTile, MvtValue, mvt_value_to_serial_val,
    stringified_to_mvt_value,
};
use tippecanoe::protozero::PbfWriter;
use tippecanoe::serial::SerialVal;

use super::csv::RawCsv;
use super::key::{Key, TieBreak, cmp, is_libdeflate_gzip};
use super::stats::{Accum, std_max, std_min};
use super::tile::{self, Decoded, Rules, Scratch, Val};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const NAMES: [&str; 4] = ["roads", "water", "places", "b\u{e5}d"];
const KEYS: [&str; 8] = ["name", "id", "kind", "pop", "area", "flag", "GEOID", "x"];

fn value(rng: &mut Rng, w: &mut PbfWriter<'_>) {
    match rng.below(9) {
        0 | 1 => {
            let s = format!("v{}", rng.below(40));
            w.add_string(1, s.as_bytes());
        }
        2 => w.add_float(2, rng.below(1000) as f32 / 8.0),
        3 => w.add_double(3, rng.below(100000) as f64 / 3.0),
        4 => w.add_int64(4, rng.below(1000) as i64 - 500),
        5 => w.add_uint64(5, rng.below(1 << 40)),
        6 => w.add_sint64(6, rng.below(1000) as i64 - 500),
        7 => w.add_bool(7, rng.chance(50)),
        _ => {
            // more than one field: the last one decides
            w.add_string(1, b"first");
            w.add_int64(4, rng.below(10) as i64);
        }
    }
}

fn geometry(rng: &mut Rng, type_: i32) -> Vec<u32> {
    let mut g = Vec::new();
    let zz = |v: i32| ((v << 1) ^ (v >> 31)) as u32;
    let coord = |rng: &mut Rng, g: &mut Vec<u32>| {
        g.push(zz(rng.below(600) as i32 - 300));
        g.push(zz(rng.below(600) as i32 - 300));
    };
    let parts = 1 + rng.below(3);
    for _ in 0..parts {
        if rng.chance(5) {
            // commands as an encoder that merges nothing would write them
            g.push(1 << 3 | 1);
            coord(rng, &mut g);
            g.push(1 << 3 | 2);
            coord(rng, &mut g);
            g.push(1 << 3 | 2);
            coord(rng, &mut g);
            g.push(2 << 3 | 7);
            continue;
        }
        if rng.chance(2) {
            // a command with no points
            g.push(2);
        }
        let n = if type_ == MVT_POINT {
            1 + rng.below(2)
        } else {
            1
        };
        g.push((n as u32) << 3 | 1);
        for _ in 0..n {
            coord(rng, &mut g);
        }
        if type_ != MVT_POINT {
            let m = 1 + rng.below(5);
            g.push((m as u32) << 3 | 2);
            for _ in 0..m {
                coord(rng, &mut g);
            }
            if type_ == MVT_POLYGON {
                g.push(1 << 3 | 7);
            }
        }
    }
    if rng.chance(3) {
        // a count with too few coordinates, at the end, where decoding stops
        g.push(3 << 3 | 2);
        coord(rng, &mut g);
    }
    g
}

/// A random tile, with the oddities tile-join meets in tiles from other tools.
fn random_tile(rng: &mut Rng, many_values: bool) -> Vec<u8> {
    let mut tile = Vec::new();
    let mut w = PbfWriter::new(&mut tile);
    let layers = 1 + rng.below(3);
    for _ in 0..layers {
        let mut layer = Vec::new();
        let mut lw = PbfWriter::new(&mut layer);
        if rng.chance(95) {
            lw.add_uint32(15, 2);
        }
        lw.add_string(1, NAMES[rng.below(4) as usize].as_bytes());
        lw.add_uint32(5, [4096, 4096, 1024, 8192][rng.below(4) as usize]);
        if rng.chance(3) {
            // an unknown field
            lw.add_uint32(9, 7);
        }
        let mut nkeys = 1 + rng.below(KEYS.len() as u64);
        for i in 0..nkeys {
            lw.add_string(3, KEYS[i as usize].as_bytes());
        }
        if many_values {
            // enough keys that some share a hash slot in tile-join's table
            for i in 0..3000 {
                lw.add_string(3, format!("key number {}", i).as_bytes());
            }
            nkeys += 3000;
        }
        if rng.chance(5) {
            // a duplicate key
            lw.add_string(3, KEYS[0].as_bytes());
            nkeys += 1;
        }
        if rng.chance(3) {
            // not UTF-8
            lw.add_string(3, b"k\xff");
            nkeys += 1;
        }
        let nvalues = if many_values { 3000 } else { 1 + rng.below(30) };
        for i in 0..nvalues {
            let mut v = Vec::new();
            let mut vw = PbfWriter::new(&mut v);
            if many_values {
                vw.add_string(1, format!("value number {}", i).as_bytes());
            } else {
                value(rng, &mut vw);
            }
            lw.add_message(4, &v);
        }
        let nfeatures = 1 + rng.below(if many_values { 400 } else { 20 });
        for _ in 0..nfeatures {
            let mut f = Vec::new();
            let mut fw = PbfWriter::new(&mut f);
            if rng.chance(30) {
                fw.add_uint64(1, rng.below(1 << 20));
            }
            let type_ = 1 + rng.below(3) as i32;
            fw.add_enum(3, type_);
            let ntags = rng.below(if many_values { 40 } else { 6 });
            let tags: Vec<u32> = (0..ntags)
                .flat_map(|_| [rng.below(nkeys) as u32, rng.below(nvalues) as u32])
                .collect();
            if rng.chance(5) && tags.len() >= 2 {
                // two tag fields
                fw.add_packed_uint32(2, tags[..2].iter().copied());
                fw.add_packed_uint32(2, tags[2..].iter().copied());
            } else {
                fw.add_packed_uint32(2, tags.iter().copied());
            }
            fw.add_packed_uint32(4, geometry(rng, type_));
            lw.add_message(2, &f);
        }
        w.add_message(3, &layer);
    }
    if rng.chance(5) {
        w.add_uint32(7, 1);
    }
    tile
}

#[test]
fn decoding_matches_tippecanoe() {
    let mut rng = Rng(1);
    for i in 0..300 {
        let data = random_tile(&mut rng, i % 50 == 0);
        let ours = Decoded::decode(&data).unwrap();
        let mut theirs = MvtTile::default();
        let mut was_compressed = false;
        assert!(theirs.try_decode(&data, &mut was_compressed).unwrap());
        assert_eq!(ours.layers.len(), theirs.layers.len());
        for (a, b) in ours.layers.iter().zip(&theirs.layers) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.version, b.version);
            assert_eq!(a.extent, b.extent);
            assert_eq!(a.keys, b.keys);
            assert_eq!(a.values.len(), b.values.len());
            for (va, vb) in a.values.iter().zip(&b.values) {
                assert_eq!(va.to_mvt(), *vb);
                assert_eq!(va.mvt_type(), vb.type_);
            }
            assert_eq!(a.features.len(), b.features.len());
            for (fa, fb) in a.features.iter().zip(&b.features) {
                assert_eq!(fa.id, fb.id);
                assert_eq!(fa.has_id, fb.has_id);
                assert_eq!(fa.type_, fb.type_);
                assert_eq!(
                    &ours.tags[fa.tags.start as usize..fa.tags.end as usize],
                    &fb.tags[..]
                );
                let mut points = Vec::new();
                let commands = &ours.geoms[fa.geom.start as usize..fa.geom.end as usize];
                tile::for_each_point_for_test(commands, |op, x, y| points.push((op, x, y)));
                let expected: Vec<(i32, i64, i64)> =
                    fb.geometry.iter().map(|g| (g.op, g.x, g.y)).collect();
                assert_eq!(points, expected);
            }
        }
    }
}

/// tile-join's rules for a test, and the same as `Rules`.
struct TestRules {
    header: Vec<String>,
    mapping: BTreeMap<String, Vec<String>>,
    exclude: BTreeSet<String>,
    include: BTreeSet<String>,
    keep_layers: BTreeSet<String>,
    remove_layers: BTreeSet<String>,
    renames: BTreeMap<String, String>,
    ifmatched: bool,
    exclude_all: bool,
    exclude_all_tile_attributes: bool,
    exclude_all_tile_geometries: bool,
    pe: bool,
}

/// What tile-join's `append_tile` collects besides the tile.
struct Collected {
    layermap: BTreeMap<String, LayermapEntry>,
    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
    minlon2: f64,
    maxlon2: f64,
}

/// tile-join's `append_tile`, from tippecanoe-rs, without the feature filter.
#[allow(clippy::needless_range_loop)]
fn append_tile(
    tile: &mut MvtTile,
    z: i32,
    x: u32,
    y: u32,
    r: &TestRules,
    outtile: &mut MvtTile,
    a: &mut Collected,
) {
    for layer in tile.layers.iter_mut() {
        if let Some(found) = r.renames.get(&layer.name) {
            layer.name = found.clone();
        }
        let layer: &MvtLayer = layer;
        if !r.keep_layers.is_empty() && !r.keep_layers.contains(&layer.name) {
            continue;
        }
        if r.remove_layers.contains(&layer.name) {
            continue;
        }
        let mut ol = 0;
        while ol < outtile.layers.len() {
            if layer.name == outtile.layers[ol].name {
                break;
            }
            ol += 1;
        }
        if ol == outtile.layers.len() {
            outtile.layers.push(MvtLayer::default());
            outtile.layers[ol].name = layer.name.clone();
            outtile.layers[ol].version = layer.version;
            outtile.layers[ol].extent = layer.extent;
        }
        let outlayer = &mut outtile.layers[ol];
        if layer.extent != outlayer.extent && layer.extent > outlayer.extent {
            for feature in outlayer.features.iter_mut() {
                for g in feature.geometry.iter_mut() {
                    g.x = g.x * layer.extent / outlayer.extent;
                    g.y = g.y * layer.extent / outlayer.extent;
                }
            }
            outlayer.extent = layer.extent;
        }
        let mut tilestats_found = a.layermap.contains_key(&layer.name);
        let mut minx = i64::MAX;
        let mut miny = i64::MAX;
        let mut maxx = i64::MIN;
        let mut maxy = i64::MIN;
        let mut minx2 = i64::MAX;
        let mut maxx2 = i64::MIN;
        let mut features_added_to_layer = false;

        for feat in &layer.features {
            let exclude_attributes: BTreeSet<String> = BTreeSet::new();
            #[derive(Default)]
            struct Match {
                has_id: bool,
                id: u64,
                attributes: BTreeMap<String, (MvtValue, SerialVal)>,
                key_order: Vec<String>,
            }
            let mut matches: Vec<Match> = Vec::new();
            let mut matched = false;
            {
                let mut m = Match {
                    id: feat.id,
                    has_id: feat.has_id,
                    ..Default::default()
                };
                for t in (0..feat.tags.len().saturating_sub(1)).step_by(2) {
                    let key = &layer.keys[feat.tags[t] as usize];
                    let val = &layer.values[feat.tags[t + 1] as usize];
                    let sv = mvt_value_to_serial_val(val);
                    if val.type_ == MVT_NULL {
                        continue;
                    }
                    if !r.exclude_all_tile_attributes
                        && (r.include.contains(key)
                            || (!r.exclude_all
                                && !r.exclude.contains(key)
                                && !exclude_attributes.contains(key)))
                    {
                        m.attributes
                            .entry(key.clone())
                            .or_insert((val.clone(), sv.clone()));
                        m.key_order.push(key.clone());
                    }
                    if !r.header.is_empty()
                        && *key == r.header[0]
                        && let Some(fields) = r.mapping.get(&sv.s)
                    {
                        let fields = fields.clone();
                        matched = true;
                        for i in 1..fields.len() {
                            let joinkey = r.header[i].clone();
                            let mut joinval = fields[i].clone();
                            let mut attr_type = MVT_STRING;
                            if !joinval.is_empty() {
                                if joinval.as_bytes()[0] == b'"' {
                                    joinval = tippecanoe::csv::csv_dequote(joinval);
                                } else if tippecanoe::csv::is_number(&joinval) {
                                    attr_type = MVT_DOUBLE;
                                }
                            } else if r.pe {
                                attr_type = MVT_NULL;
                            }
                            if r.include.contains(&joinkey)
                                || (!r.exclude_all
                                    && !r.exclude.contains(&joinkey)
                                    && !exclude_attributes.contains(&joinkey)
                                    && attr_type != MVT_NULL)
                            {
                                let t = if attr_type == MVT_STRING {
                                    MVT_STRING
                                } else {
                                    MVT_DOUBLE
                                };
                                m.attributes.remove(&joinkey);
                                let outsv = SerialVal {
                                    type_: t,
                                    s: joinval.clone(),
                                };
                                let outval = stringified_to_mvt_value(t, &joinval);
                                m.attributes
                                    .entry(joinkey.clone())
                                    .or_insert((outval, outsv));
                                m.key_order.push(joinkey);
                            }
                        }
                    }
                }
                if matched {
                    matches.push(m);
                }
            }
            if !matched && !r.ifmatched {
                let mut m = Match {
                    id: feat.id,
                    has_id: feat.has_id,
                    ..Default::default()
                };
                if !r.exclude_all_tile_attributes {
                    for t in (0..feat.tags.len().saturating_sub(1)).step_by(2) {
                        let key = &layer.keys[feat.tags[t] as usize];
                        let val = &layer.values[feat.tags[t + 1] as usize];
                        let sv = mvt_value_to_serial_val(val);
                        if r.include.contains(key)
                            || (!r.exclude_all
                                && !r.exclude.contains(key)
                                && !exclude_attributes.contains(key))
                        {
                            m.attributes.entry(key.clone()).or_insert((val.clone(), sv));
                            m.key_order.push(key.clone());
                        }
                    }
                }
                matches.push(m);
            }

            for m in matches.iter_mut() {
                if !tilestats_found {
                    let n = a.layermap.len();
                    a.layermap.insert(layer.name.clone(), LayermapEntry::new(n));
                    tilestats_found = true;
                    let tilestats = a.layermap.get_mut(&layer.name).unwrap();
                    tilestats.minzoom = z;
                    tilestats.maxzoom = z;
                }
                let tilestats = a.layermap.get_mut(&layer.name).unwrap();
                let mut outfeature = MvtFeature {
                    id: m.id,
                    has_id: m.has_id,
                    ..Default::default()
                };
                for k in &m.key_order {
                    if let Some(fa) = m.attributes.remove(k) {
                        outlayer.tag(&mut outfeature, k, &fa.0);
                        add_to_tilestats(&mut tilestats.tilestats, k, &fa.1);
                    }
                }
                if r.exclude_all_tile_geometries {
                    outfeature.type_ = -1;
                } else {
                    outfeature.type_ = feat.type_;
                    outfeature.geometry = feat.geometry.clone();
                    if layer.extent != outlayer.extent {
                        for g in outfeature.geometry.iter_mut() {
                            g.x = g.x * outlayer.extent / layer.extent;
                            g.y = g.y * outlayer.extent / layer.extent;
                        }
                    }
                    for g in &outfeature.geometry {
                        if g.op == MVT_MOVETO || g.op == MVT_LINETO {
                            let mut gx = std::cmp::min(outlayer.extent, std::cmp::max(0, g.x));
                            let mut gy = std::cmp::min(outlayer.extent, std::cmp::max(0, g.y));
                            gx = gx * (1i64 << (32 - z)) / outlayer.extent;
                            gy = gy * (1i64 << (32 - z)) / outlayer.extent;
                            gx += (1i64 << (32 - z)) * x as i64;
                            gy += (1i64 << (32 - z)) * y as i64;
                            minx = std::cmp::min(minx, gx);
                            miny = std::cmp::min(miny, gy);
                            maxx = std::cmp::max(maxx, gx);
                            maxy = std::cmp::max(maxy, gy);
                            if gx < (1i64 << 31) {
                                gx += 1i64 << 32;
                            }
                            minx2 = std::cmp::min(minx2, gx);
                            maxx2 = std::cmp::max(maxx2, gx);
                        }
                    }
                }
                features_added_to_layer = true;
                outlayer.features.push(outfeature);
                if z < tilestats.minzoom {
                    tilestats.minzoom = z;
                }
                if z > tilestats.maxzoom {
                    tilestats.maxzoom = z;
                }
                if feat.type_ == MVT_POINT {
                    tilestats.points += 1;
                } else if feat.type_ == MVT_LINESTRING {
                    tilestats.lines += 1;
                } else if feat.type_ == MVT_POLYGON {
                    tilestats.polygons += 1;
                }
            }
        }
        if features_added_to_layer {
            use tippecanoe::projection::tile2lonlat;
            let (lon1, lat1) = tile2lonlat(minx, maxy, 32);
            let (lon2, lat2) = tile2lonlat(maxx, miny, 32);
            a.minlat = std_min(a.minlat, std_min(lat1, lat2));
            a.minlon = std_min(a.minlon, std_min(lon1, lon2));
            a.maxlat = std_max(a.maxlat, std_max(lat1, lat2));
            a.maxlon = std_max(a.maxlon, std_max(lon1, lon2));
            let (lon1, _) = tile2lonlat(minx2, maxy, 32);
            let (lon2, _) = tile2lonlat(maxx2, miny, 32);
            a.minlon2 = std_min(a.minlon2, std_min(lon1, lon2));
            a.maxlon2 = std_max(a.maxlon2, std_max(lon1, lon2));
        }
    }
}

/// tile-join's `join_worker` for one tile.
fn reference_join(
    inputs: &[Vec<u8>],
    z: i32,
    x: u32,
    y: u32,
    r: &TestRules,
    a: &mut Collected,
) -> Option<Vec<u8>> {
    let mut tile = MvtTile::default();
    for message in inputs {
        let mut t = MvtTile::default();
        let mut was_compressed = false;
        assert!(t.try_decode(message, &mut was_compressed).unwrap());
        append_tile(&mut t, z, x, y, r, &mut tile, a);
    }
    let mut outtile = MvtTile::default();
    for layer in tile.layers {
        if !layer.features.is_empty() {
            outtile.layers.push(layer);
        }
    }
    if outtile.layers.is_empty() {
        None
    } else {
        Some(outtile.encode())
    }
}

fn rules_for(r: &TestRules) -> Rules {
    let csv = RawCsv {
        header: r.header.clone(),
        rows: r
            .mapping
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
    .prepare(r.pe);
    Rules {
        header: csv.header,
        mapping: csv.mapping,
        exclude: r.exclude.clone(),
        include: r.include.clone(),
        keep_layers: r.keep_layers.clone(),
        remove_layers: r.remove_layers.clone(),
        renames: r.renames.clone(),
        ifmatched: r.ifmatched,
        exclude_all: r.exclude_all,
        exclude_all_tile_attributes: r.exclude_all_tile_attributes,
        exclude_all_tile_geometries: r.exclude_all_tile_geometries,
        filter: None,
        unidecode_data: Vec::new(),
    }
}

fn plain_rules() -> TestRules {
    TestRules {
        header: Vec::new(),
        mapping: BTreeMap::new(),
        exclude: BTreeSet::new(),
        include: BTreeSet::new(),
        keep_layers: BTreeSet::new(),
        remove_layers: BTreeSet::new(),
        renames: BTreeMap::new(),
        ifmatched: false,
        exclude_all: false,
        exclude_all_tile_attributes: false,
        exclude_all_tile_geometries: false,
        pe: false,
    }
}

fn some_rules(i: usize) -> TestRules {
    let set = |v: &[&str]| {
        v.iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>()
    };
    let mut r = plain_rules();
    match i % 9 {
        0 => {}
        1 => r.exclude = set(&["name", "pop"]),
        2 => {
            r.exclude_all = true;
            r.include = set(&["kind"]);
        }
        3 => r.exclude_all_tile_attributes = true,
        4 => r.exclude_all_tile_geometries = true,
        5 => {
            r.keep_layers = set(&["roads", "water"]);
            r.renames.insert("water".to_string(), "roads".to_string());
        }
        6..=8 => {
            r.header = ["kind", "joined", "number", "empty", "quoted"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            for v in 0..40 {
                if v % 3 == 0 {
                    continue;
                }
                let key = format!("v{}", v);
                r.mapping.insert(
                    key.clone(),
                    vec![
                        key,
                        format!("j{}", v % 7),
                        format!("{}.5", v),
                        String::new(),
                        "\"a \"\"b\"\"\"".to_string(),
                    ],
                );
            }
            r.ifmatched = i % 9 == 7;
            r.pe = i % 9 == 8;
            if i % 9 == 8 {
                r.exclude = set(&["number"]);
            }
        }
        _ => unreachable!(),
    }
    r.remove_layers = if i % 11 == 10 {
        set(&["places"])
    } else {
        BTreeSet::new()
    };
    r
}

#[test]
fn joining_matches_tile_join() {
    // tile-stats read tippecanoe-rs's globals, which other tests set
    let _g = crate::tiler::oracle::GLOBALS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    tippecanoe::globals::MAX_TILESTATS_SAMPLE_VALUES
        .store(1000, std::sync::atomic::Ordering::Relaxed);
    let mut rng = Rng(7);
    for i in 0..400 {
        let n = 1 + rng.below(3) as usize;
        let inputs: Vec<Vec<u8>> = (0..n)
            .map(|k| random_tile(&mut rng, i % 97 == 0 && k == 0))
            .collect();
        let (z, x, y) = (
            rng.below(14) as i32,
            rng.below(1 << 13) as u32,
            rng.below(1 << 13) as u32,
        );
        let (x, y) = (x % (1 << z), y % (1 << z));
        let r = some_rules(i);

        let mut collected = Collected {
            layermap: BTreeMap::new(),
            minlat: i32::MAX as f64,
            minlon: i32::MAX as f64,
            maxlat: i32::MIN as f64,
            maxlon: i32::MIN as f64,
            minlon2: i32::MAX as f64,
            maxlon2: i32::MIN as f64,
        };
        let expected = reference_join(&inputs, z, x, y, &r, &mut collected);

        let rules = rules_for(&r);
        let mut acc = Accum::new(1000);
        let mut scratch = Scratch::default();
        let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
        let mut buffers = vec![Vec::new(); refs.len()];
        let mut d = libdeflater::Decompressor::new();
        let decoded = tile::decode_all(&refs, &mut buffers, &mut d, z, x, y).unwrap();
        let mut pbf = Vec::new();
        let anything =
            tile::join_tile(&decoded, z, x, y, &rules, &mut acc, &mut scratch, &mut pbf).unwrap();
        assert_eq!(anything, expected.is_some(), "case {}", i);
        if let Some(expected) = expected {
            assert!(pbf == expected, "case {}: tiles differ", i);
        }

        let ours = merge_layermaps(&[acc.layermap()]);
        let theirs = merge_layermaps(&[collected.layermap]);
        assert_eq!(format!("{:?}", ours), format!("{:?}", theirs), "case {}", i);
        assert_eq!(
            (
                acc.minlat,
                acc.minlon,
                acc.maxlat,
                acc.maxlon,
                acc.minlon2,
                acc.maxlon2
            ),
            (
                collected.minlat,
                collected.minlon,
                collected.maxlat,
                collected.maxlon,
                collected.minlon2,
                collected.maxlon2
            ),
            "case {}",
            i
        );
    }
}

#[test]
fn values_hash_and_compare_as_tippecanoe_does() {
    let vals = [
        Val::Str("a".into()),
        Val::Str("a\0b".into()),
        Val::Float(1.5),
        Val::Float(-0.0),
        Val::Double(1.5),
        Val::Double(f64::NAN),
        Val::Int(-3),
        Val::Uint(3),
        Val::Sint(-3),
        Val::Bool(true),
        Val::Bool(false),
    ];
    for a in &vals {
        // one layer with the value tagged twice: the dedup and the encoding
        let mut ours = MvtLayer::default();
        let mut f = MvtFeature::default();
        ours.tag(&mut f, "k", &a.to_mvt());
        ours.tag(&mut f, "k", &a.to_mvt());
        assert_eq!(Val::from_mvt(&a.to_mvt()).mvt_type(), a.mvt_type());
        for b in &vals {
            assert_eq!(
                a.to_mvt() == b.to_mvt(),
                tile::same_for_test(a, b),
                "{:?} {:?}",
                a,
                b
            );
        }
        let sv = mvt_value_to_serial_val(&a.to_mvt());
        let (t, s) = a.serial();
        assert_eq!((sv.type_, sv.s.as_str()), (t, s.as_ref()));
    }
}

#[test]
fn csv_rows_too_long_are_errors() {
    let r = TestRules {
        header: vec!["kind".to_string(), "a".to_string()],
        mapping: [(
            "v1".to_string(),
            vec!["v1".to_string(), "x".to_string(), "y".to_string()],
        )]
        .into_iter()
        .collect(),
        ..plain_rules()
    };
    let rules = rules_for(&r);
    let mut tile = Vec::new();
    let mut w = PbfWriter::new(&mut tile);
    let mut layer = Vec::new();
    let mut lw = PbfWriter::new(&mut layer);
    lw.add_uint32(15, 2);
    lw.add_string(1, b"l");
    lw.add_uint32(5, 4096);
    lw.add_string(3, b"kind");
    let mut v = Vec::new();
    PbfWriter::new(&mut v).add_string(1, b"v1");
    lw.add_message(4, &v);
    let mut f = Vec::new();
    let mut fw = PbfWriter::new(&mut f);
    fw.add_enum(3, 1);
    fw.add_packed_uint32(2, [0u32, 0]);
    fw.add_packed_uint32(4, [9u32, 2, 2]);
    lw.add_message(2, &f);
    w.add_message(3, &layer);

    let decoded = vec![Decoded::decode(&tile).unwrap()];
    let mut acc = Accum::new(1000);
    let e = tile::join_tile(
        &decoded,
        0,
        0,
        0,
        &rules,
        &mut acc,
        &mut Scratch::default(),
        &mut Vec::new(),
    )
    .err()
    .unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_CSV);
}

#[test]
fn zlib_keys_order_as_zlib_bytes() {
    let mut rng = Rng(3);
    let tiles: Vec<Vec<u8>> = (0..60).map(|_| random_tile(&mut rng, false)).collect();
    let zlib: Vec<Vec<u8>> = tiles
        .iter()
        .map(|t| crate::compress::gzip(t, false))
        .collect();
    let libdeflate: Vec<Vec<u8>> = tiles
        .iter()
        .map(|t| crate::compress::gzip(t, true))
        .collect();
    for (i, l) in libdeflate.iter().enumerate() {
        assert!(is_libdeflate_gzip(l));
        assert!(!is_libdeflate_gzip(&zlib[i]));
        for (j, z) in zlib.iter().enumerate() {
            // a manby tile against a tippecanoe one
            let a = Key::new(l.clone().into(), true, TieBreak::Zlib);
            let b = Key::new(z.clone().into(), true, TieBreak::Zlib);
            assert_eq!(cmp(&a, &b), zlib[i].cmp(z), "{} {}", i, j);
            // two manby tiles
            let c = Key::new(libdeflate[j].clone().into(), true, TieBreak::Zlib);
            let a = Key::new(l.clone().into(), true, TieBreak::Zlib);
            assert_eq!(cmp(&a, &c), zlib[i].cmp(&zlib[j]), "{} {}", i, j);
            // the bytes as stored, for other tools' tiles and on request
            let a = Key::new(l.clone().into(), false, TieBreak::Zlib);
            let c = Key::new(libdeflate[j].clone().into(), true, TieBreak::Bytes);
            assert_eq!(cmp(&a, &c), l.cmp(&libdeflate[j]), "{} {}", i, j);
            // against uncompressed tiles, decided by the header
            let a = Key::new(l.clone().into(), true, TieBreak::Zlib);
            let d = Key::new(tiles[j].clone().into(), true, TieBreak::Zlib);
            assert_eq!(cmp(&a, &d), zlib[i].cmp(&tiles[j]), "{} {}", i, j);
        }
    }
}

#[test]
fn values_out_of_order_with_csv_text() {
    // the joined values keep the text tile-stats record for them
    let raw = RawCsv {
        header: vec!["k".to_string(), "n".to_string()],
        rows: [("a".to_string(), vec!["a".to_string(), "1.50".to_string()])]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    };
    let csv = raw.prepare(false);
    let f = &csv.mapping["a"].fields[0];
    assert_eq!(f.text, "1.50");
    assert_eq!(f.sv_type, MVT_DOUBLE);
    assert!(matches!(f.value, Val::Float(v) if v == 1.5));
}

#[test]
fn batches_never_split_a_tile() {
    use super::merge::{Queue, Reader, Settings};
    use super::source::Source;
    let _g = crate::tiler::oracle::GLOBALS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tippecanoe-rs/tests");
    let dir = std::env::temp_dir().join(format!("manby-join-batches-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = Vec::new();
    for (out, input) in [
        ("a.pmtiles", "ne_110m_ocean/in.json"),
        ("b.pmtiles", "ne_110m_admin_0_countries/in.json.gz"),
        ("c.pmtiles", "ne_110m_admin_0_countries/in.json.gz"),
    ] {
        let out = dir.join(out).to_string_lossy().into_owned();
        let argv: Vec<String> = ["tippecanoe", "-q", "-f", "-z3", "-o", &out]
            .iter()
            .map(|s| s.to_string())
            .chain([tests.join(input).to_string_lossy().into_owned()])
            .collect();
        crate::run(&crate::options::parse_args(&argv).unwrap()).unwrap();
        names.push(out);
    }
    let settings = Settings {
        want_overzoom: false,
        buffer: 5,
        tie_break: TieBreak::Zlib,
    };
    let run = |limit: usize| {
        let sources: Vec<Source> = names
            .iter()
            .map(|n| Source::open(n, 0, 32).unwrap())
            .collect();
        let mut queue = Queue::new();
        for (i, s) in sources.iter().enumerate() {
            queue
                .add(Reader::begin(s, i, &settings).unwrap(), &settings)
                .unwrap();
        }
        let mut all = Vec::new();
        loop {
            let mut groups = Vec::new();
            let more = queue.run(&settings, 0, 32, limit, &mut groups).unwrap();
            for g in groups {
                all.push((
                    g.z,
                    g.x,
                    g.y,
                    g.members.iter().map(|m| m.0).collect::<Vec<_>>(),
                ));
            }
            if !more {
                break;
            }
        }
        all
    };
    let whole = run(usize::MAX);
    assert!(whole.iter().any(|g| g.3.len() == 3));
    assert_eq!(run(1), whole);
    assert_eq!(run(7), whole);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An unchanged tile keeps its compression only if zlib or libdeflate made
/// it; any other gzip stream is compressed again.
#[test]
fn only_zlib_and_libdeflate_streams_pass_through() {
    let _g = crate::tiler::oracle::GLOBALS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("manby-join-through-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let input = dir.join("in");

    // tiles as a join writes them, so that joining them again changes nothing
    let mut rng = Rng(11);
    let mut tiles = Vec::new();
    while tiles.len() < 4 {
        let raw = random_tile(&mut rng, false);
        let decoded = vec![Decoded::decode(&raw).unwrap()];
        let mut pbf = Vec::new();
        let rules = rules_for(&plain_rules());
        let mut acc = Accum::new(1000);
        if tile::join_tile(
            &decoded,
            3,
            1,
            2,
            &rules,
            &mut acc,
            &mut Scratch::default(),
            &mut pbf,
        )
        .unwrap()
        {
            tiles.push(pbf);
        }
    }
    let zlib = crate::compress::gzip(&tiles[0], false);
    let libdeflate = crate::compress::gzip(&tiles[1], true);
    // zlib's stream under another system's header, as Java's GZIPOutputStream
    // writes it, and with a modification time
    let mut other_os = crate::compress::gzip(&tiles[2], false);
    other_os[9] = 0;
    let mut mtime = crate::compress::gzip(&tiles[3], false);
    mtime[4] = 1;
    let stored = [zlib, libdeflate, other_os, mtime];
    for (i, data) in stored.iter().enumerate() {
        let d = input.join(format!("3/{}", i));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("2.pbf"), data).unwrap();
    }
    std::fs::write(
        input.join("metadata.json"),
        r#"{"name": "in", "generator": "tippecanoe v2.82.0"}"#,
    )
    .unwrap();

    let output = dir.join("out");
    let o = super::JoinOptions {
        inputs: vec![input.to_string_lossy().into_owned()],
        output: crate::options::Output::Directory(output.to_string_lossy().into_owned()),
        quiet: true,
        ..Default::default()
    };
    super::join(&o).unwrap();

    for (i, data) in stored.iter().enumerate() {
        let out = std::fs::read(output.join(format!("3/{}/2.pbf", i))).unwrap();
        let mut back = Vec::new();
        assert!(tile::decompress(
            &mut libdeflater::Decompressor::new(),
            &out,
            &mut back
        ));
        assert_eq!(back, tiles[i], "tile {}", i);
        assert_eq!(out == *data, i < 2, "tile {}", i);
        assert!(
            out[..10] == *super::key::zlib_header() || out[..10] == super::key::LIBDEFLATE_HEADER,
            "tile {}",
            i
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Overzooming makes what tippecanoe-rs's `overzoom_source_tiles` makes with
/// tile-join's arguments, children included.
#[test]
fn overzooming_matches_tippecanoe() {
    use std::collections::HashMap;
    let mut rng = Rng(5);
    let mut cases = 0;
    for i in 0..120 {
        let data = random_tile(&mut rng, i % 40 == 0);
        let ours = Decoded::decode(&data).unwrap();
        let mut theirs = MvtTile::default();
        let mut was_compressed = false;
        assert!(theirs.try_decode(&data, &mut was_compressed).unwrap());
        let (sz, sx, sy) = (3, 1i64, 2i64);
        let depth = 1 + rng.below(3) as i32;
        let n = 1i64 << depth;
        let (dx, dy) = (rng.below(n as u64) as i64, rng.below(n as u64) as i64);
        let dst = (sz + depth, sx * n + dx, sy * n + dy);
        for buffer in [0, 5, 64] {
            let mut our_children = Vec::new();
            let pbf =
                super::overzoom::overzoom(&ours, (sz, sx, sy), dst, buffer, &mut our_children)
                    .unwrap();
            let mut their_children = Vec::new();
            let expected = tippecanoe::clip::overzoom_source_tiles(
                &[tippecanoe::geometry::SourceTile {
                    tile: theirs.clone(),
                    z: sz,
                    x: sx as i32,
                    y: sy as i32,
                }],
                dst.0,
                dst.1 as i32,
                dst.2 as i32,
                -1,
                buffer,
                &BTreeSet::new(),
                &BTreeSet::new(),
                &[],
                false,
                Some(&mut their_children),
                false,
                None,
                false,
                &HashMap::new(),
                &[],
                0.0,
                0.0,
                &[],
                "",
                "",
                usize::MAX,
                &[],
                false,
            );
            assert!(pbf == expected, "case {} buffer {}", i, buffer);
            assert_eq!(our_children, their_children, "case {} buffer {}", i, buffer);
            cases += !expected.is_empty() as usize;
        }
    }
    assert!(cases > 100);
}
