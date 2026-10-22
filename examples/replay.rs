//! Replays a dump of tippecanoe's `traverse_zooms` inputs through manby's
//! tiler, times it, and compares the tiles with tippecanoe's.
//!
//!   cargo run --release --example replay -- DUMP_DIR [REFERENCE_TILE_DIR] [THREADS]
//!
//! The dumps come from a tippecanoe built with a wrapper around
//! `traverse_zooms` that writes its inputs to `$TILE_DUMP`: `in.txt` with the
//! options and globals, the zoom 0 temporary file `geom0`, and the string pool.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use manby::sink::TileSink;
use manby::store::{Attr, Feature, FeatureStore, Layer};
use manby::tiler::{TileOptions, tile_with};
use tippecanoe::attribute::AttributeOp;
use tippecanoe::geometry::{VT_LINETO, VT_MOVETO};
use tippecanoe::globals as g;
use tippecanoe::globals::OrderField;
use tippecanoe::mbtiles::{LayermapEntry, Tilestat};
use tippecanoe::serial::{Node, SerialVal, deserialize_feature};

fn unhex(s: &str) -> Vec<u8> {
    let b = s.strip_prefix('x').unwrap().as_bytes();
    (0..b.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(std::str::from_utf8(&b[i..i + 2]).unwrap(), 16).unwrap())
        .collect()
}

fn unhex_str(s: &str) -> String {
    let b = unhex(s);
    if b == b"\x80size" {
        return g::ORDER_BY_SIZE.to_string();
    }
    String::from_utf8(b).unwrap()
}

fn bits(s: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(s, 16).unwrap())
}

fn varint(p: &mut &[u8]) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let c = p[0];
        *p = &p[1..];
        v |= ((c & 0x7f) as u64) << shift;
        shift += 7;
        if c & 0x80 == 0 {
            return v;
        }
    }
}

fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[derive(Default)]
struct MemSink {
    tiles: Mutex<BTreeMap<(u8, u32, u32), Vec<u8>>>,
}

impl TileSink for MemSink {
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> manby::Result<()> {
        self.tiles.lock().unwrap().insert((z, x, y), data.to_vec());
        Ok(())
    }

    fn erase_zoom(&self, z: u8) -> manby::Result<()> {
        self.tiles.lock().unwrap().retain(|k, _| k.0 != z);
        Ok(())
    }

    fn finish(self: Box<Self>, _metadata: &tippecanoe::mbtiles::Metadata) -> manby::Result<()> {
        Ok(())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let reference = args.get(2).filter(|s| !s.is_empty());
    let threads: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);

    let t0 = Instant::now();
    let text = std::fs::read_to_string(format!("{}/in.txt", dir)).unwrap();
    let mut kv: HashMap<String, Vec<String>> = HashMap::new();
    let mut accum: HashMap<String, AttributeOp> = HashMap::new();
    let mut order_by: Vec<OrderField> = Vec::new();
    let mut unidecode: Vec<String> = Vec::new();
    let mut layermaps: Vec<BTreeMap<String, LayermapEntry>> = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let w: Vec<&str> = line.split(' ').collect();
        match w[0] {
            "a" => {
                let op = [
                    AttributeOp::Sum,
                    AttributeOp::Product,
                    AttributeOp::Mean,
                    AttributeOp::Concat,
                    AttributeOp::Comma,
                    AttributeOp::Max,
                    AttributeOp::Min,
                    AttributeOp::Count,
                ][w[2].parse::<usize>().unwrap()];
                accum.insert(unhex_str(w[1]), op);
            }
            "o" => order_by.push(OrderField::new(unhex_str(w[1]), w[2] == "1")),
            "u" => unidecode.push(unhex_str(w[1])),
            "layermaps" => {
                let n: usize = w[1].parse().unwrap();
                for _ in 0..n {
                    let seg: Vec<&str> = lines.next().unwrap().split(' ').collect();
                    let count: usize = seg[2].parse().unwrap();
                    let mut m = BTreeMap::new();
                    for _ in 0..count {
                        let l: Vec<&str> = lines.next().unwrap().split(' ').collect();
                        let mut e = LayermapEntry::new(l[2].parse().unwrap());
                        e.minzoom = l[3].parse().unwrap();
                        e.maxzoom = l[4].parse().unwrap();
                        e.points = l[6].parse().unwrap();
                        e.lines = l[7].parse().unwrap();
                        e.polygons = l[8].parse().unwrap();
                        e.retain = l[9].parse().unwrap();
                        let nstats: usize = l[10].parse().unwrap();
                        for _ in 0..nstats {
                            let st: Vec<&str> = lines.next().unwrap().split(' ').collect();
                            let mut ts = Tilestat {
                                type_: st[2].parse().unwrap(),
                                min: bits(st[3]),
                                max: bits(st[4]),
                                ..Default::default()
                            };
                            for sv in &st[6..] {
                                let (t, s) = sv.split_once(':').unwrap();
                                ts.sample_values
                                    .push(SerialVal::new(t.parse().unwrap(), &unhex_str(s)));
                            }
                            e.tilestats.insert(unhex_str(st[1]), ts);
                        }
                        m.insert(unhex_str(l[1]), e);
                    }
                    layermaps.push(m);
                }
            }
            _ => {
                kv.insert(
                    w[0].to_string(),
                    w[1..].iter().map(|s| s.to_string()).collect(),
                );
            }
        }
    }
    let gv = |k: &str| -> &Vec<String> { kv.get(k).unwrap_or_else(|| panic!("{}", k)) };
    let gi = |k: &str| -> i64 { gv(k)[0].parse().unwrap() };
    let gopt = |k: &str| -> Option<String> {
        if gv(k)[0] == "-" {
            None
        } else {
            Some(unhex_str(&gv(k)[0]))
        }
    };

    let mut o = TileOptions {
        minzoom: gi("minzoom") as i32,
        maxzoom: gi("maxzoom") as i32,
        basezoom: gi("basezoom") as i32,
        droprate: bits(&gv("droprate")[0]),
        iz: gi("iz") as i32,
        geometry_scale: gi("geometry_scale") as i32,
        buffer: gi("buffer") as i32,
        full_detail: gi("full_detail") as i32,
        low_detail: gi("low_detail") as i32,
        min_detail: gi("min_detail") as i32,
        gamma: bits(&gv("gamma")[0]),
        simplification: bits(&gv("simplification")[0]),
        maxzoom_simplification: bits(&gv("maxzoom_simplification")[0]),
        max_tile_size: gi("max_tile_size") as usize,
        max_tile_features: gi("max_tile_features") as usize,
        cluster_distance: gi("cluster_distance") as i32,
        cluster_maxzoom: gi("cluster_maxzoom") as i32,
        tiny_polygon_size: gi("tiny_polygon_size") as i32,
        limit_tile_feature_count: gi("limit_tile_feature_count") as usize,
        limit_tile_feature_count_at_maxzoom: gi("limit_tile_feature_count_at_maxzoom") as usize,
        extend_zooms_max: gi("extend_zooms_max"),
        retain_points_multiplier: gi("retain_points_multiplier") as i32,
        preserve_multiplier_density_threshold: gv("preserve_multiplier_density_threshold")[0]
            .parse()
            .unwrap(),
        order_by,
        clipbboxes: gi("clipbboxes") > 0,
        extra_detail: gi("extra_detail") as i32,
        attribute_accum: accum,
        filter: gopt("filter").map(|f| tippecanoe::evaluator::parse_filter(&f)),
        drop_by_attribute: gopt("drop_by_attribute"),
        drop_by_attribute_descending: gv("drop_by_attribute")[1] == "1",
        unidecode_data: unidecode,
        prefilter: gopt("prefilter"),
        postfilter: gopt("postfilter"),
        cpus: gi("cpus") as usize,
        temp_files: gi("temp_files") as usize,
        quiet: std::env::var_os("MANBY_VERBOSE").is_none(),
        threads,
        ..TileOptions::default()
    };
    for (i, v) in gv("prevent").iter().enumerate() {
        o.prevent[i] = v != "0";
    }
    for (i, v) in gv("additional").iter().enumerate() {
        o.additional[i] = v != "0";
    }
    let nodes_bytes = std::fs::read(format!("{}/nodes", dir)).unwrap_or_default();
    o.shared_nodes = nodes_bytes
        .chunks(8)
        .map(|c| Node {
            index: u64::from_ne_bytes(c.try_into().unwrap()),
        })
        .collect();
    o.shared_nodes_bloom = std::fs::read(format!("{}/bloom", dir)).unwrap_or_default();

    // the globals that tippecanoe-rs's functions read
    g::GEOMETRY_SCALE.store(o.geometry_scale, Relaxed);
    for c in 0..256 {
        g::set_prevent(c, o.prevent[c] as i32);
        g::set_additional(c, o.additional[c] as i32);
    }
    g::QUIET.store(1, Relaxed);
    let mt = gv("max_tilestats");
    g::MAX_TILESTATS_ATTRIBUTES.store(mt[0].parse().unwrap(), Relaxed);
    g::MAX_TILESTATS_SAMPLE_VALUES.store(mt[1].parse().unwrap(), Relaxed);
    g::MAX_TILESTATS_VALUES.store(mt[2].parse().unwrap(), Relaxed);
    tippecanoe::projection::set_index_hilbert(o.additional[tippecanoe::options::A_HILBERT]);

    // the features
    let pool = std::fs::read(format!("{}/pool", dir)).unwrap();
    let pool_off: Vec<i64> = gv("pool_off").iter().map(|s| s.parse().unwrap()).collect();
    let initial_x: Vec<u32> = gv("initial_x").iter().map(|s| s.parse().unwrap()).collect();
    let initial_y: Vec<u32> = gv("initial_y").iter().map(|s| s.parse().unwrap()).collect();
    let geom = std::fs::read(format!("{}/geom0", dir)).unwrap();

    let mut p: &[u8] = &geom[8..];
    let iz = zigzag(varint(&mut p)) as i32;
    let ix = varint(&mut p) as u32;
    let iy = varint(&mut p) as u32;
    o.iz = iz;
    o.ix = ix;
    o.iy = iy;
    let (ox, oy) = if iz != 0 {
        ((ix << (32 - iz)) as i64, (iy << (32 - iz)) as i64)
    } else {
        (0, 0)
    };

    let layer_names: Vec<BTreeMap<usize, String>> = layermaps
        .iter()
        .map(|m| m.iter().map(|(k, e)| (e.id, k.clone())).collect())
        .collect();
    let merged = tippecanoe::mbtiles::merge_layermaps(&layermaps);

    let mut store = FeatureStore::default();
    let mut keys: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut values: HashMap<(i8, Vec<u8>), u32> = HashMap::new();
    let mut layers: HashMap<String, u32> = HashMap::new();
    let cstr = |off: usize| -> &[u8] {
        let end = pool[off..].iter().position(|&c| c == 0).unwrap();
        &pool[off..off + end]
    };

    loop {
        let len = zigzag(varint(&mut p));
        if len <= 0 {
            break;
        }
        let bytes = &p[..len as usize];
        p = &p[len as usize..];
        let sf = deserialize_feature(bytes, iz as u32, ix, iy, &initial_x, &initial_y);
        assert_eq!(sf.gap, 0);

        let name = &layer_names[sf.segment as usize][&(sf.layer as usize)];
        let layer = *layers.entry(name.clone()).or_insert_with(|| {
            store.layers.push(Layer {
                name: name.clone(),
                entry: merged.get(name).cloned().unwrap_or_default(),
            });
            (store.layers.len() - 1) as u32
        });

        let attr_start = store.attrs.len() as u64;
        let base = pool_off[sf.segment as usize] as usize;
        for (&k, &v) in sf.keys.iter().zip(sf.values.iter()) {
            let kb = cstr(base + k as usize + 1).to_vec();
            let next = keys.len() as u32;
            let key = *keys.entry(kb.clone()).or_insert_with(|| {
                store.keys.push(String::from_utf8(kb).unwrap());
                next
            });
            let t = pool[base + v as usize] as i8;
            let vb = cstr(base + v as usize + 1).to_vec();
            let next = values.len() as u32;
            let value = *values.entry((t, vb.clone())).or_insert_with(|| {
                store
                    .values
                    .push(SerialVal::new(t as i32, &String::from_utf8(vb).unwrap()));
                next
            });
            store.attrs.push(Attr { key, value });
        }

        let geom_start = store.geometry.len() as u64;
        for d in &sf.geometry {
            let mut d = *d;
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                d.x += ox;
                d.y += oy;
            }
            store.geometry.push(d);
        }
        let mut bbox = sf.bbox;
        if bbox[0] <= bbox[2] {
            bbox = [bbox[0] + ox, bbox[1] + oy, bbox[2] + ox, bbox[3] + oy];
        }

        store.features.push(Feature {
            index: sf.index,
            // the temporary file is already in order; the tiler does not read the sort key
            ix: sf.index,
            seq: sf.seq as u64,
            label_point: sf.label_point,
            extent: sf.extent,
            id: sf.id,
            bbox,
            geom_start,
            geom_len: sf.geometry.len() as u32,
            attr_start,
            attr_len: sf.keys.len() as u32,
            layer,
            t: sf.t,
            has_id: sf.has_id,
            feature_minzoom: sf.feature_minzoom,
            tippecanoe_minzoom: sf.tippecanoe_minzoom as i8,
            tippecanoe_maxzoom: sf.tippecanoe_maxzoom as i8,
        });
    }
    eprintln!(
        "loaded {} features, {} values in {:.2}s",
        store.features.len(),
        store.values.len(),
        t0.elapsed().as_secs_f64()
    );

    let sink = MemSink::default();
    let t1 = Instant::now();
    let stats = tile_with(&store, &o, &sink).unwrap();
    let elapsed = t1.elapsed().as_secs_f64();
    let tiles = sink.tiles.into_inner().unwrap();
    eprintln!(
        "tiled {} tiles through zoom {} in {:.2}s",
        tiles.len(),
        stats.maxzoom,
        elapsed
    );

    if let Some(refdir) = reference {
        let mut differ = 0;
        let mut missing = 0;
        let mut count = 0;
        for zd in std::fs::read_dir(refdir).unwrap().flatten() {
            let Ok(z) = zd.file_name().to_string_lossy().parse::<u8>() else {
                continue;
            };
            for xd in std::fs::read_dir(zd.path()).unwrap().flatten() {
                let x: u32 = xd.file_name().to_string_lossy().parse().unwrap();
                for yd in std::fs::read_dir(xd.path()).unwrap().flatten() {
                    let name = yd.file_name().to_string_lossy().to_string();
                    let y: u32 = name.trim_end_matches(".pbf").parse().unwrap();
                    count += 1;
                    match tiles.get(&(z, x, y)) {
                        None => missing += 1,
                        Some(t) => {
                            if *t != std::fs::read(yd.path()).unwrap() {
                                if differ < 5 {
                                    eprintln!("tile {}/{}/{} differs", z, x, y);
                                }
                                differ += 1;
                            }
                        }
                    }
                }
            }
        }
        let extra = tiles.len() as i64 - (count - missing) as i64;
        eprintln!(
            "reference: {} tiles; {} differ, {} missing, {} extra",
            count, differ, missing, extra
        );
        if differ > 0 || missing > 0 || extra != 0 {
            std::process::exit(1);
        }
    }
}
