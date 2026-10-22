//! Port of mvt.hpp and mvt.cpp: the in-memory vector tile and its protobuf encoding.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::serial::SerialVal;

pub const MVT_MOVETO: i32 = 1;
pub const MVT_LINETO: i32 = 2;
pub const MVT_CLOSEPATH: i32 = 7;

pub const MVT_POINT: i32 = 1;
pub const MVT_LINESTRING: i32 = 2;
pub const MVT_POLYGON: i32 = 3;

pub const MVT_STRING: i32 = 0;
pub const MVT_FLOAT: i32 = 1;
pub const MVT_DOUBLE: i32 = 2;
pub const MVT_INT: i32 = 3;
pub const MVT_UINT: i32 = 4;
pub const MVT_SINT: i32 = 5;
pub const MVT_BOOL: i32 = 6;
pub const MVT_NULL: i32 = 7;
pub const MVT_NO_SUCH_KEY: i32 = 8;

/// `mvt_geometry`. Equality and ordering look only at the coordinates, `y`
/// first, as in the C++.
#[derive(Clone, Copy, Debug, Default)]
pub struct MvtGeometry {
    pub x: i64,
    pub y: i64,
    pub op: i32,
}

impl MvtGeometry {
    pub fn new(op: i32, x: i64, y: i64) -> MvtGeometry {
        MvtGeometry { x, y, op }
    }
}

impl PartialEq for MvtGeometry {
    fn eq(&self, s: &MvtGeometry) -> bool {
        self.y == s.y && self.x == s.x
    }
}

impl Eq for MvtGeometry {}

impl PartialOrd for MvtGeometry {
    fn partial_cmp(&self, s: &MvtGeometry) -> Option<Ordering> {
        Some(self.cmp(s))
    }
}

impl Ord for MvtGeometry {
    fn cmp(&self, s: &MvtGeometry) -> Ordering {
        self.y.cmp(&s.y).then(self.x.cmp(&s.x))
    }
}

#[derive(Clone, Debug, Default)]
pub struct MvtFeature {
    pub tags: Vec<u32>,
    pub geometry: Vec<MvtGeometry>,
    pub type_: i32,
    pub id: u64,
    pub has_id: bool,
    pub dropped: i32,
    /// used for ordering in overzoom
    pub seq: usize,
}

/// The C++ union of numeric values. All members share the same 8 bytes, so
/// reading a member other than the one last written reinterprets the bits,
/// as it does in the C++ on a little-endian machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NumericValue {
    bits: u64,
}

impl NumericValue {
    pub fn float_value(&self) -> f32 {
        f32::from_bits(self.bits as u32)
    }
    pub fn set_float_value(&mut self, v: f32) {
        self.bits = (self.bits & !0xFFFF_FFFF) | v.to_bits() as u64;
    }
    pub fn double_value(&self) -> f64 {
        f64::from_bits(self.bits)
    }
    pub fn set_double_value(&mut self, v: f64) {
        self.bits = v.to_bits();
    }
    pub fn int_value(&self) -> i64 {
        self.bits as i64
    }
    pub fn set_int_value(&mut self, v: i64) {
        self.bits = v as u64;
    }
    pub fn uint_value(&self) -> u64 {
        self.bits
    }
    pub fn set_uint_value(&mut self, v: u64) {
        self.bits = v;
    }
    pub fn sint_value(&self) -> i64 {
        self.bits as i64
    }
    pub fn set_sint_value(&mut self, v: i64) {
        self.bits = v as u64;
    }
    pub fn bool_value(&self) -> bool {
        (self.bits & 0xFF) != 0
    }
    pub fn set_bool_value(&mut self, v: bool) {
        self.bits = (self.bits & !0xFF) | v as u64;
    }
    pub fn null_value(&self) -> i32 {
        self.bits as u32 as i32
    }
    pub fn set_null_value(&mut self, v: i32) {
        self.bits = (self.bits & !0xFFFF_FFFF) | v as u32 as u64;
    }
}

/// `mvt_value`. The C++ keeps string values as offsets into a string pool shared
/// by the whole tile, to save allocations. Here each string value holds its own
/// `Arc<str>`, which is just as cheap to copy, so the `tile_stringpool`
/// parameters of the C++ are gone.
#[derive(Clone, Debug)]
pub struct MvtValue {
    pub type_: i32,
    pub count: i64,
    pub string_value: Arc<str>,
    pub numeric_value: NumericValue,
}

impl Default for MvtValue {
    fn default() -> Self {
        MvtValue::from_double(0.0)
    }
}

impl MvtValue {
    /// `mvt_value(double)`.
    pub fn from_double(v: f64) -> MvtValue {
        let mut numeric_value = NumericValue::default();
        numeric_value.set_double_value(v);
        MvtValue {
            type_: MVT_DOUBLE,
            count: 0,
            string_value: Arc::from(""),
            numeric_value,
        }
    }

    pub fn get_string_value(&self) -> String {
        if self.type_ == MVT_STRING {
            self.string_value.to_string()
        } else {
            self.to_string_value()
        }
    }

    /// `get_string_view()` and `c_str()`.
    pub fn get_str(&self) -> &str {
        &self.string_value
    }

    pub fn set_string_value(&mut self, val: &str) {
        self.type_ = MVT_STRING;
        self.string_value = Arc::from(val);
    }

    pub fn is_numeric(&self) -> bool {
        self.type_ == MVT_FLOAT
            || self.type_ == MVT_DOUBLE
            || self.type_ == MVT_INT
            || self.type_ == MVT_UINT
            || self.type_ == MVT_SINT
    }

    pub fn to_double(&self) -> f64 {
        mvt_value_to_double(self)
    }

    pub fn get_count(&self) -> usize {
        self.count as usize
    }

    pub fn set_double_count(&mut self, v: f64, c: usize) {
        self.type_ = MVT_DOUBLE;
        self.numeric_value.set_double_value(v);
        self.count = c as i64;
    }

    /// `mvt_value::toString()`.
    pub fn to_string_value(&self) -> String {
        todo!("MvtValue::to_string_value")
    }
}

/// `mvt_value::operator<`.
impl PartialOrd for MvtValue {
    fn partial_cmp(&self, o: &MvtValue) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for MvtValue {
    fn cmp(&self, o: &MvtValue) -> Ordering {
        todo!("MvtValue::cmp")
    }
}

/// `mvt_value::operator==`.
impl PartialEq for MvtValue {
    fn eq(&self, o: &MvtValue) -> bool {
        todo!("MvtValue::eq")
    }
}

impl Eq for MvtValue {}

/// `std::hash<mvt_value>`.
impl Hash for MvtValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        todo!("MvtValue::hash")
    }
}

#[derive(Clone, Debug)]
pub struct MvtLayer {
    pub version: i32,
    pub name: String,
    pub features: Vec<MvtFeature>,
    pub keys: Vec<String>,
    pub values: Vec<MvtValue>,
    pub extent: i64,

    // For tracking the key-value constants already used in this layer
    pub key_dedup: Vec<isize>,
    pub value_dedup: Vec<isize>,
}

impl Default for MvtLayer {
    fn default() -> Self {
        MvtLayer {
            version: 0,
            name: String::new(),
            features: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
            extent: 0,
            key_dedup: vec![-1; 65536],
            value_dedup: vec![-1; 65536],
        }
    }
}

impl MvtLayer {
    /// Add a key-value pair to a feature, using this layer's constant pool
    pub fn tag(&mut self, feature: &mut MvtFeature, key: &str, value: &MvtValue) {
        todo!("MvtLayer::tag")
    }

    pub fn detail(&self) -> i32 {
        ((self.extent as f64).ln() / 2f64.ln()).round() as i32
    }
}

#[derive(Clone, Debug, Default)]
pub struct MvtTile {
    pub layers: Vec<MvtLayer>,
}

impl MvtTile {
    pub fn encode(&self) -> Vec<u8> {
        todo!("MvtTile::encode")
    }

    pub fn decode(&mut self, message: &[u8], was_compressed: &mut bool) -> bool {
        todo!("MvtTile::decode")
    }
}

pub fn is_compressed(data: &[u8]) -> bool {
    todo!("mvt::is_compressed")
}

pub fn decompress(input: &[u8], output: &mut Vec<u8>) -> i32 {
    todo!("mvt::decompress")
}

pub fn compress(input: &[u8], output: &mut Vec<u8>, gz: bool) -> i32 {
    todo!("mvt::compress")
}

pub fn dezig(n: u32) -> i32 {
    todo!("mvt::dezig")
}

pub fn stringified_to_mvt_value(type_: i32, s: &str) -> MvtValue {
    todo!("mvt::stringified_to_mvt_value")
}

pub fn mvt_value_to_long_long(v: &MvtValue) -> i64 {
    todo!("mvt::mvt_value_to_long_long")
}

pub fn mvt_value_to_double(v: &MvtValue) -> f64 {
    todo!("mvt::mvt_value_to_double")
}

pub fn is_integer(s: &str, v: &mut i64) -> bool {
    todo!("mvt::is_integer")
}

pub fn is_unsigned_integer(s: &str, v: &mut u64) -> bool {
    todo!("mvt::is_unsigned_integer")
}

pub fn mvt_value_to_serial_val(v: &MvtValue) -> SerialVal {
    todo!("mvt::mvt_value_to_serial_val")
}

#[allow(clippy::too_many_arguments)]
pub fn get_bbox(
    geom: &[MvtGeometry],
    xmin: &mut i64,
    ymin: &mut i64,
    xmax: &mut i64,
    ymax: &mut i64,
    z: i32,
    tx: i32,
    ty: i32,
    detail: i32,
) {
    todo!("mvt::get_bbox")
}
