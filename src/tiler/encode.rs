//! Encoding a tile into exactly the bytes that tippecanoe-rs's
//! `MvtTile::encode` makes from the layers `MvtLayer::tag` builds, without
//! building those layers first.
//!
//! tippecanoe dedups keys through a direct-mapped cache, which can repeat a
//! key, so keys go through the same cache here. Values are sorted by their
//! encoded bytes and merged when equal before they are written, so they can
//! be deduped any way at all on the way in.

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use tippecanoe::geometry::DrawVec;
use tippecanoe::mvt::{
    MVT_BOOL, MVT_CLOSEPATH, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINETO, MVT_MOVETO, MVT_SINT,
    MVT_STRING, MVT_UINT, MvtValue, stringified_to_mvt_value,
};
use tippecanoe::protozero::{add_varint_to_buffer, encode_zigzag32, encode_zigzag64};
use tippecanoe::text::fnv1a;

use super::util::{Helpers, par_ranges};
use crate::buf::Buf;
use crate::store::Values;

/// The size of tippecanoe's key dedup cache.
const DEDUP_SLOTS: u64 = 65536;

pub fn key_slot(key: &str) -> u32 {
    (fnv1a(key.as_bytes()) % DEDUP_SLOTS) as u32
}

fn field(out: &mut Vec<u8>, tag: u32, wire: u32) {
    add_varint_to_buffer(out, ((tag << 3) | wire) as u64);
}

fn bytes_field(out: &mut Vec<u8>, tag: u32, value: &[u8]) {
    field(out, tag, 2);
    add_varint_to_buffer(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// The encoded `Value` message of a tile value. `None` for nulls, which are never written.
pub fn encode_value(v: &MvtValue) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(12);
    let n = &v.numeric_value;
    match v.type_ {
        MVT_STRING => bytes_field(&mut out, 1, v.string_value.as_bytes()),
        MVT_FLOAT => {
            field(&mut out, 2, 5);
            out.extend_from_slice(&n.float_value().to_le_bytes());
        }
        MVT_DOUBLE => {
            field(&mut out, 3, 1);
            out.extend_from_slice(&n.double_value().to_le_bytes());
        }
        MVT_INT => {
            field(&mut out, 4, 0);
            add_varint_to_buffer(&mut out, n.int_value() as u64);
        }
        MVT_UINT => {
            field(&mut out, 5, 0);
            add_varint_to_buffer(&mut out, n.uint_value());
        }
        MVT_SINT => {
            field(&mut out, 6, 0);
            add_varint_to_buffer(&mut out, encode_zigzag64(n.sint_value()));
        }
        MVT_BOOL => {
            field(&mut out, 7, 0);
            out.push(n.bool_value() as u8);
        }
        _ => return None,
    }
    Some(out)
}

/// The encodings of the store's attribute values, and their order by those bytes.
pub struct ValueCodes {
    bytes: Buf<u8>,
    off: Buf<u32>,
    /// Equal for values with equal encodings.
    rank: Buf<u32>,
}

impl ValueCodes {
    pub fn new(values: &Values) -> ValueCodes {
        let n = values.len();
        let parts = 64;
        let encoded: Vec<(Buf<u8>, Buf<u32>)> = (0..parts)
            .into_par_iter()
            .map(|p| {
                let mut bytes = Buf::new();
                let mut lens = Buf::new();
                for i in n * p / parts..n * (p + 1) / parts {
                    let v = values.get(i as u32);
                    let e =
                        encode_value(&stringified_to_mvt_value(v.type_, v.s)).unwrap_or_default();
                    bytes.extend_from_slice(&e);
                    lens.push(e.len() as u32);
                    if i % (1 << 16) == 0 {
                        crate::buf::relieve();
                    }
                }
                (bytes, lens)
            })
            .collect();
        let mut bytes = Buf::with_capacity(encoded.iter().map(|e| e.0.len()).sum());
        let mut off = Buf::with_capacity(n + 1);
        for (b, lens) in encoded {
            let mut at = bytes.len() as u32;
            for &l in lens.iter() {
                off.push(at);
                at += l;
            }
            bytes.extend_from_slice(&b);
        }
        off.push(bytes.len() as u32);

        let mut codes = ValueCodes {
            bytes,
            off,
            rank: Buf::new(),
        };
        // sorted by the first bytes of their codes, and the rest only where
        // those are equal, so that the sort seldom looks up the codes
        let prefix = |b: &[u8]| {
            let mut k = [0u8; 8];
            let m = b.len().min(8);
            k[..m].copy_from_slice(&b[..m]);
            u64::from_be_bytes(k)
        };
        let mut keys: Buf<(u64, u32)> = Buf::with_capacity(n);
        for v in 0..n as u32 {
            keys.push((prefix(codes.get(v)), v));
            if v % (1 << 16) == 0 {
                crate::buf::relieve();
            }
        }
        keys.par_sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| codes.get(a.1).cmp(codes.get(b.1)))
        });
        let mut rank = Buf::new();
        rank.resize(n, 0u32);
        let mut r = 0u32;
        for (i, &(k, v)) in keys.iter().enumerate() {
            if i > 0 {
                let (pk, pv) = keys[i - 1];
                if k != pk || codes.get(v) != codes.get(pv) {
                    r += 1;
                }
            }
            rank[v as usize] = r;
            if i % (1 << 16) == 0 {
                crate::buf::relieve();
            }
        }
        codes.rank = rank;
        codes
    }

    fn get(&self, v: u32) -> &[u8] {
        &self.bytes[self.off[v as usize] as usize..self.off[v as usize + 1] as usize]
    }
}

enum Val {
    Store(u32),
    Extra(Vec<u8>),
}

struct Feat {
    id: u64,
    /// Which of the caller's geometries is this feature's.
    geom: u32,
    /// Its tags in the layer's `tags`.
    tags_start: u32,
    tags_len: u32,
    t: i32,
    has_id: bool,
}

/// One layer being encoded.
pub struct LayerEnc<'c> {
    codes: &'c ValueCodes,
    name: String,
    extent: i64,
    keys: Vec<String>,
    key_cache: FxHashMap<u32, u32>,
    vals: Vec<Val>,
    store_vals: FxHashMap<u32, u32>,
    extra_vals: FxHashMap<Vec<u8>, u32>,
    feats: Vec<Feat>,
    /// Key index and local value index, alternating, for each feature in turn.
    tags: Vec<u32>,
}

impl<'c> LayerEnc<'c> {
    pub fn new(codes: &'c ValueCodes, name: &str, extent: i64) -> LayerEnc<'c> {
        LayerEnc {
            codes,
            name: name.to_string(),
            extent,
            keys: Vec::new(),
            key_cache: FxHashMap::default(),
            vals: Vec::new(),
            store_vals: FxHashMap::default(),
            extra_vals: FxHashMap::default(),
            feats: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.feats.is_empty()
    }

    /// Starts a feature; its tags follow.
    pub fn feature(&mut self, t: i32, has_id: bool, id: u64, geom: usize) {
        self.feats.push(Feat {
            t,
            has_id,
            id,
            tags_start: self.tags.len() as u32,
            tags_len: 0,
            geom: geom as u32,
        });
    }

    fn key(&mut self, key: &str, slot: u32) -> u32 {
        if let Some(&k) = self.key_cache.get(&slot)
            && self.keys[k as usize] == key
        {
            return k;
        }
        let k = self.keys.len() as u32;
        self.keys.push(key.to_string());
        self.key_cache.insert(slot, k);
        k
    }

    /// Tags the current feature with a store value.
    pub fn tag_store(&mut self, key: &str, slot: u32, value: u32) {
        let k = self.key(key, slot);
        let next = self.vals.len() as u32;
        let v = *self.store_vals.entry(value).or_insert(next);
        if v == next {
            self.vals.push(Val::Store(value));
        }
        self.tags.push(k);
        self.tags.push(v);
        self.feats.last_mut().unwrap().tags_len += 2;
    }

    /// Tags the current feature with a value that is not in the store.
    pub fn tag_extra(&mut self, key: &str, value: &MvtValue) {
        let k = self.key(key, key_slot(key));
        let bytes = encode_value(value).unwrap_or_default();
        let next = self.vals.len() as u32;
        let v = match self.extra_vals.get(&bytes) {
            Some(&v) => v,
            None => {
                self.extra_vals.insert(bytes.clone(), next);
                self.vals.push(Val::Extra(bytes));
                next
            }
        };
        self.tags.push(k);
        self.tags.push(v);
        self.feats.last_mut().unwrap().tags_len += 2;
    }

    fn val_bytes(&self, v: u32) -> &[u8] {
        match &self.vals[v as usize] {
            Val::Store(s) => self.codes.get(*s),
            Val::Extra(b) => b,
        }
    }

    /// Appends the layer message to `out`. `geometries[i]` is feature geometry `i`.
    pub fn finish(
        self,
        geometries: &[&DrawVec],
        helpers: &Helpers,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        let mut layer: Vec<u8> = Vec::new();
        field(&mut layer, 15, 0);
        add_varint_to_buffer(&mut layer, 2);
        bytes_field(&mut layer, 1, self.name.as_bytes());
        field(&mut layer, 5, 0);
        add_varint_to_buffer(&mut layer, self.extent as u32 as u64);

        for key in &self.keys {
            bytes_field(&mut layer, 3, key.as_bytes());
        }

        // the values, sorted by their bytes and merged when equal
        let mut order: Vec<u32> = (0..self.vals.len() as u32).collect();
        if self.extra_vals.is_empty() {
            let rank = |v: u32| match &self.vals[v as usize] {
                Val::Store(s) => self.codes.rank[*s as usize],
                Val::Extra(_) => unreachable!(),
            };
            order.sort_unstable_by_key(|&v| (rank(v), v));
        } else {
            order.sort_by(|&a, &b| self.val_bytes(a).cmp(self.val_bytes(b)).then(a.cmp(&b)));
        }
        let mut mapping = vec![0u32; self.vals.len()];
        let mut index = 0u32;
        let mut i = 0;
        while i < order.len() {
            let bytes = self.val_bytes(order[i]);
            bytes_field(&mut layer, 4, bytes);
            mapping[order[i] as usize] = index;
            while i + 1 < order.len() && self.val_bytes(order[i + 1]) == bytes {
                mapping[order[i + 1] as usize] = index;
                i += 1;
            }
            index += 1;
            i += 1;
        }

        let encode = |f: &Feat, out: &mut Vec<u8>| -> Result<(), String> {
            let tags = &self.tags[f.tags_start as usize..][..f.tags_len as usize];
            let mut feature: Vec<u8> = Vec::with_capacity(16 + tags.len());
            if f.t >= 0 {
                field(&mut feature, 3, 0);
                add_varint_to_buffer(&mut feature, f.t as i64 as u64);
            }
            if !tags.is_empty() {
                let mut packed = Vec::with_capacity(tags.len() * 2);
                for (j, &t) in tags.iter().enumerate() {
                    let t = if j % 2 == 1 { mapping[t as usize] } else { t };
                    add_varint_to_buffer(&mut packed, t as u64);
                }
                bytes_field(&mut feature, 2, &packed);
            }
            if f.has_id {
                field(&mut feature, 1, 0);
                add_varint_to_buffer(&mut feature, f.id);
            }
            let geometry = encode_geometry(geometries[f.geom as usize])?;
            if !geometry.is_empty() {
                let mut packed = Vec::with_capacity(geometry.len() * 2);
                for g in geometry {
                    add_varint_to_buffer(&mut packed, g as u64);
                }
                bytes_field(&mut feature, 4, &packed);
            }
            bytes_field(out, 2, &feature);
            Ok(())
        };

        if self.feats.len() >= 16384 {
            let chunks: Vec<Result<Vec<u8>, String>> =
                par_ranges(helpers, self.feats.len(), 8192, |r| {
                    let mut out = Vec::new();
                    for f in &self.feats[r] {
                        encode(f, &mut out)?;
                    }
                    Ok(out)
                });
            for c in chunks {
                layer.extend_from_slice(&c?);
            }
        } else {
            for f in &self.feats {
                encode(f, &mut layer)?;
            }
        }

        bytes_field(out, 3, &layer);
        Ok(())
    }
}

/// The geometry commands of a feature, as `MvtTile::encode` writes them.
fn encode_geometry(geom: &DrawVec) -> Result<Vec<u32>, String> {
    let mut geometry: Vec<u32> = Vec::with_capacity(geom.len() * 2 + 2);

    let mut px: i64 = 0;
    let mut py: i64 = 0;
    let mut cmd_idx: i32 = -1;
    let mut cmd: i32 = -1;
    let mut length: i32 = 0;

    for g in geom {
        let op = g.op as i32;

        if op != cmd {
            if cmd_idx >= 0 {
                geometry[cmd_idx as usize] = ((length << 3) | (cmd & ((1 << 3) - 1))) as u32;
            }

            cmd = op;
            length = 0;
            cmd_idx = geometry.len() as i32;
            geometry.push(0);
        }

        if op == MVT_MOVETO || op == MVT_LINETO {
            let dx = g.x - px;
            let dy = g.y - py;

            if dx < i32::MIN as i64
                || dx > i32::MAX as i64
                || dy < i32::MIN as i64
                || dy > i32::MAX as i64
            {
                return Err(format!(
                    "Internal error: Geometry delta is too big: {},{}",
                    dx, dy
                ));
            }

            geometry.push(encode_zigzag32(dx as i32));
            geometry.push(encode_zigzag32(dy as i32));

            px = g.x;
            py = g.y;
            length += 1;
        } else if op == MVT_CLOSEPATH {
            length += 1;
        } else {
            return Err("Internal error: corrupted geometry".to_string());
        }
    }

    if cmd_idx >= 0 {
        geometry[cmd_idx as usize] = ((length << 3) | (cmd & ((1 << 3) - 1))) as u32;
    }

    Ok(geometry)
}
