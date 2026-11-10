//! Times manby's sinks against tippecanoe-rs's writers on the same tiles, and
//! checks that both write the same output.
//!
//!   cargo run --release --example sink_bench -- [options]
//!
//!   --tiles N        synthetic tiles to write (default 300000)
//!   --from X         instead, every tile of the PMTiles archive X
//!   --threads N      threads writing tiles (default: all cores)
//!   --budget BYTES   PMTiles memory budget (default: a quarter of RAM)
//!   --outputs LIST   any of pmtiles,mbtiles,dir (default: pmtiles)
//!   --rounds N       runs of each writer (default 3)
//!   --skip-tippecanoe  time manby only

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;
use tippecanoe::mbtiles::Metadata;

use manby::sink::dir::DirSink;
use manby::sink::mbtiles::MbtilesSink;
use manby::sink::pmtiles::PmtilesSink;
use manby::sink::{SinkSettings, TileSink};

#[cfg(not(windows))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Tiles as z/x/y and a byte range of a shared buffer.
struct Tiles<'a> {
    bytes: &'a [u8],
    tiles: Vec<(u8, u32, u32, usize, usize)>,
}

impl Tiles<'_> {
    fn data(&self, i: usize) -> &[u8] {
        let t = &self.tiles[i];
        &self.bytes[t.3..t.4]
    }

    fn total_bytes(&self) -> usize {
        self.tiles.iter().map(|t| t.4 - t.3).sum()
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// `n` tiles from zoom 0 down, with tippecanoe-like sizes (mostly a few
/// hundred bytes to a few kilobytes, some up to 400 KB) and about a quarter
/// of them repeating an earlier tile.
fn synthetic(n: usize, buffer: &mut Vec<u8>) -> Vec<(u8, u32, u32, usize, usize)> {
    let mut rng = Rng(42);
    let mut tiles = Vec::with_capacity(n);
    let fill = |len: usize, rng: &mut Rng, buffer: &mut Vec<u8>| {
        let start = buffer.len();
        while buffer.len() < start + len {
            buffer.extend_from_slice(&rng.next().to_le_bytes()[..4]);
        }
        buffer.truncate(start + len);
        (start, start + len)
    };
    let pool: Vec<(usize, usize)> = (0..64)
        .map(|i| fill(30 + i * 23, &mut rng, buffer))
        .collect();
    let mut prev = pool[0];
    let mut id: u64 = 0;
    while tiles.len() < n {
        let t = tippecanoe::pmtiles::tileid_to_zxy(id);
        let range = match rng.next() % 100 {
            0..=9 => prev,
            10..=24 => pool[(rng.next() % pool.len() as u64) as usize],
            r => {
                let len = match r {
                    25..=84 => 100 + rng.next() % 1_400,
                    85..=96 => 2_000 + rng.next() % 18_000,
                    97..=98 => 20_000 + rng.next() % 80_000,
                    _ => 100_000 + rng.next() % 300_000,
                };
                fill(len as usize, &mut rng, buffer)
            }
        };
        tiles.push((t.z, t.x, t.y, range.0, range.1));
        prev = range;
        id += 1 + if rng.next().is_multiple_of(3) {
            rng.next() % 40
        } else {
            0
        };
    }
    tiles
}

/// Every tile of a PMTiles archive.
fn replay(map: &[u8]) -> Vec<(u8, u32, u32, usize, usize)> {
    tippecanoe::pmtiles_file::pmtiles_entries_tms(map, 0, 32)
        .into_iter()
        .map(|e| {
            (
                e.z,
                e.x,
                e.y,
                e.offset as usize,
                e.offset as usize + e.length as usize,
            )
        })
        .collect()
}

fn metadata(maxzoom: i32) -> Metadata {
    Metadata {
        name: "bench".to_string(),
        description: "bench".to_string(),
        version: 2,
        type_: "overlay".to_string(),
        format: "pbf".to_string(),
        minzoom: 0,
        maxzoom,
        minlat: 24.5,
        minlon: -124.75,
        maxlat: 49.4,
        maxlon: -66.9,
        minlat2: 24.5,
        minlon2: -124.75,
        maxlat2: 49.4,
        maxlon2: -66.9,
        center_lon: -95.0,
        center_lat: 37.0,
        center_z: maxzoom,
        generator: "tippecanoe v2.82.0".to_string(),
        generator_options: "sink_bench".to_string(),
        vector_layers_json:
            "[{\"id\":\"bench\",\"description\":\"\",\"minzoom\":0,\"maxzoom\":14,\"fields\":{}}]"
                .to_string(),
        tilestats_json: "{\"layerCount\":1,\"layers\":[]}".to_string(),
        ..Default::default()
    }
}

fn secs(t: Instant) -> f64 {
    t.elapsed().as_secs_f64()
}

fn feed(sink: &dyn TileSink, tiles: &Tiles) {
    (0..tiles.tiles.len()).into_par_iter().for_each(|i| {
        let t = &tiles.tiles[i];
        sink.write_tile(t.0, t.1, t.2, tiles.data(i)).unwrap();
    });
}

/// tippecanoe-rs: every tile through one locked connection, then close, then
/// for PMTiles the conversion. Returns (insert, close, convert) seconds.
fn tippecanoe_db(path: &Path, tiles: &Tiles, m: &Metadata, pmtiles: bool) -> (f64, f64, f64) {
    let p = path.to_str().unwrap();
    let _ = fs::remove_file(p);
    let argv = vec!["sink_bench".to_string()];
    let start = Instant::now();
    let db = Mutex::new(tippecanoe::mbtiles::mbtiles_open(p, &argv, 0));
    (0..tiles.tiles.len()).into_par_iter().for_each(|i| {
        let t = &tiles.tiles[i];
        let db = db.lock().unwrap();
        tippecanoe::mbtiles::mbtiles_write_tile(
            &db,
            t.0 as i32,
            t.1 as i32,
            t.2 as i32,
            tiles.data(i),
        );
    });
    let insert = secs(start);
    let start = Instant::now();
    let db = db.into_inner().unwrap();
    tippecanoe::mbtiles::mbtiles_write_metadata(&db, m, false);
    tippecanoe::mbtiles::mbtiles_close(db, "sink_bench");
    let close = secs(start);
    let start = Instant::now();
    if pmtiles {
        tippecanoe::pmtiles_file::mbtiles_map_image_to_pmtiles(p, m.clone(), true, true, true);
    }
    (insert, close, secs(start))
}

fn tippecanoe_dir(path: &Path, tiles: &Tiles, m: &Metadata) -> (f64, f64) {
    let p = path.to_str().unwrap();
    let _ = fs::remove_dir_all(p);
    let argv = vec!["sink_bench".to_string()];
    tippecanoe::dirtiles::check_dir(p, &argv, false, false);
    let start = Instant::now();
    let lock = Mutex::new(());
    (0..tiles.tiles.len()).into_par_iter().for_each(|i| {
        let t = &tiles.tiles[i];
        let _guard = lock.lock().unwrap();
        tippecanoe::dirtiles::dir_write_tile(p, t.0 as i32, t.1 as i32, t.2 as i32, tiles.data(i));
    });
    let write = secs(start);
    let start = Instant::now();
    tippecanoe::dirtiles::dir_write_metadata(p, m);
    (write, secs(start))
}

/// Writes with a manby sink. Returns (write, finish) seconds.
fn manby_sink(sink: Box<dyn TileSink>, tiles: &Tiles, m: &Metadata) -> (f64, f64) {
    let start = Instant::now();
    feed(sink.as_ref(), tiles);
    let write = secs(start);
    let start = Instant::now();
    sink.finish(m).unwrap();
    (write, secs(start))
}

fn mbtiles_rows(path: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(path).unwrap();
    let mut rows = Vec::new();
    for sql in [
        "SELECT type || '|' || name || '|' || coalesce(sql, '') FROM sqlite_master ORDER BY name",
        "SELECT name || '|' || value FROM metadata ORDER BY rowid",
        "SELECT zoom_level || '/' || tile_column || '/' || tile_row || '|' || tile_id FROM map ORDER BY 1",
        "SELECT zoom_level || '|' || tile_id || '|' || length(tile_data) || '|' || hex(substr(tile_data, 1, 64)) FROM images ORDER BY 1",
        "SELECT tbl || '|' || coalesce(idx, '') || '|' || stat FROM sqlite_stat1 ORDER BY 1",
    ] {
        let mut stmt = db.prepare(sql).unwrap();
        rows.extend(
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap()),
        );
    }
    rows
}

fn same_tree(a: &Path, b: &Path) -> bool {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push((
                    p.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(&p).unwrap(),
                ));
            }
        }
    }
    let (mut x, mut y) = (Vec::new(), Vec::new());
    walk(a, a, &mut x);
    walk(b, b, &mut y);
    x.sort();
    y.sort();
    x == y
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    let n: usize = arg("--tiles").map_or(300_000, |s| s.parse().unwrap());
    let rounds: usize = arg("--rounds").map_or(3, |s| s.parse().unwrap());
    let outputs = arg("--outputs").unwrap_or_else(|| "pmtiles".to_string());
    let skip_tippecanoe = args.iter().any(|a| a == "--skip-tippecanoe");
    if let Some(threads) = arg("--threads") {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads.parse().unwrap())
            .build_global()
            .unwrap();
    }
    let mut settings = SinkSettings {
        tmpdir: std::env::temp_dir(),
        program: "sink_bench".to_string(),
        ..SinkSettings::default()
    };
    if let Some(budget) = arg("--budget") {
        settings.memory_budget = budget.parse().unwrap();
    }

    let mut buffer = Vec::new();
    let mut _map = None;
    let (bytes, list): (&[u8], _) = match arg("--from") {
        Some(path) => {
            let f = fs::File::open(&path).unwrap();
            // SAFETY: the archive is not changed while the benchmark runs.
            let map = unsafe { memmap2::Mmap::map(&f) }.unwrap();
            let list = replay(&map);
            _map = Some(map);
            (_map.as_deref().unwrap(), list)
        }
        None => {
            let list = synthetic(n, &mut buffer);
            (&buffer[..], list)
        }
    };
    let tiles = Tiles { bytes, tiles: list };
    let maxzoom = tiles.tiles.iter().map(|t| t.0).max().unwrap_or(0) as i32;
    let m = metadata(maxzoom);
    let distinct: std::collections::HashSet<&[u8]> =
        (0..tiles.tiles.len()).map(|i| tiles.data(i)).collect();
    println!(
        "{} tiles, {} distinct, {:.1} MB, zoom 0-{}, {} threads",
        tiles.tiles.len(),
        distinct.len(),
        tiles.total_bytes() as f64 / 1e6,
        maxzoom,
        rayon::current_num_threads()
    );

    let dir = std::env::temp_dir().join(format!("manby-sink-bench-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name);

    if outputs.contains("pmtiles") {
        for round in 0..rounds {
            let p = path("manby.pmtiles");
            let _ = fs::remove_file(&p);
            let start = Instant::now();
            let sink = Box::new(PmtilesSink::create(p.to_str().unwrap(), &settings).unwrap());
            let (write, finish) = manby_sink(sink, &tiles, &m);
            println!(
                "pmtiles  manby       total {:7.3} s   write {:7.3}  finish {:7.3}",
                secs(start),
                write,
                finish
            );
            if skip_tippecanoe {
                continue;
            }
            let t = path("tippecanoe.pmtiles");
            let start = Instant::now();
            let (insert, close, convert) = tippecanoe_db(&t, &tiles, &m, true);
            println!(
                "pmtiles  tippecanoe  total {:7.3} s   insert {:7.3}  close {:7.3}  convert {:7.3}",
                secs(start),
                insert,
                close,
                convert
            );
            if round == 0 {
                let same = fs::read(&p).unwrap() == fs::read(&t).unwrap();
                println!(
                    "pmtiles  {} ({} bytes)",
                    if same { "identical" } else { "DIFFERENT" },
                    fs::metadata(&p).unwrap().len()
                );
            }
            let _ = fs::remove_file(&t);
            let _ = fs::remove_file(&p);
        }
    }

    if outputs.contains("mbtiles") {
        for round in 0..rounds {
            let p = path("manby.mbtiles");
            let _ = fs::remove_file(&p);
            let start = Instant::now();
            let sink = Box::new(MbtilesSink::create(p.to_str().unwrap(), &settings).unwrap());
            let (write, finish) = manby_sink(sink, &tiles, &m);
            println!(
                "mbtiles  manby       total {:7.3} s   write {:7.3}  finish {:7.3}",
                secs(start),
                write,
                finish
            );
            if skip_tippecanoe {
                continue;
            }
            let t = path("tippecanoe.mbtiles");
            let start = Instant::now();
            let (insert, close, _) = tippecanoe_db(&t, &tiles, &m, false);
            println!(
                "mbtiles  tippecanoe  total {:7.3} s   insert {:7.3}  close {:7.3}",
                secs(start),
                insert,
                close
            );
            if round == 0 {
                let same = mbtiles_rows(&p) == mbtiles_rows(&t);
                println!(
                    "mbtiles  {}",
                    if same { "same rows" } else { "DIFFERENT ROWS" }
                );
            }
            let _ = fs::remove_file(&t);
            let _ = fs::remove_file(&p);
        }
    }

    if outputs.contains("dir") {
        for round in 0..rounds {
            let p = path("manby-dir");
            let _ = fs::remove_dir_all(&p);
            let start = Instant::now();
            let sink = Box::new(DirSink::create(p.to_str().unwrap(), &settings).unwrap());
            let (write, finish) = manby_sink(sink, &tiles, &m);
            println!(
                "dir      manby       total {:7.3} s   write {:7.3}  finish {:7.3}",
                secs(start),
                write,
                finish
            );
            if skip_tippecanoe {
                let _ = fs::remove_dir_all(&p);
                continue;
            }
            let t = path("tippecanoe-dir");
            let start = Instant::now();
            let (write, finish) = tippecanoe_dir(&t, &tiles, &m);
            println!(
                "dir      tippecanoe  total {:7.3} s   write {:7.3}  finish {:7.3}",
                secs(start),
                write,
                finish
            );
            if round == 0 {
                println!(
                    "dir      {}",
                    if same_tree(&p, &t) {
                        "same files"
                    } else {
                        "DIFFERENT FILES"
                    }
                );
            }
            let _ = fs::remove_dir_all(&t);
            let _ = fs::remove_dir_all(&p);
        }
    }
    let _ = fs::remove_dir_all(&dir);
}
