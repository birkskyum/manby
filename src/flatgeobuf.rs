//! Port of flatgeobuf.hpp and flatgeobuf.cpp: features from a FlatGeobuf file,
//! parsed in parallel.

use std::sync::Arc;

use crate::errors::EXIT_IMPOSSIBLE;
use crate::flatgeobuf_generated::{
    ColumnType, Feature, Geometry, GeometryType, size_prefixed_root_as_feature,
    size_prefixed_root_as_header,
};
use crate::geometry::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use crate::globals::{CPUS, QUIET, get};
use crate::milo::dtoa_milo;
use crate::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_STRING};
use crate::projection::projection;
use crate::serial::{KeyPool, SerialFeature, SerialVal, SerializationState, serialize_feature_sst};
use crate::thread::thread_create;

const MAGICBYTES: [u8; 8] = [0x66, 0x67, 0x62, 0x03, 0x66, 0x67, 0x62, 0x01];

/// `struct NodeItem`: four doubles and an offset.
const SIZEOF_NODE_ITEM: u64 = 40;

/// What an uncaught C++ exception does: print what it was and abort.
fn throw(type_: &str, what: &str) -> ! {
    eprintln!(
        "terminate called after throwing an instance of '{}'\n  what():  {}",
        type_, what
    );
    std::process::abort();
}

// copied from https://github.com/flatgeobuf/flatgeobuf/blob/master/src/cpp/packedrtree.cpp#L365
#[allow(non_snake_case)]
fn PackedRTreeSize(numItems: u64, nodeSize: u16) -> u64 {
    if nodeSize < 2 {
        throw("std::invalid_argument", "Node size must be at least 2");
    }
    if numItems == 0 {
        throw(
            "std::invalid_argument",
            "Number of items must be greater than 0",
        );
    }
    let nodeSizeMin: u16 = std::cmp::min(std::cmp::max(nodeSize, 2u16), 65535u16);
    // limit so that resulting size in bytes can be represented by uint64_t
    if numItems > 1u64 << 56 {
        throw(
            "std::overflow_error",
            "Number of items must be less than 2^56",
        );
    }
    let mut n = numItems;
    let mut numNodes = n;
    loop {
        n = (n + nodeSizeMin as u64 - 1) / nodeSizeMin as u64;
        numNodes += n;
        if n == 1 {
            break;
        }
    }
    numNodes * SIZEOF_NODE_ITEM
}

#[allow(non_snake_case)]
fn readPoints(geometry: &Geometry<'_>) -> DrawVec {
    let xy = geometry.xy().unwrap();
    let mut dv = DrawVec::new();

    let mut i = 0;
    while i < xy.len() {
        let (x, y) = (projection().project)(xy.get(i), xy.get(i + 1), 32);
        dv.push(Draw::new(VT_MOVETO, x, y));
        i += 2;
    }
    dv
}

#[allow(non_snake_case)]
fn readLinePart(geometry: &Geometry<'_>) -> DrawVec {
    let xy = geometry.xy().unwrap();
    let ends = geometry.ends();
    let mut current_end: usize = 0;
    let mut dv = DrawVec::new();

    let mut i: u32 = 0;
    while (i as usize) < xy.len() {
        let (x, y) = (projection().project)(xy.get(i as usize), xy.get(i as usize + 1), 32);
        let at_end = match &ends {
            Some(ends) => current_end < ends.len() && i == ends.get(current_end).wrapping_mul(2),
            None => false,
        };
        if i == 0 || at_end {
            dv.push(Draw::new(VT_MOVETO, x, y));
            if i > 0 {
                current_end += 1;
            }
        } else {
            dv.push(Draw::new(VT_LINETO, x, y));
        }
        i += 2;
    }
    dv
}

#[allow(non_snake_case)]
fn readGeometry(geometry: &Geometry<'_>, h_geometry_type: GeometryType) -> DrawVec {
    let mut geometry_type = h_geometry_type;
    if h_geometry_type == GeometryType::Unknown {
        geometry_type = geometry.type_();
    }

    if geometry_type == GeometryType::Point {
        readPoints(geometry)
    } else if geometry_type == GeometryType::MultiPoint {
        readPoints(geometry)
    } else if geometry_type == GeometryType::LineString {
        readLinePart(geometry)
    } else if geometry_type == GeometryType::MultiLineString {
        readLinePart(geometry)
    } else if geometry_type == GeometryType::Polygon {
        readLinePart(geometry)
    } else if geometry_type == GeometryType::MultiPolygon {
        // if it is a GeometryCollection, parse Parts, ignore XY
        let mut dv = DrawVec::new();
        let parts = geometry.parts().unwrap();
        for part in 0..parts.len() {
            let dv2 = readLinePart(&parts.get(part));
            for d in dv2 {
                dv.push(d);
            }
            dv.push(Draw::new(VT_CLOSEPATH, 0, 0));
        }
        dv
    } else {
        eprintln!(
            "flatgeobuf has unsupported geometry type {}",
            h_geometry_type.0 as u32
        );
        std::process::exit(EXIT_IMPOSSIBLE);
    }
}

/// `memcpy` of `N` bytes at `at`.
fn bytes_at<const N: usize>(data: &[u8], at: usize) -> [u8; N] {
    data[at..at + N].try_into().unwrap()
}

#[allow(non_snake_case, clippy::too_many_arguments)]
fn readFeature(
    feature: &Feature<'_>,
    feature_sequence_id: i64,
    h_geometry_type: GeometryType,
    h_column_names: &[String],
    h_column_types: &[ColumnType],
    sst: &mut SerializationState<'_>,
    layer: i32,
    layername: &str,
) {
    let dv = readGeometry(&feature.geometry().unwrap(), h_geometry_type);

    let drawvec_type: i8;

    let mut geometry_type = h_geometry_type;
    if h_geometry_type == GeometryType::Unknown {
        geometry_type = feature.geometry().unwrap().type_();
    }

    match geometry_type {
        GeometryType::Point | GeometryType::MultiPoint => {
            drawvec_type = 1;
        }
        GeometryType::LineString | GeometryType::MultiLineString => {
            drawvec_type = 2;
        }
        GeometryType::Polygon | GeometryType::MultiPolygon => {
            drawvec_type = 3;
        }
        _ => {
            eprintln!(
                "flatgeobuf has unsupported geometry type {}",
                h_geometry_type.0 as u32
            );
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }

    let mut sf = SerialFeature {
        layer: layer as i64,
        segment: sst.segment,
        has_id: feature_sequence_id >= 0,
        id: feature_sequence_id as u64,
        tippecanoe_minzoom: -1,
        tippecanoe_maxzoom: -1,
        feature_minzoom: 0,
        seq: sst.layer_seq,
        geometry: dv,
        t: drawvec_type,
        ..Default::default()
    };

    let mut full_keys: Vec<Arc<String>> = Vec::new();
    let mut full_values: Vec<SerialVal> = Vec::new();
    let mut key_pool = KeyPool::default();

    // assume tabular schema with columns in header
    let mut p_pos: usize = 0;
    if let Some(properties) = feature.properties() {
        let data = properties.bytes();
        while p_pos < data.len() {
            let col_idx = u16::from_ne_bytes(bytes_at::<2>(data, p_pos)) as usize;

            // https://github.com/protomaps/tippecanoe/issues/7
            // check if column name is tippecanoe:minzoom, tippecanoe:maxzoom or tippecanoe:layer

            let col_type = h_column_types[col_idx];
            let val_at = p_pos + 2;

            let mut sv = SerialVal::default();
            if col_type == ColumnType::Byte {
                sv.type_ = MVT_DOUBLE;
                let byte_val = i8::from_ne_bytes(bytes_at::<1>(data, val_at));
                sv.s = byte_val.to_string();
                p_pos += 2 + 1;
            } else if col_type == ColumnType::UByte {
                sv.type_ = MVT_DOUBLE;
                let ubyte_val = u8::from_ne_bytes(bytes_at::<1>(data, val_at));
                sv.s = ubyte_val.to_string();
                p_pos += 2 + 1;
            } else if col_type == ColumnType::Bool {
                sv.type_ = MVT_BOOL;
                let bool_val = u8::from_ne_bytes(bytes_at::<1>(data, val_at));
                if bool_val != 0 {
                    sv.s = "true".to_string();
                } else {
                    sv.s = "false".to_string();
                }
                p_pos += 2 + 1;
            } else if col_type == ColumnType::Short {
                sv.type_ = MVT_DOUBLE;
                let short_val = i16::from_ne_bytes(bytes_at::<2>(data, val_at));
                sv.s = short_val.to_string();
                p_pos += 2 + 2;
            } else if col_type == ColumnType::UShort {
                sv.type_ = MVT_DOUBLE;
                let ushort_val = u16::from_ne_bytes(bytes_at::<2>(data, val_at));
                sv.s = ushort_val.to_string();
                p_pos += 2 + 2;
            } else if col_type == ColumnType::Int {
                sv.type_ = MVT_DOUBLE;
                let int_val = i32::from_ne_bytes(bytes_at::<4>(data, val_at));
                sv.s = int_val.to_string();
                p_pos += 2 + 4;
            } else if col_type == ColumnType::UInt {
                sv.type_ = MVT_DOUBLE;
                let uint_val = u32::from_ne_bytes(bytes_at::<4>(data, val_at));
                sv.s = uint_val.to_string();
                p_pos += 2 + 4;
            } else if col_type == ColumnType::Long {
                sv.type_ = MVT_DOUBLE;
                let long_val = i64::from_ne_bytes(bytes_at::<8>(data, val_at));
                sv.s = long_val.to_string();
                p_pos += 2 + 8;
            } else if col_type == ColumnType::ULong {
                sv.type_ = MVT_DOUBLE;
                let ulong_val = u64::from_ne_bytes(bytes_at::<8>(data, val_at));
                sv.s = ulong_val.to_string();
                p_pos += 2 + 8;
            } else if col_type == ColumnType::Float {
                sv.type_ = MVT_DOUBLE;
                let float_val = f32::from_ne_bytes(bytes_at::<4>(data, val_at));
                sv.s = dtoa_milo(float_val as f64);
                p_pos += 2 + 4;
            } else if col_type == ColumnType::Double {
                sv.type_ = MVT_DOUBLE;
                let double_val = f64::from_ne_bytes(bytes_at::<8>(data, val_at));
                sv.s = dtoa_milo(double_val);
                p_pos += 2 + 8;
            } else if col_type == ColumnType::String
                || col_type == ColumnType::Json
                || col_type == ColumnType::DateTime
            {
                sv.type_ = MVT_STRING;
                let val_len = u32::from_ne_bytes(bytes_at::<4>(data, val_at)) as usize;
                let s = &data[val_at + 4..val_at + 4 + val_len];
                sv.s = String::from_utf8_lossy(s).into_owned();
                p_pos += 2 + 4 + val_len;
            } else {
                // Binary is not representable in MVT
                eprintln!(
                    "flatgeobuf has unsupported column type {}",
                    col_type.0 as u32
                );
                std::process::exit(EXIT_IMPOSSIBLE);
            }
            full_keys.push(key_pool.pool(&h_column_names[col_idx]));
            full_values.push(sv);
        }
    }

    sf.full_keys = full_keys;
    sf.full_values = full_values;

    serialize_feature_sst(sst, &mut sf, layername);
}

/// `struct fgb_queued_feature`. The serialization states that the C++ keeps
/// a pointer to here are passed to `fgb_run_queue()` instead.
#[derive(Clone, Copy)]
struct FgbQueuedFeature<'a> {
    feature: Feature<'a>,
    feature_sequence_id: i64,
    h_geometry_type: GeometryType,
    h_column_names: &'a [String],
    h_column_types: &'a [ColumnType],
    layer: i32,
    layername: &'a str,
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

fn fgb_run_parse_feature(
    qra: &QueueRunArg,
    feature_queue: &[FgbQueuedFeature<'_>],
    sst: &mut SerializationState<'_>,
) {
    for qf in &feature_queue[qra.start..qra.end] {
        readFeature(
            &qf.feature,
            qf.feature_sequence_id,
            qf.h_geometry_type,
            qf.h_column_names,
            qf.h_column_types,
            sst,
            qf.layer,
            qf.layername,
        );
    }
}

#[allow(non_snake_case)]
fn fgbRunQueue(feature_queue: &mut Vec<FgbQueuedFeature<'_>>, sst: &mut [SerializationState<'_>]) {
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
        let queue: &[FgbQueuedFeature<'_>] = feature_queue;
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (q, s) in qra.iter().zip(sst.iter_mut()) {
                debug_assert_eq!(q.segment, s.segment as usize);
                handles.push(thread_create(scope, move || {
                    fgb_run_parse_feature(q, queue, s);
                }));
            }

            for h in handles {
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
    feature_queue: &mut Vec<FgbQueuedFeature<'a>>,
    feature: Feature<'a>,
    feature_sequence_id: i64,
    h_geometry_type: GeometryType,
    h_column_names: &'a [String],
    h_column_types: &'a [ColumnType],
    sst: &mut [SerializationState<'_>],
    layer: i32,
    layername: &'a str,
) {
    let qf = FgbQueuedFeature {
        feature,
        feature_sequence_id,
        h_geometry_type,
        h_column_names,
        h_column_types,
        layer,
        layername,
    };

    feature_queue.push(qf);

    if feature_queue.len() > get(&CPUS) * 500 {
        fgbRunQueue(feature_queue, sst);
    }
}

/// `flatbuffers::GetPrefixedSize`: the little-endian size at the start of `buf`.
fn get_prefixed_size(buf: &[u8]) -> u32 {
    let mut b = [0u8; 4];
    for (i, c) in buf.iter().take(4).enumerate() {
        b[i] = *c;
    }
    u32::from_le_bytes(b)
}

/// `buf[start .. start + len]`, cut short at the end of `buf`.
fn clamp(buf: &[u8], start: usize, len: usize) -> &[u8] {
    let start = std::cmp::min(start, buf.len());
    let end = std::cmp::min(start.saturating_add(len), buf.len());
    &buf[start..end]
}

pub fn parse_flatgeobuf(
    sst: &mut [SerializationState<'_>],
    src: &[u8],
    layer: i32,
    layername: &str,
) {
    let len = src.len();
    let header_size = get_prefixed_size(clamp(src, MAGICBYTES.len(), 4)) as usize;

    let header = match size_prefixed_root_as_header(clamp(src, MAGICBYTES.len(), header_size + 4)) {
        Ok(h) => h,
        Err(_) => {
            eprintln!("flatgeobuf header verification failed");
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    };

    let features_count = header.features_count();
    let node_size = header.index_node_size();

    let mut h_column_names: Vec<String> = Vec::new();
    let mut h_column_types: Vec<ColumnType> = Vec::new();

    if let Some(columns) = header.columns() {
        for i in 0..columns.len() {
            let name = columns.get(i).name();
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            h_column_names.push(String::from_utf8_lossy(&name[..end]).into_owned());
            h_column_types.push(columns.get(i).type_());
        }
    }

    let h_geometry_type = header.geometry_type();

    let mut feature_sequence_id: i64 = -1;
    let mut index_size: i64 = 0;
    if node_size > 0 {
        if get(&QUIET) == 0 {
            eprintln!("detected indexed FlatGeobuf: assigning feature IDs by sequence");
        }
        index_size = PackedRTreeSize(features_count, node_size) as i64;
        feature_sequence_id = 0;
    }
    let mut start: usize = (MAGICBYTES.len() as u64 + 4 + header_size as u64)
        .saturating_add(index_size as u64) as usize;

    let mut feature_queue: Vec<FgbQueuedFeature<'_>> = Vec::new();

    while start < len {
        let feature_size = get_prefixed_size(&src[start..]) as usize;

        let feature = match size_prefixed_root_as_feature(clamp(src, start, feature_size + 4)) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("flatgeobuf feature buffer verification failed");
                std::process::exit(EXIT_IMPOSSIBLE);
            }
        };

        queueFeature(
            &mut feature_queue,
            feature,
            feature_sequence_id,
            h_geometry_type,
            &h_column_names,
            &h_column_types,
            sst,
            layer,
            layername,
        );

        if feature_sequence_id >= 0 {
            feature_sequence_id += 1;
        }
        start += 4 + feature_size;
    }

    fgbRunQueue(&mut feature_queue, sst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_rtree_size() {
        assert_eq!(PackedRTreeSize(1, 16), 2 * 40);
        assert_eq!(PackedRTreeSize(16, 16), 17 * 40);
        assert_eq!(PackedRTreeSize(17, 16), (17 + 2 + 1) * 40);
        assert_eq!(PackedRTreeSize(1000, 16), (1000 + 63 + 4 + 1) * 40);
    }
}
