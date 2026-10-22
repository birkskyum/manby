//! Port of mvt.hpp and mvt.cpp: the in-memory vector tile and its protobuf encoding.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::errors::{EXIT_IMPOSSIBLE, EXIT_MVT};
use crate::protozero::{self, PbfReader, PbfWriter};
use crate::serial::SerialVal;
use crate::text::{fnv1a, fnv1a_cstr, fnv1a_mem};

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

/// The empty string that every new `MvtValue` starts with, shared so that
/// creating a value does not allocate.
static EMPTY_STRING: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from(""));

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
            string_value: EMPTY_STRING.clone(),
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
}

/// `std::hash<mvt_value>`, the value that `mvt_layer::tag` uses to find
/// duplicate values.
pub fn mvt_value_hash(k: &MvtValue) -> u64 {
    let bits = k.numeric_value.bits;

    match k.type_ {
        MVT_STRING => fnv1a_cstr(k.string_value.as_bytes(), 0),

        MVT_FLOAT => fnv1a_mem(&(bits as u32).to_le_bytes()),

        MVT_DOUBLE => fnv1a_mem(&bits.to_le_bytes()),

        MVT_INT => fnv1a_mem(&bits.to_le_bytes()),

        MVT_UINT => fnv1a_mem(&bits.to_le_bytes()),

        MVT_SINT => fnv1a_mem(&bits.to_le_bytes()),

        MVT_BOOL => fnv1a_mem(&[bits as u8]),

        MVT_NULL => fnv1a_mem(&(bits as u32).to_le_bytes()),

        _ => {
            eprintln!("mvt_value hash can't happen");
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
}

/// `std::hash<mvt_value>`.
impl Hash for MvtValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(mvt_value_hash(self));
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
    pub fn detail(&self) -> i32 {
        ((self.extent as f64).ln() / 2f64.ln()).round() as i32
    }
}

#[derive(Clone, Debug, Default)]
pub struct MvtTile {
    pub layers: Vec<MvtLayer>,
}

// mvt.cpp

impl MvtGeometry {
    pub fn new(nop: i32, nx: i64, ny: i64) -> MvtGeometry {
        MvtGeometry {
            x: nx,
            y: ny,
            op: nop,
        }
    }
}

/// The part of a C string before its terminating NUL.
fn c_str(s: &str) -> &str {
    match s.find('\0') {
        Some(n) => &s[..n],
        None => s,
    }
}

// https://github.com/mapbox/mapnik-vector-tile/blob/master/src/vector_tile_compression.hpp
pub fn is_compressed(data: &[u8]) -> bool {
    data.len() > 2 && ((data[0] == 0x78 && data[1] == 0x9C) || (data[0] == 0x1F && data[1] == 0x8B))
}

// https://github.com/mapbox/mapnik-vector-tile/blob/master/src/vector_tile_compression.hpp
pub fn decompress(input: &[u8], output: &mut Vec<u8>) -> i32 {
    // PORT: the C++ calls inflateInit2() with 32 + 15, which makes zlib detect
    // a gzip or a zlib header by itself. flate2 has no constructor for that,
    // so this makes the same choice zlib does: gzip if the input starts with
    // the gzip magic number, otherwise zlib.
    let mut inflate_s = if input.len() >= 2 && input[0] == 0x1F && input[1] == 0x8B {
        Decompress::new_gzip(15)
    } else {
        Decompress::new_with_window_bits(true, 15)
    };
    let mut next_in: usize = 0;
    let mut next_out: usize = 0;

    loop {
        let existing_output = next_out;
        let avail_in = input.len() - next_in;

        output.resize(existing_output + 2 * avail_in + 100, 0);

        let total_in = inflate_s.total_in();
        let total_out = inflate_s.total_out();
        let ret = inflate_s.decompress(
            &input[next_in..],
            &mut output[existing_output..],
            FlushDecompress::None,
        );
        next_in += (inflate_s.total_in() - total_in) as usize;
        next_out += (inflate_s.total_out() - total_out) as usize;

        match ret {
            Err(e) => {
                // PORT: zlib returns Z_NEED_DICT for a stream that wants a
                // preset dictionary. The C++ treats that like Z_OK and loops
                // forever; this reports it as an error instead.
                if e.needs_dictionary().is_some() {
                    eprintln!("Decompression error: ");
                } else {
                    eprintln!("Decompression error: data error");
                }
                return 0;
            }
            Ok(Status::BufError) => {
                eprintln!("Decompression error: no data in buffer");
                return 0;
            }
            Ok(Status::StreamEnd) => {
                break;
            }
            Ok(Status::Ok) => {
                // ret must be Z_OK or Z_NEED_DICT;
                // continue decompresing
            }
        }
    }

    output.truncate(next_out);
    1
}

// https://github.com/mapbox/mapnik-vector-tile/blob/master/src/vector_tile_compression.hpp
pub fn compress(input: &[u8], output: &mut Vec<u8>, gz: bool) -> i32 {
    // Z_DEFAULT_COMPRESSION is level 6, and flate2 always uses memLevel 8
    // and Z_DEFAULT_STRATEGY, like the C++.
    let mut deflate_s = if gz {
        Compress::new_gzip(Compression::new(6), 15)
    } else {
        Compress::new_with_window_bits(Compression::new(6), true, 15)
    };
    let mut length: usize = 0;
    loop {
        let increase = input.len() / 2 + 1024;
        output.resize(length + increase, 0);
        let total_in = deflate_s.total_in() as usize;
        let total_out = deflate_s.total_out();
        let ret = deflate_s.compress(
            &input[total_in..],
            &mut output[length..length + increase],
            FlushCompress::Finish,
        );
        if ret.is_err() {
            return -1;
        }
        let produced = (deflate_s.total_out() - total_out) as usize;
        let avail_out = increase - produced;
        length += increase - avail_out;
        if avail_out != 0 {
            break;
        }
    }
    output.truncate(length);
    0
}

impl MvtTile {
    /// `mvt_tile::decode`. A malformed protobuf makes the C++ throw a
    /// protozero exception, which every caller catches to print its own
    /// message. Here that comes back as `false`; call `try_decode` to tell the
    /// two cases apart.
    pub fn decode(&mut self, message: &[u8], was_compressed: &mut bool) -> bool {
        self.try_decode(message, was_compressed).unwrap_or(false)
    }

    /// `mvt_tile::decode`, with the protozero exception that the C++ would
    /// throw returned as the error.
    #[allow(clippy::field_reassign_with_default)]
    pub fn try_decode(
        &mut self,
        message: &[u8],
        was_compressed: &mut bool,
    ) -> protozero::Result<bool> {
        self.layers.clear();
        let uncompressed;
        let src: &[u8];

        if is_compressed(message) {
            let mut out = Vec::new();
            if decompress(message, &mut out) == 0 {
                std::process::exit(EXIT_MVT);
            }
            uncompressed = out;
            src = &uncompressed;
            *was_compressed = true;
        } else {
            src = message;
            *was_compressed = false;
        }

        let mut reader = PbfReader::new(src);

        while reader.next()? {
            match reader.tag() {
                3 => {
                    /* layer */
                    let mut layer_reader = reader.get_message()?;
                    let mut layer = MvtLayer::default();

                    while layer_reader.next()? {
                        match layer_reader.tag() {
                            1 => {
                                /* name */
                                layer.name = layer_reader.get_string()?;
                            }

                            3 => {
                                /* key */
                                layer.keys.push(layer_reader.get_string()?);
                            }

                            4 => {
                                /* value */
                                let mut value_reader = layer_reader.get_message()?;
                                let mut value = MvtValue::default();

                                value.type_ = MVT_NULL;
                                value.numeric_value.set_null_value(0);

                                while value_reader.next()? {
                                    match value_reader.tag() {
                                        1 => {
                                            /* string */
                                            let v = value_reader.get_view()?;
                                            value.set_string_value(&String::from_utf8_lossy(v));
                                        }

                                        2 => {
                                            /* float */
                                            value.type_ = MVT_FLOAT;
                                            value
                                                .numeric_value
                                                .set_float_value(value_reader.get_float()?);
                                        }

                                        3 => {
                                            /* double */
                                            value.type_ = MVT_DOUBLE;
                                            value
                                                .numeric_value
                                                .set_double_value(value_reader.get_double()?);
                                        }

                                        4 => {
                                            /* int */
                                            value.type_ = MVT_INT;
                                            value
                                                .numeric_value
                                                .set_int_value(value_reader.get_int64()?);
                                        }

                                        5 => {
                                            /* uint */
                                            value.type_ = MVT_UINT;
                                            value
                                                .numeric_value
                                                .set_uint_value(value_reader.get_uint64()?);
                                        }

                                        6 => {
                                            /* sint */
                                            value.type_ = MVT_SINT;
                                            value
                                                .numeric_value
                                                .set_sint_value(value_reader.get_sint64()?);
                                        }

                                        7 => {
                                            /* bool */
                                            value.type_ = MVT_BOOL;
                                            value
                                                .numeric_value
                                                .set_bool_value(value_reader.get_bool()?);
                                        }

                                        _ => {
                                            value_reader.skip()?;
                                        }
                                    }
                                }

                                layer.values.push(value);
                            }

                            5 => {
                                /* extent */
                                layer.extent = layer_reader.get_uint32()? as i64;
                            }

                            15 => {
                                /* version */
                                layer.version = layer_reader.get_uint32()? as i32;
                            }

                            2 => {
                                /* feature */
                                let mut feature_reader = layer_reader.get_message()?;
                                let mut feature = MvtFeature::default();
                                let mut geoms: Vec<u32> = Vec::new();

                                while feature_reader.next()? {
                                    match feature_reader.tag() {
                                        1 => {
                                            /* id */
                                            feature.id = feature_reader.get_uint64()?;
                                            feature.has_id = true;
                                        }

                                        2 => {
                                            /* tag */
                                            let pi = feature_reader.get_packed_uint32()?;
                                            feature.tags.reserve(pi.size());
                                            for it in pi {
                                                feature.tags.push(it?);
                                            }
                                        }

                                        3 => {
                                            /* feature type */
                                            feature.type_ = feature_reader.get_enum()?;
                                        }

                                        4 => {
                                            /* geometry */
                                            let pi = feature_reader.get_packed_uint32()?;
                                            geoms.reserve(pi.size());
                                            for it in pi {
                                                geoms.push(it?);
                                            }
                                        }

                                        _ => {
                                            feature_reader.skip()?;
                                        }
                                    }
                                }

                                let mut px: i64 = 0;
                                let mut py: i64 = 0;
                                feature.geometry.reserve(geoms.len()); // probably not quite right, but still plausible
                                let mut g = 0;
                                while g < geoms.len() {
                                    let geom = geoms[g];
                                    let op = geom & 7;
                                    let count = geom >> 3;

                                    if op == MVT_MOVETO as u32 || op == MVT_LINETO as u32 {
                                        let mut k = 0;
                                        while k < count && g + 2 < geoms.len() {
                                            px += protozero::decode_zigzag32(geoms[g + 1]) as i64;
                                            py += protozero::decode_zigzag32(geoms[g + 2]) as i64;
                                            g += 2;

                                            feature
                                                .geometry
                                                .push(MvtGeometry::new(op as i32, px, py));
                                            k += 1;
                                        }
                                    } else {
                                        feature.geometry.push(MvtGeometry::new(op as i32, 0, 0));
                                    }
                                    g += 1;
                                }

                                layer.features.push(feature);
                            }

                            _ => {
                                layer_reader.skip()?;
                            }
                        }
                    }

                    self.layers.push(layer);
                }

                _ => {
                    reader.skip()?;
                }
            }
        }

        Ok(true)
    }
}

/// `sorted_value`, ordered by its encoded bytes and then its original index.
struct SortedValue {
    val: Vec<u8>,
    orig: usize,
}

impl SortedValue {
    #[allow(clippy::if_same_then_else)]
    fn lt(&self, sv: &SortedValue) -> bool {
        if self.val < sv.val {
            return true;
        } else if self.val == sv.val && self.orig < sv.orig {
            return true;
        }

        false
    }
}

impl MvtTile {
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        let mut writer = PbfWriter::new(&mut data);

        for layer in &self.layers {
            let mut layer_string = Vec::new();
            let mut layer_writer = PbfWriter::new(&mut layer_string);

            layer_writer.add_uint32(15, layer.version as u32); /* version */
            layer_writer.add_string(1, layer.name.as_bytes()); /* name */
            layer_writer.add_uint32(5, layer.extent as u32); /* extent */

            for key in &layer.keys {
                layer_writer.add_string(3, key.as_bytes()); /* key */
            }

            let mut sorted_values: Vec<SortedValue> = Vec::with_capacity(layer.values.len());

            for (v, pbv) in layer.values.iter().enumerate() {
                let mut value_string = Vec::new();
                let mut value_writer = PbfWriter::new(&mut value_string);

                match pbv.type_ {
                    MVT_STRING => value_writer.add_string(1, pbv.string_value.as_bytes()),
                    MVT_FLOAT => value_writer.add_float(2, pbv.numeric_value.float_value()),
                    MVT_DOUBLE => value_writer.add_double(3, pbv.numeric_value.double_value()),
                    MVT_INT => value_writer.add_int64(4, pbv.numeric_value.int_value()),
                    MVT_UINT => value_writer.add_uint64(5, pbv.numeric_value.uint_value()),
                    MVT_SINT => value_writer.add_sint64(6, pbv.numeric_value.sint_value()),
                    MVT_BOOL => value_writer.add_bool(7, pbv.numeric_value.bool_value()),
                    MVT_NULL => {
                        eprintln!("Internal error: trying to write null attribute to tile");
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }
                    _ => {
                        eprintln!(
                            "Internal error: trying to write undefined attribute type to tile"
                        );
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }
                }

                sorted_values.push(SortedValue {
                    val: value_string,
                    orig: v,
                });
            }

            // std::stable_sort with a comparator that is a total order
            sorted_values.sort_by(|a, b| {
                if a.lt(b) {
                    Ordering::Less
                } else if b.lt(a) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            });
            let mut mapping: Vec<usize> = vec![0; sorted_values.len()];

            let mut value_index: usize = 0;
            let mut v = 0;
            while v < sorted_values.len() {
                mapping[sorted_values[v].orig] = value_index;
                layer_writer.add_message(4, &sorted_values[v].val);

                // crunch out duplicates that were missed by the hashing
                while v + 1 < sorted_values.len()
                    && sorted_values[v].val == sorted_values[v + 1].val
                {
                    sorted_values[v].val.clear();
                    mapping[sorted_values[v + 1].orig] = value_index;
                    v += 1;
                }

                sorted_values[v].val.clear();
                value_index += 1;
                v += 1;
            }

            sorted_values.clear();

            for feature in &layer.features {
                let mut feature_string = Vec::new();
                let mut feature_writer = PbfWriter::new(&mut feature_string);

                if feature.type_ >= 0 {
                    feature_writer.add_enum(3, feature.type_);
                }

                let mut sorted_tags: Vec<u32> = feature.tags.clone();
                let mut v = 1;
                while v < sorted_tags.len() {
                    sorted_tags[v] = mapping[sorted_tags[v] as usize] as u32;
                    v += 2;
                }
                feature_writer.add_packed_uint32(2, sorted_tags.iter().copied());

                if feature.has_id {
                    feature_writer.add_uint64(1, feature.id);
                }

                let mut geometry: Vec<u32> = Vec::new();

                let mut px: i64 = 0;
                let mut py: i64 = 0;
                let mut cmd_idx: i32 = -1;
                let mut cmd: i32 = -1;
                let mut length: i32 = 0;

                let geom = &feature.geometry;

                for g in geom {
                    let op = g.op;

                    if op != cmd {
                        if cmd_idx >= 0 {
                            geometry[cmd_idx as usize] =
                                ((length << 3) | (cmd & ((1 << 3) - 1))) as u32;
                        }

                        cmd = op;
                        length = 0;
                        cmd_idx = geometry.len() as i32;
                        geometry.push(0);
                    }

                    if op == MVT_MOVETO || op == MVT_LINETO {
                        let wwx = g.x;
                        let wwy = g.y;

                        let dx = wwx - px;
                        let dy = wwy - py;

                        if dx < i32::MIN as i64
                            || dx > i32::MAX as i64
                            || dy < i32::MIN as i64
                            || dy > i32::MAX as i64
                        {
                            eprintln!("Internal error: Geometry delta is too big: {},{}", dx, dy);
                            std::process::exit(EXIT_IMPOSSIBLE);
                        }

                        geometry.push(protozero::encode_zigzag32(dx as i32));
                        geometry.push(protozero::encode_zigzag32(dy as i32));

                        px = wwx;
                        py = wwy;
                        length += 1;
                    } else if op == MVT_CLOSEPATH {
                        length += 1;
                    } else {
                        eprintln!("\nInternal error: corrupted geometry");
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }
                }

                if cmd_idx >= 0 {
                    geometry[cmd_idx as usize] = ((length << 3) | (cmd & ((1 << 3) - 1))) as u32;
                }

                feature_writer.add_packed_uint32(4, geometry.iter().copied());
                layer_writer.add_message(2, &feature_string);
            }

            writer.add_message(3, &layer_string);
        }

        data
    }
}

/// `mvt_value::operator<`.
fn mvt_value_lt(s: &MvtValue, o: &MvtValue) -> bool {
    if s.type_ < o.type_ {
        return true;
    }
    if s.type_ == o.type_ {
        let a = &s.numeric_value;
        let b = &o.numeric_value;
        match s.type_ {
            MVT_STRING => return s.string_value.as_bytes() < o.string_value.as_bytes(),

            MVT_FLOAT => return a.float_value() < b.float_value(),

            MVT_DOUBLE => return a.double_value() < b.double_value(),

            MVT_INT => return a.int_value() < b.int_value(),

            MVT_UINT => return a.uint_value() < b.uint_value(),

            MVT_SINT => return a.sint_value() < b.sint_value(),

            MVT_BOOL => return (a.bool_value() as u8) < (b.bool_value() as u8),

            MVT_NULL => return a.null_value() < b.null_value(),

            _ => {
                eprintln!("mvt_value::operator<<: can't happen");
                std::process::exit(EXIT_IMPOSSIBLE);
            }
        }
    }

    false
}

/// `mvt_value::operator<`. Values that the C++ `<` considers neither less nor
/// greater (such as NaN against anything) come out `Equal`, which is how
/// `std::map` and `std::sort` treat them.
impl PartialOrd for MvtValue {
    fn partial_cmp(&self, o: &MvtValue) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for MvtValue {
    fn cmp(&self, o: &MvtValue) -> Ordering {
        if mvt_value_lt(self, o) {
            Ordering::Less
        } else if mvt_value_lt(o, self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

/// `mvt_value::operator==`.
impl PartialEq for MvtValue {
    fn eq(&self, o: &MvtValue) -> bool {
        if self.type_ == o.type_ {
            let a = &self.numeric_value;
            let b = &o.numeric_value;
            match self.type_ {
                MVT_STRING => return self.string_value.as_bytes() == o.string_value.as_bytes(),

                MVT_FLOAT => return a.float_value() == b.float_value(),

                MVT_DOUBLE => return a.double_value() == b.double_value(),

                MVT_INT => return a.int_value() == b.int_value(),

                MVT_UINT => return a.uint_value() == b.uint_value(),

                MVT_SINT => return a.sint_value() == b.sint_value(),

                MVT_BOOL => return a.bool_value() == b.bool_value(),

                MVT_NULL => return a.null_value() == b.null_value(),

                _ => {
                    eprintln!("mvt_value::operator==: can't happen");
                    std::process::exit(EXIT_IMPOSSIBLE);
                }
            }
        }

        false
    }
}

impl Eq for MvtValue {}

fn quote(s: &str) -> String {
    let mut buf: Vec<u8> = Vec::new();

    for &ch in s.as_bytes() {
        if ch == b'\\' || ch == b'"' {
            buf.push(b'\\');
            buf.push(ch);
        } else if ch < b' ' {
            buf.extend_from_slice(format!("\\u{:04x}", ch).as_bytes());
        } else {
            buf.push(ch);
        }
    }

    // Only ASCII was added, so this is still UTF-8.
    String::from_utf8(buf).unwrap()
}

impl MvtValue {
    /// `mvt_value::toString()`.
    pub fn to_string_value(&self) -> String {
        match self.type_ {
            MVT_STRING => quote(&self.get_string_value()),
            MVT_INT => self.numeric_value.int_value().to_string(),
            MVT_DOUBLE => {
                let v = self.numeric_value.double_value();
                if v == (v as i64) as f64 {
                    (v as i64).to_string()
                } else {
                    crate::milo::dtoa_milo(v)
                }
            }
            MVT_FLOAT => {
                let v = self.numeric_value.float_value() as f64;
                if v == (v as i64) as f64 {
                    (v as i64).to_string()
                } else {
                    crate::milo::dtoa_milo(v)
                }
            }
            MVT_SINT => self.numeric_value.sint_value().to_string(),
            MVT_UINT => self.numeric_value.uint_value().to_string(),
            MVT_BOOL => {
                if self.numeric_value.bool_value() {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            MVT_NULL => "null".to_string(),
            _ => "unknown ".to_string() + &self.type_.to_string(),
        }
    }
}

impl MvtLayer {
    /// Add a key-value pair to a feature, using this layer's constant pool
    pub fn tag(&mut self, feature: &mut MvtFeature, key: &str, value: &MvtValue) {
        let key_hash = (fnv1a(key.as_bytes()) % self.key_dedup.len() as u64) as usize;
        if self.key_dedup[key_hash] >= 0 && self.keys[self.key_dedup[key_hash] as usize] == key {
        } else {
            self.key_dedup[key_hash] = self.keys.len() as isize;
            self.keys.push(key.to_string());
        }
        feature.tags.push(self.key_dedup[key_hash] as u32);

        let value_hash = (mvt_value_hash(value) % self.value_dedup.len() as u64) as usize;
        if self.value_dedup[value_hash] >= 0
            && self.values[self.value_dedup[value_hash] as usize] == *value
        {
        } else {
            self.value_dedup[value_hash] = self.values.len() as isize;
            self.values.push(value.clone());
        }
        feature.tags.push(self.value_dedup[value_hash] as u32);
    }
}

/// The byte at `i` of a C string, or NUL past its end.
fn byte_at(s: &[u8], i: usize) -> u8 {
    if i < s.len() { s[i] } else { 0 }
}

/// Checks what follows the number that `strtoll` or `strtoull` parsed from `s`,
/// with `end` the offset of `endptr`.
fn integer_rest_ok(s: &[u8], mut end: usize) -> bool {
    if byte_at(s, end) != b'\0' {
        // Special case: If it is an integer followed by .0000 or similar,
        // it is still an integer

        if byte_at(s, end) != b'.' {
            return false;
        }
        end += 1;
        while byte_at(s, end) != b'\0' {
            if byte_at(s, end) != b'0' {
                return false;
            }
            end += 1;
        }

        return true;
    }

    true
}

pub fn is_integer(s: &str, v: &mut i64) -> bool {
    let s = c_str(s);
    crate::cstd::set_errno(0);

    let (val, endptr) = crate::cstd::strtoll(s, 0);
    *v = val;
    let errno = crate::cstd::errno();
    if *v == 0 && errno != 0 {
        return false;
    }
    if (*v == i64::MIN || *v == i64::MAX) && (errno == libc::ERANGE || errno == libc::EINVAL) {
        return false;
    }
    integer_rest_ok(s.as_bytes(), endptr)
}

/// C `isspace` in the C locale.
fn isspace(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == 0x0B || c == 0x0C || c == b'\r'
}

pub fn is_unsigned_integer(s: &str, v: &mut u64) -> bool {
    let mut s = c_str(s);
    crate::cstd::set_errno(0);

    // Special check because MacOS stroull() returns 1
    // for -18446744073709551615
    while isspace(byte_at(s.as_bytes(), 0)) {
        s = &s[1..];
    }
    if byte_at(s.as_bytes(), 0) == b'-' {
        return false;
    }

    let (val, endptr) = crate::cstd::strtoull(s, 0);
    *v = val;
    let errno = crate::cstd::errno();
    if *v == 0 && errno != 0 {
        return false;
    }
    if *v == u64::MAX && (errno == libc::ERANGE || errno == libc::EINVAL) {
        return false;
    }
    integer_rest_ok(s.as_bytes(), endptr)
}

/// libc `strtof`, which cstd.rs does not have. Returns the value and the
/// number of bytes consumed (0 if no conversion), and sets `errno` like libc.
fn strtof(s: &str) -> (f32, usize) {
    let c = std::ffi::CString::new(c_str(s)).unwrap();
    let mut endptr: *mut libc::c_char = std::ptr::null_mut();
    // SAFETY: `c` is a NUL-terminated string that outlives the call, and
    // strtof only reads it and points `endptr` into it.
    let f = unsafe { libc::strtof(c.as_ptr(), &mut endptr) };
    (f, endptr as usize - c.as_ptr() as usize)
}

// This converts a serial_val-style attribute value to an mvt_value
// to store in a tile. If the value is numeric, it tries to choose
// the type (int, uint, sint, float, or double) that will give the
// smallest representation in the tile without losing precision,
// regardless of how the value was represented in the original source.
pub fn stringified_to_mvt_value(type_: i32, s: &str) -> MvtValue {
    let s = c_str(s);
    let mut tv = MvtValue::default();

    match type_ {
        MVT_DOUBLE => {
            let mut v: i64 = 0;
            let mut uv: u64 = 0;
            if is_unsigned_integer(s, &mut uv) {
                if uv <= i64::MAX as u64 {
                    tv.type_ = MVT_INT;
                    tv.numeric_value.set_int_value(uv as i64);
                } else {
                    tv.type_ = MVT_UINT;
                    tv.numeric_value.set_uint_value(uv);
                }
            } else if is_integer(s, &mut v) {
                tv.type_ = MVT_SINT;
                tv.numeric_value.set_sint_value(v);
            } else {
                crate::cstd::set_errno(0);

                let (f, endptr) = strtof(s);

                // HUGE_VAL, HUGE_VALF and HUGE_VALL all compare equal to a float infinity
                if endptr == 0 || (f == f32::INFINITY && crate::cstd::errno() == libc::ERANGE) {
                    let (d, endptr) = crate::cstd::strtod(s);
                    if endptr == 0 || (d == f64::INFINITY && crate::cstd::errno() == libc::ERANGE) {
                        eprintln!("Warning: numeric value {} could not be represented", s);
                    }
                    tv.type_ = MVT_DOUBLE;
                    tv.numeric_value.set_double_value(d);
                } else {
                    let d = crate::cstd::atof(s);
                    if f as f64 == d {
                        tv.type_ = MVT_FLOAT;
                        tv.numeric_value.set_float_value(f);
                    } else {
                        // Conversion succeeded, but lost precision, so use double
                        tv.type_ = MVT_DOUBLE;
                        tv.numeric_value.set_double_value(d);
                    }
                }
            }
        }
        MVT_BOOL => {
            tv.type_ = MVT_BOOL;
            tv.numeric_value
                .set_bool_value(byte_at(s.as_bytes(), 0) == b't');
        }
        MVT_NULL => {
            tv.type_ = MVT_NULL;
            tv.numeric_value.set_null_value(0);
        }
        _ => {
            tv.type_ = MVT_STRING;
            tv.set_string_value(s);
        }
    }

    tv
}

// This converts a mvt_value attribute value from a tile back into
// a serial_val for more convenient parsing and comparison without
// having to handle all of the vector tile numeric types separately.
// All numeric types are given the type mvt_double in the serial_val
// whether the actual value is integer or floating point.
pub fn mvt_value_to_serial_val(v: &MvtValue) -> SerialVal {
    let mut sv = SerialVal::default();

    match v.type_ {
        MVT_STRING => {
            sv.type_ = MVT_STRING;
            sv.s = v.get_string_value();
        }
        MVT_FLOAT => {
            sv.type_ = MVT_DOUBLE;
            sv.s = crate::milo::dtoa_milo(v.numeric_value.float_value() as f64);
        }
        MVT_DOUBLE => {
            sv.type_ = MVT_DOUBLE;
            sv.s = crate::milo::dtoa_milo(v.numeric_value.double_value());
        }
        MVT_INT => {
            sv.type_ = MVT_DOUBLE;
            sv.s = v.numeric_value.int_value().to_string();
        }
        MVT_UINT => {
            sv.type_ = MVT_DOUBLE;
            sv.s = v.numeric_value.uint_value().to_string();
        }
        MVT_SINT => {
            sv.type_ = MVT_DOUBLE;
            sv.s = v.numeric_value.sint_value().to_string();
        }
        MVT_BOOL => {
            sv.type_ = MVT_BOOL;
            sv.s = if v.numeric_value.bool_value() {
                "true"
            } else {
                "false"
            }
            .to_string();
        }
        MVT_NULL => {
            sv.type_ = MVT_NULL;
            sv.s = "null".to_string();
        }
        _ => {
            eprintln!("unhandled mvt_type {}", v.type_);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }

    sv
}

// This extracts an integer value from an mvt_value
pub fn mvt_value_to_long_long(v: &MvtValue) -> i64 {
    match v.type_ {
        MVT_STRING => crate::cstd::atoll(c_str(v.get_str())),
        MVT_FLOAT => v.numeric_value.float_value() as i64,
        MVT_DOUBLE => v.numeric_value.double_value() as i64,
        MVT_INT => v.numeric_value.int_value(),
        MVT_UINT => v.numeric_value.uint_value() as i64,
        MVT_SINT => v.numeric_value.sint_value(),
        MVT_BOOL => v.numeric_value.bool_value() as i64,
        MVT_NULL => 0,
        _ => {
            eprintln!("unhandled mvt_type {}", v.type_);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
}

// This extracts a double value from an mvt_value
pub fn mvt_value_to_double(v: &MvtValue) -> f64 {
    match v.type_ {
        MVT_STRING => crate::cstd::atof(c_str(v.get_str())),
        MVT_FLOAT => v.numeric_value.float_value() as f64,
        MVT_DOUBLE => v.numeric_value.double_value(),
        MVT_INT => v.numeric_value.int_value() as f64,
        MVT_UINT => v.numeric_value.uint_value() as f64,
        MVT_SINT => v.numeric_value.sint_value() as f64,
        MVT_BOOL => v.numeric_value.bool_value() as i32 as f64,
        MVT_NULL => 0.0,
        _ => {
            eprintln!("unhandled mvt_type {}", v.type_);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
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
    *xmin = i64::MAX;
    *ymin = i64::MAX;
    *xmax = 0;
    *ymax = 0;

    for g in geom {
        if g.op == MVT_MOVETO || g.op == MVT_LINETO {
            let mut x = g.x;
            let mut y = g.y;

            // to world scale
            x *= 1i64 << (32 - z - detail);
            y *= 1i64 << (32 - z - detail);

            // to world origin
            if z > 0 {
                x += (1i64 << (32 - z)) * tx as i64;
                y += (1i64 << (32 - z)) * ty as i64;
            }

            *xmin = std::cmp::min(*xmin, x);
            *ymin = std::cmp::min(*ymin, y);
            *xmax = std::cmp::max(*xmax, x);
            *ymax = std::cmp::max(*ymax, y);
        }
    }
}

/// Declared in mvt.hpp but never defined in the C++. This is protozero's
/// `decode_zigzag32`, which is what the name describes.
pub fn dezig(n: u32) -> i32 {
    protozero::decode_zigzag32(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (input, is_integer, its value, is_unsigned_integer, its value,
    /// stringified_to_mvt_value type, toString()), from the C++ on macOS.
    #[rustfmt::skip]
    const CONVERSIONS: &[(&str, bool, i64, bool, u64, i32, &str)] = &[
        (" 12", true, 12, true, 12, MVT_INT, "12"),
        ("-0", true, 0, false, 0, MVT_SINT, "0"),
        ("0x10", true, 16, true, 16, MVT_INT, "16"),
        ("1.000", true, 1, true, 1, MVT_INT, "1"),
        ("1.5", false, 1, false, 1, MVT_FLOAT, "1.5"),
        ("0.1", false, 0, false, 0, MVT_DOUBLE, "0.1"),
        ("18446744073709551615", false, 9223372036854775807, true, 18446744073709551615, MVT_UINT, "18446744073709551615"),
        ("18446744073709551616", false, 9223372036854775807, false, 18446744073709551615, MVT_FLOAT, "18446744073709553000"),
        ("9223372036854775807", true, 9223372036854775807, true, 9223372036854775807, MVT_INT, "9223372036854775807"),
        ("9223372036854775808", false, 9223372036854775807, true, 9223372036854775808, MVT_UINT, "9223372036854775808"),
        ("-9223372036854775808", true, -9223372036854775808, false, 0, MVT_SINT, "-9223372036854775808"),
        ("-9223372036854775809", false, -9223372036854775808, false, 0, MVT_FLOAT, "-9223372036854775808"),
        ("1e39", false, 1, false, 1, MVT_DOUBLE, "1e+39"),
        ("1e400", false, 1, false, 1, MVT_DOUBLE, "inf"),
        ("-1e39", false, -1, false, 0, MVT_DOUBLE, "-1e+39"),
        ("3.4028235e38", false, 3, false, 3, MVT_DOUBLE, "3.4028235e+38"),
        ("nan", false, 0, false, 0, MVT_DOUBLE, "nan"),
        ("inf", false, 0, false, 0, MVT_FLOAT, "inf"),
        ("-inf", false, 0, false, 0, MVT_FLOAT, "-inf"),
        ("12abc", false, 12, false, 12, MVT_FLOAT, "12"),
        (" -5", true, -5, false, 0, MVT_SINT, "-5"),
        ("007", true, 7, true, 7, MVT_INT, "7"),
        ("1e-50", false, 1, false, 1, MVT_DOUBLE, "1e-50"),
        ("0x1.8p3", false, 1, false, 1, MVT_FLOAT, "12"),
        ("2.5e3", false, 2, false, 2, MVT_FLOAT, "2500"),
        ("-1.5", false, -1, false, 0, MVT_FLOAT, "-1.5"),
    ];

    #[test]
    fn conversions_match_the_cpp() {
        for &(s, i, iv, u, uv, t, string) in CONVERSIONS {
            let mut v = 0;
            let mut w = 0;
            assert_eq!(is_integer(s, &mut v), i, "is_integer {s}");
            assert_eq!(v, iv, "is_integer value {s}");
            assert_eq!(is_unsigned_integer(s, &mut w), u, "is_unsigned_integer {s}");
            assert_eq!(w, uv, "is_unsigned_integer value {s}");
            let mv = stringified_to_mvt_value(MVT_DOUBLE, s);
            assert_eq!(mv.type_, t, "type {s}");
            assert_eq!(mv.to_string_value(), string, "toString {s}");
        }
    }

    /// On macOS, strtoll() sets EINVAL when there are no digits, so these are
    /// not integers there; glibc leaves errno alone and calls them 0.
    #[test]
    #[cfg(target_os = "macos")]
    fn no_digits_on_macos() {
        for s in ["", "-", ".", "abc"] {
            let mut v = 0;
            let mut w = 0;
            assert!(!is_integer(s, &mut v), "{s}");
            assert!(!is_unsigned_integer(s, &mut w), "{s}");
            let mv = stringified_to_mvt_value(MVT_DOUBLE, s);
            assert_eq!(mv.type_, MVT_DOUBLE);
            assert_eq!(mv.to_string_value(), "0");
        }
    }

    #[test]
    fn other_types() {
        let v = stringified_to_mvt_value(MVT_BOOL, "true");
        assert_eq!(
            (v.type_, v.to_string_value()),
            (MVT_BOOL, "true".to_string())
        );
        let v = stringified_to_mvt_value(MVT_BOOL, "false");
        assert_eq!(v.to_string_value(), "false");
        let v = stringified_to_mvt_value(MVT_NULL, "null");
        assert_eq!(
            (v.type_, v.to_string_value()),
            (MVT_NULL, "null".to_string())
        );
        let v = stringified_to_mvt_value(MVT_STRING, "a\"b\\c\nd\0e");
        assert_eq!(v.type_, MVT_STRING);
        assert_eq!(v.get_str(), "a\"b\\c\nd");
        assert_eq!(v.to_string_value(), "a\\\"b\\\\c\\u000ad");

        let sv = mvt_value_to_serial_val(&stringified_to_mvt_value(MVT_DOUBLE, "1.5"));
        assert_eq!((sv.type_, sv.s.as_str()), (MVT_DOUBLE, "1.5"));
        let sv = mvt_value_to_serial_val(&stringified_to_mvt_value(MVT_DOUBLE, "-7"));
        assert_eq!((sv.type_, sv.s.as_str()), (MVT_DOUBLE, "-7"));
        assert_eq!(
            mvt_value_to_long_long(&stringified_to_mvt_value(MVT_STRING, "42x")),
            42
        );
        assert_eq!(
            mvt_value_to_double(&stringified_to_mvt_value(MVT_STRING, "4.5x")),
            4.5
        );
    }

    #[test]
    fn ordering_and_equality() {
        let a = stringified_to_mvt_value(MVT_STRING, "a");
        let b = stringified_to_mvt_value(MVT_STRING, "b");
        let one = stringified_to_mvt_value(MVT_DOUBLE, "1");
        let two = stringified_to_mvt_value(MVT_DOUBLE, "2");
        assert!(a < b);
        assert!(a < one); // string sorts before int by type
        assert!(one < two);
        assert_eq!(one, stringified_to_mvt_value(MVT_DOUBLE, "1.0"));
        assert_ne!(one, MvtValue::from_double(1.0)); // int and double are different types

        let nan = MvtValue::from_double(f64::NAN);
        assert_eq!(nan.cmp(&MvtValue::from_double(1.0)), Ordering::Equal);
        assert_ne!(nan, nan.clone());

        // the hash looks at the bytes, so -0 and 0 hash differently but are equal
        let z = MvtValue::from_double(0.0);
        let nz = MvtValue::from_double(-0.0);
        assert_eq!(z, nz);
        assert_ne!(mvt_value_hash(&z), mvt_value_hash(&nz));
    }

    fn sample_tile() -> MvtTile {
        let mut layer = MvtLayer {
            name: "layer".to_string(),
            version: 2,
            extent: 4096,
            ..Default::default()
        };

        let mut f = MvtFeature {
            type_: MVT_POLYGON,
            id: 7,
            has_id: true,
            ..Default::default()
        };
        f.geometry = vec![
            MvtGeometry::new(MVT_MOVETO, 0, 0),
            MvtGeometry::new(MVT_LINETO, 10, 0),
            MvtGeometry::new(MVT_LINETO, 10, -10),
            MvtGeometry::new(MVT_CLOSEPATH, 0, 0),
        ];
        layer.tag(&mut f, "name", &stringified_to_mvt_value(MVT_STRING, "x"));
        layer.tag(&mut f, "n", &stringified_to_mvt_value(MVT_DOUBLE, "1"));
        layer.features.push(f);

        let mut f = MvtFeature {
            type_: MVT_POINT,
            ..Default::default()
        };
        f.geometry = vec![MvtGeometry::new(MVT_MOVETO, 5, 6)];
        layer.tag(&mut f, "name", &stringified_to_mvt_value(MVT_STRING, "x"));
        layer.tag(&mut f, "b", &stringified_to_mvt_value(MVT_BOOL, "true"));
        layer.features.push(f);

        MvtTile {
            layers: vec![layer],
        }
    }

    #[test]
    fn tag_dedups() {
        let tile = sample_tile();
        let layer = &tile.layers[0];
        assert_eq!(layer.keys, vec!["name", "n", "b"]);
        assert_eq!(layer.values.len(), 3);
        assert_eq!(layer.features[0].tags, vec![0, 0, 1, 1]);
        assert_eq!(layer.features[1].tags, vec![0, 0, 2, 2]);
    }

    #[test]
    fn encode_decode_round_trip() {
        let tile = sample_tile();
        let enc = tile.encode();

        let mut back = MvtTile::default();
        let mut was_compressed = true;
        assert!(back.decode(&enc, &mut was_compressed));
        assert!(!was_compressed);
        assert_eq!(back.encode(), enc);

        let layer = &back.layers[0];
        assert_eq!(layer.name, "layer");
        assert_eq!(layer.extent, 4096);
        assert_eq!(layer.features[0].geometry.len(), 4);
        assert_eq!(layer.features[0].geometry[2].y, -10);
        assert_eq!(layer.features[0].geometry[3].op, MVT_CLOSEPATH);
        assert!(layer.features[0].has_id);
        assert!(!layer.features[1].has_id);

        // values come back sorted by their encoding: the string, then the int, then the bool
        assert_eq!(layer.values[0].type_, MVT_STRING);
        assert_eq!(layer.values[1].type_, MVT_INT);
        assert_eq!(layer.values[2].type_, MVT_BOOL);

        for gz in [true, false] {
            let mut c = Vec::new();
            assert_eq!(compress(&enc, &mut c, gz), 0);
            assert!(is_compressed(&c));
            let mut back = MvtTile::default();
            assert!(back.decode(&c, &mut was_compressed));
            assert!(was_compressed);
            assert_eq!(back.encode(), enc);
        }
    }

    #[test]
    fn malformed_tiles() {
        let mut tile = MvtTile::default();
        let mut was_compressed = false;
        // a layer whose length runs past the end
        assert_eq!(
            tile.try_decode(&[0x1a, 0x10, 0x00], &mut was_compressed),
            Err(protozero::Exception::EndOfBuffer)
        );
        assert!(!tile.decode(&[0x1a, 0x10, 0x00], &mut was_compressed));
        // unknown top-level fields are skipped
        assert_eq!(
            tile.try_decode(&[0x08, 0x01], &mut was_compressed),
            Ok(true)
        );
        assert!(tile.layers.is_empty());

        let mut out = Vec::new();
        assert_eq!(decompress(&[0x78, 0x9c, 0x01], &mut out), 0);
    }

    #[test]
    fn bbox() {
        let geom = vec![
            MvtGeometry::new(MVT_MOVETO, 1, 2),
            MvtGeometry::new(MVT_LINETO, 3, -4),
            MvtGeometry::new(MVT_CLOSEPATH, 100, 100),
        ];
        let (mut xmin, mut ymin, mut xmax, mut ymax) = (0, 0, 0, 0);
        get_bbox(
            &geom, &mut xmin, &mut ymin, &mut xmax, &mut ymax, 1, 1, 0, 12,
        );
        let scale = 1i64 << 19;
        assert_eq!(xmin, scale + (1i64 << 31));
        assert_eq!(ymin, -4 * scale);
        assert_eq!(xmax, 3 * scale + (1i64 << 31));
        assert_eq!(ymax, 2 * scale);
    }
}
