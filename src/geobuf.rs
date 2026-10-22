//! Port of geobuf.hpp and geobuf.cpp: features from a Geobuf file, parsed in
//! parallel.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::EXIT_IMPOSSIBLE;
use crate::geometry::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use crate::globals::{CPUS, get};
use crate::jsonpull::{JsonPull, JsonType};
use crate::milo::dtoa_milo;
use crate::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_NULL, MVT_STRING};
use crate::projection::projection;
use crate::protozero::{Exception, PbfReader};
use crate::serial::{KeyPool, SerialFeature, SerialVal, SerializationState, serialize_feature_sst};
use crate::text::integer_zoom;
use crate::thread::thread_create;

const POINT: i32 = 0;
const MULTIPOINT: i32 = 1;
const LINESTRING: i32 = 2;
const MULTILINESTRING: i32 = 3;
const POLYGON: i32 = 4;
const MULTIPOLYGON: i32 = 5;

/// What the uncaught protozero exception does in the C++: terminate.
fn uncaught<T>(r: crate::protozero::Result<T>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            let name = match e {
                Exception::VarintTooLong => "protozero::varint_too_long_exception",
                Exception::UnknownPbfWireType => "protozero::unknown_pbf_wire_type_exception",
                Exception::EndOfBuffer => "protozero::end_of_buffer_exception",
                Exception::InvalidTag => "protozero::invalid_tag_exception",
                Exception::InvalidLength => "protozero::invalid_length_exception",
            };
            eprintln!(
                "terminate called after throwing an instance of '{}'\n  what():  {}",
                name,
                e.what()
            );
            std::process::abort();
        }
    }
}

/// `struct queued_feature`. The keys and the serialization states that the
/// C++ keeps pointers to here are passed to `runQueue()` instead, as they are
/// when it runs.
#[derive(Clone)]
struct QueuedFeature<'a> {
    pbf: PbfReader<'a>,
    dim: usize,
    e: f64,
    layer: i32,
    layername: String,
}

#[allow(non_snake_case)]
fn ensureDim(dim: usize) {
    if dim < 2 {
        eprintln!("Geometry has fewer than 2 dimensions: {}", dim);
        std::process::exit(EXIT_IMPOSSIBLE);
    }
}

#[allow(non_snake_case)]
fn readValue(pbf: &mut PbfReader<'_>) -> SerialVal {
    let mut sv = SerialVal {
        type_: MVT_NULL,
        s: "null".to_string(),
    };

    while uncaught(pbf.next()) {
        match pbf.tag() {
            1 => {
                sv.type_ = MVT_STRING;
                sv.s = uncaught(pbf.get_string());
            }

            2 => {
                sv.type_ = MVT_DOUBLE;
                sv.s = dtoa_milo(uncaught(pbf.get_double()));
            }

            3 => {
                sv.type_ = MVT_DOUBLE;
                sv.s = uncaught(pbf.get_uint64()).to_string();
            }

            4 => {
                sv.type_ = MVT_DOUBLE;
                sv.s = (uncaught(pbf.get_uint64()) as i64)
                    .wrapping_neg()
                    .to_string();
            }

            5 => {
                sv.type_ = MVT_BOOL;
                if uncaught(pbf.get_bool()) {
                    sv.s = "true".to_string();
                } else {
                    sv.s = "false".to_string();
                }
            }

            6 => {
                sv.type_ = MVT_STRING; // stringified JSON
                sv.s = uncaught(pbf.get_string());

                if sv.s == "null" {
                    sv.type_ = MVT_NULL;
                }
            }

            _ => {
                uncaught(pbf.skip());
            }
        }
    }

    sv
}

#[allow(non_snake_case)]
fn readPoint(coords: &[i64], dim: usize, e: f64) -> DrawVec {
    ensureDim(dim);

    let (x, y) = (projection().project)(coords[0] as f64 / e, coords[1] as f64 / e, 32);
    let mut dv = DrawVec::new();
    dv.push(Draw::new(VT_MOVETO, x, y));
    dv
}

#[allow(non_snake_case)]
fn readLinePart(
    coords: &[i64],
    dim: usize,
    e: f64,
    start: usize,
    end: usize,
    closed: bool,
) -> DrawVec {
    ensureDim(dim);

    let mut dv = DrawVec::new();
    let mut prev: Vec<i64> = vec![0; dim];
    let mut p: Vec<f64> = vec![0.0; dim];

    let mut i = start;
    while i + dim - 1 < end {
        if i + dim - 1 >= coords.len() {
            eprintln!(
                "Internal error: line segment {} vs {}",
                i + dim - 1,
                coords.len()
            );
            std::process::exit(EXIT_IMPOSSIBLE);
        }

        for d in 0..dim {
            prev[d] = prev[d].wrapping_add(coords[i + d]);
            p[d] = prev[d] as f64 / e;
        }

        let (x, y) = (projection().project)(p[0], p[1], 32);

        if i == start {
            dv.push(Draw::new(VT_MOVETO, x, y));
        } else {
            dv.push(Draw::new(VT_LINETO, x, y));
        }
        i += dim;
    }

    if closed && !dv.is_empty() {
        dv.push(Draw::new(VT_LINETO, dv[0].x, dv[0].y));
    }

    dv
}

#[allow(non_snake_case)]
fn readLine(coords: &[i64], dim: usize, e: f64, closed: bool) -> DrawVec {
    readLinePart(coords, dim, e, 0, coords.len(), closed)
}

#[allow(non_snake_case)]
fn readMultiLine(coords: &[i64], lengths: &[i32], dim: usize, e: f64, closed: bool) -> DrawVec {
    if lengths.is_empty() {
        return readLinePart(coords, dim, e, 0, coords.len(), closed);
    }

    let mut dv = DrawVec::new();
    let mut here: usize = 0;
    for &len in lengths {
        let n = (len as isize as usize).wrapping_mul(dim);
        let dv2 = readLinePart(coords, dim, e, here, here.wrapping_add(n), closed);
        here = here.wrapping_add(n);

        for d in dv2 {
            dv.push(d);
        }
    }

    dv
}

#[allow(non_snake_case)]
fn readMultiPolygon(coords: &[i64], lengths: &[i32], dim: usize, e: f64) -> DrawVec {
    ensureDim(dim);

    if lengths.is_empty() {
        return readLinePart(coords, dim, e, 0, coords.len(), true);
    }

    let polys = lengths[0] as isize as usize;
    let mut n: usize = 1;
    let mut here: usize = 0;
    let mut dv = DrawVec::new();

    for _ in 0..polys {
        let rings = lengths[n] as isize as usize;
        n += 1;

        for _ in 0..rings {
            let len = (lengths[n] as isize as usize).wrapping_mul(dim);
            let dv2 = readLinePart(coords, dim, e, here, here.wrapping_add(len), true);
            here = here.wrapping_add(len);
            n += 1;

            for d in dv2 {
                dv.push(d);
            }
        }

        dv.push(Draw::new(VT_CLOSEPATH, 0, 0)); // mark that the next ring is outer
    }

    dv
}

#[derive(Clone, Default)]
struct DrawvecType {
    dv: DrawVec,
    type_: i32,
}

#[allow(non_snake_case)]
fn readGeometry(pbf: &mut PbfReader<'_>, dim: usize, e: f64, keys: &[String]) -> Vec<DrawvecType> {
    let mut ret: Vec<DrawvecType> = Vec::new();
    let mut coords: Vec<i64> = Vec::new();
    let mut lengths: Vec<i32> = Vec::new();
    let mut type_: i32 = -1;

    while uncaught(pbf.next()) {
        match pbf.tag() {
            1 => {
                type_ = uncaught(pbf.get_enum());
            }

            2 => {
                let pi = uncaught(pbf.get_packed_uint32());
                for it in pi {
                    lengths.push(uncaught(it) as i32);
                }
            }

            3 => {
                let pi = uncaught(pbf.get_packed_sint64());
                for it in pi {
                    coords.push(uncaught(it));
                }
            }

            4 => {
                let mut geometry_reader = uncaught(pbf.get_message());
                let dv2 = readGeometry(&mut geometry_reader, dim, e, keys);

                for d in dv2 {
                    ret.push(d);
                }
            }

            _ => {
                uncaught(pbf.skip());
            }
        }
    }

    let mut dv = DrawvecType::default();
    if type_ == POINT {
        dv.dv = readPoint(&coords, dim, e);
    } else if type_ == MULTIPOINT {
        dv.dv = readLine(&coords, dim, e, false);
    } else if type_ == LINESTRING {
        dv.dv = readLine(&coords, dim, e, false);
    } else if type_ == POLYGON {
        dv.dv = readMultiLine(&coords, &lengths, dim, e, true);
    } else if type_ == MULTILINESTRING {
        dv.dv = readMultiLine(&coords, &lengths, dim, e, false);
    } else if type_ == MULTIPOLYGON {
        dv.dv = readMultiPolygon(&coords, &lengths, dim, e);
    } else {
        // GeometryCollection
        return ret;
    }

    dv.type_ = type_ / 2 + 1;
    ret.push(dv);
    ret
}

static WARNED_STRING_ID: AtomicBool = AtomicBool::new(false);
static WARNED_RANGE_ID: AtomicBool = AtomicBool::new(false);

#[allow(non_snake_case, clippy::too_many_arguments)]
fn readFeature(
    pbf: &mut PbfReader<'_>,
    dim: usize,
    e: f64,
    keys: &[String],
    sst: &mut SerializationState<'_>,
    layer: i32,
    layername: String,
    key_pool: &mut KeyPool,
) {
    let mut layername = layername;
    let mut dv: Vec<DrawvecType> = Vec::new();
    let mut id: i64 = 0;
    let mut has_id = false;
    let mut values: Vec<SerialVal> = Vec::new();
    let mut other: BTreeMap<String, SerialVal> = BTreeMap::new();

    let mut full_keys: Vec<Arc<String>> = Vec::new();
    let mut full_values: Vec<SerialVal> = Vec::new();

    while uncaught(pbf.next()) {
        match pbf.tag() {
            1 => {
                let mut geometry_reader = uncaught(pbf.get_message());
                let dv2 = readGeometry(&mut geometry_reader, dim, e, keys);
                for d in dv2 {
                    dv.push(d);
                }
            }

            11 => {
                if !WARNED_STRING_ID.load(Ordering::Relaxed) {
                    eprintln!("Non-numeric feature IDs not supported");
                    WARNED_STRING_ID.store(true, Ordering::Relaxed);
                }
                uncaught(pbf.skip());
            }

            12 => {
                has_id = true;
                id = uncaught(pbf.get_sint64());
                if id < 0 {
                    if !WARNED_RANGE_ID.load(Ordering::Relaxed) {
                        eprintln!("Out of range feature id {}", id);
                        WARNED_RANGE_ID.store(true, Ordering::Relaxed);
                    }
                    has_id = false;
                }
            }

            13 => {
                let mut value_reader = uncaught(pbf.get_message());
                values.push(readValue(&mut value_reader));
            }

            14 => {
                let mut properties: Vec<usize> = Vec::new();
                let pi = uncaught(pbf.get_packed_uint32());
                for it in pi {
                    properties.push(uncaught(it) as usize);
                }

                let mut i = 0;
                while i + 1 < properties.len() {
                    if properties[i] >= keys.len() {
                        eprintln!("Out of bounds key: {} in {}", properties[i], keys.len());
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    if properties[i + 1] >= values.len() {
                        eprintln!(
                            "Out of bounds value: {} in {}",
                            properties[i + 1],
                            values.len()
                        );
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    full_keys.push(key_pool.pool(&keys[properties[i]]));
                    full_values.push(values[properties[i + 1]].clone());
                    i += 2;
                }

                values.clear();
            }

            15 => {
                let mut misc: Vec<usize> = Vec::new();
                let pi = uncaught(pbf.get_packed_uint32());
                for it in pi {
                    misc.push(uncaught(it) as usize);
                }

                let mut i = 0;
                while i + 1 < misc.len() {
                    if misc[i] >= keys.len() {
                        eprintln!("Out of bounds key: {} in {}", misc[i], keys.len());
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    if misc[i + 1] >= values.len() {
                        eprintln!("Out of bounds value: {} in {}", misc[i + 1], values.len());
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    other
                        .entry(keys[misc[i]].clone())
                        .or_insert_with(|| values[misc[i + 1]].clone());
                    i += 2;
                }

                values.clear();
            }

            _ => {
                uncaught(pbf.skip());
            }
        }
    }

    for d in &dv {
        let mut sf = SerialFeature {
            layer: layer as i64,
            segment: sst.segment,
            has_id,
            id: id as u64,
            tippecanoe_minzoom: -1,
            tippecanoe_maxzoom: -1,
            feature_minzoom: 0,
            seq: sst.layer_seq,
            geometry: d.dv.clone(),
            t: d.type_ as i8,
            full_keys: full_keys.clone(),
            full_values: full_values.clone(),
            ..Default::default()
        };

        if let Some(tip) = other.get("tippecanoe") {
            let s = tip.s.as_bytes();
            let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            let mut jp = JsonPull::begin_string(&s[..end]);
            if let Some(o) = jp.read_tree() {
                let o = o.root();

                if let Some(min) = o.hash_get("minzoom") {
                    if min.type_() == JsonType::Number {
                        sf.tippecanoe_minzoom = integer_zoom(sst.fname, &dtoa_milo(min.number()));
                    }
                }

                if let Some(max) = o.hash_get("maxzoom") {
                    if max.type_() == JsonType::Number {
                        sf.tippecanoe_maxzoom = integer_zoom(sst.fname, &dtoa_milo(max.number()));
                    }
                }

                if let Some(tlayer) = o.hash_get("layer") {
                    if tlayer.type_() == JsonType::String {
                        layername = tlayer.string().to_string();
                    }
                }
            }
        }

        serialize_feature_sst(sst, &mut sf, &layername);
    }
}

struct QueueRunArg {
    start: usize,
    end: usize,
    segment: usize,
}

impl QueueRunArg {
    fn new(start1: usize, end1: usize, segment1: usize) -> QueueRunArg {
        QueueRunArg {
            start: start1,
            end: end1,
            segment: segment1,
        }
    }
}

fn run_parse_feature(
    qra: &QueueRunArg,
    feature_queue: &[QueuedFeature<'_>],
    keys: &[String],
    sst: &mut SerializationState<'_>,
) {
    let mut key_pool = KeyPool::default();

    for qf in &feature_queue[qra.start..qra.end] {
        let mut pbf = qf.pbf;
        readFeature(
            &mut pbf,
            qf.dim,
            qf.e,
            keys,
            sst,
            qf.layer,
            qf.layername.clone(),
            &mut key_pool,
        );
    }
}

#[allow(non_snake_case)]
fn runQueue(
    feature_queue: &mut Vec<QueuedFeature<'_>>,
    keys: &[String],
    sst: &mut [SerializationState<'_>],
) {
    if feature_queue.is_empty() {
        return;
    }

    let cpus = get(&CPUS);
    let mut qra: Vec<QueueRunArg> = Vec::new();

    for i in 0..cpus {
        sst[i].layer_seq = sst[0].layer_seq + (feature_queue.len() * i / cpus) as i64;

        qra.push(QueueRunArg::new(
            feature_queue.len() * i / cpus,
            feature_queue.len() * (i + 1) / cpus,
            i,
        ));
    }

    {
        let queue: &[QueuedFeature<'_>] = feature_queue;
        std::thread::scope(|scope| {
            let mut pthreads = Vec::new();
            for (q, s) in qra.iter().zip(sst.iter_mut()) {
                debug_assert_eq!(q.segment, s.segment as usize);
                pthreads.push(thread_create(scope, move || {
                    run_parse_feature(q, queue, keys, s);
                }));
            }

            for h in pthreads {
                if let Err(e) = h.join() {
                    std::panic::resume_unwind(e);
                }
            }
        });
    }

    // Lack of atomicity is OK, since we are single-threaded again here
    let was = sst[cpus - 1].layer_seq;
    sst[0].layer_seq = was;
    feature_queue.clear();
}

#[allow(non_snake_case, clippy::too_many_arguments)]
fn queueFeature<'a>(
    feature_queue: &mut Vec<QueuedFeature<'a>>,
    pbf: PbfReader<'a>,
    dim: usize,
    e: f64,
    keys: &'a [String],
    sst: &mut [SerializationState<'_>],
    layer: i32,
    layername: &str,
) {
    let qf = QueuedFeature {
        pbf,
        dim,
        e,
        layer,
        layername: layername.to_string(),
    };

    feature_queue.push(qf);

    if feature_queue.len() > get(&CPUS) * 500 {
        runQueue(feature_queue, keys, sst);
    }
}

#[allow(non_snake_case)]
fn outBareGeometry(
    dv: &DrawVec,
    type_: i32,
    sst: &mut SerializationState<'_>,
    layer: i32,
    layername: &str,
) {
    let mut sf = SerialFeature {
        layer: layer as i64,
        segment: sst.segment,
        has_id: false,
        tippecanoe_minzoom: -1,
        tippecanoe_maxzoom: -1,
        feature_minzoom: 0,
        seq: sst.layer_seq,
        geometry: dv.clone(),
        t: type_ as i8,
        ..Default::default()
    };

    serialize_feature_sst(sst, &mut sf, layername);
}

#[allow(non_snake_case, clippy::too_many_arguments)]
fn readFeatureCollection<'a>(
    feature_queue: &mut Vec<QueuedFeature<'a>>,
    pbf: &mut PbfReader<'a>,
    dim: usize,
    e: f64,
    keys: &'a [String],
    sst: &mut [SerializationState<'_>],
    layer: i32,
    layername: &str,
) {
    while uncaught(pbf.next()) {
        match pbf.tag() {
            1 => {
                let feature_reader = uncaught(pbf.get_message());
                queueFeature(
                    feature_queue,
                    feature_reader,
                    dim,
                    e,
                    keys,
                    sst,
                    layer,
                    layername,
                );
            }

            _ => {
                uncaught(pbf.skip());
            }
        }
    }
}

pub fn parse_geobuf(sst: &mut [SerializationState<'_>], src: &[u8], layer: i32, layername: &str) {
    // The keys are read ahead, so that they can be shared with the threads
    // while more of the file is still being read. `nkeys` is how many of them
    // the C++ would have read so far.
    let mut keys: Vec<String> = Vec::new();
    {
        let mut pbf = PbfReader::new(src);
        while uncaught(pbf.next()) {
            match pbf.tag() {
                1 => keys.push(uncaught(pbf.get_string())),
                _ => uncaught(pbf.skip()),
            }
        }
    }

    let mut pbf = PbfReader::new(src);
    let mut feature_queue: Vec<QueuedFeature<'_>> = Vec::new();

    let mut dim: usize = 2;
    let mut e: f64 = 1e6;
    let mut nkeys: usize = 0;

    while uncaught(pbf.next()) {
        match pbf.tag() {
            1 => {
                uncaught(pbf.skip());
                nkeys += 1;
            }

            2 => {
                dim = uncaught(pbf.get_int64()) as usize;
            }

            3 => {
                e = 10f64.powf(uncaught(pbf.get_int64()) as f64);
            }

            4 => {
                let mut feature_collection_reader = uncaught(pbf.get_message());
                readFeatureCollection(
                    &mut feature_queue,
                    &mut feature_collection_reader,
                    dim,
                    e,
                    &keys[..nkeys],
                    sst,
                    layer,
                    layername,
                );
            }

            5 => {
                let feature_reader = uncaught(pbf.get_message());
                queueFeature(
                    &mut feature_queue,
                    feature_reader,
                    dim,
                    e,
                    &keys[..nkeys],
                    sst,
                    layer,
                    layername,
                );
            }

            6 => {
                let mut geometry_reader = uncaught(pbf.get_message());
                let dv = readGeometry(&mut geometry_reader, dim, e, &keys[..nkeys]);
                for d in &dv {
                    // Always on thread 0
                    outBareGeometry(&d.dv, d.type_, &mut sst[0], layer, layername);
                }
            }

            _ => {
                uncaught(pbf.skip());
            }
        }
    }

    runQueue(&mut feature_queue, &keys[..nkeys], sst);
}
