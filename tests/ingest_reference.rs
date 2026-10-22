//! Checks manby's reading against tippecanoe-rs's: the same inputs go through
//! tippecanoe-rs's parse_json and serialize_feature into its temporary files,
//! which are read back and compared feature by feature with manby's store.
//!
//! `reference_golden` does this for the inputs and options of every golden
//! test that manby can read. `REFERENCE_INPUT` and `REFERENCE_ARGS` in the
//! environment run one comparison instead (see `reference_one`).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, Ordering};

use manby::options::{Options, parse_args, set_tippecanoe_globals};
use manby::store::FeatureStore;
use tippecanoe::geometry::Draw;
use tippecanoe::mbtiles::{LayermapEntry, merge_layermaps};
use tippecanoe::serial::{Reader, SerializationState};

/// tippecanoe-rs's globals are process-wide, so the tests here take turns.
static GLOBALS: Mutex<()> = Mutex::new(());

fn tmpfd(what: &str) -> (i32, String) {
    let mut name = format!("/tmp/manby-ref-{}.XXXXXXXX", what);
    let fd = tippecanoe::tippecanoe_main::mkstemp_cloexec(&mut name);
    assert!(fd >= 0);
    (fd, name)
}

fn writer(name: &str) -> BufWriter<File> {
    let f = tippecanoe::tippecanoe_main::fopen_oflag(name, "wb", libc::O_WRONLY).unwrap();
    BufWriter::new(f)
}

fn new_reader() -> Reader {
    let mut r = Reader::default();
    let (pool, pn) = tmpfd("pool");
    let (tree, tn) = tmpfd("tree");
    let (geom, gn) = tmpfd("geom");
    let (index, xn) = tmpfd("index");
    let (vertex, vn) = tmpfd("vertex");
    let (node, nn) = tmpfd("node");
    r.vertexfd = vertex;
    r.nodefd = node;
    r.poolfd = pool;
    r.treefd = tree;
    r.geomfd = geom;
    r.indexfd = index;
    {
        let rs = r.state.get_mut().unwrap();
        rs.poolfile = tippecanoe::memfile::memfile_open(pool);
        rs.treefile = tippecanoe::memfile::memfile_open(tree);
        rs.geomfile = Some(writer(&gn));
        rs.indexfile = Some(writer(&xn));
        rs.vertexfile = Some(writer(&vn));
        rs.nodefile = Some(writer(&nn));
        let mut in_memory = false;
        tippecanoe::memfile::memfile_write(
            rs.treefile.as_mut().unwrap(),
            &[0u8; 32],
            &mut in_memory,
        );
        rs.file_bbox = [u32::MAX as i64, u32::MAX as i64, 0, 0];
    }
    for n in [pn, tn, gn, xn, vn, nn] {
        let _ = std::fs::remove_file(n);
    }
    r
}

#[derive(Debug, PartialEq)]
struct Feat {
    seq: u64,
    ix: u64,
    t: i8,
    index: u64,
    label_point: u64,
    extent: i64,
    has_id: bool,
    id: u64,
    minzoom: i32,
    maxzoom: i32,
    layer: String,
    geometry: Vec<(i8, i64, i64)>,
    bbox: [i64; 4],
    attrs: Vec<(String, i32, String)>,
    /// What -at needs: the reading segment, the layer number and the string
    /// pool offsets that the segment serialized.
    segment: u16,
    layer_number: i64,
    pool: Vec<(i64, i64)>,
}

struct Reference {
    features: Vec<Feat>,
    layermap: BTreeMap<String, LayermapEntry>,
    dist_sum: f64,
    dist_count: usize,
    area_sum: f64,
    bbox: [i64; 4],
    bbox1: [i64; 4],
    bbox2: [i64; 4],
    initial_x: Vec<u32>,
    initial_y: Vec<u32>,
    geom_bytes: u64,
}

fn cstring_at(pool: &[u8], off: usize) -> String {
    let s = &pool[off..];
    let n = s.iter().position(|&c| c == 0).unwrap();
    String::from_utf8(s[..n].to_vec()).unwrap()
}

/// What tippecanoe-rs reads from the options' inputs, in its read modes.
fn reference(o: &Options) -> Reference {
    let cpus = o.tippecanoe_cpus;
    let readers: Vec<Reader> = (0..cpus).map(|_| new_reader()).collect();
    let progress_seq = AtomicI64::new(0);
    let initialized: Vec<AtomicI32> = (0..2 * cpus).map(|_| AtomicI32::new(0)).collect();
    let initial_x: Vec<AtomicU32> = (0..2 * cpus).map(|_| AtomicU32::new(0)).collect();
    let initial_y: Vec<AtomicU32> = (0..2 * cpus).map(|_| AtomicU32::new(0)).collect();

    let mut sources = o.all_sources();
    for (l, s) in sources.iter_mut().enumerate() {
        if s.layer.is_empty() {
            s.layer = manby::ingest::layer_name_for(&s.file, &o.tileset_name(), l);
        }
    }
    let mut layermap: BTreeMap<String, LayermapEntry> = BTreeMap::new();
    for (l, s) in sources.iter().enumerate() {
        let mut e = LayermapEntry::new(l);
        e.description = s.description.clone();
        layermap.entry(s.layer.clone()).or_insert(e);
    }
    let layermaps: Vec<Mutex<BTreeMap<String, LayermapEntry>>> =
        (0..cpus).map(|_| Mutex::new(layermap.clone())).collect();

    let (mut dist_sum, mut dist_count, mut area_sum) = (0.0f64, 0usize, 0.0f64);
    let mut overall_offset: i64 = 0;
    let filters = o.filters();

    for src in &sources {
        let layer = layermap[&src.layer].id;
        let mut data = Vec::new();
        match File::open(&src.file) {
            Ok(mut f) => {
                f.read_to_end(&mut data).unwrap();
            }
            Err(_) => continue,
        }
        let is_gz = src.file.ends_with(".gz");
        let mappable = !is_gz && !data.is_empty();
        let mut sep = if o.read_parallel { b'\n' } else { 0 };
        if mappable && data[0] == 0x1E {
            sep = 0x1E;
        }

        let sst_for = |i: usize, seq: i64| SerializationState {
            fname: &src.file,
            line: 0,
            layer_seq: seq,
            progress_seq: &progress_seq,
            readers: &readers,
            segment: i as i32,
            initial_x: &initial_x[i],
            initial_y: &initial_y[i],
            initialized: &initialized[i],
            dist_sum: 0.0,
            dist_count: 0,
            area_sum: 0.0,
            want_dist: o.guess_maxzoom,
            maxzoom: o.maxzoom,
            basezoom: o.basezoom,
            filters,
            uses_gamma: o.gamma != 0.0,
            layermap: &layermaps[i],
            attribute_types: &o.attribute_types,
            exclude: &o.exclude,
            include: &o.include,
            exclude_all: o.exclude_all as i32,
        };

        let parallel = |bytes: &[u8],
                        sep: u8,
                        base: i64,
                        dist_sum: &mut f64,
                        dist_count: &mut usize,
                        area_sum: &mut f64| {
            let len = bytes.len();
            let mut segs = vec![0usize; cpus + 1];
            segs[cpus] = len;
            for (i, s) in segs.iter_mut().enumerate().take(cpus).skip(1) {
                let mut p = len * i / cpus;
                while p < len && bytes[p] != sep {
                    p += 1;
                }
                *s = p;
            }
            for i in 0..cpus {
                let mut sst = sst_for(i, segs[i] as i64 + base);
                let mut jp = tippecanoe::geojson::json_begin_map(&bytes[segs[i]..segs[i + 1]]);
                tippecanoe::geojson::parse_json(&mut sst, &mut jp, layer as i32, &src.layer);
                *dist_sum += sst.dist_sum;
                *dist_count += sst.dist_count;
                *area_sum += sst.area_sum;
            }
        };

        if mappable && sep != 0 {
            parallel(
                &data,
                sep,
                overall_offset,
                &mut dist_sum,
                &mut dist_count,
                &mut area_sum,
            );
            overall_offset += data.len() as i64;
            continue;
        }

        let gz = sources[layer].file.ends_with(".gz") && sources[layer].file.len() > 3;
        if gz && data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
            let mut out = Vec::new();
            flate2::read::MultiGzDecoder::new(&data[..])
                .read_to_end(&mut out)
                .unwrap();
            data = out;
        }
        if data.first() == Some(&0x1E) {
            sep = 0x1E;
        }
        if sep != 0 {
            let mut start = 0;
            let mut p = 2000;
            let mut chunks = Vec::new();
            while p < data.len() {
                if p - start > 10000000 && data[p - 1] == sep {
                    chunks.push((start, p));
                    start = p;
                }
                p += 2000;
            }
            if start < data.len() {
                chunks.push((start, data.len()));
            }
            for (cs, ce) in chunks {
                parallel(
                    &data[cs..ce],
                    sep,
                    overall_offset,
                    &mut dist_sum,
                    &mut dist_count,
                    &mut area_sum,
                );
                overall_offset += (ce - cs) as i64;
            }
        } else {
            let mut sst = sst_for(0, overall_offset);
            sst.dist_sum = dist_sum;
            sst.dist_count = dist_count;
            sst.area_sum = area_sum;
            let mut jp = tippecanoe::geojson::json_begin_map(&data);
            tippecanoe::geojson::parse_json(&mut sst, &mut jp, layer as i32, &src.layer);
            overall_offset = sst.layer_seq;
            dist_sum = sst.dist_sum;
            dist_count = sst.dist_count;
            area_sum = sst.area_sum;
        }
    }

    let ix: Vec<u32> = initial_x
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .collect();
    let iy: Vec<u32> = initial_y
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .collect();
    let layermaps: Vec<BTreeMap<String, LayermapEntry>> = layermaps
        .into_iter()
        .map(|m| m.into_inner().unwrap())
        .collect();

    let mut features = Vec::new();
    let mut geom_bytes = 0u64;
    let mut bbox = [u32::MAX as i64, u32::MAX as i64, 0, 0];
    let mut bbox1: [i64; 4] = [0xFFFFFFFF, 0xFFFFFFFF, 0, 0];
    let mut bbox2: [i64; 4] = [0x1FFFFFFFF, 0xFFFFFFFF, 0x100000000, 0];
    for (seg, r) in readers.into_iter().enumerate() {
        let mut rs = r.state.into_inner().unwrap();
        rs.geomfile.take().unwrap().flush().unwrap();
        rs.indexfile.take().unwrap().flush().unwrap();
        for i in 0..2 {
            bbox[i] = bbox[i].min(rs.file_bbox[i]);
            bbox1[i] = bbox1[i].min(rs.file_bbox1[i]);
            bbox2[i] = bbox2[i].min(rs.file_bbox2[i]);
        }
        for i in 2..4 {
            bbox[i] = bbox[i].max(rs.file_bbox[i]);
            bbox1[i] = bbox1[i].max(rs.file_bbox1[i]);
            bbox2[i] = bbox2[i].max(rs.file_bbox2[i]);
        }

        // SAFETY: the reader's descriptors, which nothing else closes.
        let geomf = unsafe { File::from_raw_fd(r.geomfd) };
        let indexf = unsafe { File::from_raw_fd(r.indexfd) };
        geom_bytes += r.geompos.load(Ordering::Relaxed) as u64;
        let mut geom = vec![0u8; r.geompos.load(Ordering::Relaxed) as usize];
        geomf.read_exact_at(&mut geom, 0).unwrap();
        let mut index = vec![0u8; r.indexpos.load(Ordering::Relaxed) as usize];
        indexf.read_exact_at(&mut index, 0).unwrap();

        let pf = rs.poolfile.as_mut().unwrap();
        let pool: Vec<u8> = match pf.fp.as_mut() {
            None => pf.map.clone(),
            Some(fp) => {
                fp.flush().unwrap();
                let poolf = unsafe { File::from_raw_fd(pf.fd) };
                let mut b = vec![0u8; pf.off];
                poolf.read_exact_at(&mut b, 0).unwrap();
                std::mem::forget(poolf);
                b
            }
        };
        let names: HashMap<usize, String> = layermaps[seg]
            .iter()
            .map(|(k, v)| (v.id, k.clone()))
            .collect();

        for rec in index.chunks_exact(32) {
            let x = tippecanoe::tippecanoe_main::Index::from_bytes(rec);
            let mut p: &[u8] = &geom[x.start as usize..x.end as usize];
            let len = tippecanoe::serial::deserialize_long_long(&mut p) as usize;
            assert_eq!(len, p.len());
            let sf = tippecanoe::serial::deserialize_feature(p, 0, 0, 0, &ix, &iy);
            let attrs = sf
                .keys
                .iter()
                .zip(sf.values.iter())
                .map(|(&k, &v)| {
                    (
                        cstring_at(&pool, k as usize + 1),
                        pool[v as usize] as i8 as i32,
                        cstring_at(&pool, v as usize + 1),
                    )
                })
                .collect();
            features.push(Feat {
                seq: x.seq(),
                ix: x.ix,
                t: sf.t,
                index: sf.index,
                label_point: sf.label_point,
                extent: sf.extent,
                has_id: sf.has_id,
                id: sf.id,
                minzoom: sf.tippecanoe_minzoom,
                maxzoom: sf.tippecanoe_maxzoom,
                layer: names[&(sf.layer as usize)].clone(),
                geometry: sf.geometry.iter().map(|d| (d.op, d.x, d.y)).collect(),
                bbox: sf.bbox,
                attrs,
                segment: seg as u16,
                layer_number: sf.layer,
                pool: sf
                    .keys
                    .iter()
                    .copied()
                    .zip(sf.values.iter().copied())
                    .collect(),
            });
            assert_eq!(sf.segment as usize, seg);
            assert_eq!(x.t() as i8, sf.t);
            assert_eq!(x.seq(), sf.seq as u64 & ((1 << 46) - 1));
        }
        std::mem::forget(geomf);
        std::mem::forget(indexf);
    }
    features.sort_by_key(|f| f.seq);

    Reference {
        features,
        layermap: merge_layermaps(&layermaps),
        dist_sum,
        dist_count,
        area_sum,
        bbox,
        bbox1,
        bbox2,
        initial_x: ix,
        initial_y: iy,
        geom_bytes,
    }
}

fn store_features(s: &FeatureStore) -> Vec<Feat> {
    let mut out: Vec<Feat> = s
        .features
        .iter()
        .map(|f| Feat {
            seq: f.seq & ((1 << 46) - 1),
            ix: f.ix,
            t: f.t,
            index: f.index(),
            label_point: f.label_point,
            extent: f.extent,
            has_id: f.has_id,
            id: f.id,
            minzoom: f.tippecanoe_minzoom as i32,
            maxzoom: f.tippecanoe_maxzoom as i32,
            layer: s.layers[f.layer as usize].name.clone(),
            geometry: s
                .feature_geometry(f)
                .iter()
                .map(|d: &Draw| (d.op, d.x, d.y))
                .collect(),
            bbox: s.feature_bbox(f),
            attrs: s
                .feature_attrs(f)
                .iter()
                .map(|a| {
                    let v = s.values.get(a.value);
                    (s.keys[a.key as usize].clone(), v.type_, v.s.to_string())
                })
                .collect(),
            segment: f.segment,
            layer_number: s.segment_layers[f.segment as usize][f.layer as usize],
            pool: s.pool_offsets
                [f.attr_start as usize..f.attr_start as usize + f.attr_len as usize]
                .iter()
                .map(|p| (p.key, p.value))
                .collect(),
        })
        .collect();
    out.sort_by_key(|f| f.seq);
    out
}

/// Compares manby with tippecanoe-rs for one command line. Returns a
/// description of the first difference.
fn compare(
    argv: &[String],
    cpus: usize,
    chunk_bytes: usize,
    threads: usize,
) -> Result<usize, String> {
    let mut o = parse_args(argv).map_err(|e| format!("parse: {}", e.message))?;
    o.quiet = true;
    o.tippecanoe_cpus = cpus;
    o.chunk_bytes = chunk_bytes;
    o.threads = threads;
    // what -at needs from reading is compared too
    o.additional
        .set(tippecanoe::options::A_VARIABLE_DEPTH_PYRAMID, true);
    o.resolve().map_err(|e| format!("resolve: {}", e.message))?;
    set_tippecanoe_globals(&o, o.cluster_maxzoom);
    tippecanoe::globals::QUIET.store(1, Ordering::Relaxed);

    let store = manby::ingest::read_inputs(&o).map_err(|e| format!("manby: {}", e.message))?;
    let r = reference(&o);
    let mine = store_features(&store);

    if mine.len() != r.features.len() {
        return Err(format!(
            "{} features, tippecanoe-rs has {}",
            mine.len(),
            r.features.len()
        ));
    }
    for (a, b) in mine.iter().zip(r.features.iter()) {
        if a != b {
            return Err(format!(
                "feature differs:\nmanby: {:?}\ntippecanoe-rs: {:?}",
                a, b
            ));
        }
    }

    let lm = merge_layermaps(&[store.layermap()]);
    let fmt = |m: &BTreeMap<String, LayermapEntry>| {
        m.iter()
            .map(|(k, v)| {
                format!(
                    "{} {} {} {} {:?} {:?}",
                    k,
                    v.points,
                    v.lines,
                    v.polygons,
                    v.description,
                    v.tilestats
                        .iter()
                        .map(|(k, t)| (k.clone(), t.sample_values.clone(), t.min, t.max, t.type_))
                        .collect::<Vec<_>>()
                )
            })
            .collect::<Vec<_>>()
    };
    if fmt(&lm) != fmt(&r.layermap) {
        return Err(format!(
            "layers differ:\nmanby: {:?}\ntippecanoe-rs: {:?}",
            fmt(&lm),
            fmt(&r.layermap)
        ));
    }
    if store.dist_sum.to_bits() != r.dist_sum.to_bits()
        || store.dist_count != r.dist_count
        || store.area_sum.to_bits() != r.area_sum.to_bits()
    {
        return Err(format!(
            "sums differ: {} {} {} vs {} {} {}",
            store.dist_sum, store.dist_count, store.area_sum, r.dist_sum, r.dist_count, r.area_sum
        ));
    }
    // tippecanoe has room for twice as many segments as it uses
    let (rx, ry) = (&r.initial_x[..cpus], &r.initial_y[..cpus]);
    if store.initial_x != rx || store.initial_y != ry {
        return Err(format!(
            "initial points differ: {:?} {:?} vs {:?} {:?}",
            store.initial_x, store.initial_y, rx, ry
        ));
    }
    if store.serialized_bytes != r.geom_bytes {
        return Err(format!(
            "serialized bytes differ: {} vs {}",
            store.serialized_bytes, r.geom_bytes
        ));
    }
    if store.bbox != r.bbox || store.bbox1 != r.bbox1 || store.bbox2 != r.bbox2 {
        return Err(format!(
            "bounds differ: {:?} {:?} {:?} vs {:?} {:?} {:?}",
            store.bbox, store.bbox1, store.bbox2, r.bbox, r.bbox1, r.bbox2
        ));
    }
    Ok(mine.len())
}

fn tests_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tippecanoe-rs/tests")
}

/// The arguments of a golden test, decoded from its file name as the Makefile does.
fn golden_args(name: &str) -> Vec<String> {
    name.split('_')
        .map(|a| {
            a.replace("%5f", "_")
                .replace("%7b", "{")
                .replace("%2f", "/")
                .replace("%3d", "=")
                .replace("%3a", ":")
                .replace("%22", "\"")
                .replace("%20", " ")
        })
        .filter(|a| !a.is_empty())
        .collect()
}

/// The command lines of the golden tests, with absolute input paths.
fn golden_cases() -> Vec<(String, Vec<String>)> {
    let tests = std::path::PathBuf::from("tests");
    let mut cases = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(&tests)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    for dir in dirs {
        let out = dir.join("out");
        if !out.is_dir() {
            continue;
        }
        let mut inputs = Vec::new();
        for suffix in [".json", ".json.gz"] {
            let mut v: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| p.ends_with(suffix) && !(suffix == ".json" && p.ends_with(".json.gz")))
                .collect();
            v.sort();
            inputs.extend(v);
        }
        let mut outs: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        outs.sort();
        for o in outs {
            let mut argv = vec![
                "tippecanoe".to_string(),
                "-q".to_string(),
                "-a@".to_string(),
                "-f".to_string(),
                "-o".to_string(),
                "/tmp/manby-ref.mbtiles".to_string(),
            ];
            argv.extend(golden_args(o.trim_end_matches(".json")));
            argv.extend(inputs.iter().cloned());
            cases.push((
                format!("{}/{}", dir.file_name().unwrap().to_string_lossy(), o),
                argv,
            ));
        }
    }
    cases
}

#[test]
fn reference_golden() {
    let _lock = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    // The globals are process-wide, so the cases run one at a time, from
    // where the golden tests run.
    std::env::set_current_dir(tests_dir().join("..")).unwrap();
    let mut failures = Vec::new();
    let mut compared = 0;
    let mut skipped = Vec::new();
    for (name, argv) in golden_cases() {
        for (cpus, chunk_bytes, threads) in [(4, 0, 0), (1, 100, 3), (16, 7, 5)] {
            match compare(&argv, cpus, chunk_bytes, threads) {
                Ok(_) => compared += 1,
                Err(e) if e.starts_with("parse:") || e.starts_with("resolve:") => {
                    skipped.push(format!("{}: {}", name, e));
                    break;
                }
                Err(e) => failures.push(format!(
                    "{} (cpus {} chunk {}): {}",
                    name, cpus, chunk_bytes, e
                )),
            }
        }
    }
    for s in &skipped {
        eprintln!("skipped {}", s);
    }
    eprintln!("compared {} golden cases", compared);
    if !failures.is_empty() {
        for f in failures.iter().take(10) {
            eprintln!("{}\n", f);
        }
        panic!("{} golden cases differ", failures.len());
    }
}

#[test]
fn reference_one() {
    let _lock = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let (Ok(input), Ok(args)) = (
        std::env::var("REFERENCE_INPUT"),
        std::env::var("REFERENCE_ARGS"),
    ) else {
        return;
    };
    let mut argv = vec![
        "tippecanoe".to_string(),
        "-o".to_string(),
        "/tmp/x.mbtiles".to_string(),
    ];
    argv.extend(args.split_whitespace().map(|s| s.to_string()));
    argv.push(input);
    let cpus = std::env::var("REFERENCE_CPUS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(16);
    match compare(&argv, cpus, 0, 0) {
        Ok(n) => eprintln!("{} features match", n),
        Err(e) => panic!("{}", e),
    }
}

/// Everything in a store, for checking that two stores are the same.
fn store_dump(s: &FeatureStore) -> String {
    let mut out = String::new();
    for f in &s.features {
        out += &format!("{:?}\n", f);
    }
    for g in &s.geometry {
        out += &format!("{} {} {}\n", g.op, g.x, g.y);
    }
    for a in &s.attrs {
        out += &format!("{} {}\n", a.key, a.value);
    }
    out += &format!("{:?}\n{:?}\n", s.keys, s.values);
    for l in &s.layers {
        out += &format!("{} {:?}\n", l.name, l.entry);
    }
    out += &format!(
        "{:?} {:?} {:?} {} {} {}\n",
        s.bbox, s.bbox1, s.bbox2, s.dist_sum, s.dist_count, s.area_sum
    );
    out += &format!(
        "{:?} {:?} {:?} {:?} {}\n",
        s.initial_x, s.initial_y, s.segment_layers, s.pool_offsets, s.serialized_bytes
    );
    out
}

/// The store must not depend on the number of threads or on how the input
/// is split for reading.
#[test]
fn deterministic_store() {
    let _lock = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let tests = tests_dir();
    let inputs = [
        "ne_110m_populated_places/in.json",
        "ne_110m_admin_0_countries/in.json.gz",
        "tl_2015_us_county/piscataquis.json",
        "dateline/in.json",
    ];
    for args in [
        "-zg",
        "-P -zg --drop-densest-as-needed",
        "-z5 -ai --calculate-feature-index",
        "-P -z5 -at",
    ] {
        let mut argv = vec![
            "tippecanoe".to_string(),
            "-o".to_string(),
            "/tmp/x.pmtiles".to_string(),
        ];
        argv.extend(args.split_whitespace().map(|s| s.to_string()));
        for i in inputs {
            argv.push(tests.join(i).to_string_lossy().into_owned());
        }
        let mut first: Option<String> = None;
        for (threads, chunk_bytes) in [(1, 0), (2, 50), (7, 1000), (18, 3)] {
            let mut o = parse_args(&argv).unwrap();
            o.quiet = true;
            o.tippecanoe_cpus = 8;
            o.threads = threads;
            o.chunk_bytes = chunk_bytes;
            o.resolve().unwrap();
            set_tippecanoe_globals(&o, o.cluster_maxzoom);
            let mut store = manby::ingest::read_inputs(&o).unwrap();
            let plan = manby::order::order(&mut store, &o).unwrap();
            let dump = store_dump(&store) + &format!("{:?}", plan);
            match &first {
                None => first = Some(dump),
                Some(f) => assert!(
                    *f == dump,
                    "{}: {} threads, {} byte chunks differ",
                    args,
                    threads,
                    chunk_bytes
                ),
            }
        }
    }
}

/// manby's replay of the string pool against tippecanoe-rs's `addpool`:
/// random strings with repeats, a memory limit that the pool runs past, and
/// strings in the order of their hashes, which makes the search tree too
/// deep.
#[test]
fn string_pool() {
    let _lock = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    use manby::ingest::serialized::{Pool, Table};
    use tippecanoe::globals::{CPUS, MEMSIZE};

    let mut x: u64 = 1;
    let mut random: Vec<(String, i8, Table)> = Vec::new();
    for i in 0..400_000u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let s = match i % 4 {
            0 => format!("{}", x % 1000),
            1 => format!("v{}", x),
            2 => format!("k{}", x % 50),
            _ => format!("{}", i),
        };
        let table = if i % 2 == 0 { Table::Key } else { Table::Value };
        random.push((s, (x % 3) as i8, table));
    }
    let mut sorted: Vec<(u64, String)> = (0..20_000)
        .map(|i| format!("s{}", i))
        .map(|s| (tippecanoe::text::fnv1a_cstr(s.as_bytes(), 0), s))
        .filter(|(h, _)| *h < 1 << 62)
        .collect();
    sorted.sort();
    let mut sorted: Vec<(String, i8, Table)> = sorted
        .into_iter()
        .map(|(_, s)| (s, 0, Table::Value))
        .collect();
    let again = sorted.clone();
    sorted.extend(again);

    for (strings, limit, fills) in [
        (&random, usize::MAX / 20, false),
        (&random, 3_000_000, true),
        (&sorted, usize::MAX / 20, false),
    ] {
        CPUS.store(1, Ordering::Relaxed);
        MEMSIZE.store(limit * 10, Ordering::Relaxed);
        let (pool_fd, _) = tmpfd("pool");
        let (tree_fd, _) = tmpfd("tree");
        let mut pf = tippecanoe::memfile::memfile_open(pool_fd).unwrap();
        let mut tf = tippecanoe::memfile::memfile_open(tree_fd).unwrap();
        let mut in_memory = false;
        tippecanoe::memfile::memfile_write(&mut tf, &[0u8; 32], &mut in_memory);
        let mut dedup = [vec![-1isize; 655536], vec![-1isize; 655536]];

        let mut mine = Pool::new(limit);
        let mut deep = 0;
        for (i, (s, type_, table)) in strings.iter().enumerate() {
            let (pool_before, tree_before) = (pf.off, tf.off);
            let want = tippecanoe::pool::addpool(
                &mut pf,
                &mut tf,
                s.as_bytes(),
                *type_,
                &mut dedup[*table as usize],
            );
            if pf.fp.is_none() && pf.off > pool_before && tf.off == tree_before {
                deep += 1;
            }
            assert_eq!(
                mine.add(s.as_bytes(), *type_, *table),
                want,
                "string {} ({})",
                i,
                s
            );
        }
        assert_eq!(pf.fp.is_some(), fills);
        if std::ptr::eq(strings, &sorted) {
            assert!(
                deep > 1000,
                "only {} strings went past the depth limit",
                deep
            );
        }
        let _ = tippecanoe::memfile::memfile_close(pf);
        let _ = tippecanoe::memfile::memfile_close(tf);
    }
}
