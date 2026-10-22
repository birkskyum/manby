//! The reference for the tests: the same features tiled by tippecanoe-rs's
//! `traverse_zooms`, from the temporary file that tippecanoe would write.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use tippecanoe::geometry::{DrawVec, VT_LINETO, VT_MOVETO};
use tippecanoe::globals as g;
use tippecanoe::mbtiles::LayermapEntry;
use tippecanoe::serial::{SerialFeature, StringPoolRef, serialize_feature};
use tippecanoe::tile::Strategy;

use super::TileOptions;
use super::util::shift_right;
use crate::sink::TileSink;
use crate::store::{FeatureStore, PoolOffsets};

/// tippecanoe-rs keeps its options in globals, so tests that set them take turns.
pub static GLOBALS: Mutex<()> = Mutex::new(());

/// Sets tippecanoe-rs's globals as `manby::run` does before tiling.
pub fn set_globals(o: &TileOptions) {
    g::GEOMETRY_SCALE.store(o.geometry_scale, Relaxed);
    for c in 0..256 {
        g::set_prevent(c, o.prevent[c] as i32);
        g::set_additional(c, o.additional[c] as i32);
    }
    g::QUIET.store(o.quiet as i32, Relaxed);
    g::QUIET_PROGRESS.store(1, Relaxed);
    g::MAX_TILE_SIZE.store(o.max_tile_size, Relaxed);
    g::MAX_TILE_FEATURES.store(o.max_tile_features, Relaxed);
    g::CLUSTER_DISTANCE.store(o.cluster_distance, Relaxed);
    g::CLUSTER_MAXZOOM.store(o.cluster_maxzoom, Relaxed);
    g::TINY_POLYGON_SIZE.store(o.tiny_polygon_size, Relaxed);
    g::LIMIT_TILE_FEATURE_COUNT.store(o.limit_tile_feature_count, Relaxed);
    g::LIMIT_TILE_FEATURE_COUNT_AT_MAXZOOM.store(o.limit_tile_feature_count_at_maxzoom, Relaxed);
    g::EXTEND_ZOOMS_MAX.store(o.extend_zooms_max, Relaxed);
    g::RETAIN_POINTS_MULTIPLIER.store(o.retain_points_multiplier, Relaxed);
    g::PRESERVE_MULTIPLIER_DENSITY_THRESHOLD
        .store(o.preserve_multiplier_density_threshold, Relaxed);
    *g::ORDER_BY.write().unwrap() = o.order_by.clone();
    *g::CLIPBBOXES.write().unwrap() = if o.clipbboxes {
        vec![Default::default()]
    } else {
        Vec::new()
    };
    g::EXTRA_DETAIL.store(o.extra_detail, Relaxed);
    g::CPUS.store(o.cpus, Relaxed);
    g::TEMP_FILES.store(o.temp_files, Relaxed);
    tippecanoe::projection::set_index_hilbert(o.additional[tippecanoe::options::A_HILBERT]);
}

/// What tippecanoe-rs made.
pub struct Reference {
    pub tiles: BTreeMap<(u8, u32, u32), Vec<u8>>,
    pub strategies: Vec<Strategy>,
    pub ret: i32,
    pub midx: u32,
    pub midy: u32,
    /// The layer maps after tiling, input and tiling segments merged.
    pub layermap: BTreeMap<String, LayermapEntry>,
}

/// The input layer map with each layer's id its index in the store.
pub fn input_layermap(store: &FeatureStore) -> BTreeMap<String, LayermapEntry> {
    store
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut e = l.entry.clone();
            e.id = i;
            (l.name.clone(), e)
        })
        .collect()
}

/// The string pool and the zoom 0 temporary file that tippecanoe would
/// write for `store`, with each key's and value's offset in the pool.
struct Zoom0 {
    pool: Vec<u8>,
    key_off: Vec<i64>,
    value_off: Vec<i64>,
    data: Vec<u8>,
}

fn zoom0(store: &FeatureStore, o: &TileOptions) -> Zoom0 {
    let gs = o.geometry_scale;

    // the string pool: a type byte, the string and a NUL for each key and value
    let mut pool: Vec<u8> = Vec::new();
    let mut key_off = Vec::with_capacity(store.keys.len());
    for k in &store.keys {
        key_off.push(pool.len() as i64);
        pool.push(tippecanoe::mvt::MVT_STRING as u8);
        pool.extend_from_slice(k.as_bytes());
        pool.push(0);
    }
    let mut value_off = Vec::with_capacity(store.values.len());
    for v in store.values.iter() {
        value_off.push(pool.len() as i64);
        pool.push(v.type_ as u8);
        pool.extend_from_slice(v.s.as_bytes());
        pool.push(0);
    }

    // the zoom 0 temporary file
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&0i64.to_ne_bytes());
    tippecanoe::serial::serialize_int(&mut data, o.iz);
    tippecanoe::serial::serialize_uint(&mut data, o.ix);
    tippecanoe::serial::serialize_uint(&mut data, o.iy);
    for f in &store.features {
        let mut geometry = DrawVec::new();
        for mut d in store.feature_geometry(f) {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                d.x = shift_right(d.x, gs);
                d.y = shift_right(d.y, gs);
            }
            geometry.push(d);
        }
        let mut sf = SerialFeature {
            layer: f.layer as i64,
            segment: 0,
            seq: f.seq as i64,
            t: f.t,
            feature_minzoom: f.feature_minzoom,
            has_id: f.has_id,
            id: f.id,
            tippecanoe_minzoom: f.tippecanoe_minzoom as i32,
            tippecanoe_maxzoom: f.tippecanoe_maxzoom as i32,
            geometry,
            index: f.index(),
            gap: 0,
            label_point: f.label_point,
            extent: f.extent,
            ..Default::default()
        };
        for a in store.feature_attrs(f) {
            sf.keys.push(key_off[a.key as usize]);
            sf.values.push(value_off[a.value as usize]);
        }
        let s = serialize_feature(&mut sf, 0, 0);
        tippecanoe::serial::serialize_long_long(&mut data, s.len() as i64);
        data.extend_from_slice(&s);
    }
    tippecanoe::serial::serialize_ulong_long(&mut data, 0);
    let complexity = data.len() as i64;
    data[0..8].copy_from_slice(&complexity.to_ne_bytes());
    Zoom0 {
        pool,
        key_off,
        value_off,
        data,
    }
}

/// Fills in what `-at` needs to know about the temporary files that the
/// reference reads: one segment, whose layer numbers are the store's, the
/// pool offsets, and the size of the zoom 0 file.
pub fn serialized_layout(store: &mut FeatureStore, o: &mut TileOptions) {
    let z0 = zoom0(store, o);
    store.initial_x = vec![0];
    store.initial_y = vec![0];
    store.segment_layers = vec![(0..store.layers.len() as i64).collect()];
    store.pool_offsets = store
        .attrs
        .iter()
        .map(|a| PoolOffsets {
            key: z0.key_off[a.key as usize],
            value: z0.value_off[a.value as usize],
        })
        .collect();
    o.estimated_complexity = z0.data.len() as i64;
}

/// Tiles `store` with tippecanoe-rs. The caller holds `GLOBALS` and has set them.
pub fn reference(store: &FeatureStore, o: &TileOptions) -> Reference {
    let dir = tempdir();
    let Zoom0 { pool, data, .. } = zoom0(store, o);

    let geomname = format!("{}/geom0", dir);
    {
        let mut f = std::fs::File::create(&geomname).unwrap();
        f.write_all(&data).unwrap();
    }
    let file = std::fs::File::open(&geomname).unwrap();
    std::fs::remove_file(&geomname).unwrap();

    let mut geomfd = vec![-1i32; o.temp_files];
    let mut geom_size = vec![0i64; o.temp_files];
    geomfd[0] = file.into_raw_fd();
    geom_size[0] = data.len() as i64;

    let pool_ref = StringPoolRef {
        pool: Some(Arc::new(pool)),
        base: 0,
    };
    let midx = AtomicU32::new(0);
    let midy = AtomicU32::new(0);
    let mut maxzoom = o.maxzoom;
    let mut layermaps = vec![input_layermap(store)];
    let mut strategies = Vec::new();
    let outdir = format!("{}/out", dir);
    g::EXTEND_ZOOMS_MAX.store(o.extend_zooms_max, Relaxed);

    let filter = o.filter.as_ref().map(|t| t.root());
    let ret = tippecanoe::tile::traverse_zooms(
        &mut geomfd,
        &mut geom_size,
        &pool_ref,
        &midx,
        &midy,
        &mut maxzoom,
        o.minzoom,
        None,
        Some(&outdir),
        o.buffer,
        "test",
        &dir,
        o.gamma,
        o.full_detail,
        o.low_detail,
        o.min_detail,
        &[0],
        &[0],
        &[0],
        o.simplification,
        o.maxzoom_simplification,
        &mut layermaps,
        o.prefilter.as_deref(),
        o.postfilter.as_deref(),
        &o.attribute_accum,
        filter,
        &mut strategies,
        o.iz,
        &o.shared_nodes,
        o.shared_nodes.len() * std::mem::size_of::<tippecanoe::serial::Node>(),
        &o.shared_nodes_bloom,
        o.basezoom,
        o.droprate,
        &o.unidecode_data,
        o.drop_by_attribute.as_deref(),
        o.drop_by_attribute_descending,
    );
    g::EXTEND_ZOOMS_MAX.store(o.extend_zooms_max, Relaxed);

    let mut tiles = BTreeMap::new();
    read_tiles(&outdir, &mut tiles);
    let _ = std::fs::remove_dir_all(&dir);

    Reference {
        tiles,
        strategies,
        ret,
        midx: midx.load(Relaxed),
        midy: midy.load(Relaxed),
        layermap: tippecanoe::mbtiles::merge_layermaps(&layermaps),
    }
}

fn read_tiles(outdir: &str, tiles: &mut BTreeMap<(u8, u32, u32), Vec<u8>>) {
    let Ok(zs) = std::fs::read_dir(outdir) else {
        return;
    };
    for zd in zs.flatten() {
        let Ok(z) = zd.file_name().to_string_lossy().parse::<u8>() else {
            continue;
        };
        for xd in std::fs::read_dir(zd.path()).unwrap().flatten() {
            let x: u32 = xd.file_name().to_string_lossy().parse().unwrap();
            for yd in std::fs::read_dir(xd.path()).unwrap().flatten() {
                let name = yd.file_name().to_string_lossy().to_string();
                let y: u32 = name.trim_end_matches(".pbf").parse().unwrap();
                tiles.insert((z, x, y), std::fs::read(yd.path()).unwrap());
            }
        }
    }
}

fn tempdir() -> String {
    static N: AtomicI64 = AtomicI64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "manby-oracle-{}-{}",
        std::process::id(),
        N.fetch_add(1, Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().to_string()
}

/// A sink that keeps the tiles in memory.
#[derive(Default)]
pub struct MemorySink {
    pub tiles: Mutex<BTreeMap<(u8, u32, u32), Vec<u8>>>,
}

impl TileSink for MemorySink {
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> crate::Result<()> {
        self.tiles.lock().unwrap().insert((z, x, y), data.to_vec());
        Ok(())
    }

    fn erase_zoom(&self, z: u8) -> crate::Result<()> {
        self.tiles.lock().unwrap().retain(|k, _| k.0 != z);
        Ok(())
    }

    fn finish(self: Box<Self>, _metadata: &tippecanoe::mbtiles::Metadata) -> crate::Result<()> {
        Ok(())
    }
}
