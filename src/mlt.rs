//! MapLibre Tiles (MLT), written instead of Mapbox Vector Tiles with
//! `--tile-format=mlt`. See DESIGN.md.
//!
//! The tiler makes each tile's MVT bytes as usual, and `encode_tile` turns
//! them into MLT with mlt-core before the tile is compressed and its size
//! checked. An MLT tile therefore holds the layers, features, ids, geometry
//! and attribute values of the MVT tile, in the same order, typed as MLT's
//! columns require: one type per attribute and layer, where integers mixed
//! with floats become 64-bit floats and any other mix becomes strings. The
//! `web` profile stores integer columns that need more than 32 bits as
//! doubles where that is exact, because MapLibre GL JS reads 64-bit integers
//! as BigInt, which its style expressions do not compare as numbers.
//!
//! mlt-core needs Rust 1.98, so it is behind the `mlt` cargo feature.

/// The tile encoding manby writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TileFormat {
    #[default]
    Mvt,
    Mlt,
}

impl TileFormat {
    pub fn parse(s: &str) -> Option<TileFormat> {
        match s {
            "mvt" | "pbf" => Some(TileFormat::Mvt),
            "mlt" => Some(TileFormat::Mlt),
            _ => None,
        }
    }

    /// The `format` of the tileset metadata (MBTiles, TileJSON, PMTiles).
    pub fn metadata_format(self) -> &'static str {
        match self {
            TileFormat::Mvt => "pbf",
            TileFormat::Mlt => "mlt",
        }
    }

    /// The extension of tile files in a directory, with its dot.
    pub fn extension(self) -> &'static str {
        match self {
            TileFormat::Mvt => ".pbf",
            TileFormat::Mlt => ".mlt",
        }
    }
}

/// Which encodings the MLT encoder may use, by the clients the tiles are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MltProfile {
    /// Tiles every MapLibre GL JS that reads MLT (5.12 and later) reads, with
    /// attribute values of the same JavaScript types as from MVT: no polygon
    /// triangles (GL JS ignores them), no FastPFOR, FSST or shared
    /// dictionaries (GL JS before 6.0 cannot read them all), and integer
    /// columns that need more than 32 bits as doubles where that is exact
    /// (GL JS reads 64-bit integers as BigInt, which style expressions do
    /// not compare as numbers).
    #[default]
    Web,
    /// mlt-core's defaults for MapLibre Native: polygon triangles where they
    /// save the client more time than their bytes cost, and every codec.
    /// MapLibre GL JS reads these from 6.0, with 64-bit integers as BigInt.
    Native,
    /// The fewest bytes. Sorts features within each layer when that is
    /// smaller, so their drawing order can differ from MVT.
    MinSize,
    /// The quickest to encode: no sorting, no optional codecs.
    Fast,
}

impl MltProfile {
    pub fn parse(s: &str) -> Option<MltProfile> {
        match s {
            "web" => Some(MltProfile::Web),
            "native" => Some(MltProfile::Native),
            "min-size" => Some(MltProfile::MinSize),
            "fast" => Some(MltProfile::Fast),
            _ => None,
        }
    }
}

/// How tiles are encoded as MLT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MltSettings {
    pub profile: MltProfile,
    /// The MLT bytes are gzipped afterwards (no `-pC`).
    pub gzipped: bool,
}

/// Whether this manby was built with MLT support.
pub const AVAILABLE: bool = cfg!(feature = "mlt");

#[cfg(feature = "mlt")]
mod imp {
    use mlt_core::compact_str::{CompactString, ToCompactString};
    use mlt_core::encoder::{EncoderConfig, Network, Profile};
    use mlt_core::geo_types::Coord;
    use mlt_core::{
        Decoder, GeometryType, InferredKind, LayerWriter, Parser, PropKind, PropValueRef, TileLayer,
    };

    use super::{MltProfile, MltSettings};

    /// mlt-core's encoder settings for `s`.
    pub fn encoder_config(s: &MltSettings) -> EncoderConfig {
        // Tiles behind gzip are priced like tiles a client fetches over a
        // network; tiles without it are what the client stores and reads.
        let network = if s.gzipped {
            Network::Cellular4G
        } else {
            Network::Offline
        };
        match s.profile {
            // mlt-core's pricing for GL JS online, which leaves out FSST and
            // FastPFOR, without shared dictionaries, whatever the compression:
            // decoders before GL JS 6.0 do not read them all.
            MltProfile::Web => EncoderConfig::from_profile(Profile::Web(Network::Cellular4G))
                .with_shared_dict(false),
            MltProfile::Native => EncoderConfig::from_profile(Profile::Native(network)),
            MltProfile::MinSize => EncoderConfig::from_profile(Profile::MinSize),
            MltProfile::Fast => EncoderConfig::from_profile(Profile::FastEncoding),
        }
    }

    /// Protobuf fields of the MVT bytes, borrowed from them.
    struct Pbf<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Pbf<'a> {
        fn new(buf: &'a [u8]) -> Pbf<'a> {
            Pbf { buf, pos: 0 }
        }

        fn varint(&mut self) -> Result<u64, String> {
            let mut v = 0u64;
            for shift in (0..64).step_by(7) {
                let b = *self.buf.get(self.pos).ok_or("MVT: truncated varint")?;
                self.pos += 1;
                v |= u64::from(b & 0x7F) << shift;
                if b < 0x80 {
                    return Ok(v);
                }
            }
            Err("MVT: varint too long".to_string())
        }

        fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
            let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
            let end = end.ok_or("MVT: truncated field")?;
            let out = &self.buf[self.pos..end];
            self.pos = end;
            Ok(out)
        }

        /// The next field: its number and value.
        fn field(&mut self) -> Result<Option<(u64, Field<'a>)>, String> {
            if self.pos >= self.buf.len() {
                return Ok(None);
            }
            let key = self.varint()?;
            let value = match key & 7 {
                0 => Field::Varint(self.varint()?),
                1 => Field::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().unwrap())),
                2 => {
                    let n = self.varint()? as usize;
                    Field::Bytes(self.take(n)?)
                }
                5 => Field::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().unwrap())),
                w => return Err(format!("MVT: wire type {}", w)),
            };
            Ok(Some((key >> 3, value)))
        }
    }

    enum Field<'a> {
        Varint(u64),
        Fixed64(u64),
        Fixed32(u32),
        Bytes(&'a [u8]),
    }

    /// The packed varints of `bytes`, or the one varint of a field written unpacked.
    fn packed(field: Field<'_>, out: &mut Vec<u32>) -> Result<(), String> {
        match field {
            Field::Bytes(b) => {
                let mut p = Pbf::new(b);
                while p.pos < b.len() {
                    out.push(p.varint()? as u32);
                }
            }
            Field::Varint(v) => out.push(v as u32),
            _ => return Err("MVT: packed field of the wrong type".to_string()),
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum Value<'a> {
        Str(&'a str),
        Float(f32),
        Double(f64),
        Int(i64),
        UInt(u64),
        SInt(i64),
        Bool(bool),
        Null,
    }

    impl Value<'_> {
        /// The column kind this value asks for, as mlt-core's MVT reader decides it.
        fn kind(self) -> InferredKind {
            match self {
                Value::Bool(_) => InferredKind::Bool,
                Value::Int(_) | Value::SInt(_) => InferredKind::I64,
                Value::UInt(_) => InferredKind::U64,
                Value::Float(_) => InferredKind::F32,
                Value::Double(_) => InferredKind::F64,
                Value::Str(_) => InferredKind::Str,
                Value::Null => InferredKind::Unknown,
            }
        }

        /// The value's text in a column that mixes types, as mlt-core writes it.
        fn text(self) -> CompactString {
            match self {
                Value::Str(s) => s.into(),
                Value::Float(f) => f.to_compact_string(),
                Value::Double(f) => f.to_compact_string(),
                Value::Int(i) | Value::SInt(i) => i.to_compact_string(),
                Value::UInt(u) => u.to_compact_string(),
                Value::Bool(b) => b.to_compact_string(),
                Value::Null => CompactString::default(),
            }
        }
    }

    fn value(bytes: &[u8]) -> Result<Value<'_>, String> {
        let mut p = Pbf::new(bytes);
        let mut v = Value::Null;
        while let Some((n, f)) = p.field()? {
            v = match (n, f) {
                (1, Field::Bytes(b)) => {
                    Value::Str(std::str::from_utf8(b).map_err(|e| format!("MVT: {}", e))?)
                }
                (2, Field::Fixed32(x)) => Value::Float(f32::from_bits(x)),
                (3, Field::Fixed64(x)) => Value::Double(f64::from_bits(x)),
                (4, Field::Varint(x)) => Value::Int(x as i64),
                (5, Field::Varint(x)) => Value::UInt(x),
                (6, Field::Varint(x)) => Value::SInt(((x >> 1) as i64) ^ -((x & 1) as i64)),
                (7, Field::Varint(x)) => Value::Bool(x != 0),
                _ => v,
            };
        }
        Ok(v)
    }

    struct Feature<'a> {
        id: Option<u64>,
        tags: Vec<u32>,
        geom_type: u64,
        geometry: Vec<u32>,
        _bytes: std::marker::PhantomData<&'a ()>,
    }

    struct Layer<'a> {
        name: &'a str,
        extent: u32,
        keys: Vec<&'a str>,
        values: Vec<Value<'a>>,
        features: Vec<Feature<'a>>,
    }

    fn layer(bytes: &[u8]) -> Result<Layer<'_>, String> {
        let mut p = Pbf::new(bytes);
        let mut l = Layer {
            name: "",
            extent: 4096,
            keys: Vec::new(),
            values: Vec::new(),
            features: Vec::new(),
        };
        let mut version = 1;
        while let Some((n, f)) = p.field()? {
            match (n, f) {
                (1, Field::Bytes(b)) => {
                    l.name = std::str::from_utf8(b).map_err(|e| format!("MVT: {}", e))?
                }
                (2, Field::Bytes(b)) => l.features.push(feature(b)?),
                (3, Field::Bytes(b)) => l
                    .keys
                    .push(std::str::from_utf8(b).map_err(|e| format!("MVT: {}", e))?),
                (4, Field::Bytes(b)) => l.values.push(value(b)?),
                (5, Field::Varint(x)) => l.extent = x as u32,
                (15, Field::Varint(x)) => version = x,
                _ => {}
            }
        }
        if l.name.is_empty() {
            return Err("MVT: a layer without a name".to_string());
        }
        if !(1..=3).contains(&version) {
            return Err(format!("MVT: layer {} has version {}", l.name, version));
        }
        Ok(l)
    }

    fn feature(bytes: &[u8]) -> Result<Feature<'_>, String> {
        let mut p = Pbf::new(bytes);
        let mut f = Feature {
            id: None,
            tags: Vec::new(),
            geom_type: 0,
            geometry: Vec::new(),
            _bytes: std::marker::PhantomData,
        };
        while let Some((n, field)) = p.field()? {
            match (n, field) {
                (1, Field::Varint(x)) => f.id = Some(x),
                (2, field) => packed(field, &mut f.tags)?,
                (3, Field::Varint(x)) => f.geom_type = x,
                (4, field) => packed(field, &mut f.geometry)?,
                _ => {}
            }
        }
        Ok(f)
    }

    /// A feature's geometry decoded as fast-mvt decodes it for mlt-core: the
    /// coordinates of each part, and how the parts group into polygons.
    #[derive(Default)]
    struct Parts {
        coords: Vec<Coord<i32>>,
        /// End of each part in `coords`.
        ends: Vec<usize>,
        /// For polygons, whether each ring starts a new polygon.
        exterior: Vec<bool>,
    }

    fn signed_area(c: &[Coord<i32>]) -> i64 {
        let cross = |a: Coord<i32>, b: Coord<i32>| {
            i64::from(a.x) * i64::from(b.y) - i64::from(a.y) * i64::from(b.x)
        };
        let Some((&first, rest)) = c.split_first() else {
            return 0;
        };
        let mut prev = first;
        let mut area = 0i64;
        for &p in rest {
            area += cross(prev, p);
            prev = p;
        }
        area + cross(prev, first)
    }

    fn geometry(geom_type: u64, cmds: &[u32], parts: &mut Parts) -> Result<GeometryType, String> {
        let bad = || "MVT: invalid geometry".to_string();
        parts.coords.clear();
        parts.ends.clear();
        parts.exterior.clear();
        let mut cursor = Coord { x: 0i32, y: 0i32 };
        let mut i = 0;
        let next = |cursor: &mut Coord<i32>, i: &mut usize| -> Result<Coord<i32>, String> {
            let (dx, dy) = (
                *cmds.get(*i).ok_or_else(bad)?,
                *cmds.get(*i + 1).ok_or_else(bad)?,
            );
            let d = |v: u32| ((v >> 1) as i32) ^ -((v & 1) as i32);
            cursor.x = cursor.x.saturating_add(d(dx));
            cursor.y = cursor.y.saturating_add(d(dy));
            *i += 2;
            Ok(*cursor)
        };
        let command = |v: u32| (v & 7, (v >> 3) as usize);
        match geom_type {
            1 => {
                while i < cmds.len() {
                    let (c, n) = command(cmds[i]);
                    if c != 1 {
                        return Err(bad());
                    }
                    i += 1;
                    for _ in 0..n {
                        let p = next(&mut cursor, &mut i)?;
                        parts.coords.push(p);
                        parts.ends.push(parts.coords.len());
                    }
                }
                Ok(match parts.coords.len() {
                    0 => return Err(bad()),
                    1 => GeometryType::Point,
                    _ => GeometryType::MultiPoint,
                })
            }
            2 | 3 => {
                let polygon = geom_type == 3;
                let mut have_exterior = false;
                while i < cmds.len() {
                    let start = parts.coords.len();
                    let (c, n) = command(cmds[i]);
                    if c != 1 || n == 0 {
                        return Err(bad());
                    }
                    i += 1;
                    for _ in 0..n {
                        let p = next(&mut cursor, &mut i)?;
                        parts.coords.push(p);
                    }
                    if i < cmds.len() && command(cmds[i]).0 == 2 {
                        let m = command(cmds[i]).1;
                        i += 1;
                        for _ in 0..m {
                            let p = next(&mut cursor, &mut i)?;
                            parts.coords.push(p);
                        }
                    }
                    if polygon {
                        if i >= cmds.len() || command(cmds[i]) != (7, 1) {
                            return Err(bad());
                        }
                        i += 1;
                        let area = signed_area(&parts.coords[start..]);
                        let first = parts.coords[start];
                        parts.coords.push(first);
                        parts.exterior.push(area > 0 || !have_exterior);
                        have_exterior = true;
                    }
                    parts.ends.push(parts.coords.len());
                }
                let polygons = parts.exterior.iter().filter(|&&e| e).count();
                Ok(match (polygon, parts.ends.len(), polygons) {
                    (_, 0, _) => return Err(bad()),
                    (false, 1, _) => GeometryType::LineString,
                    (false, _, _) => GeometryType::MultiLineString,
                    (true, _, 1) => GeometryType::Polygon,
                    (true, _, _) => GeometryType::MultiPolygon,
                })
            }
            t => Err(format!("MVT: geometry type {}", t)),
        }
    }

    /// The value of `v` in a column of `kind`, as mlt-core's MVT reader converts it.
    fn prop<'a>(
        kind: PropKind,
        v: Value<'a>,
        texts: &'a [CompactString],
        next_text: &mut usize,
    ) -> Result<PropValueRef<'a>, String> {
        Ok(match (kind, v) {
            (PropKind::Bool, Value::Bool(b)) => PropValueRef::Bool(b),
            (PropKind::I64, Value::Int(i) | Value::SInt(i)) => PropValueRef::I64(i),
            (PropKind::I64, Value::UInt(u)) if i64::try_from(u).is_ok() => {
                PropValueRef::I64(u as i64)
            }
            (PropKind::U64, Value::UInt(u)) => PropValueRef::U64(u),
            (PropKind::F32, Value::Float(f)) => PropValueRef::F32(f),
            (PropKind::F64, Value::Double(f)) => PropValueRef::F64(f),
            (PropKind::F64, Value::Float(f)) => PropValueRef::F64(f64::from(f)),
            (PropKind::F64, Value::Int(i) | Value::SInt(i)) => PropValueRef::F64(i as f64),
            (PropKind::F64, Value::UInt(u)) => PropValueRef::F64(u as f64),
            (PropKind::Str, Value::Str(s)) => PropValueRef::Str(s),
            (PropKind::Str, _) => {
                let t = texts
                    .get(*next_text)
                    .ok_or("MLT: text values out of step")?;
                *next_text += 1;
                PropValueRef::Str(t.as_str())
            }
            (k, _) => return Err(format!("MLT: a value that does not fit its {:?} column", k)),
        })
    }

    /// One MVT layer as an encoded MLT layer.
    fn encode_layer(
        l: &Layer<'_>,
        config: EncoderConfig,
        narrow_integers: bool,
        parts: &mut Parts,
    ) -> Result<Vec<u8>, String> {
        // The columns in first-seen order and their kinds, by key name as
        // mlt-core's reader merges them.
        let mut key_cols: Vec<Option<usize>> = vec![None; l.keys.len()];
        let mut names: Vec<&str> = Vec::new();
        let mut inferred: Vec<InferredKind> = Vec::new();
        let tag = |f: &Feature<'_>, j: usize| -> Result<(usize, Value<'_>), String> {
            let (k, v) = (f.tags[j] as usize, f.tags[j + 1] as usize);
            if k >= l.keys.len() {
                return Err(format!("MVT: invalid key index {}", k));
            }
            let v = *l
                .values
                .get(v)
                .ok_or(format!("MVT: invalid value index {}", v))?;
            Ok((k, v))
        };
        for f in &l.features {
            if f.tags.len() % 2 != 0 {
                return Err(format!(
                    "MVT: invalid feature tags length: {}",
                    f.tags.len()
                ));
            }
            for j in (0..f.tags.len()).step_by(2) {
                let (k, v) = tag(f, j)?;
                let col = match key_cols[k] {
                    Some(c) => c,
                    None => {
                        let c = names
                            .iter()
                            .position(|&n| n == l.keys[k])
                            .unwrap_or_else(|| {
                                names.push(l.keys[k]);
                                inferred.push(InferredKind::Unknown);
                                names.len() - 1
                            });
                        key_cols[k] = Some(c);
                        c
                    }
                };
                inferred[col] = inferred[col].merge(v.kind());
            }
        }
        let mut kinds: Vec<PropKind> = inferred.iter().map(|k| k.prop_kind()).collect();

        if narrow_integers {
            let mut ranges: Vec<(i128, i128)> = vec![(i128::MAX, i128::MIN); kinds.len()];
            for f in &l.features {
                for j in (0..f.tags.len()).step_by(2) {
                    let (k, v) = tag(f, j)?;
                    let n = match v {
                        Value::Int(i) | Value::SInt(i) => i as i128,
                        Value::UInt(u) => u as i128,
                        _ => continue,
                    };
                    let r = &mut ranges[key_cols[k].unwrap_or_default()];
                    *r = (r.0.min(n), r.1.max(n));
                }
            }
            for (kind, &(lo, hi)) in kinds.iter_mut().zip(&ranges) {
                *kind = narrowest(*kind, lo, hi);
            }
        }

        // The text of every non-string value in a string column, in writing order.
        let mut texts: Vec<CompactString> = Vec::new();
        for f in &l.features {
            for j in (0..f.tags.len()).step_by(2) {
                let (k, v) = tag(f, j)?;
                let col = key_cols[k].unwrap_or_default();
                if kinds[col] == PropKind::Str && !matches!(v, Value::Str(_) | Value::Null) {
                    texts.push(v.text());
                }
            }
        }

        let err = |e: mlt_core::MltError| format!("MLT layer {}: {}", l.name, e);
        let mut w = LayerWriter::new(l.name, l.extent).map_err(err)?;
        let mut keys = Vec::with_capacity(names.len());
        for (name, &kind) in names.iter().zip(&kinds) {
            keys.push(w.add_property(name, kind).map_err(err)?);
        }
        let mut next_text = 0;
        for f in &l.features {
            let gtype = geometry(f.geom_type, &f.geometry, parts)?;
            let mut fw = w.feature(gtype);
            fw.id(f.id);
            for j in (0..f.tags.len()).step_by(2) {
                let (k, v) = tag(f, j)?;
                if matches!(v, Value::Null) {
                    continue;
                }
                let col = key_cols[k].unwrap_or_default();
                let value = prop(kinds[col], v, &texts, &mut next_text)?;
                fw.property(keys[col], value).map_err(err)?;
            }
            let mut start = 0;
            for (n, &end) in parts.ends.iter().enumerate() {
                let c = parts.coords[start..end].iter().copied();
                match gtype {
                    GeometryType::Point | GeometryType::MultiPoint => fw.points(c),
                    GeometryType::LineString | GeometryType::MultiLineString => fw.line(c),
                    _ if parts.exterior[n] => fw.exterior_ring(c),
                    _ => fw.hole(c),
                }
                .map_err(err)?;
                start = end;
            }
            fw.finish().map_err(err)?;
        }
        w.encode(config).map_err(err)
    }

    /// The type of an integer column holding `lo..=hi`. mlt-core stores
    /// integer columns in 32 bits where the values fit, and MapLibre GL JS
    /// reads wider ones as BigInt, which its style expressions do not compare
    /// as numbers. So a wider column becomes a double where that is exact (up
    /// to 2^53), which GL JS reads as a number, as it reads MVT integers.
    fn narrowest(kind: PropKind, lo: i128, hi: i128) -> PropKind {
        let fits_32_bits = (lo >= 0 && hi <= i128::from(u32::MAX))
            || (lo >= i128::from(i32::MIN) && hi <= i128::from(i32::MAX));
        if matches!(kind, PropKind::I64 | PropKind::U64)
            && lo <= hi
            && !fits_32_bits
            && lo >= -(1 << 53)
            && hi <= 1 << 53
        {
            PropKind::F64
        } else {
            kind
        }
    }

    /// The MLT tile with the content of the MVT tile `mvt`.
    pub fn encode_tile(mvt: &[u8], s: &MltSettings) -> Result<Vec<u8>, String> {
        encode_tile_with(mvt, encoder_config(s), s.profile == MltProfile::Web)
    }

    /// The MLT tile with the content of the MVT tile `mvt`, with explicit
    /// encoder settings, and with integers typed as mlt-core types them or
    /// narrowed for MapLibre GL JS.
    pub fn encode_tile_with(
        mvt: &[u8],
        config: EncoderConfig,
        narrow_integers: bool,
    ) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(mvt.len());
        let mut parts = Parts::default();
        let mut p = Pbf::new(mvt);
        while let Some((n, f)) = p.field()? {
            if let (3, Field::Bytes(b)) = (n, f) {
                out.extend_from_slice(&encode_layer(
                    &layer(b)?,
                    config,
                    narrow_integers,
                    &mut parts,
                )?);
            }
        }
        Ok(out)
    }

    /// The MLT tile mlt-core's own MVT reader makes from `mvt`, which refuses
    /// tiles that decode to more than 32 MiB. For checking `encode_tile_with`
    /// without narrowed integers.
    pub fn encode_tile_with_mlt_core(mvt: &[u8], config: EncoderConfig) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(mvt.len());
        for layer in mlt_core::mvt::mvt_to_tile_layers(mvt).map_err(|e| e.to_string())? {
            out.extend_from_slice(&layer.encode(config).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// The layers of an MLT tile, decoded.
    pub fn decode_tile(mlt: &[u8]) -> Result<Vec<TileLayer>, String> {
        let mut decoder = Decoder::default();
        let mut layers = Vec::new();
        for layer in Parser::default()
            .parse_layers(mlt)
            .map_err(|e| e.to_string())?
        {
            match layer.into_tile(&mut decoder).map_err(|e| e.to_string())? {
                Some(l) => layers.push(l),
                None => return Err("the tile has a layer of an unknown kind".to_string()),
            }
        }
        Ok(layers)
    }
}

#[cfg(feature = "mlt")]
pub use imp::{decode_tile, encode_tile_with, encode_tile_with_mlt_core, encoder_config};
#[cfg(feature = "mlt")]
pub use mlt_core::encoder::EncoderConfig;

/// The MLT tile with the content of the MVT tile `mvt`.
pub fn encode_tile(mvt: &[u8], settings: &MltSettings) -> Result<Vec<u8>, String> {
    #[cfg(feature = "mlt")]
    {
        imp::encode_tile(mvt, settings)
    }
    #[cfg(not(feature = "mlt"))]
    {
        let _ = (mvt, settings);
        Err(NOT_BUILT.to_string())
    }
}

/// The error for `--tile-format=mlt` in a manby built without the feature.
pub const NOT_BUILT: &str = "this manby was built without MLT support; build it with `cargo +1.98.1 build --release --features mlt`";

#[cfg(all(test, feature = "mlt"))]
mod tests {
    use mlt_core::geo_types::Geometry;
    use mlt_core::{PropKind, PropValue};
    use tippecanoe::mvt::{
        MVT_BOOL, MVT_CLOSEPATH, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINESTRING, MVT_LINETO,
        MVT_MOVETO, MVT_POINT, MVT_POLYGON, MVT_SINT, MVT_STRING, MVT_UINT, MvtFeature,
        MvtGeometry, MvtLayer, MvtTile, MvtValue,
    };

    use super::*;

    fn value(t: i32, f: impl FnOnce(&mut MvtValue)) -> MvtValue {
        let mut v = MvtValue::from_double(0.0);
        v.type_ = t;
        f(&mut v);
        v
    }

    fn feature(t: i32, id: Option<u64>, ops: &[Op]) -> MvtFeature {
        MvtFeature {
            type_: t,
            id: id.unwrap_or(0),
            has_id: id.is_some(),
            geometry: ops
                .iter()
                .map(|&(op, x, y)| MvtGeometry::new(op, x, y))
                .collect(),
            ..Default::default()
        }
    }

    /// A geometry command and its coordinates.
    type Op = (i32, i64, i64);

    fn ring(x: i64, y: i64, size: i64, clockwise: bool) -> Vec<Op> {
        let corners = if clockwise {
            [(x, y), (x + size, y), (x + size, y + size), (x, y + size)]
        } else {
            [(x, y), (x, y + size), (x + size, y + size), (x + size, y)]
        };
        let mut ops: Vec<Op> = vec![(MVT_MOVETO, corners[0].0, corners[0].1)];
        ops.extend(corners[1..].iter().map(|&(x, y)| (MVT_LINETO, x, y)));
        ops.push((MVT_CLOSEPATH, 0, 0));
        ops
    }

    /// A tile with every geometry kind, value type and column mix.
    fn sample_tile() -> Vec<u8> {
        let mut layer = MvtLayer {
            name: "things".to_string(),
            extent: 4096,
            version: 2,
            ..Default::default()
        };
        let line = [
            (MVT_MOVETO, 1, 1),
            (MVT_LINETO, 50, 9),
            (MVT_LINETO, -3, 70),
        ];
        let mut lines = line.to_vec();
        lines.extend([(MVT_MOVETO, 100, 100), (MVT_LINETO, 120, 140)]);
        let mut multipolygon = ring(0, 0, 100, true);
        multipolygon.extend(ring(10, 10, 20, false));
        multipolygon.extend(ring(200, 200, 50, true));
        // A first ring with negative area still starts a polygon.
        let mut hole_first = ring(0, 0, 30, false);
        hole_first.extend(ring(5, 5, 5, false));
        let geometries: Vec<(i32, Vec<Op>)> = vec![
            (MVT_POINT, vec![(MVT_MOVETO, 5, 7)]),
            (
                MVT_POINT,
                vec![
                    (MVT_MOVETO, 1, 2),
                    (MVT_MOVETO, 3, -4),
                    (MVT_MOVETO, 9000, 2),
                ],
            ),
            (MVT_LINESTRING, line.to_vec()),
            (MVT_LINESTRING, lines),
            (MVT_POLYGON, ring(0, 0, 64, true)),
            (MVT_POLYGON, multipolygon),
            (MVT_POLYGON, hole_first),
        ];
        let int = |i: i64| value(MVT_INT, |v| v.numeric_value.set_int_value(i));
        let uint = |u: u64| value(MVT_UINT, |v| v.numeric_value.set_uint_value(u));
        let sint = |i: i64| value(MVT_SINT, |v| v.numeric_value.set_sint_value(i));
        let double = |d: f64| value(MVT_DOUBLE, |v| v.numeric_value.set_double_value(d));
        let float = |f: f32| value(MVT_FLOAT, |v| v.numeric_value.set_float_value(f));
        let string = |s: &str| value(MVT_STRING, |v| v.set_string_value(s));
        let boolean = |b: bool| value(MVT_BOOL, |v| v.numeric_value.set_bool_value(b));
        for (i, (t, ops)) in geometries.into_iter().enumerate() {
            let i64v = i as i64;
            let mut f = feature(t, (i % 3 != 0).then_some(i as u64 * 7), &ops);
            layer.tag(&mut f, "name", &string(&format!("feature {}", i)));
            layer.tag(&mut f, "uint", &uint(i as u64 * 1000));
            layer.tag(
                &mut f,
                "signed",
                &if i % 2 == 0 {
                    sint(-i64v)
                } else {
                    uint(i as u64)
                },
            );
            layer.tag(
                &mut f,
                "real",
                &if i % 2 == 0 {
                    int(i64v)
                } else {
                    double(i64v as f64 + 0.25)
                },
            );
            layer.tag(&mut f, "float", &float(i as f32 / 3.0));
            layer.tag(&mut f, "flag", &boolean(i % 2 == 0));
            if i != 3 {
                layer.tag(
                    &mut f,
                    "mixed",
                    &if i % 2 == 0 {
                        string("x")
                    } else {
                        double(1e20 / (i64v as f64))
                    },
                );
            }
            if i == 4 {
                layer.tag(&mut f, "boolnum", &boolean(true));
            }
            if i == 5 {
                layer.tag(&mut f, "boolnum", &int(5));
            }
            layer.tag(
                &mut f,
                "big",
                &uint(if i == 6 { 5_000_000_000 } else { i as u64 }),
            );
            layer.tag(
                &mut f,
                "huge",
                &uint(if i == 6 { 1 << 60 } else { i as u64 }),
            );
            layer.features.push(f);
        }
        let empty = MvtLayer {
            name: "empty".to_string(),
            extent: 4096,
            version: 2,
            ..Default::default()
        };
        MvtTile {
            layers: vec![layer, empty],
        }
        .encode()
    }

    #[test]
    fn same_as_mlt_cores_reader() {
        let mvt = sample_tile();
        for profile in [
            MltProfile::Web,
            MltProfile::Native,
            MltProfile::MinSize,
            MltProfile::Fast,
        ] {
            for gzipped in [true, false] {
                let config = encoder_config(&MltSettings { profile, gzipped });
                assert_eq!(
                    encode_tile_with(&mvt, config, false).unwrap(),
                    encode_tile_with_mlt_core(&mvt, config).unwrap(),
                    "{:?}",
                    config
                );
            }
        }
    }

    #[test]
    fn web_integers_are_numbers_in_javascript() {
        let mvt = sample_tile();
        let kinds = |narrow: bool| {
            let config = encoder_config(&MltSettings::default());
            let layers = decode_tile(&encode_tile_with(&mvt, config, narrow).unwrap()).unwrap();
            let l = &layers[0];
            let kind = |name: &str| {
                let c = l.property_names().iter().position(|n| n == name).unwrap();
                l.features()[6].properties()[c].kind()
            };
            [kind("uint"), kind("signed"), kind("big"), kind("huge")]
        };
        // mlt-core stores columns that fit in 32 bits in 32 bits either way
        assert_eq!(
            kinds(true),
            [PropKind::U32, PropKind::I32, PropKind::F64, PropKind::U64]
        );
        assert_eq!(
            kinds(false),
            [PropKind::U32, PropKind::I32, PropKind::U64, PropKind::U64]
        );
    }

    #[test]
    fn decodes_to_the_mvt_content() {
        let mlt = encode_tile(&sample_tile(), &MltSettings::default()).unwrap();
        let layers = decode_tile(&mlt).unwrap();
        // the empty layer has no features, so it encodes to nothing
        assert_eq!(layers.len(), 1);
        let l = &layers[0];
        assert_eq!(l.name(), "things");
        assert_eq!(l.feature_count(), 7);
        let col = |name: &str| l.property_names().iter().position(|n| n == name).unwrap();
        let f = l.features();
        assert_eq!(f[0].id(), None);
        assert_eq!(f[1].id(), Some(7));
        assert!(matches!(f[0].geometry(), Geometry::Point(_)));
        assert!(matches!(f[1].geometry(), Geometry::MultiPoint(_)));
        assert!(matches!(f[2].geometry(), Geometry::LineString(_)));
        assert!(matches!(f[3].geometry(), Geometry::MultiLineString(_)));
        assert!(matches!(f[4].geometry(), Geometry::Polygon(_)));
        match f[5].geometry() {
            Geometry::MultiPolygon(m) => {
                assert_eq!(m.0.len(), 2);
                assert_eq!(m.0[0].interiors().len(), 1);
            }
            g => panic!("{:?}", g),
        }
        match f[6].geometry() {
            Geometry::Polygon(p) => assert_eq!(p.interiors().len(), 1),
            g => panic!("{:?}", g),
        }
        assert_eq!(
            f[1].properties()[col("name")],
            PropValue::Str(Some("feature 1".into()))
        );
        // integers mixed with floats are floats; other mixes are text
        assert_eq!(f[2].properties()[col("real")], PropValue::F64(Some(2.0)));
        assert_eq!(f[1].properties()[col("real")], PropValue::F64(Some(1.25)));
        assert_eq!(
            f[2].properties()[col("mixed")],
            PropValue::Str(Some("x".into()))
        );
        // mlt-core's text for a float, which is neither Rust's nor tippecanoe's
        assert_eq!(
            f[1].properties()[col("mixed")],
            PropValue::Str(Some("1e+20".into()))
        );
        assert!(f[3].properties()[col("mixed")].is_null());
        assert_eq!(
            f[4].properties()[col("boolnum")],
            PropValue::Str(Some("true".into()))
        );
        assert_eq!(
            f[5].properties()[col("boolnum")],
            PropValue::Str(Some("5".into()))
        );
    }
}
