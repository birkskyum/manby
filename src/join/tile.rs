//! One output tile of tile-join: the input tiles of a z/x/y decoded,
//! filtered, joined with the CSV and appended in order, then encoded. This is
//! tile-join's `append_tile` and tippecanoe-rs's `mvt_tile::decode` and
//! `mvt_tile::encode`, making the same bytes, without their 65536-slot hash
//! tables per layer (a megabyte each, for every layer of every tile) and with
//! the geometry carried over as encoded commands, decoded into points only
//! where its scale changes.
//!
//! The decoding follows `mvt_tile::decode` call for call on tippecanoe-rs's
//! protozero reader, so that malformed tiles fail in the same places.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ops::Range;

use rustc_hash::FxHashMap;
use tippecanoe::errors::{EXIT_IMPOSSIBLE, EXIT_JSON, EXIT_MVT};
use tippecanoe::jsonpull::{JsonRef, JsonType};
use tippecanoe::mvt::{
    MVT_BOOL, MVT_CLOSEPATH, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINESTRING, MVT_LINETO,
    MVT_MOVETO, MVT_NULL, MVT_POINT, MVT_POLYGON, MVT_SINT, MVT_STRING, MVT_UINT, MvtFeature,
    MvtLayer, MvtValue,
};
use tippecanoe::protozero::{self, PbfReader};
use tippecanoe::text::{fnv1a, fnv1a_cstr, fnv1a_mem};

use super::csv::Row;
use super::stats::Accum;
use crate::{Error, Result};

/// A value as tippecanoe-rs decodes it: the type of the last value field of
/// the message, with that field's value.
#[derive(Clone, Debug)]
pub enum Val<'a> {
    Str(Cow<'a, str>),
    Float(f32),
    Double(f64),
    Int(i64),
    Uint(u64),
    Sint(i64),
    Bool(bool),
    Null,
}

impl Val<'_> {
    pub fn mvt_type(&self) -> i32 {
        match self {
            Val::Str(_) => MVT_STRING,
            Val::Float(_) => MVT_FLOAT,
            Val::Double(_) => MVT_DOUBLE,
            Val::Int(_) => MVT_INT,
            Val::Uint(_) => MVT_UINT,
            Val::Sint(_) => MVT_SINT,
            Val::Bool(_) => MVT_BOOL,
            Val::Null => MVT_NULL,
        }
    }

    /// `std::hash<mvt_value>`.
    fn hash(&self) -> u64 {
        match self {
            Val::Str(s) => fnv1a_cstr(s.as_bytes(), 0),
            Val::Float(f) => fnv1a_mem(&f.to_bits().to_le_bytes()),
            Val::Double(d) => fnv1a_mem(&d.to_bits().to_le_bytes()),
            Val::Int(i) | Val::Sint(i) => fnv1a_mem(&i.to_le_bytes()),
            Val::Uint(u) => fnv1a_mem(&u.to_le_bytes()),
            Val::Bool(b) => fnv1a_mem(&[*b as u8]),
            Val::Null => fnv1a_mem(&0u32.to_le_bytes()),
        }
    }

    /// `mvt_value::operator==`.
    fn same(&self, o: &Val<'_>) -> bool {
        match (self, o) {
            (Val::Str(a), Val::Str(b)) => a.as_bytes() == b.as_bytes(),
            (Val::Float(a), Val::Float(b)) => a == b,
            (Val::Double(a), Val::Double(b)) => a == b,
            (Val::Int(a), Val::Int(b)) | (Val::Sint(a), Val::Sint(b)) => a == b,
            (Val::Uint(a), Val::Uint(b)) => a == b,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (Val::Null, Val::Null) => true,
            _ => false,
        }
    }

    /// The value message, as `mvt_tile::encode` writes it.
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            Val::Str(s) => {
                out.push(1 << 3 | 2);
                varint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Val::Float(f) => {
                out.push(2 << 3 | 5);
                out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Val::Double(d) => {
                out.push(3 << 3 | 1);
                out.extend_from_slice(&d.to_bits().to_le_bytes());
            }
            Val::Int(i) => {
                out.push(4 << 3);
                varint(out, *i as u64);
            }
            Val::Uint(u) => {
                out.push(5 << 3);
                varint(out, *u);
            }
            Val::Sint(i) => {
                out.push(6 << 3);
                varint(out, ((*i << 1) ^ (*i >> 63)) as u64);
            }
            Val::Bool(b) => {
                out.push(7 << 3);
                out.push(*b as u8);
            }
            Val::Null => {
                return Err(Error::new(
                    EXIT_IMPOSSIBLE,
                    "Internal error: trying to write null attribute to tile",
                ));
            }
        }
        Ok(())
    }

    /// `mvt_value_to_serial_val`: the type and text tile-stats keep.
    pub fn serial(&self) -> (i32, Cow<'_, str>) {
        match self {
            Val::Str(s) => (MVT_STRING, Cow::Borrowed(s)),
            Val::Float(f) => (
                MVT_DOUBLE,
                Cow::Owned(tippecanoe::milo::dtoa_milo(*f as f64)),
            ),
            Val::Double(d) => (MVT_DOUBLE, Cow::Owned(tippecanoe::milo::dtoa_milo(*d))),
            Val::Int(i) | Val::Sint(i) => (MVT_DOUBLE, Cow::Owned(i.to_string())),
            Val::Uint(u) => (MVT_DOUBLE, Cow::Owned(u.to_string())),
            Val::Bool(b) => (MVT_BOOL, Cow::Borrowed(if *b { "true" } else { "false" })),
            Val::Null => (MVT_NULL, Cow::Borrowed("null")),
        }
    }

    /// The same value as tippecanoe-rs's `MvtValue`.
    pub fn to_mvt(&self) -> MvtValue {
        let mut v = MvtValue {
            type_: self.mvt_type(),
            ..Default::default()
        };
        match self {
            Val::Str(s) => v.set_string_value(s),
            Val::Float(f) => v.numeric_value.set_float_value(*f),
            Val::Double(d) => v.numeric_value.set_double_value(*d),
            Val::Int(i) => v.numeric_value.set_int_value(*i),
            Val::Uint(u) => v.numeric_value.set_uint_value(*u),
            Val::Sint(i) => v.numeric_value.set_sint_value(*i),
            Val::Bool(b) => v.numeric_value.set_bool_value(*b),
            Val::Null => v.numeric_value.set_null_value(0),
        }
        v
    }

    /// The value of tippecanoe-rs's `MvtValue`.
    pub fn from_mvt(v: &MvtValue) -> Val<'static> {
        let n = &v.numeric_value;
        match v.type_ {
            MVT_STRING => Val::Str(Cow::Owned(v.get_str().to_string())),
            MVT_FLOAT => Val::Float(n.float_value()),
            MVT_DOUBLE => Val::Double(n.double_value()),
            MVT_INT => Val::Int(n.int_value()),
            MVT_UINT => Val::Uint(n.uint_value()),
            MVT_SINT => Val::Sint(n.sint_value()),
            MVT_BOOL => Val::Bool(n.bool_value()),
            _ => Val::Null,
        }
    }
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag32(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

fn unzigzag32(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// A string field, as `String::from_utf8_lossy` reads it.
fn lossy(b: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(b)
}

pub struct Feature {
    pub id: u64,
    pub has_id: bool,
    pub type_: i32,
    /// In [`Decoded::tags`].
    pub tags: Range<u32>,
    /// The geometry commands, in [`Decoded::geoms`].
    pub geom: Range<u32>,
}

pub struct Layer<'a> {
    pub name: Cow<'a, str>,
    pub version: i32,
    pub extent: i64,
    pub keys: Vec<Cow<'a, str>>,
    pub values: Vec<Val<'a>>,
    pub features: Vec<Feature>,
}

/// A tile as `mvt_tile::decode` reads it, borrowing its strings.
#[derive(Default)]
pub struct Decoded<'a> {
    pub layers: Vec<Layer<'a>>,
    pub tags: Vec<u32>,
    pub geoms: Vec<u32>,
}

impl<'a> Decoded<'a> {
    /// `mvt_tile::decode` of an uncompressed tile.
    pub fn decode(src: &'a [u8]) -> protozero::Result<Decoded<'a>> {
        let mut t = Decoded::default();
        let mut reader = PbfReader::new(src);
        while reader.next()? {
            if reader.tag() != 3 {
                reader.skip()?;
                continue;
            }
            let mut layer_reader = reader.get_message()?;
            let mut layer = Layer {
                name: Cow::Borrowed(""),
                version: 0,
                extent: 0,
                keys: Vec::new(),
                values: Vec::new(),
                features: Vec::new(),
            };
            while layer_reader.next()? {
                match layer_reader.tag() {
                    1 => layer.name = lossy(layer_reader.get_view()?),
                    3 => layer.keys.push(lossy(layer_reader.get_view()?)),
                    4 => {
                        let mut value_reader = layer_reader.get_message()?;
                        let mut value = Val::Null;
                        while value_reader.next()? {
                            value = match value_reader.tag() {
                                1 => Val::Str(lossy(value_reader.get_view()?)),
                                2 => Val::Float(value_reader.get_float()?),
                                3 => Val::Double(value_reader.get_double()?),
                                4 => Val::Int(value_reader.get_int64()?),
                                5 => Val::Uint(value_reader.get_uint64()?),
                                6 => Val::Sint(value_reader.get_sint64()?),
                                7 => Val::Bool(value_reader.get_bool()?),
                                _ => {
                                    value_reader.skip()?;
                                    continue;
                                }
                            };
                        }
                        layer.values.push(value);
                    }
                    5 => layer.extent = layer_reader.get_uint32()? as i64,
                    15 => layer.version = layer_reader.get_uint32()? as i32,
                    2 => {
                        let mut feature_reader = layer_reader.get_message()?;
                        let mut f = Feature {
                            id: 0,
                            has_id: false,
                            type_: 0,
                            tags: t.tags.len() as u32..0,
                            geom: t.geoms.len() as u32..0,
                        };
                        while feature_reader.next()? {
                            match feature_reader.tag() {
                                1 => {
                                    f.id = feature_reader.get_uint64()?;
                                    f.has_id = true;
                                }
                                2 => {
                                    for it in feature_reader.get_packed_uint32()? {
                                        t.tags.push(it?);
                                    }
                                }
                                3 => f.type_ = feature_reader.get_enum()?,
                                4 => {
                                    for it in feature_reader.get_packed_uint32()? {
                                        t.geoms.push(it?);
                                    }
                                }
                                _ => feature_reader.skip()?,
                            }
                        }
                        f.tags.end = t.tags.len() as u32;
                        f.geom.end = t.geoms.len() as u32;
                        layer.features.push(f);
                    }
                    _ => layer_reader.skip()?,
                }
            }
            t.layers.push(layer);
        }
        Ok(t)
    }
}

/// `is_compressed`: gzip or zlib, by the first two bytes.
pub fn is_compressed(data: &[u8]) -> bool {
    data.len() > 2 && ((data[0] == 0x78 && data[1] == 0x9C) || (data[0] == 0x1F && data[1] == 0x8B))
}

/// Decompresses a tile, with libdeflate where it can, which makes the same
/// bytes as zlib for any stream both accept. Anything libdeflate refuses
/// goes to tippecanoe-rs's zlib `decompress`, for zlib's result and messages.
/// False if zlib fails too, which it has said.
pub fn decompress(d: &mut libdeflater::Decompressor, data: &[u8], out: &mut Vec<u8>) -> bool {
    out.clear();
    if data[0] == 0x1F && data.len() >= 18 {
        let n = data.len();
        let isize = u32::from_le_bytes([data[n - 4], data[n - 3], data[n - 2], data[n - 1]]);
        // ISIZE is the size modulo 2^32, and nothing checks it before inflating
        if (isize as usize) < (64 << 20).max(n * 1032) {
            out.resize(isize as usize, 0);
            if let Ok(k) = d.gzip_decompress(data, out)
                && k == isize as usize
            {
                return true;
            }
            out.clear();
        }
    }
    tippecanoe::mvt::decompress(data, out) != 0
}

/// What a feature's geometry becomes in the output tile.
#[derive(Clone)]
pub(crate) enum Geom {
    None,
    /// Commands of input tile `tile`, unchanged in scale.
    Commands {
        tile: u32,
        range: Range<u32>,
    },
    /// Decoded and rescaled points in [`Out::points`].
    Points(Range<u32>),
}

pub(crate) struct OutFeature {
    pub id: u64,
    pub has_id: bool,
    pub type_: i32,
    /// Pairs of key and value numbers, in [`Out::tags`].
    pub tags: Range<u32>,
    pub geom: Geom,
}

pub(crate) struct OutLayer<'t> {
    pub name: &'t str,
    pub version: i32,
    pub extent: i64,
    keys: Vec<&'t str>,
    /// `key_dedup`: the slot of a key hash, and the key number it last held.
    key_slots: FxHashMap<u32, u32>,
    values: Vec<&'t Val<'t>>,
    value_slots: FxHashMap<u32, u32>,
    pub features: Vec<OutFeature>,
}

impl<'t> OutLayer<'t> {
    pub fn new(name: &'t str, version: i32, extent: i64) -> OutLayer<'t> {
        OutLayer {
            name,
            version,
            extent,
            keys: Vec::new(),
            key_slots: FxHashMap::default(),
            values: Vec::new(),
            value_slots: FxHashMap::default(),
            features: Vec::new(),
        }
    }

    /// `mvt_layer::tag`: the key and value numbers, adding them to the
    /// layer's lists unless the hash slot of each holds the same already.
    pub fn tag(&mut self, key: &'t str, value: &'t Val<'t>, tags: &mut Vec<u32>) {
        let slot = (fnv1a(key.as_bytes()) % 65536) as u32;
        let k = match self.key_slots.get(&slot) {
            Some(&k) if self.keys[k as usize] == key => k,
            _ => {
                let k = self.keys.len() as u32;
                self.keys.push(key);
                self.key_slots.insert(slot, k);
                k
            }
        };
        tags.push(k);

        let slot = (value.hash() % 65536) as u32;
        let v = match self.value_slots.get(&slot) {
            Some(&v) if self.values[v as usize].same(value) => v,
            _ => {
                let v = self.values.len() as u32;
                self.values.push(value);
                self.value_slots.insert(slot, v);
                v
            }
        };
        tags.push(v);
    }
}

/// A point of decoded geometry: `mvt_geometry`.
#[derive(Clone, Copy)]
pub(crate) struct Point {
    pub op: i32,
    pub x: i64,
    pub y: i64,
}

/// The output tile being built.
#[derive(Default)]
pub(crate) struct Out<'t> {
    pub layers: Vec<OutLayer<'t>>,
    pub tags: Vec<u32>,
    pub points: Vec<Point>,
}

impl Out<'_> {
    /// `mvt_tile::encode` of the layers that have features, into `pbf`.
    /// False if none has.
    pub fn encode(
        &self,
        tiles: &[Decoded<'_>],
        scratch: &mut Scratch,
        pbf: &mut Vec<u8>,
    ) -> Result<bool> {
        pbf.clear();
        let mut anything = false;
        for layer in &self.layers {
            if layer.features.is_empty() {
                continue;
            }
            anything = true;
            encode_layer(layer, self, tiles, scratch)?;
            pbf.push(3 << 3 | 2);
            varint(pbf, scratch.layer.len() as u64);
            pbf.extend_from_slice(&scratch.layer);
        }
        Ok(anything)
    }
}

/// The decoding of a feature's commands in `mvt_tile::decode`, calling `f`
/// with each point.
pub(crate) fn for_each_point(geoms: &[u32], mut f: impl FnMut(Point)) {
    let (mut px, mut py) = (0i64, 0i64);
    let mut g = 0;
    while g < geoms.len() {
        let geom = geoms[g];
        let op = geom & 7;
        let count = geom >> 3;
        if op == MVT_MOVETO as u32 || op == MVT_LINETO as u32 {
            let mut k = 0;
            while k < count && g + 2 < geoms.len() {
                px += unzigzag32(geoms[g + 1]) as i64;
                py += unzigzag32(geoms[g + 2]) as i64;
                g += 2;
                f(Point {
                    op: op as i32,
                    x: px,
                    y: py,
                });
                k += 1;
            }
        } else {
            f(Point {
                op: op as i32,
                x: 0,
                y: 0,
            });
        }
        g += 1;
    }
}

/// What a tile-join run does to each feature: the options that `append_tile`
/// reads.
pub struct Rules {
    pub header: Vec<String>,
    pub mapping: std::collections::HashMap<String, Row>,
    pub exclude: BTreeSet<String>,
    pub include: BTreeSet<String>,
    pub keep_layers: BTreeSet<String>,
    pub remove_layers: BTreeSet<String>,
    pub renames: std::collections::BTreeMap<String, String>,
    pub ifmatched: bool,
    pub exclude_all: bool,
    pub exclude_all_tile_attributes: bool,
    pub exclude_all_tile_geometries: bool,
    pub filter: Option<tippecanoe::jsonpull::JsonTree>,
    pub unidecode_data: Vec<String>,
}

impl Rules {
    fn keeps(&self, key: &str, exclude_attributes: &BTreeSet<String>) -> bool {
        self.include.contains(key)
            || (!self.exclude_all
                && !self.exclude.contains(key)
                && !exclude_attributes.contains(key))
    }
}

/// A value of a feature on its way to the output: from the tile, or from the
/// CSV with the text tile-stats keep for it.
#[derive(Clone, Copy)]
enum Attr<'t> {
    Tile(&'t Val<'t>),
    Csv(&'t Val<'t>, i32, &'t str),
}

/// The attributes of one output feature: `match`'s `attributes` and
/// `key_order`, as small lists rather than a map, since features have few.
#[derive(Default)]
struct Attrs<'t> {
    attributes: Vec<(&'t str, Attr<'t>)>,
    key_order: Vec<&'t str>,
}

impl<'t> Attrs<'t> {
    fn clear(&mut self) {
        self.attributes.clear();
        self.key_order.clear();
    }

    /// `attributes.insert`, which leaves an existing value alone.
    fn insert(&mut self, k: &'t str, v: Attr<'t>) {
        if !self.attributes.iter().any(|(key, _)| *key == k) {
            self.attributes.push((k, v));
        }
    }

    fn erase(&mut self, k: &str) {
        if let Some(i) = self.attributes.iter().position(|(key, _)| *key == k) {
            self.attributes.remove(i);
        }
    }

    fn take(&mut self, k: &str) -> Option<Attr<'t>> {
        let i = self.attributes.iter().position(|(key, _)| *key == k)?;
        Some(self.attributes.remove(i).1)
    }
}

/// Scratch space kept between tiles by each thread.
#[derive(Default)]
pub struct Scratch {
    pub decompressor: Option<libdeflater::Decompressor>,
    pub buffers: Vec<Vec<u8>>,
    values: Vec<u8>,
    order: Vec<(u32, u32)>,
    mapping: Vec<u32>,
    feature: Vec<u8>,
    layer: Vec<u8>,
    geometry: Vec<u32>,
    encoded_tags: Vec<u32>,
}

fn decode_error(z: i32, x: u32, y: u32) -> Error {
    Error::new(
        EXIT_MVT,
        format!("PBF decoding error in tile {}/{}/{}", z, x, y),
    )
}

/// Decompresses each of `inputs` as needed and decodes it. The buffers in
/// `scratch` hold the decompressed bytes.
pub fn decode_all<'a>(
    inputs: &'a [&'a [u8]],
    buffers: &'a mut [Vec<u8>],
    decompressor: &mut libdeflater::Decompressor,
    z: i32,
    x: u32,
    y: u32,
) -> Result<Vec<Decoded<'a>>> {
    // Decompress all first, so that the decoded tiles can borrow the buffers.
    for (data, buf) in inputs.iter().zip(buffers.iter_mut()) {
        buf.clear();
        if is_compressed(data) && !decompress(decompressor, data, buf) {
            // tippecanoe-rs's decompress has said why
            return Err(Error::new(EXIT_MVT, String::new()));
        }
    }
    let mut out = Vec::with_capacity(inputs.len());
    for (data, buf) in inputs.iter().zip(buffers.iter()) {
        let src: &'a [u8] = if is_compressed(data) { buf } else { data };
        out.push(Decoded::decode(src).map_err(|_| decode_error(z, x, y))?);
    }
    Ok(out)
}

/// The world coordinates `append_tile` collects for the bounding box of what
/// one input layer added.
struct Bounds {
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    minx2: i64,
    maxx2: i64,
}

impl Bounds {
    fn new() -> Bounds {
        Bounds {
            minx: i64::MAX,
            miny: i64::MAX,
            maxx: i64::MIN,
            maxy: i64::MIN,
            minx2: i64::MAX,
            maxx2: i64::MIN,
        }
    }

    /// Adds a point of a tile of `extent` at z/x/y, pinned to the tile.
    fn add(&mut self, p: Point, extent: i64, z: i32, x: u32, y: u32) {
        let mut gx = extent.min(0.max(p.x));
        let mut gy = extent.min(0.max(p.y));
        gx = gx * (1i64 << (32 - z)) / extent;
        gy = gy * (1i64 << (32 - z)) / extent;
        gx += (1i64 << (32 - z)) * x as i64;
        gy += (1i64 << (32 - z)) * y as i64;
        self.minx = self.minx.min(gx);
        self.miny = self.miny.min(gy);
        self.maxx = self.maxx.max(gx);
        self.maxy = self.maxy.max(gy);
        if gx < (1i64 << 31) {
            gx += 1i64 << 32;
        }
        self.minx2 = self.minx2.min(gx);
        self.maxx2 = self.maxx2.max(gx);
    }
}

/// Joins the decoded input tiles of z/x/y, in order, into one tile, and
/// returns its encoding, or None if no feature is left. The tile-stats and
/// bounds go to `acc`.
#[allow(clippy::too_many_arguments)]
pub fn join_tile<'t>(
    tiles: &'t [Decoded<'t>],
    z: i32,
    x: u32,
    y: u32,
    rules: &'t Rules,
    acc: &mut Accum,
    scratch: &mut Scratch,
    pbf: &mut Vec<u8>,
) -> Result<bool> {
    let mut out = Out::default();
    let mut attrs = Attrs::default();
    let mut exclude_attributes: BTreeSet<String> = BTreeSet::new();

    for (ti, tile) in tiles.iter().enumerate() {
        for layer in &tile.layers {
            let name: &'t str = match rules.renames.get(layer.name.as_ref()) {
                Some(found) => found,
                None => &layer.name,
            };
            if !rules.keep_layers.is_empty() && !rules.keep_layers.contains(name) {
                continue;
            }
            if rules.remove_layers.contains(name) {
                continue;
            }

            let ol = match out.layers.iter().position(|l| l.name == name) {
                Some(ol) => ol,
                None => {
                    out.layers
                        .push(OutLayer::new(name, layer.version, layer.extent));
                    out.layers.len() - 1
                }
            };

            if layer.extent != out.layers[ol].extent && layer.extent > out.layers[ol].extent {
                // tile-join scales up the layer so far rather than scaling
                // down the one being added
                let from = out.layers[ol].extent;
                rescale(&mut out, ol, tiles, layer.extent, from)?;
                out.layers[ol].extent = layer.extent;
            }
            let out_extent = out.layers[ol].extent;

            // the layer as tippecanoe-rs's types, for the feature filter
            let filter_layer = rules.filter.as_ref().map(|_| MvtLayer {
                version: layer.version,
                name: name.to_string(),
                features: Vec::new(),
                keys: layer.keys.iter().map(|k| k.to_string()).collect(),
                values: layer.values.iter().map(|v| v.to_mvt()).collect(),
                extent: layer.extent,
                key_dedup: Vec::new(),
                value_dedup: Vec::new(),
            });

            let mut bounds = Bounds::new();
            let mut features_added_to_layer = false;
            let mut layer_stats: Option<usize> = None;

            for feat in &layer.features {
                let tags = &tile.tags[feat.tags.start as usize..feat.tags.end as usize];
                exclude_attributes.clear();
                if let (Some(filter), Some(fl)) = (&rules.filter, &filter_layer) {
                    let root = filter.root();
                    if root.type_() != JsonType::Hash {
                        return Err(filter_not_a_hash(root));
                    }
                    let mf = MvtFeature {
                        tags: tags.to_vec(),
                        type_: feat.type_,
                        id: feat.id,
                        has_id: feat.has_id,
                        ..Default::default()
                    };
                    if !tippecanoe::evaluator::evaluate_feature(
                        &mf,
                        fl,
                        root,
                        &mut exclude_attributes,
                        z,
                        &rules.unidecode_data,
                    ) {
                        continue;
                    }
                }

                let key_of = |i: u32| -> Result<&'t str> {
                    layer
                        .keys
                        .get(i as usize)
                        .map(|k| k.as_ref())
                        .ok_or_else(|| bad_index(z, x, y))
                };
                let value_of = |i: u32| -> Result<&'t Val<'t>> {
                    layer
                        .values
                        .get(i as usize)
                        .ok_or_else(|| bad_index(z, x, y))
                };

                // look for CSV matches
                attrs.clear();
                let mut matched = false;
                for t in (0..tags.len().saturating_sub(1)).step_by(2) {
                    let key = key_of(tags[t])?;
                    let val = value_of(tags[t + 1])?;
                    if let Val::Null = val {
                        continue;
                    }
                    if !rules.exclude_all_tile_attributes && rules.keeps(key, &exclude_attributes) {
                        attrs.insert(key, Attr::Tile(val));
                        attrs.key_order.push(key);
                    }
                    if !rules.header.is_empty() && key == rules.header[0] {
                        let sv = val.serial().1;
                        if let Some(row) = rules.mapping.get(sv.as_ref()) {
                            matched = true;
                            if row.too_long {
                                return Err(super::csv_row_too_long(&sv));
                            }
                            for field in &row.fields {
                                let joinkey: &'t str = &rules.header[field.column];
                                if rules.include.contains(joinkey)
                                    || (!rules.exclude_all
                                        && !rules.exclude.contains(joinkey)
                                        && !exclude_attributes.contains(joinkey)
                                        && field.attr_type != MVT_NULL)
                                {
                                    field.warn();
                                    attrs.erase(joinkey);
                                    attrs.insert(
                                        joinkey,
                                        Attr::Csv(&field.value, field.sv_type, &field.text),
                                    );
                                    attrs.key_order.push(joinkey);
                                }
                            }
                        }
                    }
                }

                if !matched {
                    if rules.ifmatched {
                        continue;
                    }
                    // keep the feature as it was
                    attrs.clear();
                    if !rules.exclude_all_tile_attributes {
                        for t in (0..tags.len().saturating_sub(1)).step_by(2) {
                            let key = key_of(tags[t])?;
                            let val = value_of(tags[t + 1])?;
                            if rules.keeps(key, &exclude_attributes) {
                                attrs.insert(key, Attr::Tile(val));
                                attrs.key_order.push(key);
                            }
                        }
                    }
                }

                let li = *layer_stats.get_or_insert_with(|| acc.layer(name, z));
                let max_samples = acc.max_samples;
                let stats = &mut acc.layers[li].1;
                let tags_start = out.tags.len() as u32;
                for i in 0..attrs.key_order.len() {
                    let k = attrs.key_order[i];
                    if let Some(fa) = attrs.take(k) {
                        match fa {
                            Attr::Tile(v) => {
                                out.layers[ol].tag(k, v, &mut out.tags);
                                let (t, s) = v.serial();
                                stats.add(k, t, &s, max_samples);
                            }
                            Attr::Csv(v, t, s) => {
                                out.layers[ol].tag(k, v, &mut out.tags);
                                stats.add(k, t, s, max_samples);
                            }
                        }
                    }
                }
                let tags_range = tags_start..out.tags.len() as u32;

                let (type_, geom) = if rules.exclude_all_tile_geometries {
                    (-1, Geom::None)
                } else {
                    let commands = &tile.geoms[feat.geom.start as usize..feat.geom.end as usize];
                    let geom = if layer.extent != out_extent {
                        let start = out.points.len() as u32;
                        for_each_point(commands, |p| {
                            out.points.push(Point {
                                op: p.op,
                                x: p.x * out_extent / layer.extent,
                                y: p.y * out_extent / layer.extent,
                            })
                        });
                        let range = start..out.points.len() as u32;
                        if out_extent == 0 && range.start != range.end {
                            return Err(zero_extent(name, z, x, y));
                        }
                        for p in &out.points[range.start as usize..range.end as usize] {
                            if p.op == MVT_MOVETO || p.op == MVT_LINETO {
                                bounds.add(*p, out_extent, z, x, y);
                            }
                        }
                        Geom::Points(range)
                    } else {
                        let mut zero = false;
                        for_each_point(commands, |p| {
                            if p.op == MVT_MOVETO || p.op == MVT_LINETO {
                                if out_extent == 0 {
                                    zero = true;
                                } else {
                                    bounds.add(p, out_extent, z, x, y);
                                }
                            }
                        });
                        if zero {
                            return Err(zero_extent(name, z, x, y));
                        }
                        Geom::Commands {
                            tile: ti as u32,
                            range: feat.geom.clone(),
                        }
                    };
                    (feat.type_, geom)
                };

                features_added_to_layer = true;
                out.layers[ol].features.push(OutFeature {
                    id: feat.id,
                    has_id: feat.has_id,
                    type_,
                    tags: tags_range,
                    geom,
                });

                let stats = &mut acc.layers[li].1;
                stats.zoom(z);
                if feat.type_ == MVT_POINT {
                    stats.points += 1;
                } else if feat.type_ == MVT_LINESTRING {
                    stats.lines += 1;
                } else if feat.type_ == MVT_POLYGON {
                    stats.polygons += 1;
                }
            }

            if features_added_to_layer {
                acc.bounds(
                    bounds.minx,
                    bounds.miny,
                    bounds.maxx,
                    bounds.maxy,
                    bounds.minx2,
                    bounds.maxx2,
                );
            }
        }
    }

    out.encode(tiles, scratch, pbf)
}

fn bad_index(z: i32, x: u32, y: u32) -> Error {
    Error::new(
        EXIT_MVT,
        format!(
            "PBF decoding error in tile {}/{}/{}: a feature's key or value is out of range",
            z, x, y
        ),
    )
}

fn zero_extent(name: &str, z: i32, x: u32, y: u32) -> Error {
    Error::new(
        EXIT_MVT,
        format!(
            "Layer {} of tile {}/{}/{} has an extent of 0",
            name, z, x, y
        ),
    )
}

fn filter_not_a_hash(root: JsonRef<'_>) -> Error {
    let json = root.stringify_raw();
    let n = json.iter().position(|&c| c == 0).unwrap_or(json.len());
    Error::new(
        EXIT_JSON,
        format!(
            "Error: filter is not a hash: {}",
            String::from_utf8_lossy(&json[..n])
        ),
    )
}

/// Scales the features of output layer `ol` so far from `from` to `to`, as
/// tile-join does when a later input has the larger extent.
fn rescale(out: &mut Out<'_>, ol: usize, tiles: &[Decoded<'_>], to: i64, from: i64) -> Result<()> {
    for fi in 0..out.layers[ol].features.len() {
        let range = match &out.layers[ol].features[fi].geom {
            Geom::None => continue,
            Geom::Points(r) => r.clone(),
            Geom::Commands { tile, range } => {
                let commands =
                    &tiles[*tile as usize].geoms[range.start as usize..range.end as usize];
                let start = out.points.len() as u32;
                for_each_point(commands, |p| out.points.push(p));
                let r = start..out.points.len() as u32;
                out.layers[ol].features[fi].geom = Geom::Points(r.clone());
                r
            }
        };
        if from == 0 && range.start != range.end {
            return Err(Error::new(
                EXIT_MVT,
                format!("Layer {} has an extent of 0", out.layers[ol].name),
            ));
        }
        for p in &mut out.points[range.start as usize..range.end as usize] {
            p.x = p.x * to / from;
            p.y = p.y * to / from;
        }
    }
    Ok(())
}

/// The geometry commands `mvt_tile::encode` writes for a feature's points,
/// given one at a time.
struct CommandWriter<'a> {
    out: &'a mut Vec<u32>,
    px: i64,
    py: i64,
    cmd_idx: i64,
    cmd: i32,
    length: i32,
    error: Option<Error>,
}

impl<'a> CommandWriter<'a> {
    fn new(out: &'a mut Vec<u32>) -> CommandWriter<'a> {
        CommandWriter {
            out,
            px: 0,
            py: 0,
            cmd_idx: -1,
            cmd: -1,
            length: 0,
            error: None,
        }
    }

    fn push(&mut self, g: Point) {
        if self.error.is_some() {
            return;
        }
        let op = g.op;
        if op != self.cmd {
            if self.cmd_idx >= 0 {
                self.out[self.cmd_idx as usize] = ((self.length << 3) | (self.cmd & 7)) as u32;
            }
            self.cmd = op;
            self.length = 0;
            self.cmd_idx = self.out.len() as i64;
            self.out.push(0);
        }
        if op == MVT_MOVETO || op == MVT_LINETO {
            let dx = g.x - self.px;
            let dy = g.y - self.py;
            if dx < i32::MIN as i64
                || dx > i32::MAX as i64
                || dy < i32::MIN as i64
                || dy > i32::MAX as i64
            {
                self.error = Some(Error::new(
                    EXIT_IMPOSSIBLE,
                    format!("Internal error: Geometry delta is too big: {},{}", dx, dy),
                ));
                return;
            }
            self.out.push(zigzag32(dx as i32));
            self.out.push(zigzag32(dy as i32));
            self.px = g.x;
            self.py = g.y;
            self.length += 1;
        } else if op == MVT_CLOSEPATH {
            self.length += 1;
        } else {
            self.error = Some(Error::new(
                EXIT_IMPOSSIBLE,
                "\nInternal error: corrupted geometry",
            ));
        }
    }

    fn finish(self) -> Result<()> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.cmd_idx >= 0 {
            self.out[self.cmd_idx as usize] = ((self.length << 3) | (self.cmd & 7)) as u32;
        }
        Ok(())
    }
}

/// Encodes `layer` into `scratch.layer` as `mvt_tile::encode` does.
fn encode_layer(
    layer: &OutLayer<'_>,
    out: &Out<'_>,
    tiles: &[Decoded<'_>],
    scratch: &mut Scratch,
) -> Result<()> {
    let w = &mut scratch.layer;
    w.clear();
    w.push(15 << 3);
    varint(w, layer.version as u32 as u64);
    w.push(1 << 3 | 2);
    varint(w, layer.name.len() as u64);
    w.extend_from_slice(layer.name.as_bytes());
    w.push(5 << 3);
    varint(w, layer.extent as u32 as u64);
    for key in &layer.keys {
        w.push(3 << 3 | 2);
        varint(w, key.len() as u64);
        w.extend_from_slice(key.as_bytes());
    }

    // The values in the order of their encoding, duplicates crunched out.
    let values = &mut scratch.values;
    let order = &mut scratch.order;
    values.clear();
    order.clear();
    let mut spans: Vec<Range<usize>> = Vec::with_capacity(layer.values.len());
    for v in &layer.values {
        let start = values.len();
        v.encode(values)?;
        spans.push(start..values.len());
    }
    order.extend((0..layer.values.len() as u32).map(|i| (i, 0)));
    // the index decides between equal encodings, so this is the stable order
    order.sort_unstable_by(|a, b| {
        values[spans[a.0 as usize].clone()]
            .cmp(&values[spans[b.0 as usize].clone()])
            .then(a.0.cmp(&b.0))
    });
    let mapping = &mut scratch.mapping;
    mapping.clear();
    mapping.resize(layer.values.len(), 0);
    let mut value_index = 0u32;
    let mut v = 0;
    while v < order.len() {
        let here = spans[order[v].0 as usize].clone();
        mapping[order[v].0 as usize] = value_index;
        w.push(4 << 3 | 2);
        varint(w, here.len() as u64);
        w.extend_from_slice(&values[here.clone()]);
        while v + 1 < order.len()
            && values[spans[order[v + 1].0 as usize].clone()] == values[here.clone()]
        {
            mapping[order[v + 1].0 as usize] = value_index;
            v += 1;
        }
        value_index += 1;
        v += 1;
    }

    let fw = &mut scratch.feature;
    let geometry = &mut scratch.geometry;
    let encoded_tags = &mut scratch.encoded_tags;
    for feature in &layer.features {
        fw.clear();
        if feature.type_ >= 0 {
            fw.push(3 << 3);
            varint(fw, feature.type_ as i64 as u64);
        }
        let tags = &out.tags[feature.tags.start as usize..feature.tags.end as usize];
        if !tags.is_empty() {
            encoded_tags.clear();
            for (i, &t) in tags.iter().enumerate() {
                encoded_tags.push(if i % 2 == 1 { mapping[t as usize] } else { t });
            }
            packed(fw, 2, encoded_tags);
        }
        if feature.has_id {
            fw.push(1 << 3);
            varint(fw, feature.id);
        }
        geometry.clear();
        let mut writer = CommandWriter::new(geometry);
        match &feature.geom {
            Geom::None => {}
            Geom::Commands { tile, range } => {
                let commands =
                    &tiles[*tile as usize].geoms[range.start as usize..range.end as usize];
                for_each_point(commands, |p| writer.push(p));
            }
            Geom::Points(r) => {
                for p in &out.points[r.start as usize..r.end as usize] {
                    writer.push(*p);
                }
            }
        }
        writer.finish()?;
        packed(fw, 4, geometry);
        w.push(2 << 3 | 2);
        varint(w, fw.len() as u64);
        w.extend_from_slice(fw);
    }
    Ok(())
}

/// A packed field of varints, left out when empty, as protozero writes it.
fn packed(w: &mut Vec<u8>, tag: u32, values: &[u32]) {
    if values.is_empty() {
        return;
    }
    let len: usize = values.iter().map(|&v| varint_len(v as u64)).sum();
    varint(w, (tag << 3 | 2) as u64);
    varint(w, len as u64);
    for &v in values {
        varint(w, v as u64);
    }
}

fn varint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// The points of a feature's commands, as `mvt_tile::decode` makes them.
#[cfg(test)]
pub(crate) fn for_each_point_for_test(geoms: &[u32], mut f: impl FnMut(i32, i64, i64)) {
    for_each_point(geoms, |p| f(p.op, p.x, p.y));
}

/// `mvt_value::operator==`.
#[cfg(test)]
pub(crate) fn same_for_test(a: &Val<'_>, b: &Val<'_>) -> bool {
    a.same(b)
}
