//! The sinks against tippecanoe-rs's own writers, on the same tiles.

use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::Connection;
use tippecanoe::mbtiles::Metadata;

use super::dir::DirSink;
use super::mbtiles::MbtilesSink;
use super::pmtiles::PmtilesSink;
use super::{SinkSettings, TileSink};

pub(crate) type Tile = (u8, u32, u32, Vec<u8>);

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

/// `n` tiles over zooms 0 to `maxzoom` of a region, with sizes from a few
/// bytes to tens of kilobytes, about a third of them repeating earlier tiles
/// (some on neighbouring tile ids, which makes runs) and one empty tile.
pub(crate) fn make_tiles(n: usize, maxzoom: u8, seed: u64) -> Vec<Tile> {
    make_tiles_from(n, 0, maxzoom, seed)
}

/// Like `make_tiles`, starting at tile id `start`.
pub(crate) fn make_tiles_from(n: usize, start: u64, maxzoom: u8, seed: u64) -> Vec<Tile> {
    let mut rng = Rng(seed);
    let pool: Vec<Vec<u8>> = (0..12)
        .map(|i| (0..(20 + i * 37)).map(|_| rng.next() as u8).collect())
        .collect();
    let mut tiles = Vec::with_capacity(n);
    let mut seen = std::collections::HashSet::new();
    let mut id = start + 3 + rng.next() % 50;
    let mut prev: Vec<u8> = Vec::new();
    while tiles.len() < n {
        let t = tippecanoe::pmtiles::tileid_to_zxy(id);
        if t.z > maxzoom {
            id = start + rng.next() % 50;
            continue;
        }
        if seen.insert(id) {
            let data = match rng.next() % 10 {
                0..=1 if !prev.is_empty() => prev.clone(),
                2..=3 => pool[(rng.next() % pool.len() as u64) as usize].clone(),
                4 if tiles.len() == 7 => Vec::new(),
                _ => {
                    let len = match rng.next() % 20 {
                        0 => 20_000 + rng.next() % 40_000,
                        1..=5 => 2_000 + rng.next() % 8_000,
                        _ => 50 + rng.next() % 1_500,
                    };
                    (0..len).map(|_| rng.next() as u8).collect()
                }
            };
            tiles.push((t.z, t.x, t.y, data.clone()));
            prev = data;
        }
        id += 1 + if rng.next() % 4 == 0 {
            rng.next() % 300
        } else {
            0
        };
    }
    tiles
}

pub(crate) fn make_metadata(maxzoom: i32) -> Metadata {
    Metadata {
        name: "test 'quoted' \u{e9}".to_string(),
        description: "a \"description\"".to_string(),
        version: 2,
        type_: "overlay".to_string(),
        format: "pbf".to_string(),
        minzoom: 0,
        maxzoom,
        minlat: -33.123457,
        minlon: -122.000001,
        maxlat: 47.5,
        maxlon: 151.25,
        minlat2: -33.123457,
        minlon2: 151.25,
        maxlat2: 47.5,
        maxlon2: 237.999999,
        center_lon: -0.0,
        center_lat: 12.345678,
        center_z: maxzoom,
        attribution: "\u{a9} someone".to_string(),
        generator: "tippecanoe v2.82.0".to_string(),
        generator_options: "tippecanoe -o 'x y.pmtiles'".to_string(),
        strategies_json: "[{},{\"dropped_by_rate\":5}]".to_string(),
        vector_layers_json:
            "[{\"id\":\"l\",\"description\":\"\",\"minzoom\":0,\"maxzoom\":5,\"fields\":{}}]"
                .to_string(),
        tilestats_json: "{\"layerCount\":1,\"layers\":[]}".to_string(),
        decisions_json: "{\"basezoom\":3,\"droprate\":2.5,\"retain_points_multiplier\":1}"
            .to_string(),
    }
}

pub(crate) fn temp_path(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("manby-sink-{}-{}", std::process::id(), name));
    let _ = fs::remove_file(&p);
    let _ = fs::remove_dir_all(&p);
    p
}

fn settings() -> SinkSettings {
    SinkSettings {
        tmpdir: std::env::temp_dir(),
        program: "test".to_string(),
        ..SinkSettings::default()
    }
}

/// Writes `tiles` to `sink` from `threads` threads, in an order made from `seed`.
pub(crate) fn feed(sink: &dyn TileSink, tiles: &[Tile], threads: usize, seed: u64) {
    let mut order: Vec<usize> = (0..tiles.len()).collect();
    let mut rng = Rng(seed);
    for i in (1..order.len()).rev() {
        order.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    pool.install(|| {
        order.par_iter().for_each(|&i| {
            let (z, x, y, data) = &tiles[i];
            sink.write_tile(*z, *x, *y, data).unwrap();
        })
    });
}

/// What tippecanoe-rs writes: an MBTiles database, converted to PMTiles if
/// `pmtiles` is set. `erase` erases a zoom after the first `split` tiles.
pub(crate) fn tippecanoe_output(
    path: &Path,
    tiles: &[Tile],
    m: &Metadata,
    pmtiles: bool,
    tile_compression: bool,
    erase: Option<(usize, u8)>,
) {
    let path = path.to_str().unwrap();
    let argv = vec!["test".to_string()];
    let db = tippecanoe::mbtiles::mbtiles_open(path, &argv, 0);
    for (i, (z, x, y, data)) in tiles.iter().enumerate() {
        if let Some((split, ez)) = erase
            && i == split
        {
            tippecanoe::mbtiles::mbtiles_erase_zoom(&db, ez as i32);
        }
        tippecanoe::mbtiles::mbtiles_write_tile(&db, *z as i32, *x as i32, *y as i32, data);
    }
    tippecanoe::mbtiles::mbtiles_write_metadata(&db, m, false);
    tippecanoe::mbtiles::mbtiles_close(db, "test");
    if pmtiles {
        tippecanoe::pmtiles_file::mbtiles_map_image_to_pmtiles(
            path,
            m.clone(),
            tile_compression,
            true,
            true,
        );
    }
}

fn manby_pmtiles(
    path: &Path,
    tiles: &[Tile],
    m: &Metadata,
    settings: &SinkSettings,
    threads: usize,
    seed: u64,
) -> Vec<u8> {
    let _ = fs::remove_file(path);
    let sink = Box::new(PmtilesSink::create(path.to_str().unwrap(), settings).unwrap());
    feed(sink.as_ref(), tiles, threads, seed);
    sink.finish(m).unwrap();
    fs::read(path).unwrap()
}

fn check_pmtiles(name: &str, tiles: &[Tile], maxzoom: i32, settings: &SinkSettings) {
    let m = make_metadata(maxzoom);
    let expected_path = temp_path(&format!("{}-tippecanoe.pmtiles", name));
    tippecanoe_output(
        &expected_path,
        tiles,
        &m,
        true,
        settings.tile_compression,
        None,
    );
    let expected = fs::read(&expected_path).unwrap();

    let path = temp_path(&format!("{}.pmtiles", name));
    let got = manby_pmtiles(&path, tiles, &m, settings, 8, 1);
    assert!(got == expected, "{}: archives differ", name);
    let _ = fs::remove_file(&expected_path);
    let _ = fs::remove_file(&path);
}

#[test]
fn pmtiles_same_as_tippecanoe() {
    check_pmtiles("small", &make_tiles(3000, 14, 1), 14, &settings());
    check_pmtiles("one", &make_tiles(1, 0, 2), 0, &settings());
    check_pmtiles("none", &[], 0, &settings());
    let uncompressed = SinkSettings {
        tile_compression: false,
        ..settings()
    };
    check_pmtiles("uncompressed", &make_tiles(500, 9, 3), 9, &uncompressed);
}

#[test]
fn pmtiles_with_leaf_directories() {
    let tiles = make_tiles(40_000, 14, 4);
    check_pmtiles("leaves", &tiles, 14, &settings());

    let path = temp_path("leaves-check.pmtiles");
    let archive = manby_pmtiles(&path, &tiles, &make_metadata(14), &settings(), 4, 9);
    let header = tippecanoe::pmtiles::deserialize_header(&archive[..127]);
    assert!(header.leaf_dirs_bytes > 0);
    let _ = fs::remove_file(&path);
}

#[test]
fn pmtiles_spilled_to_disk() {
    let small_budget = SinkSettings {
        memory_budget: 100_000,
        ..settings()
    };
    check_pmtiles("spill", &make_tiles(3000, 14, 5), 14, &small_budget);
    let nothing_in_memory = SinkSettings {
        memory_budget: 0,
        ..settings()
    };
    check_pmtiles("spill-all", &make_tiles(800, 12, 6), 12, &nothing_in_memory);
}

#[test]
fn pmtiles_independent_of_threads_and_order() {
    let tiles = make_tiles(5000, 14, 7);
    let m = make_metadata(14);
    let path = temp_path("order.pmtiles");
    let one = manby_pmtiles(&path, &tiles, &m, &settings(), 1, 11);
    let many = manby_pmtiles(&path, &tiles, &m, &settings(), 16, 12);
    let spilled = manby_pmtiles(
        &path,
        &tiles,
        &m,
        &SinkSettings {
            memory_budget: 1 << 20,
            ..settings()
        },
        5,
        13,
    );
    assert!(one == many);
    assert!(one == spilled);
    let _ = fs::remove_file(&path);
}

#[test]
fn pmtiles_erase_zoom() {
    let first = make_tiles(2000, 10, 8);
    // the zoom is written, erased, and written again with other tiles
    let ez = first[first.len() / 2].0;
    let mut again = make_tiles_from(400, super::first_tile_id(ez), ez, 21);
    again.retain(|t| t.0 == ez);
    assert!(again.len() > 100);
    let mut all = first.clone();
    all.extend(again.iter().cloned());
    let m = make_metadata(10);

    let expected_path = temp_path("erase-tippecanoe.pmtiles");
    tippecanoe_output(
        &expected_path,
        &all,
        &m,
        true,
        true,
        Some((first.len(), ez)),
    );
    let expected = fs::read(&expected_path).unwrap();

    let path = temp_path("erase.pmtiles");
    let sink = Box::new(PmtilesSink::create(path.to_str().unwrap(), &settings()).unwrap());
    feed(sink.as_ref(), &first, 6, 3);
    sink.erase_zoom(ez).unwrap();
    feed(sink.as_ref(), &again, 6, 4);
    sink.finish(&m).unwrap();
    assert!(fs::read(&path).unwrap() == expected);
    let _ = fs::remove_file(&expected_path);
    let _ = fs::remove_file(&path);
}

#[test]
fn pmtiles_errors() {
    let path = temp_path("errors.pmtiles");
    fs::write(&path, "x").unwrap();
    let p = path.to_str().unwrap();
    let e = PmtilesSink::create(p, &settings()).err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_EXISTS);
    assert!(e.message.starts_with("Tileset \""));
    let e = PmtilesSink::create(
        p,
        &SinkSettings {
            allow_existing: true,
            ..settings()
        },
    )
    .err()
    .unwrap();
    assert!(
        e.message
            .ends_with("--allow-existing is not supported for pmtiles")
    );

    let sink = Box::new(
        PmtilesSink::create(
            p,
            &SinkSettings {
                force: true,
                ..settings()
            },
        )
        .unwrap(),
    );
    sink.write_tile(1, 0, 0, b"a").unwrap();
    sink.write_tile(1, 0, 0, b"b").unwrap();
    assert!(sink.write_tile(1, 2, 0, b"c").is_err());
    let e = sink.finish(&make_metadata(1)).err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_SQLITE);
    let _ = fs::remove_file(&path);
}

/// Rows that make up an MBTiles database's content, in a fixed order.
fn mbtiles_rows(path: &Path) -> Vec<String> {
    let db = Connection::open(path).unwrap();
    let mut rows = Vec::new();
    for sql in [
        "SELECT type || '|' || name || '|' || tbl_name || '|' || coalesce(sql, '') FROM sqlite_master ORDER BY name",
        "SELECT name || '|' || value FROM metadata ORDER BY rowid",
        "SELECT zoom_level || '/' || tile_column || '/' || tile_row || '|' || tile_id FROM map ORDER BY zoom_level, tile_column, tile_row",
        "SELECT zoom_level || '|' || tile_id || '|' || hex(tile_data) FROM images ORDER BY zoom_level, tile_id",
        "SELECT tbl || '|' || coalesce(idx, '') || '|' || stat FROM sqlite_stat1 ORDER BY tbl, idx",
    ] {
        let mut stmt = db.prepare(sql).unwrap();
        let r: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.push(format!("-- {}", sql));
        rows.extend(r);
    }
    rows
}

#[test]
fn mbtiles_same_as_tippecanoe() {
    let first = make_tiles(3000, 12, 31);
    let ez = first[first.len() / 2].0;
    let mut again = make_tiles_from(300, super::first_tile_id(ez), ez, 32);
    again.retain(|t| t.0 == ez);
    let mut all = first.clone();
    all.extend(again.iter().cloned());
    let m = make_metadata(12);

    let expected_path = temp_path("same-tippecanoe.mbtiles");
    tippecanoe_output(
        &expected_path,
        &all,
        &m,
        false,
        true,
        Some((first.len(), ez)),
    );

    let path = temp_path("same.mbtiles");
    let sink = Box::new(MbtilesSink::create(path.to_str().unwrap(), &settings()).unwrap());
    feed(sink.as_ref(), &first, 8, 1);
    sink.erase_zoom(ez).unwrap();
    feed(sink.as_ref(), &again, 8, 2);
    sink.finish(&m).unwrap();

    assert_eq!(mbtiles_rows(&path), mbtiles_rows(&expected_path));
    let _ = fs::remove_file(&expected_path);
    let _ = fs::remove_file(&path);
}

#[test]
fn mbtiles_existing() {
    let path = temp_path("existing.mbtiles");
    let p = path.to_str().unwrap();
    let m = make_metadata(3);
    let sink = Box::new(MbtilesSink::create(p, &settings()).unwrap());
    sink.write_tile(3, 1, 1, b"x").unwrap();
    sink.finish(&m).unwrap();

    let e = MbtilesSink::create(p, &settings()).err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_EXISTS);
    assert!(e.message.ends_with("test: table metadata already exists"));

    // -F adds tiles; the old metadata stays
    let sink = Box::new(
        MbtilesSink::create(
            p,
            &SinkSettings {
                allow_existing: true,
                ..settings()
            },
        )
        .unwrap(),
    );
    sink.write_tile(3, 2, 1, b"y").unwrap();
    sink.finish(&m).unwrap();
    let db = Connection::open(&path).unwrap();
    let n: i64 = db
        .query_row("SELECT count(*) FROM tiles", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    drop(db);

    // a tile written twice fails as tippecanoe's insert does
    let sink = Box::new(
        MbtilesSink::create(
            p,
            &SinkSettings {
                force: true,
                ..settings()
            },
        )
        .unwrap(),
    );
    sink.write_tile(3, 2, 1, b"y").unwrap();
    let _ = sink.write_tile(3, 2, 1, b"z");
    let e = sink.finish(&m).err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_SQLITE);
    assert!(e.message.contains("UNIQUE constraint failed"));
    let _ = fs::remove_file(&path);
}

/// Every file under `dir` with its contents, in name order.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let name = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.push((name, fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn dir_same_as_tippecanoe() {
    let first = make_tiles(1500, 10, 41);
    let ez = first[first.len() / 2].0;
    let mut again = make_tiles_from(300, super::first_tile_id(ez), ez, 42);
    again.retain(|t| t.0 == ez);
    let m = make_metadata(10);

    let expected = temp_path("dir-tippecanoe");
    let e = expected.to_str().unwrap();
    let argv = vec!["test".to_string()];
    tippecanoe::dirtiles::check_dir(e, &argv, false, false);
    for (z, x, y, data) in &first {
        tippecanoe::dirtiles::dir_write_tile(e, *z as i32, *x as i32, *y as i32, data);
    }
    tippecanoe::dirtiles::dir_erase_zoom(e, ez as i32);
    for (z, x, y, data) in &again {
        tippecanoe::dirtiles::dir_write_tile(e, *z as i32, *x as i32, *y as i32, data);
    }
    tippecanoe::dirtiles::dir_write_metadata(e, &m);

    let path = temp_path("dir");
    let sink = Box::new(DirSink::create(path.to_str().unwrap(), &settings()).unwrap());
    feed(sink.as_ref(), &first, 8, 1);
    sink.erase_zoom(ez).unwrap();
    feed(sink.as_ref(), &again, 8, 2);
    sink.finish(&m).unwrap();

    assert!(tree(&path) == tree(&expected));
    let _ = fs::remove_dir_all(&expected);
    let _ = fs::remove_dir_all(&path);
}

#[test]
fn dir_existing() {
    let path = temp_path("dir-existing");
    let p = path.to_str().unwrap();
    let sink = Box::new(DirSink::create(p, &settings()).unwrap());
    sink.write_tile(2, 1, 3, b"t").unwrap();
    let e = sink.write_tile(2, 1, 3, b"t").err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_EXISTS);
    assert!(
        e.message
            .starts_with("Can't write tile to already existing")
    );
    sink.finish(&make_metadata(2)).unwrap();

    let e = DirSink::create(p, &settings()).err().unwrap();
    assert_eq!(e.exit_code, tippecanoe::errors::EXIT_EXISTS);
    assert!(e.message.ends_with("metadata.json: file exists"));

    fs::remove_file(path.join("metadata.json")).unwrap();
    let e = DirSink::create(p, &settings()).err().unwrap();
    assert_eq!(e.message, format!("{}/2/1/3.pbf: file exists", p));

    DirSink::create(
        p,
        &SinkSettings {
            force: true,
            ..settings()
        },
    )
    .unwrap();
    assert!(!path.join("2/1/3.pbf").exists());
    let _ = fs::remove_dir_all(&path);
}
