//! Port of serial.hpp and serial.cpp: the serialized form of features in the
//! temporary files, and the attribute value type used while reading input.
//!
//! The `FILE *` and `std::string` overloads of `serialize_*` get distinct names
//! here: the ones that append to a byte buffer keep the C++ name, and the ones
//! that write to a file start with `f`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use crate::geometry::{Draw, DrawVec};
use crate::mvt::MVT_DOUBLE;

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

pub fn serialize_int(out: &mut Vec<u8>, n: i32) {
    todo!("serial::serialize_int")
}

pub fn serialize_long_long(out: &mut Vec<u8>, n: i64) {
    todo!("serial::serialize_long_long")
}

pub fn serialize_ulong_long(out: &mut Vec<u8>, zigzag: u64) {
    todo!("serial::serialize_ulong_long")
}

pub fn serialize_byte(out: &mut Vec<u8>, n: i8) {
    todo!("serial::serialize_byte")
}

pub fn serialize_uint(out: &mut Vec<u8>, n: u32) {
    todo!("serial::serialize_uint")
}

/// `deserialize_int(const char **f, int *n)`: reads a value and advances `f`.
pub fn deserialize_int(f: &mut &[u8]) -> i32 {
    todo!("serial::deserialize_int")
}

pub fn deserialize_long_long(f: &mut &[u8]) -> i64 {
    todo!("serial::deserialize_long_long")
}

pub fn deserialize_ulong_long(f: &mut &[u8]) -> u64 {
    todo!("serial::deserialize_ulong_long")
}

pub fn deserialize_uint(f: &mut &[u8]) -> u32 {
    todo!("serial::deserialize_uint")
}

pub fn deserialize_byte(f: &mut &[u8]) -> i8 {
    todo!("serial::deserialize_byte")
}

/// `serialize_feature(serial_feature *sf, long long wx, long long wy)`.
pub fn serialize_feature(sf: &mut SerialFeature, wx: i64, wy: i64) -> Vec<u8> {
    todo!("serial::serialize_feature")
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
    todo!("serial::deserialize_feature")
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

impl Vertex {
    pub fn new(one: Draw, joint: Draw, two: Draw) -> Vertex {
        let (p1, p2) = if one < two { (one, two) } else { (two, one) };
        Vertex { p1, mid: joint, p2 }
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

pub fn nodecmp(a: &Node, b: &Node) -> Ordering {
    todo!("serial::nodecmp")
}

pub fn coerce_value(
    key: &str,
    vt: &mut i32,
    val: &mut String,
    attribute_types: &HashMap<String, i32>,
) {
    todo!("serial::coerce_value")
}
