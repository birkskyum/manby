//! What one thread reads from one range of the input: features, with their
//! keys, values and layers interned in tables of the chunk's own, which are
//! merged into the store afterwards.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tippecanoe::geometry::Draw;
use tippecanoe::mbtiles::{LayermapEntry, Tilestat};
use tippecanoe::mvt::{MVT_DOUBLE, MVT_NULL};
use tippecanoe::serial::SerialVal;

use crate::store::{Attr, Feature};

/// Number of shards of the global value table, which are built in parallel.
pub const SHARDS: usize = 64;

pub fn hash_bytes(type_: i32, s: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64_with_seed(s, type_ as u64)
}

/// An interned string or typed value: its type and the bytes of its string.
#[derive(Default)]
pub struct Interner {
    pub types: Vec<i8>,
    pub hashes: Vec<u64>,
    /// End offsets in `bytes`; entry `i` is `bytes[ends[i-1]..ends[i]]`.
    pub ends: Vec<usize>,
    pub bytes: Vec<u8>,
    table: Vec<u32>,
    /// For each shard, the entries that hash into it, in order.
    pub shards: Vec<Vec<u32>>,
}

const EMPTY: u32 = u32::MAX;

impl Interner {
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn get(&self, i: u32) -> (i32, &[u8]) {
        let i = i as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        (self.types[i] as i32, &self.bytes[start..self.ends[i]])
    }

    pub fn get_str(&self, i: u32) -> &str {
        // SAFETY: only valid UTF-8 is interned.
        unsafe { std::str::from_utf8_unchecked(self.get(i).1) }
    }

    fn grow(&mut self) {
        let n = if self.table.is_empty() {
            1024
        } else {
            self.table.len() * 2
        };
        self.table = vec![EMPTY; n];
        let mask = n - 1;
        for (i, &h) in self.hashes.iter().enumerate() {
            let mut slot = (h as usize) & mask;
            while self.table[slot] != EMPTY {
                slot = (slot + 1) & mask;
            }
            self.table[slot] = i as u32;
        }
    }

    /// The id of `s` with `type_`, adding it if it is new. `s` must be UTF-8.
    pub fn intern(&mut self, type_: i32, s: &[u8]) -> u32 {
        if self.hashes.len() * 2 >= self.table.len() {
            self.grow();
        }
        let h = hash_bytes(type_, s);
        let mask = self.table.len() - 1;
        let mut slot = (h as usize) & mask;
        loop {
            let e = self.table[slot];
            if e == EMPTY {
                break;
            }
            if self.hashes[e as usize] == h {
                let (t, b) = self.get(e);
                if t == type_ && b == s {
                    return e;
                }
            }
            slot = (slot + 1) & mask;
        }
        let id = self.types.len() as u32;
        self.table[slot] = id;
        self.types.push(type_ as i8);
        self.hashes.push(h);
        self.bytes.extend_from_slice(s);
        self.ends.push(self.bytes.len());
        if self.shards.is_empty() {
            self.shards = vec![Vec::new(); SHARDS];
        }
        self.shards[(h >> 58) as usize % SHARDS].push(id);
        id
    }
}

/// tippecanoe's `add_to_tilestats`, with the sample values kept in a set
/// capped at `max_samples`: it keeps the same smallest distinct values.
#[derive(Clone, Debug)]
pub struct Stat {
    pub samples: BTreeSet<SerialValKey>,
    pub min: f64,
    pub max: f64,
    pub type_: i32,
}

impl Default for Stat {
    fn default() -> Self {
        Stat {
            samples: BTreeSet::new(),
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            type_: 0,
        }
    }
}

/// A `SerialVal` ordered as `serial_val::operator<` orders it: by string, then type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SerialValKey {
    pub s: String,
    pub type_: i32,
}

/// libc `atof` on a value tippecanoe stringified: Rust's parse where it
/// accepts the text, which gives the same correctly rounded result.
pub fn atof_fast(s: &str) -> f64 {
    s.parse::<f64>()
        .unwrap_or_else(|_| tippecanoe::cstd::atof(s))
}

impl Stat {
    pub fn add(&mut self, type_: i32, s: &str, max_samples: usize) {
        if type_ == MVT_NULL {
            return;
        }

        if type_ == MVT_DOUBLE {
            let d = atof_fast(s);
            if d < self.min {
                self.min = d;
            }
            if d > self.max {
                self.max = d;
            }
        }

        let full = self.samples.len() >= max_samples;
        let beyond_last = match self.samples.last() {
            Some(last) => (s, type_) > (last.s.as_str(), last.type_),
            None => true,
        };
        if !(full && beyond_last) {
            let key = SerialValKey {
                s: s.to_string(),
                type_,
            };
            if self.samples.insert(key) && self.samples.len() > max_samples {
                self.samples.pop_last();
            }
        }

        self.type_ |= 1 << type_;
    }

    /// Merges another chunk's statistics into these, as merge_layermaps does.
    pub fn merge(&mut self, other: Stat, max_samples: usize) {
        for v in other.samples {
            if self.samples.insert(v) && self.samples.len() > max_samples {
                self.samples.pop_last();
            }
        }
        self.type_ |= other.type_;
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
    }

    pub fn to_tilestat(&self) -> Tilestat {
        Tilestat {
            sample_values: self
                .samples
                .iter()
                .map(|k| SerialVal {
                    type_: k.type_,
                    s: k.s.clone(),
                })
                .collect(),
            min: self.min,
            max: self.max,
            type_: self.type_,
        }
    }
}

/// A layer as one chunk sees it.
#[derive(Clone, Debug, Default)]
pub struct ChunkLayer {
    pub name: String,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
    pub stats: BTreeMap<String, Stat>,
}

/// A message to print once the chunks are in order. Each part is a line of
/// the message; a part with a line number (relative to the chunk's first line)
/// starts with "fname:line: " when it is printed.
#[derive(Clone, Debug)]
pub struct Message {
    pub kind: MessageKind,
    pub parts: Vec<(Option<i32>, Vec<u8>)>,
}

/// A message that ends the run, as its exit status and the parts of a [`Message`].
pub type Fatal = (i32, Vec<(Option<i32>, Vec<u8>)>);

/// Messages that tippecanoe prints only once per run, and the ones it prints every time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Always,
    Once(&'static str),
}

/// What a chunk read.
#[derive(Default)]
pub struct Chunk {
    /// `seq` is the feature's rank within the chunk, `layer` an index into
    /// `layers`, and the attributes point into `keys` and `values`.
    pub features: Vec<Feature>,
    pub geometry: Vec<Draw>,
    pub attrs: Vec<Attr>,
    pub keys: Interner,
    pub values: Interner,
    pub layers: Vec<ChunkLayer>,
    pub layer_index: HashMap<String, u32>,
    /// `--no-simplification-of-shared-nodes`: the vertices with their
    /// neighbors, and the nodes that must be kept, as tippecanoe writes them
    /// to its vertex and node files.
    pub vertices: Vec<tippecanoe::serial::Vertex>,
    pub nodes: Vec<u64>,
    /// Features that `--generate-ids` gives the id seq + 1.
    pub generate_ids: Vec<u32>,

    pub file_bbox: [i64; 4],
    pub file_bbox1: [i64; 4],
    pub file_bbox2: [i64; 4],

    /// The terms tippecanoe adds to `dist_sum` and `area_sum`, in order, for `-zg`.
    pub dist_terms: Vec<f64>,
    pub dist_count: usize,
    pub area_terms: Vec<f64>,

    pub messages: Vec<Message>,
    /// A problem that stops tippecanoe: its exit status and message.
    pub fatal: Option<Fatal>,
    /// Whether this chunk took a bare geometry before it found any feature,
    /// which it would not have done if an earlier chunk had found one.
    pub bare_before_feature: bool,
    pub found_features: i64,
    pub found_geometries: i64,
    pub found_hashes: i64,
    /// The JSON syntax error that ended the chunk, if any, as a message.
    pub error: Option<Message>,
    /// Whether the chunk ended inside a value, so that it would have been
    /// read on into the next chunk.
    pub cut_short: bool,
    /// Whether the error was found at the very end of the chunk, where the
    /// next chunk's first byte might have changed it.
    pub error_at_end: bool,
}

impl Chunk {
    pub fn new(layers: &[String]) -> Chunk {
        let mut c = Chunk {
            file_bbox: [u32::MAX as i64, u32::MAX as i64, 0, 0],
            file_bbox1: [0xFFFFFFFF, 0xFFFFFFFF, 0, 0],
            file_bbox2: [0x1FFFFFFFF, 0xFFFFFFFF, 0x100000000, 0],
            ..Default::default()
        };
        for l in layers {
            c.layer(l);
        }
        c
    }

    /// The chunk's index for layer `name`, adding it if it is new.
    pub fn layer(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.layer_index.get(name) {
            return i;
        }
        let i = self.layers.len() as u32;
        self.layers.push(ChunkLayer {
            name: name.to_string(),
            ..Default::default()
        });
        self.layer_index.insert(name.to_string(), i);
        i
    }

    /// A message of one line.
    pub fn message(&mut self, kind: MessageKind, line: Option<i32>, text: impl Into<Vec<u8>>) {
        self.messages.push(Message {
            kind,
            parts: vec![(line, text.into())],
        });
    }
}

/// The merged layer statistics, in the form tippecanoe-rs's metadata takes.
pub fn layermap_entry(id: usize, l: &ChunkLayer) -> LayermapEntry {
    LayermapEntry {
        id,
        tilestats: l
            .stats
            .iter()
            .map(|(k, v)| (k.clone(), v.to_tilestat()))
            .collect(),
        points: l.points,
        lines: l.lines,
        polygons: l.polygons,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tippecanoe::mvt::MVT_STRING;

    #[test]
    fn interning() {
        let mut i = Interner::default();
        let a = i.intern(MVT_STRING, b"abc");
        let b = i.intern(MVT_DOUBLE, b"abc");
        let c = i.intern(MVT_STRING, b"abc");
        assert_eq!(a, c);
        assert_ne!(a, b);
        for n in 0..5000 {
            i.intern(MVT_DOUBLE, n.to_string().as_bytes());
        }
        for n in 0..5000 {
            let id = i.intern(MVT_DOUBLE, n.to_string().as_bytes());
            assert_eq!(i.get(id), (MVT_DOUBLE, n.to_string().as_bytes()));
        }
        assert_eq!(i.len(), 5002);
        assert_eq!(i.shards.iter().map(|s| s.len()).sum::<usize>(), 5002);
    }

    #[test]
    fn stats_match_add_to_tilestats() {
        use std::sync::atomic::Ordering;
        tippecanoe::globals::MAX_TILESTATS_SAMPLE_VALUES.store(7, Ordering::Relaxed);
        let mut ts = BTreeMap::new();
        let mut st = Stat::default();
        let mut a = Stat::default();
        let mut b = Stat::default();
        let mut x: u64 = 12345;
        for i in 0..400 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = SerialVal {
                type_: if x % 3 == 0 { MVT_STRING } else { MVT_DOUBLE },
                s: ((x >> 40) % 50).to_string(),
            };
            tippecanoe::mbtiles::add_to_tilestats(&mut ts, "k", &v);
            st.add(v.type_, &v.s, 7);
            if i < 150 {
                a.add(v.type_, &v.s, 7);
            } else {
                b.add(v.type_, &v.s, 7);
            }
        }
        a.merge(b, 7);
        let want = &ts["k"];
        for s in [&st, &a] {
            let got = s.to_tilestat();
            assert_eq!(got.sample_values, want.sample_values);
            assert_eq!(got.min, want.min);
            assert_eq!(got.max, want.max);
            assert_eq!(got.type_, want.type_);
        }
    }
}
