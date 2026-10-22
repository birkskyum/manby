//! Checks that an MLT tileset holds the same tiles as an MVT tileset made
//! with the same options: the same tiles, layers, features in the same order,
//! ids, geometry and attribute values.
//!
//!   cargo +1.98.1 run --release --features mlt --example mlt_verify -- mvt.pmtiles mlt.pmtiles
//!
//! The MVT tiles are decoded with tippecanoe-rs and the MLT tiles with
//! mlt-core. Values are compared under MLT's typing rules: integers in a
//! float column are compared as floats, and in a column that mixes types
//! every value is its text (floats in mlt-core's shortest form). The encoder may wind a polygon ring the other
//! way; such rings are counted, not failed.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use mlt_core::compact_str::ToCompactString;
use mlt_core::geo_types::{Coord, Geometry, LineString, Polygon};
use mlt_core::{PropValue, TileLayer};
use rayon::prelude::*;
use tippecanoe::mvt::{
    MVT_BOOL, MVT_CLOSEPATH, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINESTRING, MVT_LINETO,
    MVT_MOVETO, MVT_POINT, MVT_POLYGON, MVT_SINT, MVT_STRING, MVT_UINT, MvtTile, MvtValue,
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Default)]
struct Stats {
    tiles: AtomicU64,
    layers: AtomicU64,
    features: AtomicU64,
    values: AtomicU64,
    rewound_rings: AtomicU64,
    failures: AtomicU64,
    examples: Mutex<Vec<String>>,
}

impl Stats {
    fn fail(&self, message: String) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        let mut e = self.examples.lock().unwrap();
        if e.len() < 20 {
            e.push(message);
        }
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// An MVT value as tippecanoe-rs decoded it, and its text form under mlt-core's rules.
fn mvt_value(v: &MvtValue) -> Option<(Value, String)> {
    let n = &v.numeric_value;
    Some(match v.type_ {
        MVT_STRING => (
            Value::Str(v.string_value.to_string()),
            v.string_value.to_string(),
        ),
        MVT_FLOAT => (
            Value::Float(n.float_value() as f64),
            n.float_value().to_compact_string().to_string(),
        ),
        MVT_DOUBLE => (
            Value::Float(n.double_value()),
            n.double_value().to_compact_string().to_string(),
        ),
        MVT_INT => (Value::Int(n.int_value() as i128), n.int_value().to_string()),
        MVT_SINT => (
            Value::Int(n.sint_value() as i128),
            n.sint_value().to_string(),
        ),
        MVT_UINT => (
            Value::Int(n.uint_value() as i128),
            n.uint_value().to_string(),
        ),
        MVT_BOOL => (Value::Bool(n.bool_value()), n.bool_value().to_string()),
        _ => return None,
    })
}

fn mlt_value(p: &PropValue) -> Option<Value> {
    Some(match p {
        PropValue::Bool(v) => Value::Bool((*v)?),
        PropValue::I8(v) => Value::Int((*v)? as i128),
        PropValue::U8(v) => Value::Int((*v)? as i128),
        PropValue::I32(v) => Value::Int((*v)? as i128),
        PropValue::U32(v) => Value::Int((*v)? as i128),
        PropValue::I64(v) => Value::Int((*v)? as i128),
        PropValue::U64(v) => Value::Int((*v)? as i128),
        PropValue::F32(v) => Value::Float((*v)? as f64),
        PropValue::F64(v) => Value::Float((*v)?),
        PropValue::Str(v) => Value::Str(v.as_ref()?.to_string()),
    })
}

/// Whether the MLT value `m` stands for the MVT value `v` (with text form `text`).
fn same_value(m: &Value, v: &Value, text: &str) -> bool {
    match (m, v) {
        (Value::Str(s), Value::Str(t)) => s == t,
        (Value::Str(s), _) => s == text,
        (Value::Float(f), Value::Int(i)) => *f == *i as f64,
        (a, b) => a == b,
    }
}

/// The parts of an MVT geometry: points, lines or rings, without closing points.
fn mvt_parts(geometry: &[tippecanoe::mvt::MvtGeometry]) -> Parts {
    let mut parts: Parts = Vec::new();
    for g in geometry {
        if g.op == MVT_MOVETO {
            parts.push(vec![(g.x, g.y)]);
        } else if g.op == MVT_LINETO {
            if let Some(p) = parts.last_mut() {
                p.push((g.x, g.y));
            }
        } else if g.op == MVT_CLOSEPATH
            && let Some(p) = parts.last_mut()
            && p.len() > 1
            && p.first() == p.last()
        {
            p.pop();
        }
    }
    parts
}

fn line(l: &LineString<i32>) -> Vec<(i64, i64)> {
    l.0.iter()
        .map(|c: &Coord<i32>| (c.x as i64, c.y as i64))
        .collect()
}

fn ring(l: &LineString<i32>) -> Vec<(i64, i64)> {
    let mut r = line(l);
    if r.len() > 1 && r.first() == r.last() {
        r.pop();
    }
    r
}

fn polygon_rings(p: &Polygon<i32>, out: &mut Vec<Vec<(i64, i64)>>) {
    out.push(ring(p.exterior()));
    out.extend(p.interiors().iter().map(ring));
}

type Parts = Vec<Vec<(i64, i64)>>;

/// The MVT type and parts of an MLT geometry.
fn mlt_parts(g: &Geometry<i32>) -> Option<(i32, Parts)> {
    Some(match g {
        Geometry::Point(p) => (MVT_POINT, vec![vec![(p.x() as i64, p.y() as i64)]]),
        Geometry::MultiPoint(m) => (
            MVT_POINT,
            m.0.iter()
                .map(|p| vec![(p.x() as i64, p.y() as i64)])
                .collect(),
        ),
        Geometry::LineString(l) => (MVT_LINESTRING, vec![line(l)]),
        Geometry::MultiLineString(m) => (MVT_LINESTRING, m.0.iter().map(line).collect()),
        Geometry::Polygon(p) => {
            let mut rings = Vec::new();
            polygon_rings(p, &mut rings);
            (MVT_POLYGON, rings)
        }
        Geometry::MultiPolygon(m) => {
            let mut rings = Vec::new();
            for p in &m.0 {
                polygon_rings(p, &mut rings);
            }
            (MVT_POLYGON, rings)
        }
        _ => return None,
    })
}

/// A ring wound the other way, starting at the same point.
fn reversed(r: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = vec![r[0]];
    out.extend(r[1..].iter().rev());
    out
}

fn check_tile(name: &str, mvt: &[u8], mlt: &[u8], stats: &Stats) {
    let mut tile = MvtTile::default();
    let mut was_compressed = false;
    if !tile.decode(mvt, &mut was_compressed) {
        return stats.fail(format!("{}: the MVT tile does not decode", name));
    }
    let mlt_layers: Vec<TileLayer> = match manby::mlt::decode_tile(mlt) {
        Ok(l) => l,
        Err(e) => return stats.fail(format!("{}: the MLT tile does not decode: {}", name, e)),
    };
    let mvt_layers: Vec<_> = tile
        .layers
        .iter()
        .filter(|l| !l.features.is_empty())
        .collect();
    if mvt_layers.len() != mlt_layers.len() {
        return stats.fail(format!(
            "{}: {} MLT layers, not {}",
            name,
            mlt_layers.len(),
            mvt_layers.len()
        ));
    }
    for (v, m) in mvt_layers.iter().zip(&mlt_layers) {
        stats.layers.fetch_add(1, Ordering::Relaxed);
        let lname = format!("{} layer {}", name, v.name);
        if v.name != m.name() || v.extent != m.extent().get() as i64 {
            return stats.fail(format!(
                "{}: MLT layer {} extent {}, not extent {}",
                lname,
                m.name(),
                m.extent().get(),
                v.extent
            ));
        }
        if v.features.len() != m.feature_count() {
            return stats.fail(format!(
                "{}: {} MLT features, not {}",
                lname,
                m.feature_count(),
                v.features.len()
            ));
        }
        let columns: HashMap<&str, usize> = m
            .property_names()
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        for (i, (vf, mf)) in v.features.iter().zip(m.features()).enumerate() {
            stats.features.fetch_add(1, Ordering::Relaxed);
            let fname = format!("{} feature {}", lname, i);
            let id = vf.has_id.then_some(vf.id);
            if id != mf.id() {
                stats.fail(format!("{}: id {:?}, not {:?}", fname, mf.id(), id));
            }

            let expected = mvt_parts(&vf.geometry);
            match mlt_parts(mf.geometry()) {
                Some((t, parts)) if t == vf.type_ && parts.len() == expected.len() => {
                    for (a, e) in parts.iter().zip(&expected) {
                        if a == e {
                            continue;
                        }
                        if t == MVT_POLYGON && !e.is_empty() && *a == reversed(e) {
                            stats.rewound_rings.fetch_add(1, Ordering::Relaxed);
                        } else {
                            stats.fail(format!("{}: other coordinates", fname));
                            break;
                        }
                    }
                }
                _ => stats.fail(format!(
                    "{}: another kind or number of geometry parts",
                    fname
                )),
            }

            let mut seen = vec![false; m.property_names().len()];
            for pair in vf.tags.chunks(2) {
                let key = &v.keys[pair[0] as usize];
                let Some((value, text)) = mvt_value(&v.values[pair[1] as usize]) else {
                    continue;
                };
                stats.values.fetch_add(1, Ordering::Relaxed);
                let Some(&col) = columns.get(key.as_str()) else {
                    stats.fail(format!("{}: no MLT column {}", fname, key));
                    continue;
                };
                seen[col] = true;
                match mlt_value(&mf.properties()[col]) {
                    Some(m) if same_value(&m, &value, &text) => {}
                    other => stats.fail(format!(
                        "{}: {} is {:?} in MLT, not {:?}",
                        fname, key, other, value
                    )),
                }
            }
            for (col, seen) in seen.iter().enumerate() {
                if !seen && !mf.properties()[col].is_null() {
                    stats.fail(format!(
                        "{}: {} is {:?} in MLT but absent in MVT",
                        fname,
                        m.property_names()[col],
                        mf.properties()[col]
                    ));
                }
            }
        }
    }
    stats.tiles.fetch_add(1, Ordering::Relaxed);
}

struct Archive {
    bytes: Vec<u8>,
    gzipped: bool,
    tiles: HashMap<(u8, u32, u32), (usize, usize)>,
}

fn open(path: &str) -> Archive {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {}", path, e));
    let header = tippecanoe::pmtiles::deserialize_header(&bytes[..127]);
    let tiles = tippecanoe::pmtiles_file::pmtiles_entries_tms(&bytes, 0, 32)
        .into_iter()
        .map(|e| ((e.z, e.x, e.y), (e.offset as usize, e.length as usize)))
        .collect();
    Archive {
        gzipped: header.tile_compression == tippecanoe::pmtiles::COMPRESSION_GZIP,
        bytes,
        tiles,
    }
}

impl Archive {
    fn tile(&self, at: (usize, usize)) -> Vec<u8> {
        let data = &self.bytes[at.0..at.0 + at.1];
        if self.gzipped {
            let mut out = Vec::new();
            tippecanoe::mvt::decompress(data, &mut out);
            out
        } else {
            data.to_vec()
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} mvt.pmtiles mlt.pmtiles", args[0]);
        return ExitCode::from(2);
    }
    let mvt = open(&args[1]);
    let mlt = open(&args[2]);
    let stats = Stats::default();

    let mut only_mvt = 0;
    let mut both = Vec::new();
    for (k, at) in &mvt.tiles {
        match mlt.tiles.get(k) {
            Some(m) => both.push((*k, *at, *m)),
            None => only_mvt += 1,
        }
    }
    let only_mlt = mlt
        .tiles
        .keys()
        .filter(|k| !mvt.tiles.contains_key(k))
        .count();
    both.sort();
    both.par_iter().for_each(|&((z, x, y), a, b)| {
        let name = format!("{}/{}/{}", z, x, y);
        check_tile(&name, &mvt.tile(a), &mlt.tile(b), &stats);
    });

    let failures = stats.failures.load(Ordering::Relaxed);
    println!(
        "{} tiles in both ({} only in MVT, {} only in MLT): {} layers, {} features, {} values; {} rings wound the other way; {} differences",
        both.len(),
        only_mvt,
        only_mlt,
        stats.layers.load(Ordering::Relaxed),
        stats.features.load(Ordering::Relaxed),
        stats.values.load(Ordering::Relaxed),
        stats.rewound_rings.load(Ordering::Relaxed),
        failures,
    );
    for e in stats.examples.lock().unwrap().iter() {
        println!("  {}", e);
    }
    if failures == 0 && only_mvt == 0 && only_mlt == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
