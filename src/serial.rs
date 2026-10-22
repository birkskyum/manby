//! Port of serial.hpp and serial.cpp: the serialized form of features in the
//! temporary files, and the attribute value type used while reading input.
//!
//! The `FILE *` and `std::string` overloads of `serialize_*` get distinct names
//! here: the ones that append to a byte buffer keep the C++ name, and the ones
//! that write to a file start with `f`. The `serialization_state` overload of
//! `serialize_feature`, which the input parsers call, is `serialize_feature_sst`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32};
use std::sync::{Arc, Mutex};

use crate::clip::{
    clip_lines_bbox, clip_point_bbox, distance_from_line, fix_polygon, get_area, remove_noop,
    simple_clip_poly_bbox,
};
use crate::cstd::{atof, strerror, strtoull};
use crate::errors::{EXIT_IMPOSSIBLE, EXIT_WRITE};
use crate::geometry::{
    Draw, DrawVec, VT_END, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON, center_of_mass_mp,
    decode_geometry, polygon_to_anchor,
};
use crate::globals::{
    ATTRIBUTE_FOR_ID, CLIPBBOXES, CLUSTER_DISTANCE, GEOMETRY_SCALE,
    MAXIMUM_STRING_ATTRIBUTE_LENGTH, ORDER_BY_SIZE_ENABLED, PRESERVE_MULTIPLIER_DENSITY_THRESHOLD,
    QUIET, QUIET_PROGRESS, RETAIN_POINTS_MULTIPLIER, SET_ATTRIBUTES, additional, get, prevent,
};
use crate::mbtiles::{LayermapEntry, add_to_tilestats};
use crate::memfile::Memfile;
use crate::milo::dtoa_milo;
use crate::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_STRING};
use crate::options::*;
use crate::pool::addpool;
use crate::projection::{encode_index, encode_vertex};
use crate::text::truncate_string;
use crate::tippecanoe_main::{INDEX_SIZE, Index, checkdisk, progress_time};

/// This is the main representation of attribute values in memory and
/// in the string pool. The type is one of the mvt_value type (mvt_string,
/// mvt_double, mvt_bool, or mvt_null). Note that all numeric values,
/// whether integer or floating point, use mvt_double here.
#[derive(Clone, Debug, Default)]
pub struct SerialVal {
    pub type_: i32,
    pub s: String,
}

impl SerialVal {
    pub fn new(t: i32, val: &str) -> SerialVal {
        SerialVal {
            type_: t,
            s: val.to_string(),
        }
    }

    // These functions for interface compatibility with mvt_value:

    /// `serial_val(double)`.
    pub fn from_double(val: f64) -> SerialVal {
        SerialVal {
            type_: MVT_DOUBLE,
            s: crate::milo::dtoa_milo(val),
        }
    }

    pub fn to_double(&self) -> f64 {
        crate::cstd::atof(&self.s)
    }

    pub fn get_string_value(&self) -> String {
        self.s.clone()
    }

    pub fn set_string_value(&mut self, val: &str) {
        self.type_ = crate::mvt::MVT_STRING;
        self.s = val.to_string();
    }

    pub fn get_count(&self) -> usize {
        match self.s.find('\0') {
            None => 0,
            Some(found) => crate::cstd::atoll(&self.s[found + 1..]) as usize,
        }
    }

    pub fn set_double_count(&mut self, v: f64, c: usize) {
        self.type_ = MVT_DOUBLE;
        self.s = crate::milo::dtoa_milo(v) + "\0" + &c.to_string();
    }
}

/// `serial_val::operator<`, which mbtiles.cpp defines: by string, then by type.
impl Ord for SerialVal {
    fn cmp(&self, o: &SerialVal) -> Ordering {
        self.s.cmp(&o.s).then(self.type_.cmp(&o.type_))
    }
}

impl PartialOrd for SerialVal {
    fn partial_cmp(&self, o: &SerialVal) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl PartialEq for SerialVal {
    fn eq(&self, o: &SerialVal) -> bool {
        self.type_ == o.type_ && self.s == o.s
    }
}

impl Eq for SerialVal {}

#[derive(Clone, Debug, Default)]
pub struct KeyPool {
    pub mapping: HashMap<String, Arc<String>>,
}

impl KeyPool {
    pub fn pool(&mut self, s: &str) -> Arc<String> {
        if let Some(f) = self.mapping.get(s) {
            return f.clone();
        }

        let p = Arc::new(s.to_string());
        self.mapping.insert(s.to_string(), p.clone());
        p
    }
}

/// The memory a `serial_feature`'s `stringpool` pointer points into: the global
/// string pool plus this feature's segment offset. Each entry at an offset is a
/// type byte followed by a NUL-terminated string.
#[derive(Clone, Default)]
pub struct StringPoolRef {
    pub pool: Option<Arc<dyn AsRef<[u8]> + Send + Sync>>,
    pub base: usize,
}

impl StringPoolRef {
    fn bytes(&self) -> &[u8] {
        match &self.pool {
            Some(p) => &(**p).as_ref()[self.base..],
            None => &[],
        }
    }

    /// `stringpool[off]`, the type byte of the entry at `off`.
    pub fn type_at(&self, off: i64) -> i32 {
        self.bytes()[off as usize] as i8 as i32
    }

    /// `stringpool + off + 1`, the string of the entry at `off`.
    pub fn str_at(&self, off: i64) -> &str {
        let b = &self.bytes()[off as usize + 1..];
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        std::str::from_utf8(&b[..end]).unwrap_or("")
    }
}

impl std::fmt::Debug for StringPoolRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StringPoolRef")
            .field("base", &self.base)
            .finish()
    }
}

pub const FEATURE_DROPPED: i32 = -1;
pub const FEATURE_KEPT: i32 = 0;
pub const FEATURE_ADDED_FOR_MULTIPLIER_DENSITY: i32 = i32::MAX;

#[derive(Clone, Debug)]
pub struct SerialFeature {
    pub layer: i64,
    pub segment: i32,
    pub seq: i64,

    pub t: i8,
    pub feature_minzoom: i8,

    pub has_id: bool,
    pub id: u64,

    pub tippecanoe_minzoom: i32,
    pub tippecanoe_maxzoom: i32,

    pub geometry: DrawVec,
    pub index: u64,
    /// filled in during z0. square of planar distance
    pub gap: u64,
    pub label_point: u64,
    pub extent: i64,

    // These fields are not directly serialized, but are used
    // to create the keys and values references into the string pool
    // during initial serialization
    pub full_keys: Vec<Arc<String>>,
    pub full_values: Vec<SerialVal>,

    // These fields are generated from full_keys and full_values
    // during initial serialization and then replace the string
    // representations:
    pub keys: Vec<i64>,
    pub values: Vec<i64>,

    // These fields are used during tiling,
    // but are not serialized and are not expected
    // to be provided by frontends:
    pub bbox: [i64; 4],
    /// what nodes at the tile edge were added during clipping?
    pub edge_nodes: DrawVec,

    /// <0: dropped
    ///  0: kept
    /// >0: sequence number of additional feature kept by retain-points-multiplier
    /// INT_MAX: additional feature kept by preserve-multiplier-density-threshold
    pub dropped: i32,

    /// is polygon dust
    pub reduced: bool,
    /// was coalesced from multiple features
    pub coalesced: bool,
    /// current tile resolution being used for simplification
    pub line_detail: i32,
    /// extra tile resolution to retain in output
    pub extra_detail: i32,
    pub maxzoom: i32,
    /// feature spacing for --calculate-feature-density
    pub spacing: f64,
    /// simplification level at this zoom level
    pub simplification: f64,
    /// used in --detect-shared-borders
    pub arc_polygon: Vec<isize>,
    /// used in --detect-shared-borders logic
    pub renamed: isize,
    /// does this feature need the clustered/point_count attributes?
    pub clustered: i64,
    /// string pool for keys/values lookup
    pub stringpool: StringPoolRef,

    /// tile being produced
    pub z: i32,
    pub tx: i32,
    pub ty: i32,
}

impl Default for SerialFeature {
    fn default() -> Self {
        SerialFeature {
            layer: 0,
            segment: 0,
            seq: 0,
            t: 0,
            feature_minzoom: 0,
            has_id: false,
            id: 0,
            tippecanoe_minzoom: -1,
            tippecanoe_maxzoom: -1,
            geometry: DrawVec::new(),
            index: 0,
            gap: 0,
            label_point: 0,
            extent: 0,
            full_keys: Vec::new(),
            full_values: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
            bbox: [0, 0, 0, 0],
            edge_nodes: DrawVec::new(),
            dropped: FEATURE_DROPPED,
            reduced: false,
            coalesced: false,
            line_detail: 0,
            extra_detail: 0,
            maxzoom: 0,
            spacing: 0.0,
            simplification: 0.0,
            arc_polygon: Vec::new(),
            renamed: 0,
            clustered: 0,
            stringpool: StringPoolRef::default(),
            z: 0,
            tx: 0,
            ty: 0,
        }
    }
}

/// `std::shared_ptr<serial_feature>` in tile.cpp and shared_borders.cpp, where
/// each feature has exactly one owner at a time.
pub type SerialFeaturePtr = Box<SerialFeature>;

/// `struct reader`, the temporary files of one input thread.
///
/// Each input thread writes only to its own reader, but `checkdisk()` reads the
/// sizes of all of them from any thread, so the sizes are atomics outside the
/// lock and the rest is behind a `Mutex` that only its own thread takes.
pub struct Reader {
    pub poolfd: i32,
    pub treefd: i32,
    pub geomfd: i32,
    pub indexfd: i32,
    pub vertexfd: i32,
    pub nodefd: i32,

    pub geompos: AtomicI64,
    pub indexpos: AtomicI64,
    pub vertexpos: AtomicI64,
    pub nodepos: AtomicI64,

    /// `poolfile->off` and `treefile->off`, for checkdisk()
    pub poolfile_off: AtomicI64,
    pub treefile_off: AtomicI64,

    pub state: Mutex<ReaderState>,
}

pub struct ReaderState {
    pub poolfile: Option<Box<Memfile>>,
    pub treefile: Option<Box<Memfile>>,
    pub geomfile: Option<BufWriter<File>>,
    pub indexfile: Option<BufWriter<File>>,
    pub vertexfile: Option<BufWriter<File>>,
    pub nodefile: Option<BufWriter<File>>,

    pub file_bbox: [i64; 4],
    /// standard -180 to 180 world plane
    pub file_bbox1: [i64; 4],
    /// 0 to 360 world plane
    pub file_bbox2: [i64; 4],

    /// `geomst.st_size`
    pub geomst_size: i64,

    pub key_dedup: Vec<isize>,
    pub value_dedup: Vec<isize>,
}

impl Default for ReaderState {
    fn default() -> Self {
        ReaderState {
            poolfile: None,
            treefile: None,
            geomfile: None,
            indexfile: None,
            vertexfile: None,
            nodefile: None,
            file_bbox: [0, 0, 0, 0],
            file_bbox1: [0xFFFFFFFF, 0xFFFFFFFF, 0, 0],
            file_bbox2: [0x1FFFFFFFF, 0xFFFFFFFF, 0x100000000, 0],
            geomst_size: 0,
            key_dedup: vec![-1; 655536],
            value_dedup: vec![-1; 655536],
        }
    }
}

impl Default for Reader {
    fn default() -> Self {
        Reader {
            poolfd: -1,
            treefd: -1,
            geomfd: -1,
            indexfd: -1,
            vertexfd: -1,
            nodefd: -1,
            geompos: AtomicI64::new(0),
            indexpos: AtomicI64::new(0),
            vertexpos: AtomicI64::new(0),
            nodepos: AtomicI64::new(0),
            poolfile_off: AtomicI64::new(0),
            treefile_off: AtomicI64::new(0),
            state: Mutex::new(ReaderState::default()),
        }
    }
}

/// `struct serialization_state`.
///
/// `layer_seq` and the distance sums belong to one input thread, so they are
/// held here by value, and the caller reads them back afterward.
pub struct SerializationState<'a> {
    /// source file name
    pub fname: &'a str,
    /// user-oriented location within source for error reports
    pub line: i32,

    /// sequence within current layer
    pub layer_seq: i64,
    /// overall sequence for progress indicator
    pub progress_seq: &'a AtomicI64,

    /// array of data for each input thread
    pub readers: &'a [Reader],
    /// the current input thread
    pub segment: i32,

    /// relative offset of all geometries
    pub initial_x: &'a AtomicU32,
    pub initial_y: &'a AtomicU32,
    pub initialized: &'a AtomicI32,

    /// running tally for calculation of resolution within features
    pub dist_sum: f64,
    pub dist_count: usize,
    pub area_sum: f64,
    pub want_dist: bool,

    pub maxzoom: i32,
    pub basezoom: i32,

    pub filters: bool,
    pub uses_gamma: bool,

    pub layermap: &'a Mutex<BTreeMap<String, LayermapEntry>>,

    pub attribute_types: &'a HashMap<String, i32>,
    pub exclude: &'a BTreeSet<String>,
    pub include: &'a BTreeSet<String>,
    pub exclude_all: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    // these are scaled geometry,
    // but because scaling is disabled if P_SHARED_NODES is set,
    // they are effectively also world coordinates
    pub p1: Draw,
    pub mid: Draw,
    pub p2: Draw,
}

/// The size of a vertex in the temporary vertex files: the x and y of each of
/// its three points. The C++ writes its memory image, which is also 48 bytes.
pub const VERTEX_SIZE: usize = 48;

impl Vertex {
    pub fn new(one: Draw, joint: Draw, two: Draw) -> Vertex {
        let (p1, p2) = if one < two { (one, two) } else { (two, one) };
        Vertex { p1, mid: joint, p2 }
    }

    pub fn to_bytes(&self) -> [u8; VERTEX_SIZE] {
        let mut out = [0u8; VERTEX_SIZE];
        let words = [
            self.p1.x, self.p1.y, self.mid.x, self.mid.y, self.p2.x, self.p2.y,
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_ne_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Vertex {
        let w = |i: usize| i64::from_ne_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        Vertex {
            p1: Draw::new(VT_MOVETO, w(0), w(1)),
            mid: Draw::new(VT_MOVETO, w(2), w(3)),
            p2: Draw::new(VT_MOVETO, w(4), w(5)),
        }
    }
}

impl PartialEq for Vertex {
    fn eq(&self, v: &Vertex) -> bool {
        self.mid == v.mid && self.p1 == v.p1 && self.p2 == v.p2
    }
}

impl Eq for Vertex {}

impl PartialOrd for Vertex {
    fn partial_cmp(&self, v: &Vertex) -> Option<Ordering> {
        Some(self.cmp(v))
    }
}

impl Ord for Vertex {
    fn cmp(&self, v: &Vertex) -> Ordering {
        self.mid
            .cmp(&v.mid)
            .then(self.p1.cmp(&v.p1))
            .then(self.p2.cmp(&v.p2))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Node {
    // this is in quadkey coordinates so that the nodes for each tile
    // will be adjacent in memory, reducing potential thrashing during
    // the binary search.
    pub index: u64,
}

/// The size of a node in the temporary node files, which are its memory image.
pub const NODE_SIZE: usize = 8;

impl Node {
    pub fn to_bytes(&self) -> [u8; NODE_SIZE] {
        self.index.to_ne_bytes()
    }

    pub fn from_bytes(b: &[u8]) -> Node {
        Node {
            index: u64::from_ne_bytes(b[..NODE_SIZE].try_into().unwrap()),
        }
    }
}

// Offset coordinates to keep them positive
const COORD_OFFSET: i64 = 4i64 << 32;

#[allow(non_snake_case)]
fn SHIFT_RIGHT(a: i64) -> i64 {
    ((a as f64) / ((1i64 << get(&GEOMETRY_SCALE)) as f64)).round() as i64
}

#[allow(non_snake_case)]
fn SHIFT_LEFT(a: i64) -> i64 {
    let geometry_scale = get(&GEOMETRY_SCALE);
    ((a + (COORD_OFFSET >> geometry_scale)) << geometry_scale) - COORD_OFFSET
}

/// `protozero::encode_zigzag64`.
pub fn encode_zigzag64(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((-(((value as u64) >> 63) as i64)) as u64)
}

/// `protozero::decode_zigzag64`.
pub fn decode_zigzag64(value: u64) -> i64 {
    ((value >> 1) ^ ((-((value & 1) as i64)) as u64)) as i64
}

// write to file

pub fn fwrite_check(
    ptr: &[u8],
    size: usize,
    nitems: usize,
    stream: &mut dyn Write,
    fpos: &AtomicI64,
    fname: &str,
) -> usize {
    if let Err(e) = stream.write_all(&ptr[..size * nitems]) {
        eprintln!(
            "{}: Write to temporary file failed: {}",
            fname,
            strerror(e.raw_os_error().unwrap_or(libc::EIO))
        );
        std::process::exit(EXIT_WRITE);
    }
    fpos.fetch_add((size * nitems) as i64, std::sync::atomic::Ordering::Relaxed);
    nitems
}

pub fn fserialize_int(out: &mut dyn Write, n: i32, fpos: &AtomicI64, fname: &str) {
    fserialize_long_long(out, n as i64, fpos, fname);
}

pub fn fserialize_long_long(out: &mut dyn Write, n: i64, fpos: &AtomicI64, fname: &str) {
    let zigzag = encode_zigzag64(n);

    fserialize_ulong_long(out, zigzag, fpos, fname);
}

pub fn fserialize_ulong_long(out: &mut dyn Write, zigzag: u64, fpos: &AtomicI64, fname: &str) {
    let mut buf = [0u8; 10]; // ceil(64 / 7)
    let mut s = 0;
    let mut zigzag = zigzag;

    while zigzag >= 0x80 {
        buf[s] = ((zigzag & 0x7F) | 0x80) as u8;
        s += 1;
        zigzag >>= 7;
    }

    buf[s] = zigzag as u8;
    s += 1;
    fwrite_check(&buf, 1, s, out, fpos, fname);
}

pub fn fserialize_byte(out: &mut dyn Write, n: i8, fpos: &AtomicI64, fname: &str) {
    fwrite_check(&[n as u8], 1, 1, out, fpos, fname);
}

pub fn fserialize_uint(out: &mut dyn Write, n: u32, fpos: &AtomicI64, fname: &str) {
    fserialize_ulong_long(out, n as u64, fpos, fname);
}

// write to memory

pub fn serialize_ulong_long(out: &mut Vec<u8>, zigzag: u64) {
    let mut zigzag = zigzag;

    while zigzag >= 0x80 {
        out.push(((zigzag & 0x7F) | 0x80) as u8);
        zigzag >>= 7;
    }

    out.push(zigzag as u8);
}

pub fn serialize_long_long(out: &mut Vec<u8>, n: i64) {
    let zigzag = encode_zigzag64(n);

    serialize_ulong_long(out, zigzag);
}

pub fn serialize_int(out: &mut Vec<u8>, n: i32) {
    serialize_long_long(out, n as i64);
}

pub fn serialize_byte(out: &mut Vec<u8>, n: i8) {
    out.push(n as u8);
}

pub fn serialize_uint(out: &mut Vec<u8>, n: u32) {
    serialize_ulong_long(out, n as u64);
}

// read from memory

/// The next byte, or the NUL that ends a C++ `std::string` if there is none.
fn next_byte(f: &mut &[u8]) -> u8 {
    match f.split_first() {
        Some((&c, rest)) => {
            *f = rest;
            c
        }
        None => 0,
    }
}

/// `deserialize_int(const char **f, int *n)`: reads a value and advances `f`.
pub fn deserialize_int(f: &mut &[u8]) -> i32 {
    let ll = deserialize_long_long(f);
    ll as i32
}

pub fn deserialize_long_long(f: &mut &[u8]) -> i64 {
    let zigzag = deserialize_ulong_long(f);
    decode_zigzag64(zigzag)
}

pub fn deserialize_ulong_long(f: &mut &[u8]) -> u64 {
    let mut zigzag: u64 = 0;
    let mut shift: u32 = 0;

    loop {
        let c = next_byte(f);
        if (c & 0x80) == 0 {
            zigzag |= (c as u64).wrapping_shl(shift);
            break;
        } else {
            zigzag |= ((c & 0x7F) as u64).wrapping_shl(shift);
            shift += 7;
        }
    }

    zigzag
}

pub fn deserialize_uint(f: &mut &[u8]) -> u32 {
    let v = deserialize_ulong_long(f);
    v as u32
}

pub fn deserialize_byte(f: &mut &[u8]) -> i8 {
    next_byte(f) as i8
}

fn write_geometry(dv: &DrawVec, out: &mut Vec<u8>, wx: i64, wy: i64) {
    let mut wx = wx;
    let mut wy = wy;

    for d in dv {
        if d.op == VT_MOVETO || d.op == VT_LINETO {
            serialize_byte(out, d.op);
            serialize_long_long(out, d.x - wx);
            serialize_long_long(out, d.y - wy);
            wx = d.x;
            wy = d.y;
        } else {
            serialize_byte(out, d.op);
        }
    }
    serialize_byte(out, VT_END);
}

const FLAG_LAYER: i32 = 7;

const FLAG_LABEL_POINT: i32 = 6;
const FLAG_INDEX: i32 = 4;
const FLAG_EXTENT: i32 = 3;
const FLAG_ID: i32 = 2;
const FLAG_MINZOOM: i32 = 1;
const FLAG_MAXZOOM: i32 = 0;

/// `serialize_feature(serial_feature *sf, long long wx, long long wy)`.
// called from generating the next zoom level
pub fn serialize_feature(sf: &mut SerialFeature, wx: i64, wy: i64) -> Vec<u8> {
    let mut s: Vec<u8> = Vec::new();

    serialize_byte(&mut s, sf.t);

    let mut layer: i64 = 0;
    layer |= sf.layer.wrapping_shl(FLAG_LAYER as u32);
    layer |= ((sf.label_point != 0) as i64) << FLAG_LABEL_POINT;
    layer |= ((sf.index != 0) as i64) << FLAG_INDEX;
    layer |= ((sf.extent != 0) as i64) << FLAG_EXTENT;
    layer |= (sf.has_id as i64) << FLAG_ID;
    layer |= ((sf.tippecanoe_minzoom != -1) as i64) << FLAG_MINZOOM;
    layer |= ((sf.tippecanoe_maxzoom != -1) as i64) << FLAG_MAXZOOM;

    serialize_long_long(&mut s, layer);
    serialize_long_long(&mut s, sf.seq);
    if sf.tippecanoe_minzoom != -1 {
        serialize_int(&mut s, sf.tippecanoe_minzoom);
    }
    if sf.tippecanoe_maxzoom != -1 {
        serialize_int(&mut s, sf.tippecanoe_maxzoom);
    }
    if sf.has_id {
        serialize_ulong_long(&mut s, sf.id);
    }

    serialize_int(&mut s, sf.segment);

    write_geometry(&sf.geometry, &mut s, wx, wy);

    if sf.index != 0 {
        serialize_ulong_long(&mut s, sf.index);
        serialize_ulong_long(&mut s, sf.gap);
    }
    if sf.label_point != 0 {
        serialize_ulong_long(&mut s, sf.label_point);
    }
    if sf.extent != 0 {
        serialize_long_long(&mut s, sf.extent);
    }

    serialize_long_long(&mut s, sf.keys.len() as i64);

    for i in 0..sf.keys.len() {
        serialize_long_long(&mut s, sf.keys[i]);
        serialize_long_long(&mut s, sf.values[i]);
    }

    // MAGIC: This knows that the feature minzoom is the last byte of the feature,
    serialize_byte(&mut s, sf.feature_minzoom);
    s
}

/// `initial_x` and `initial_y` are indexed by the feature's segment.
pub fn deserialize_feature(
    geoms: &[u8],
    z: u32,
    tx: u32,
    ty: u32,
    initial_x: &[u32],
    initial_y: &[u32],
) -> SerialFeature {
    let mut sf = SerialFeature::default();
    let mut cp: &[u8] = geoms;

    sf.t = deserialize_byte(&mut cp);
    sf.layer = deserialize_long_long(&mut cp);

    sf.seq = 0;
    sf.seq = deserialize_long_long(&mut cp);

    sf.tippecanoe_minzoom = -1;
    sf.tippecanoe_maxzoom = -1;
    sf.id = 0;
    sf.has_id = false;
    if sf.layer & (1 << FLAG_MINZOOM) != 0 {
        sf.tippecanoe_minzoom = deserialize_int(&mut cp);
    }
    if sf.layer & (1 << FLAG_MAXZOOM) != 0 {
        sf.tippecanoe_maxzoom = deserialize_int(&mut cp);
    }
    if sf.layer & (1 << FLAG_ID) != 0 {
        sf.has_id = true;
        sf.id = deserialize_ulong_long(&mut cp);
    }

    sf.segment = deserialize_int(&mut cp);

    sf.index = 0;
    sf.gap = 0;
    sf.label_point = 0;
    sf.extent = 0;

    sf.geometry = decode_geometry(
        &mut cp,
        z as i32,
        tx,
        ty,
        &mut sf.bbox,
        initial_x[sf.segment as usize],
        initial_y[sf.segment as usize],
    );

    if sf.layer & (1 << FLAG_INDEX) != 0 {
        sf.index = deserialize_ulong_long(&mut cp);
        sf.gap = deserialize_ulong_long(&mut cp);
    }
    if sf.layer & (1 << FLAG_LABEL_POINT) != 0 {
        sf.label_point = deserialize_ulong_long(&mut cp);
    }
    if sf.layer & (1 << FLAG_EXTENT) != 0 {
        sf.extent = deserialize_long_long(&mut cp);
    }

    sf.layer >>= FLAG_LAYER;

    let count = deserialize_long_long(&mut cp);

    for _ in 0..count {
        let k = deserialize_long_long(&mut cp);
        let v = deserialize_long_long(&mut cp);
        sf.keys.push(k);
        sf.values.push(v);
    }

    // MAGIC: This knows that the feature minzoom is the last byte of the feature.
    sf.feature_minzoom = deserialize_byte(&mut cp);

    if !cp.is_empty() {
        eprintln!(
            "wrong length decoding feature: used {}, len is {}",
            geoms.len() - cp.len(),
            geoms.len()
        );
        std::process::exit(EXIT_IMPOSSIBLE);
    }

    sf
}

fn scale_geometry(sst: &SerializationState<'_>, bbox: &mut [i64; 4], geom: &mut DrawVec) -> i64 {
    let mut offset: i64 = 0;
    let mut prev: i64 = 0;
    let mut has_prev = false;
    let scale = 1.0 / ((1 << get(&GEOMETRY_SCALE)) as f64);

    for g in geom.iter_mut() {
        if g.op == VT_MOVETO || g.op == VT_LINETO {
            let mut x = g.x;
            let y = g.y;

            if additional(A_DETECT_WRAPAROUND) != 0 {
                if g.op == VT_LINETO {
                    x += offset;
                    if has_prev {
                        // jumps at least 180° but not exactly 360°,
                        // which in some data sets is an intentional
                        // line across the world
                        if x - prev > (1i64 << 31) && x - prev != (1i64 << 32) {
                            offset -= 1i64 << 32;
                            x -= 1i64 << 32;
                        } else if prev - x > (1i64 << 31) && prev - x != (1i64 << 32) {
                            offset += 1i64 << 32;
                            x += 1i64 << 32;
                        }
                    }

                    has_prev = true;
                    prev = x;
                } else {
                    offset = 0;
                    prev = x;
                }
            }

            if x < bbox[0] {
                bbox[0] = x;
            }
            if y < bbox[1] {
                bbox[1] = y;
            }
            if x > bbox[2] {
                bbox[2] = x;
            }
            if y > bbox[3] {
                bbox[3] = y;
            }

            if sst.initialized.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                if x < 0 || x >= (1i64 << 32) || y < 0 || y >= (1i64 << 32) {
                    sst.initial_x
                        .store((1i64 << 31) as u32, std::sync::atomic::Ordering::Relaxed);
                    sst.initial_y
                        .store((1i64 << 31) as u32, std::sync::atomic::Ordering::Relaxed);
                } else {
                    sst.initial_x.store(
                        SHIFT_LEFT(SHIFT_RIGHT(x)) as u32,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    sst.initial_y.store(
                        SHIFT_LEFT(SHIFT_RIGHT(y)) as u32,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }

                sst.initialized
                    .store(1, std::sync::atomic::Ordering::Relaxed);
            }

            if additional(A_GRID_LOW_ZOOMS) != 0 {
                // If we are gridding, snap to the maxzoom grid in case the incoming data
                // is already supposed to be aligned to tile boundaries (but is not, exactly,
                // because of rounding error during projection).

                g.x = ((x as f64) * scale).round() as i64;
                g.y = ((y as f64) * scale).round() as i64;
            } else {
                g.x = SHIFT_RIGHT(x);
                g.y = SHIFT_RIGHT(y);
            }
        }
    }

    geom.len() as i64
}

fn strip_zeroes(s: &str) -> &str {
    // Doesn't do anything special with '-' followed by leading zeros
    // since integer IDs must be positive

    s.trim_start_matches('0')
}

pub fn nodecmp(a: &Node, b: &Node) -> Ordering {
    if a.index < b.index {
        Ordering::Less
    } else if a.index > b.index {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn add_scaled_node(r: &Reader, rs: &mut ReaderState, sst: &SerializationState<'_>, g: Draw) {
    let x = SHIFT_LEFT(g.x);
    let y = SHIFT_LEFT(g.y);

    let n = Node {
        index: encode_vertex(x as u32, y as u32),
    };

    fwrite_check(
        &n.to_bytes(),
        NODE_SIZE,
        1,
        rs.nodefile.as_mut().unwrap(),
        &r.nodepos,
        sst.fname,
    );
}

/// The length of the C string that `s.c_str()` would give.
fn cstr_len(s: &str) -> usize {
    s.bytes().position(|c| c == 0).unwrap_or(s.len())
}

/// `std::to_string(double)`, which formats with `%f`.
pub fn to_string_double(d: f64) -> String {
    if d.is_nan() {
        if d.is_sign_negative() {
            "-nan".to_string()
        } else {
            "nan".to_string()
        }
    } else if d.is_infinite() {
        if d < 0.0 {
            "-inf".to_string()
        } else {
            "inf".to_string()
        }
    } else {
        format!("{:.6}", d)
    }
}

static WARNED_ID_NOT_NUMBER: AtomicBool = AtomicBool::new(false);
static WARNED_ID_FRAC: AtomicBool = AtomicBool::new(false);
static WARNED_ID_TOO_LARGE: AtomicBool = AtomicBool::new(false);

/// `serialize_feature(struct serialization_state *sst, serial_feature &sf, std::string const &layername)`.
// called from frontends
pub fn serialize_feature_sst(
    sst: &mut SerializationState<'_>,
    sf: &mut SerialFeature,
    layername: &str,
) -> i32 {
    use std::sync::atomic::Ordering::Relaxed;

    let readers: &[Reader] = sst.readers;
    let r: &Reader = &readers[sst.segment as usize];
    let mut r_state = r.state.lock().unwrap();
    let rs: &mut ReaderState = &mut r_state;
    let mut key_pool = KeyPool::default();

    sf.bbox[0] = i64::MAX;
    sf.bbox[1] = i64::MAX;
    sf.bbox[2] = i64::MIN;
    sf.bbox[3] = i64::MIN;

    for g in &sf.geometry {
        if g.op == VT_MOVETO || g.op == VT_LINETO {
            // standard -180 to 180 world plane
            //
            // mask X, since it wraps around
            // pin Y, since it doesn't

            let mut x = g.x & 0xFFFFFFFF;
            let y = std::cmp::max(std::cmp::min(g.y, 0xFFFFFFFF), 0);

            {
                rs.file_bbox1[0] = std::cmp::min(rs.file_bbox1[0], x);
                rs.file_bbox1[1] = std::cmp::min(rs.file_bbox1[1], y);
                rs.file_bbox1[2] = std::cmp::max(rs.file_bbox1[2], x);
                rs.file_bbox1[3] = std::cmp::max(rs.file_bbox1[3], y);

                // printf("%llx,%llx  %llx,%llx %llx,%llx  ", x, y, r->file_bbox1[0], r->file_bbox1[1], r->file_bbox1[2], r->file_bbox1[3]);

                // shift the western hemisphere 360 degrees to the east
                if x < 0x80000000 {
                    // prime meridian
                    x += 0x100000000;
                }

                rs.file_bbox2[0] = std::cmp::min(rs.file_bbox2[0], x);
                rs.file_bbox2[1] = std::cmp::min(rs.file_bbox2[1], y);
                rs.file_bbox2[2] = std::cmp::max(rs.file_bbox2[2], x);
                rs.file_bbox2[3] = std::cmp::max(rs.file_bbox2[3], y);
            }
        }
    }

    // try to remind myself that the geometry in this function is in SCALED COORDINATES
    let mut scaled_geometry: DrawVec = std::mem::take(&mut sf.geometry);
    scale_geometry(sst, &mut sf.bbox, &mut scaled_geometry);

    // This has to happen after scaling so that the wraparound detection has happened first.
    // Otherwise the inner/outer calculation will be confused by bad geometries.
    if sf.t as i32 == VT_POLYGON {
        scaled_geometry = fix_polygon(
            &scaled_geometry,
            prevent(P_USE_SOURCE_POLYGON_WINDING) != 0,
            prevent(P_REVERSE_SOURCE_POLYGON_WINDING) != 0,
        );
    }

    for c in CLIPBBOXES.read().unwrap().iter() {
        if sf.t as i32 == VT_POLYGON {
            scaled_geometry = simple_clip_poly_bbox(
                &mut scaled_geometry,
                SHIFT_RIGHT(c.minx),
                SHIFT_RIGHT(c.miny),
                SHIFT_RIGHT(c.maxx),
                SHIFT_RIGHT(c.maxy),
                prevent(P_SIMPLIFY_SHARED_NODES) != 0,
            );
        } else if sf.t as i32 == VT_LINE {
            scaled_geometry = clip_lines_bbox(
                &mut scaled_geometry,
                SHIFT_RIGHT(c.minx),
                SHIFT_RIGHT(c.miny),
                SHIFT_RIGHT(c.maxx),
                SHIFT_RIGHT(c.maxy),
            );
        } else if sf.t as i32 == VT_POINT {
            scaled_geometry = clip_point_bbox(
                &mut scaled_geometry,
                SHIFT_RIGHT(c.minx),
                SHIFT_RIGHT(c.miny),
                SHIFT_RIGHT(c.maxx),
                SHIFT_RIGHT(c.maxy),
            );
        }

        scaled_geometry = remove_noop(scaled_geometry, sf.t as i32, 0);

        sf.bbox[0] = i64::MAX;
        sf.bbox[1] = i64::MAX;
        sf.bbox[2] = i64::MIN;
        sf.bbox[3] = i64::MIN;

        for g in &scaled_geometry {
            let x = SHIFT_LEFT(g.x);
            let y = SHIFT_LEFT(g.y);

            if x < sf.bbox[0] {
                sf.bbox[0] = x;
            }
            if y < sf.bbox[1] {
                sf.bbox[1] = y;
            }
            if x > sf.bbox[2] {
                sf.bbox[2] = x;
            }
            if y > sf.bbox[3] {
                sf.bbox[3] = y;
            }
        }
    }

    if scaled_geometry.is_empty() {
        // Feature was clipped away
        return 1;
    }

    if prevent(P_SIMPLIFY_SHARED_NODES) != 0 {
        scaled_geometry = remove_noop(scaled_geometry, sf.t as i32, 0);

        if sf.t as i32 == VT_POLYGON || sf.t as i32 == VT_LINE {
            let mut i = 0;
            while i < scaled_geometry.len() {
                if scaled_geometry[i].op == VT_MOVETO {
                    let mut j = i + 1;
                    while j < scaled_geometry.len() {
                        if scaled_geometry[j].op != VT_LINETO {
                            break;
                        }
                        j += 1;
                    }

                    if sf.t as i32 == VT_POLYGON && j - i >= 4 {
                        for k in i..j - 1 {
                            // % (j - i - 1) because we don't want the duplicate last point

                            let v = Vertex::new(
                                scaled_geometry[(k - i) % (j - i - 1) + i],
                                scaled_geometry[(k - i + 1) % (j - i - 1) + i],
                                scaled_geometry[(k - i + 2) % (j - i - 1) + i],
                            );

                            fwrite_check(
                                &v.to_bytes(),
                                VERTEX_SIZE,
                                1,
                                rs.vertexfile.as_mut().unwrap(),
                                &r.vertexpos,
                                sst.fname,
                            );
                        }
                    } else if sf.t as i32 == VT_LINE && j - i >= 2 {
                        let mut k = i;
                        while k + 2 < j {
                            let v = Vertex::new(
                                scaled_geometry[k],
                                scaled_geometry[k + 1],
                                scaled_geometry[k + 2],
                            );

                            fwrite_check(
                                &v.to_bytes(),
                                VERTEX_SIZE,
                                1,
                                rs.vertexfile.as_mut().unwrap(),
                                &r.vertexpos,
                                sst.fname,
                            );
                            k += 1;
                        }
                    }

                    // since the starting point is never simplified away,
                    // don't let it be simplified away in any other polygons either.
                    // Needs to appear twice here so that the check below will see
                    // it as appearing in multiple features.
                    add_scaled_node(r, rs, sst, scaled_geometry[i]);

                    if sf.t as i32 == VT_LINE && j - i >= 2 {
                        // linestrings also need to preserve the last point

                        add_scaled_node(r, rs, sst, scaled_geometry[j - 1]);
                    } else if sf.t as i32 == VT_POLYGON && j - i >= 4 {
                        // To avoid letting polygons get simplified away to nothing,
                        // also keep the furthest-away point from the initial point
                        // (which Douglas-Peucker simplification would keep anyway,
                        // if its search weren't being split up by polygon side).

                        let mut far: f64 = 0.0;
                        let mut which = i;
                        for k in i + 1..j - 1 {
                            let xd = (scaled_geometry[k].x - scaled_geometry[i].x) as f64;
                            let yd = (scaled_geometry[k].y - scaled_geometry[i].y) as f64;
                            let d = xd * xd + yd * yd;
                            if d > far
                                || ((d == far) && (scaled_geometry[k] < scaled_geometry[which]))
                            {
                                far = d;
                                which = k;
                            }
                        }

                        add_scaled_node(r, rs, sst, scaled_geometry[which]);

                        // And, likewise, the point most distant from those two points,
                        // which probably would also be the one that Douglas-Peucker
                        // would keep next.

                        far = 0.0;
                        let mut which2 = i;

                        for k in i + 1..j - 1 {
                            let d = distance_from_line(
                                scaled_geometry[k].x,
                                scaled_geometry[k].y,
                                scaled_geometry[i].x,
                                scaled_geometry[i].y,
                                scaled_geometry[which].x,
                                scaled_geometry[which].y,
                            );
                            if (d > far)
                                || ((d == far) && (scaled_geometry[k] < scaled_geometry[which2]))
                            {
                                far = d;
                                which2 = k;
                            }
                        }

                        add_scaled_node(r, rs, sst, scaled_geometry[which2]);
                    }

                    i = j - 1;
                }
                i += 1;
            }
        }
    }

    if !sf.has_id && additional(A_GENERATE_IDS) != 0 {
        sf.has_id = true;
        sf.id = (sf.seq + 1) as u64;
    }

    if sst.want_dist {
        let mut locs: Vec<u64> = Vec::new();
        for g in &scaled_geometry {
            if g.op == VT_MOVETO || g.op == VT_LINETO {
                locs.push(encode_index(SHIFT_LEFT(g.x) as u32, SHIFT_LEFT(g.y) as u32));
            }
        }
        locs.sort();
        let mut n: usize = 0;
        let mut sum: f64 = 0.0;
        for i in 1..locs.len() {
            if locs[i - 1] != locs[i] {
                sum += ((locs[i] - locs[i - 1]) as f64).ln();
                n += 1;
            }
        }
        if n > 0 {
            let avg = (sum / n as f64).exp();
            // Convert approximately from tile units to feet
            // See comment about empirical data in main.cpp
            let dist_ft = avg.sqrt() / 33.0;

            sst.dist_sum += dist_ft.ln() * n as f64;
            sst.dist_count += n;
        }
        locs.clear();
    }

    let mut extent: f64 = 0.0;
    if additional(A_DROP_SMALLEST_AS_NEEDED) != 0
        || additional(A_COALESCE_SMALLEST_AS_NEEDED) != 0
        || get(&ORDER_BY_SIZE_ENABLED)
        || sst.want_dist
    {
        if sf.t as i32 == VT_POLYGON {
            let mut i = 0;
            while i < scaled_geometry.len() {
                if scaled_geometry[i].op == VT_MOVETO {
                    let mut j = i + 1;
                    while j < scaled_geometry.len() {
                        if scaled_geometry[j].op != VT_LINETO {
                            break;
                        }
                        j += 1;
                    }

                    extent += (SHIFT_LEFT(SHIFT_LEFT(1)) as f64) * get_area(&scaled_geometry, i, j);
                    i = j - 1;
                }
                i += 1;
            }
        } else if sf.t as i32 == VT_LINE {
            let mut dist: f64 = 0.0;
            for i in 1..scaled_geometry.len() {
                if scaled_geometry[i].op == VT_LINETO {
                    let xd = SHIFT_LEFT(scaled_geometry[i].x - scaled_geometry[i - 1].x) as f64;
                    let yd = SHIFT_LEFT(scaled_geometry[i].y - scaled_geometry[i - 1].y) as f64;
                    dist += (xd * xd + yd * yd).sqrt();
                }
            }
            // treat lines as having the area of a circle with the line as diameter
            extent = std::f64::consts::PI * (dist / 2.0) * (dist / 2.0);
        }

        // VT_POINT extent will be calculated in write_tile from the distance between adjacent features.
    }

    // Clamp before converting, since converting a double that is out of range
    // for a long long is undefined. The bounds are asymmetric: LLONG_MAX is not
    // representable as a double and rounds up to 2^63, so the upper bound has to
    // be exclusive, while LLONG_MIN is exactly -2^63 and so can be included.
    // Areas are signed, so holes that outweigh their rings can make this
    // negative.
    if extent >= i64::MIN as f64 && extent < i64::MAX as f64 {
        sf.extent = extent as i64;
    } else if extent < 0.0 {
        sf.extent = i64::MIN;
    } else {
        sf.extent = i64::MAX; // also the NaN case
    }

    if sst.want_dist && sf.t as i32 == VT_POLYGON {
        sst.area_sum += extent;
    }

    let midx: i64;
    let midy: i64;

    if sf.t as i32 == VT_POINT {
        // keep old behavior, which loses one bit of precision at the bottom
        midx = (sf.bbox[0] / 2 + sf.bbox[2] / 2) & ((1i64 << 32) - 1);
        midy = (sf.bbox[1] / 2 + sf.bbox[3] / 2) & ((1i64 << 32) - 1);
    } else if (additional(A_DROP_DENSEST_AS_NEEDED) != 0
        || additional(A_COALESCE_DENSEST_AS_NEEDED) != 0)
        && sf.t as i32 == VT_POLYGON
    {
        // This probably should really apply to all polygons,
        // but I hate to change the feature sequence in all the
        // test fixtures again
        let scaled_center = center_of_mass_mp(&scaled_geometry);
        midx = SHIFT_LEFT(scaled_center.x) & ((1i64 << 32) - 1);
        midy = SHIFT_LEFT(scaled_center.y) & ((1i64 << 32) - 1);
    } else {
        // To reduce the chances of giving multiple polygons or linestrings
        // the same index, use an arbitrary but predictable point from the
        // geometry as the index point rather than the bounding box center
        // as was previously used. The index point chosen comes from a hash
        // of the overall geometry, so features with the same geometry will
        // still have the same index. Specifically this avoids guessing
        // too high a maxzoom for a data source that has a large number of
        // LineStrings that map essentially the same route but with slight
        // jitter between them, even though the geometries themselves are
        // not very detailed.
        let mut ix: usize = 0;
        for g in &scaled_geometry {
            ix = ix.wrapping_add((g.x + g.y) as usize);
        }
        if !scaled_geometry.is_empty() {
            ix %= scaled_geometry.len();
        }

        // If off the edge of the plane, mask to bring it back into the addressable area
        midx = SHIFT_LEFT(scaled_geometry[ix].x) & ((1i64 << 32) - 1);
        midy = SHIFT_LEFT(scaled_geometry[ix].y) & ((1i64 << 32) - 1);
    }

    let bbox_index: u64 = encode_index(midx as u32, midy as u32);
    if additional(A_CALCULATE_INDEX) != 0 {
        sf.full_keys.push(key_pool.pool("tippecanoe:index"));

        let sv = SerialVal {
            type_: MVT_DOUBLE,
            s: bbox_index.to_string(),
        };
        sf.full_values.push(sv);
    }

    if sf.t as i32 == VT_POLYGON && additional(A_GENERATE_POLYGON_LABEL_POINTS) != 0 {
        let mut dv = polygon_to_anchor(&scaled_geometry);
        if !dv.is_empty() {
            dv[0].x = SHIFT_LEFT(dv[0].x) & ((1i64 << 32) - 1);
            dv[0].y = SHIFT_LEFT(dv[0].y) & ((1i64 << 32) - 1);
            sf.label_point = encode_index(dv[0].x as u32, dv[0].y as u32);
        }
    }

    if additional(A_DROP_DENSEST_AS_NEEDED) != 0
        || additional(A_COALESCE_DENSEST_AS_NEEDED) != 0
        || additional(A_CLUSTER_DENSEST_AS_NEEDED) != 0
        || additional(A_CALCULATE_FEATURE_DENSITY) != 0
        || additional(A_DROP_SMALLEST_AS_NEEDED) != 0
        || additional(A_COALESCE_SMALLEST_AS_NEEDED) != 0
        || additional(A_DROP_FRACTION_AS_NEEDED) != 0
        || additional(A_COALESCE_FRACTION_AS_NEEDED) != 0
        || prevent(P_DYNAMIC_DROP) != 0
        || additional(A_INCREASE_GAMMA_AS_NEEDED) != 0
        || additional(A_GENERATE_POLYGON_LABEL_POINTS) != 0
        || sst.uses_gamma
        || get(&RETAIN_POINTS_MULTIPLIER) > 1
        || get(&PRESERVE_MULTIPLIER_DENSITY_THRESHOLD) > 0
        || get(&CLUSTER_DISTANCE) != 0
    {
        sf.index = bbox_index;
    } else {
        sf.index = 0;
    }

    {
        let mut layermap = sst.layermap.lock().unwrap();

        if !layermap.contains_key(layername) {
            let n = layermap.len();
            layermap.insert(layername.to_string(), LayermapEntry::new(n));
        }

        match layermap.get_mut(layername) {
            Some(ai) => {
                sf.layer = ai.id as i64;

                if !sst.filters {
                    if sf.t as i32 == VT_POINT {
                        ai.points += 1;
                    } else if sf.t as i32 == VT_LINE {
                        ai.lines += 1;
                    } else if sf.t as i32 == VT_POLYGON {
                        ai.polygons += 1;
                    }
                }
            }
            None => {
                eprintln!("Internal error: can't find layer name {}", layername);
                std::process::exit(EXIT_IMPOSSIBLE);
            }
        }
    }

    for (k, v) in SET_ATTRIBUTES.read().unwrap().iter() {
        let mut found = false;
        for i in 0..sf.full_keys.len() {
            if *sf.full_keys[i] == *k {
                sf.full_values[i] = v.clone();
                found = true;
                break;
            }
        }

        if !found {
            sf.full_keys.push(key_pool.pool(k));
            sf.full_values.push(v.clone());
        }
    }

    let attribute_for_id = ATTRIBUTE_FOR_ID.read().unwrap().clone();

    let mut i = sf.full_keys.len() as isize - 1;
    while i >= 0 {
        let iu = i as usize;
        {
            let fv = &mut sf.full_values[iu];
            coerce_value(
                &sf.full_keys[iu],
                &mut fv.type_,
                &mut fv.s,
                sst.attribute_types,
            );
        }

        if prevent(P_SINGLE_PRECISION) != 0 && sf.full_values[iu].type_ == MVT_DOUBLE {
            // don't coerce integers to floats, since that is counterproductive
            if sf.full_values[iu].s.contains('.') {
                sf.full_values[iu].s = dtoa_milo(atof(&sf.full_values[iu].s) as f32 as f64);
            }
        }

        if *sf.full_keys[iu] == attribute_for_id {
            if sf.full_values[iu].type_ != MVT_DOUBLE && additional(A_CONVERT_NUMERIC_IDS) == 0 {
                if !WARNED_ID_NOT_NUMBER.load(Relaxed) {
                    eprintln!(
                        "Warning: Attribute \"{}\"=\"{}\" as feature ID is not a number",
                        cstr(&sf.full_keys[iu]),
                        cstr(&sf.full_values[iu].s)
                    );
                    WARNED_ID_NOT_NUMBER.store(true, Relaxed);
                }
            } else {
                let s = &sf.full_values[iu].s;
                let (v, consumed) = strtoull(s, 10);
                let id_value = v as i64;

                if consumed < cstr_len(s) {
                    if !WARNED_ID_FRAC.load(Relaxed) {
                        eprintln!(
                            "Warning: Can't represent non-integer feature ID {}",
                            cstr(s)
                        );
                        WARNED_ID_FRAC.store(true, Relaxed);
                    }
                } else if id_value.to_string() != strip_zeroes(s) {
                    if !WARNED_ID_TOO_LARGE.load(Relaxed) {
                        eprintln!("Warning: Can't represent too-large feature ID {}", cstr(s));
                        WARNED_ID_TOO_LARGE.store(true, Relaxed);
                    }
                } else {
                    sf.id = id_value as u64;
                    sf.has_id = true;

                    sf.full_keys.remove(iu);
                    sf.full_values.remove(iu);
                    i -= 1;
                    continue;
                }
            }
        }

        if sst.exclude_all != 0 {
            if !sst.include.contains(&*sf.full_keys[iu]) {
                sf.full_keys.remove(iu);
                sf.full_values.remove(iu);
                i -= 1;
                continue;
            }
        } else if sst.exclude.contains(&*sf.full_keys[iu]) {
            sf.full_keys.remove(iu);
            sf.full_values.remove(iu);
            i -= 1;
            continue;
        }

        i -= 1;
    }

    if !sst.filters {
        let mut layermap = sst.layermap.lock().unwrap();
        for i in 0..sf.full_keys.len() {
            let ts = layermap.get_mut(layername).unwrap();
            add_to_tilestats(&mut ts.tilestats, &sf.full_keys[i], &sf.full_values[i]);
        }
    }

    let maximum_string_attribute_length = get(&MAXIMUM_STRING_ATTRIBUTE_LENGTH);
    if maximum_string_attribute_length > 0 {
        for v in sf.full_values.iter_mut() {
            if v.type_ == MVT_STRING && v.s.len() > maximum_string_attribute_length {
                v.s = truncate_string(&v.s, maximum_string_attribute_length);
            }
        }
    }

    {
        let ReaderState {
            poolfile,
            treefile,
            key_dedup,
            value_dedup,
            ..
        } = rs;
        let poolfile = poolfile.as_mut().unwrap();
        let treefile = treefile.as_mut().unwrap();

        for i in 0..sf.full_keys.len() {
            sf.keys.push(addpool(
                poolfile,
                treefile,
                sf.full_keys[i].as_bytes(),
                MVT_STRING as i8,
                key_dedup,
            ));
            sf.values.push(addpool(
                poolfile,
                treefile,
                sf.full_values[i].s.as_bytes(),
                sf.full_values[i].type_ as i8,
                value_dedup,
            ));
        }

        r.poolfile_off.store(poolfile.off as i64, Relaxed);
        r.treefile_off.store(treefile.off as i64, Relaxed);
    }

    let geomstart = r.geompos.load(Relaxed);
    sf.geometry = scaled_geometry;

    let feature = serialize_feature(
        sf,
        SHIFT_RIGHT(sst.initial_x.load(Relaxed) as i64),
        SHIFT_RIGHT(sst.initial_y.load(Relaxed) as i64),
    );
    {
        let geomfile = rs.geomfile.as_mut().unwrap();
        fserialize_long_long(geomfile, feature.len() as i64, &r.geompos, sst.fname);
        fwrite_check(&feature, 1, feature.len(), geomfile, &r.geompos, sst.fname);
    }

    let mut index = Index::default();
    index.start = geomstart;
    index.end = r.geompos.load(Relaxed);
    index.set_segment(sst.segment as i16);
    index.set_seq(sst.layer_seq as u64);
    index.set_t(sf.t as u16);
    index.ix = bbox_index;

    fwrite_check(
        &index.to_bytes(),
        INDEX_SIZE,
        1,
        rs.indexfile.as_mut().unwrap(),
        &r.indexpos,
        sst.fname,
    );

    for i in 0..2 {
        if sf.bbox[i] < rs.file_bbox[i] {
            rs.file_bbox[i] = sf.bbox[i];
        }
    }
    for i in 2..4 {
        if sf.bbox[i] > rs.file_bbox[i] {
            rs.file_bbox[i] = sf.bbox[i];
        }
    }

    drop(r_state);

    if sst.progress_seq.load(Relaxed) % 10000 == 0 {
        checkdisk(sst.readers);
        if get(&QUIET) == 0 && get(&QUIET_PROGRESS) == 0 && progress_time() {
            eprint!(
                "Read {:.2} million features\r",
                sst.progress_seq.load(Relaxed) as f64 / 1000000.0
            );
        }
    }
    sst.progress_seq.fetch_add(1, Relaxed);
    sst.layer_seq += 1;

    1
}

/// What printing `s.c_str()` with `%s` shows: the string up to its first NUL.
fn cstr(s: &str) -> &str {
    &s[..cstr_len(s)]
}

pub fn coerce_value(
    key: &str,
    vt: &mut i32,
    val: &mut String,
    attribute_types: &HashMap<String, i32>,
) {
    if let Some(&a) = attribute_types.get(key) {
        if a == MVT_STRING {
            *vt = MVT_STRING;
        } else if a == MVT_FLOAT {
            *vt = MVT_DOUBLE;
            *val = dtoa_milo(atof(val));
        } else if a == MVT_INT {
            *vt = MVT_DOUBLE;
            if val.is_empty() {
                *val = "0".to_string();
            }

            for c in val.bytes() {
                if !c.is_ascii_digit() {
                    *val = to_string_double(atof(val).round());
                    break;
                }
            }
        } else if a == MVT_BOOL {
            if val == "false"
                || val == "0"
                || val == "null"
                || val.is_empty()
                || (*vt == MVT_DOUBLE && atof(val) == 0.0)
            {
                *vt = MVT_BOOL;
                *val = "false".to_string();
            } else {
                *vt = MVT_BOOL;
                *val = "true".to_string();
            }
        } else {
            eprintln!("Can't happen: attribute type {}", a);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        let mut out = Vec::new();
        let values: [i64; 9] = [0, 1, -1, 63, -64, 64, 1 << 40, i64::MAX, i64::MIN];
        for &v in &values {
            serialize_long_long(&mut out, v);
        }
        serialize_uint(&mut out, u32::MAX);
        serialize_ulong_long(&mut out, u64::MAX);
        serialize_byte(&mut out, -3);
        serialize_int(&mut out, -12345);

        let mut f: &[u8] = &out;
        for &v in &values {
            assert_eq!(deserialize_long_long(&mut f), v);
        }
        assert_eq!(deserialize_uint(&mut f), u32::MAX);
        assert_eq!(deserialize_ulong_long(&mut f), u64::MAX);
        assert_eq!(deserialize_byte(&mut f), -3);
        assert_eq!(deserialize_int(&mut f), -12345);
        assert!(f.is_empty());

        // The same bytes as the file forms
        let pos = AtomicI64::new(0);
        let mut file_out: Vec<u8> = Vec::new();
        for &v in &values {
            fserialize_long_long(&mut file_out, v, &pos, "test");
        }
        fserialize_uint(&mut file_out, u32::MAX, &pos, "test");
        fserialize_ulong_long(&mut file_out, u64::MAX, &pos, "test");
        fserialize_byte(&mut file_out, -3, &pos, "test");
        fserialize_int(&mut file_out, -12345, &pos, "test");
        assert_eq!(file_out, out);
        assert_eq!(
            pos.load(std::sync::atomic::Ordering::Relaxed),
            out.len() as i64
        );

        // zigzag as protozero does it
        assert_eq!(encode_zigzag64(0), 0);
        assert_eq!(encode_zigzag64(-1), 1);
        assert_eq!(encode_zigzag64(1), 2);
        assert_eq!(encode_zigzag64(i64::MIN), u64::MAX);
        assert_eq!(decode_zigzag64(u64::MAX), i64::MIN);
    }

    #[test]
    fn feature_header_round_trip() {
        // A feature with no geometry keeps the serialized header and trailer
        // apart from decode_geometry, which another module provides.
        let mut sf = SerialFeature {
            t: 3,
            layer: 5,
            seq: 1234567,
            tippecanoe_minzoom: 2,
            tippecanoe_maxzoom: -1,
            has_id: true,
            id: 99,
            segment: 1,
            index: 77,
            gap: 3,
            label_point: 0,
            extent: -42,
            keys: vec![1, 2],
            values: vec![3, 4],
            feature_minzoom: 6,
            ..Default::default()
        };
        let s = serialize_feature(&mut sf, 0, 0);

        let mut cp: &[u8] = &s;
        assert_eq!(deserialize_byte(&mut cp), 3);
        let layer = deserialize_long_long(&mut cp);
        assert_eq!(layer >> FLAG_LAYER, 5);
        assert_ne!(layer & (1 << FLAG_ID), 0);
        assert_ne!(layer & (1 << FLAG_MINZOOM), 0);
        assert_eq!(layer & (1 << FLAG_MAXZOOM), 0);
        assert_ne!(layer & (1 << FLAG_INDEX), 0);
        assert_eq!(layer & (1 << FLAG_LABEL_POINT), 0);
        assert_ne!(layer & (1 << FLAG_EXTENT), 0);
        assert_eq!(deserialize_long_long(&mut cp), 1234567);
        assert_eq!(deserialize_int(&mut cp), 2);
        assert_eq!(deserialize_ulong_long(&mut cp), 99);
        assert_eq!(deserialize_int(&mut cp), 1);
        assert_eq!(deserialize_byte(&mut cp), VT_END);
        assert_eq!(deserialize_ulong_long(&mut cp), 77);
        assert_eq!(deserialize_ulong_long(&mut cp), 3);
        assert_eq!(deserialize_long_long(&mut cp), -42);
        assert_eq!(deserialize_long_long(&mut cp), 2);
        assert_eq!(deserialize_long_long(&mut cp), 1);
        assert_eq!(deserialize_long_long(&mut cp), 3);
        assert_eq!(deserialize_long_long(&mut cp), 2);
        assert_eq!(deserialize_long_long(&mut cp), 4);
        assert_eq!(deserialize_byte(&mut cp), 6);
        assert!(cp.is_empty());
    }

    #[test]
    fn geometry_deltas() {
        let dv = vec![
            Draw::new(VT_MOVETO, 10, 20),
            Draw::new(VT_LINETO, 15, 18),
            Draw::new(crate::geometry::VT_CLOSEPATH, 0, 0),
        ];
        let mut out = Vec::new();
        write_geometry(&dv, &mut out, 4, 4);

        let mut cp: &[u8] = &out;
        assert_eq!(deserialize_byte(&mut cp), VT_MOVETO);
        assert_eq!(deserialize_long_long(&mut cp), 6);
        assert_eq!(deserialize_long_long(&mut cp), 16);
        assert_eq!(deserialize_byte(&mut cp), VT_LINETO);
        assert_eq!(deserialize_long_long(&mut cp), 5);
        assert_eq!(deserialize_long_long(&mut cp), -2);
        assert_eq!(deserialize_byte(&mut cp), crate::geometry::VT_CLOSEPATH);
        assert_eq!(deserialize_byte(&mut cp), VT_END);
        assert!(cp.is_empty());
    }

    #[test]
    fn vertex_and_node_bytes() {
        let v = Vertex::new(
            Draw::new(VT_LINETO, 5, 9),
            Draw::new(VT_LINETO, -3, 4),
            Draw::new(VT_MOVETO, 1, 2),
        );
        assert_eq!(v.p1, Draw::new(VT_MOVETO, 1, 2));
        assert_eq!(v.p2, Draw::new(VT_MOVETO, 5, 9));
        assert_eq!(Vertex::from_bytes(&v.to_bytes()), v);

        let n = Node {
            index: 0x0123456789abcdef,
        };
        assert_eq!(Node::from_bytes(&n.to_bytes()), n);
        assert_eq!(nodecmp(&n, &Node { index: 1 }), Ordering::Greater);
    }

    #[test]
    fn coercion() {
        let mut types = HashMap::new();
        types.insert("i".to_string(), MVT_INT);
        types.insert("f".to_string(), MVT_FLOAT);
        types.insert("s".to_string(), MVT_STRING);
        types.insert("b".to_string(), MVT_BOOL);

        let mut vt = MVT_STRING;
        let mut val = "3.7".to_string();
        coerce_value("i", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_DOUBLE, "4.000000"));

        let mut vt = MVT_STRING;
        let mut val = "".to_string();
        coerce_value("i", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_DOUBLE, "0"));

        let mut vt = MVT_STRING;
        let mut val = "123".to_string();
        coerce_value("i", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_DOUBLE, "123"));

        let mut vt = MVT_DOUBLE;
        let mut val = "0.0".to_string();
        coerce_value("b", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_BOOL, "false"));

        let mut vt = MVT_STRING;
        let mut val = "0.0".to_string();
        coerce_value("b", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_BOOL, "true"));

        let mut vt = MVT_DOUBLE;
        let mut val = "12".to_string();
        coerce_value("s", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_STRING, "12"));

        let mut vt = MVT_DOUBLE;
        let mut val = "12".to_string();
        coerce_value("other", &mut vt, &mut val, &types);
        assert_eq!((vt, val.as_str()), (MVT_DOUBLE, "12"));

        assert_eq!(to_string_double(-0.0), "-0.000000");
        assert_eq!(to_string_double(f64::NAN), "nan");
        assert_eq!(strip_zeroes("000120"), "120");
    }
}
