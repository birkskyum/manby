//! The in-memory feature store that ingest fills, order sorts and the tiler reads.
//!
//! A `Feature` holds what tippecanoe serializes for a feature in its temporary
//! files. Geometry and attributes live in shared arrays that the records point
//! into, so sorting moves only the fixed-size records.

use std::collections::BTreeMap;

use tippecanoe::geometry::Draw;
use tippecanoe::mbtiles::LayermapEntry;
use tippecanoe::serial::SerialVal;

/// One input feature.
#[derive(Clone, Copy, Debug, Default)]
pub struct Feature {
    /// tippecanoe's serialized `index`: the position of the feature on the
    /// space-filling curve if an option needs it (dropping or coalescing the
    /// densest, gamma, clustering, ...), otherwise 0, as the tiling reads it.
    pub index: u64,
    /// The position on the space-filling curve that features are sorted by,
    /// tippecanoe's `struct index` `ix`, whether or not `index` keeps it.
    pub ix: u64,
    /// Input sequence number, tippecanoe's `seq`, which keeps the order of
    /// features with the same index deterministic.
    pub seq: u64,
    pub label_point: u64,
    pub extent: i64,
    pub id: u64,
    /// World coordinates: min x, min y, max x, max y, of the geometry as the
    /// tiling reads it back at zoom 0 (`decode_geometry`).
    pub bbox: [i64; 4],
    /// The feature's vertices are `geometry[geom_start..geom_start + geom_len]`.
    pub geom_start: u64,
    pub geom_len: u32,
    /// The feature's attributes are `attrs[attr_start..attr_start + attr_len]`.
    pub attr_start: u64,
    pub attr_len: u32,
    /// Index into `FeatureStore::layers`: the layer that tippecanoe's tiling
    /// puts the feature in, which is the one it names except where layer
    /// numbers collide (see `segment_layers`).
    pub layer: u32,
    /// `VT_POINT`, `VT_LINE` or `VT_POLYGON`.
    pub t: i8,
    pub has_id: bool,
    /// The minimum zoom that the drop rate assigns (`calc_feature_minzoom`).
    pub feature_minzoom: i8,
    /// `"tippecanoe": {"minzoom": …, "maxzoom": …}` in the input, or -1.
    pub tippecanoe_minzoom: i8,
    pub tippecanoe_maxzoom: i8,
    /// The tippecanoe reader that read the feature (`sf.segment`), whose
    /// string pool, layer numbers and `initial_x`/`initial_y` its serialized
    /// form refers to. The same for any number of manby threads.
    pub segment: u16,
}

/// An attribute of a feature: interned key and value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attr {
    pub key: u32,
    pub value: u32,
}

/// Where a feature's attribute is in its segment's string pool, as tippecanoe
/// serializes it: the offsets of the key and of the value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolOffsets {
    pub key: i64,
    pub value: i64,
}

/// A layer: its name and the statistics tippecanoe keeps for the metadata.
#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub entry: LayermapEntry,
}

#[derive(Default)]
pub struct FeatureStore {
    pub features: Vec<Feature>,
    /// World coordinates (2^32 per side at zoom 0), with tippecanoe's move and
    /// line ops, as `decode_geometry` returns them at zoom 0.
    pub geometry: Vec<Draw>,
    pub attrs: Vec<Attr>,
    /// Interned attribute keys.
    pub keys: Vec<String>,
    /// Interned attribute values, typed as tippecanoe's string pool types them.
    pub values: Vec<SerialVal>,
    pub layers: Vec<Layer>,
    /// Bounds of the input in world coordinates, as tippecanoe's `file_bbox`.
    pub bbox: [i64; 4],
    /// tippecanoe's `file_bbox1` and `file_bbox2`: the bounds on the -180 to
    /// 180 and the 0 to 360 world planes, for the antimeridian-aware bounds.
    pub bbox1: [i64; 4],
    pub bbox2: [i64; 4],
    /// What `-zg` needs from reading: tippecanoe's `dist_sum`, `dist_count`
    /// and `area_sum`.
    pub dist_sum: f64,
    pub dist_count: usize,
    pub area_sum: f64,
    /// `--no-simplification-of-shared-nodes`: the points shared between
    /// features, sorted, and tippecanoe's bloom filter of them. Both are
    /// empty without that option.
    pub shared_nodes: Vec<tippecanoe::serial::Node>,
    pub shared_nodes_bloom: Vec<u8>,
    /// tippecanoe's `initial_x` and `initial_y`, by segment: the point that
    /// the first geometry delta of each serialized feature is relative to,
    /// after `SHIFT_RIGHT`. 0 for a segment that read no point.
    pub initial_x: Vec<u32>,
    pub initial_y: Vec<u32>,
    /// `segment_layers[segment][layer]`: the layer number that tippecanoe
    /// serializes for a feature of store layer `layer` read by `segment`,
    /// or -1 if that segment has no such layer.
    pub segment_layers: Vec<Vec<i64>>,

    // The rest is what tippecanoe's temporary files hold beyond the features
    // themselves, which `-at` needs to estimate tile sizes from their bytes.
    // Without `-at` it is left empty.
    /// For each entry of `attrs`, its offsets in its feature's segment's
    /// string pool.
    pub pool_offsets: Vec<PoolOffsets>,
    /// The bytes the features take in tippecanoe's geometry files: for each,
    /// its length as a varint and its serialized form.
    pub serialized_bytes: u64,
}

impl FeatureStore {
    pub fn feature_geometry(&self, f: &Feature) -> &[Draw] {
        &self.geometry[f.geom_start as usize..f.geom_start as usize + f.geom_len as usize]
    }

    pub fn feature_attrs(&self, f: &Feature) -> &[Attr] {
        &self.attrs[f.attr_start as usize..f.attr_start as usize + f.attr_len as usize]
    }

    /// The layer statistics in the form tippecanoe-rs's metadata code takes.
    pub fn layermap(&self) -> BTreeMap<String, LayermapEntry> {
        self.layers
            .iter()
            .map(|l| (l.name.clone(), l.entry.clone()))
            .collect()
    }
}
