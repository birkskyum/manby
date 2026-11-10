//! Port of decode.cpp, the `tippecanoe-decode` program.

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char};
use std::fs::File;
use std::io::Read;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use memmap2::Mmap;
use rusqlite::{Connection, OpenFlags, ffi};

use crate::cstd::{atoi, perror_io, strerror};
use crate::dirtiles::{dirmeta2tmp, enumerate_dirtiles};
use crate::errors::{
    EXIT_ARGS, EXIT_CLOSE, EXIT_IMPOSSIBLE, EXIT_MEMORY, EXIT_MVT, EXIT_OPEN, EXIT_PROTOBUF,
    EXIT_SQLITE,
};
use crate::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use crate::globals::PROGRESS_TIME_ENABLED;
use crate::mvt::{MVT_LINESTRING, MVT_POINT, MVT_POLYGON, MvtTile};
use crate::pmtiles_file::{
    pmtiles_entries_tms, pmtiles_get_tile, pmtiles_has_suffix, pmtilesmeta2tmp,
};
use crate::projection::{PROJECTIONS, projection, set_projection_or_exit};
use crate::usage::{getopt_string, print_usage, strip_usage_headings};
use crate::write_json::{JsonWriter, layer_to_geojson};

static MINZOOM: AtomicI32 = AtomicI32::new(0);
static MAXZOOM: AtomicI32 = AtomicI32::new(32);
static FORCE: AtomicBool = AtomicBool::new(false);
static INCLUDE_ATTR: RwLock<BTreeSet<String>> = RwLock::new(BTreeSet::new());

// The C++ defines progress_time() here to always return false. main()
// clears PROGRESS_TIME_ENABLED, which makes tippecanoe_main::progress_time
// do the same.

fn do_stats(
    tile: &MvtTile,
    size: usize,
    compressed: bool,
    z: i32,
    x: u32,
    y: u32,
    state: &mut JsonWriter<'_>,
) {
    state.json_write_hash();

    state.json_write_string("zoom");
    state.json_write_signed(z as i64);

    state.json_write_string("x");
    state.json_write_unsigned(x as u64);

    state.json_write_string("y");
    state.json_write_unsigned(y as u64);

    state.json_write_string("bytes");
    state.json_write_unsigned(size as u64);

    state.json_write_string("compressed");
    state.json_write_bool(compressed);

    state.json_write_string("layers");
    state.json_write_hash();

    for layer in &tile.layers {
        state.json_write_string(&layer.name);

        let mut points: usize = 0;
        let mut lines: usize = 0;
        let mut polygons: usize = 0;
        for feature in &layer.features {
            if feature.type_ == MVT_POINT {
                points += 1;
            } else if feature.type_ == MVT_LINESTRING {
                lines += 1;
            } else if feature.type_ == MVT_POLYGON {
                polygons += 1;
            }
        }

        state.json_write_hash();

        state.json_write_string("points");
        state.json_write_unsigned(points as u64);

        state.json_write_string("lines");
        state.json_write_unsigned(lines as u64);

        state.json_write_string("polygons");
        state.json_write_unsigned(polygons as u64);

        state.json_write_string("extent");
        state.json_write_signed(layer.extent);

        state.json_end_hash();
    }

    state.json_end_hash();
    state.json_end_hash();

    state.json_write_newline();
}

#[allow(clippy::too_many_arguments)]
fn handle(
    message: &[u8],
    z: i32,
    x: u32,
    y: u32,
    to_decode: &BTreeSet<String>,
    pipeline: bool,
    stats: bool,
    state: &mut JsonWriter<'_>,
    coordinate_mode: i32,
) {
    let mut tile = MvtTile::default();
    let mut was_compressed = false;

    match tile.try_decode(message, &mut was_compressed) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("Couldn't parse tile {}/{}/{}", z, x, y);
            std::process::exit(EXIT_MVT);
        }
        Err(_) => {
            eprintln!("PBF decoding error in tile {}/{}/{}", z, x, y);
            std::process::exit(EXIT_PROTOBUF);
        }
    }

    if stats {
        do_stats(&tile, message.len(), was_compressed, z, x, y, state);
        return;
    }

    if !pipeline {
        state.json_write_hash();

        state.json_write_string("type");
        state.json_write_string("FeatureCollection");

        {
            state.json_write_string("properties");
            state.json_write_hash();

            state.json_write_string("zoom");
            state.json_write_signed(z as i64);

            state.json_write_string("x");
            state.json_write_signed(x as i64);

            state.json_write_string("y");
            state.json_write_signed(y as i64);

            if !was_compressed {
                state.json_write_string("compressed");
                state.json_write_bool(false);
            }

            state.json_end_hash();

            if !std::ptr::eq(projection(), &PROJECTIONS[0]) {
                state.json_write_string("crs");
                state.json_write_hash();

                state.json_write_string("type");
                state.json_write_string("name");

                state.json_write_string("properties");
                state.json_write_hash();

                state.json_write_string("name");
                state.json_write_string(projection().alias);

                state.json_end_hash();
                state.json_end_hash();
            }
        }

        state.json_write_string("features");
        state.json_write_array();
        state.json_write_newline();
    }

    let include_attr = INCLUDE_ATTR.read().unwrap();
    let force = FORCE.load(Ordering::Relaxed);

    let mut first_layer = true;
    for layer in &tile.layers {
        if layer.extent <= 0 {
            eprintln!("Impossible layer extent {} in mbtiles", layer.extent);
            std::process::exit(EXIT_IMPOSSIBLE);
        }

        if !to_decode.is_empty() && !to_decode.contains(&layer.name) {
            continue;
        }

        if !pipeline {
            if !first_layer {
                state.json_comma_newline();
            }

            state.json_write_hash();

            state.json_write_string("type");
            state.json_write_string("FeatureCollection");

            state.json_write_string("properties");
            state.json_write_hash();

            state.json_write_string("layer");
            state.json_write_string(&layer.name);

            state.json_write_string("version");
            state.json_write_signed(layer.version as i64);

            state.json_write_string("extent");
            state.json_write_signed(layer.extent);

            state.json_end_hash();

            state.json_write_string("features");
            state.json_write_array();

            state.json_write_newline();
            first_layer = false;
        }

        // X and Y are unsigned, so no need to check <0
        if x as u64 > 1u64.wrapping_shl(z as u32) || y as u64 > 1u64.wrapping_shl(z as u32) {
            eprintln!("Impossible tile {}/{}/{}", z, x, y);
            std::process::exit(EXIT_IMPOSSIBLE);
        }

        let mut scale: f64 = 0.0;
        if coordinate_mode == 1 {
            // fraction
            scale = layer.extent as f64;
        } else if coordinate_mode == 2 {
            // integer
            scale = 1.0;
        }
        layer_to_geojson(
            layer,
            z as u32,
            x,
            y,
            !pipeline,
            pipeline,
            pipeline,
            false,
            0,
            0,
            0,
            !force,
            state,
            scale,
            &include_attr,
        );

        if !pipeline {
            state.json_end_array();
            state.json_end_hash();
            state.json_write_newline();
        }
    }

    if !pipeline {
        state.json_end_array();
        state.json_end_hash();
        state.json_write_newline();
    }
}

#[allow(clippy::too_many_arguments)]
fn decode(
    fname: &str,
    mut z: i32,
    mut x: u32,
    mut y: u32,
    to_decode: &BTreeSet<String>,
    pipeline: bool,
    stats: bool,
    exclude_meta: &BTreeSet<String>,
    coordinate_mode: i32,
) {
    let db: Connection;
    let mut isdir = false;
    let oz = z;
    let (ox, oy) = (x, y);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut state = JsonWriter::new_file(&mut stdout);

    let minzoom = MINZOOM.load(Ordering::Relaxed);
    let maxzoom = MAXZOOM.load(Ordering::Relaxed);

    match File::open(fname) {
        Ok(fd) => {
            match fd.metadata() {
                Ok(st) => {
                    if let Ok(map) = mmap_file(&fd, st.len())
                        && !c_streq(&map, b"SQLite format 3")
                        && !map.starts_with(b"PMTiles")
                    {
                        if z >= 0 {
                            if st.len() > 250 * 1024 * 1024 {
                                eprintln!(
                                    "{}: unrealistically large single-tile size {}",
                                    fname,
                                    st.len()
                                );
                                std::process::exit(EXIT_MEMORY);
                            } else {
                                handle(
                                    &map,
                                    z,
                                    x,
                                    y,
                                    to_decode,
                                    pipeline,
                                    stats,
                                    &mut state,
                                    coordinate_mode,
                                );
                                return;
                            }
                        } else {
                            eprintln!("Must specify zoom/x/y to decode a single pbf file");
                            std::process::exit(EXIT_ARGS);
                        }
                    }
                }
                Err(e) => perror_io("fstat", &e),
            }
            // PORT: the C++ checks the result of close(), which can't fail for a
            // descriptor opened read-only; Rust closes the file when it is dropped.
        }
        Err(e) => perror_io(fname, &e),
    }

    let mut tiles = Vec::new();

    let mut pmtiles_map: Option<Mmap> = None;
    let mut entries = Vec::new();
    let mut is_pmtiles = false;

    if std::fs::metadata(fname).is_ok_and(|st| crate::platform::mode_is_dir(&st)) {
        isdir = true;

        db = dirmeta2tmp(fname);
        tiles = enumerate_dirtiles(fname, minzoom, maxzoom);
    } else if pmtiles_has_suffix(fname) {
        let map = match File::open(fname) {
            Ok(pmtiles_fd) => {
                let len = pmtiles_fd.metadata().map(|st| st.len()).unwrap_or(0);
                mmap_file(&pmtiles_fd, len)
            }
            // mmap() of the -1 that open() returned
            Err(_) => Err(libc::EBADF),
        };
        let map = match map {
            Ok(map) => map,
            Err(errno) => {
                eprintln!("mmap in decode: {}", strerror(errno));
                std::process::exit(EXIT_MEMORY);
            }
        };
        db = pmtilesmeta2tmp(fname, &map);
        entries = pmtiles_entries_tms(&map, minzoom, maxzoom);
        pmtiles_map = Some(map);
        is_pmtiles = true;
    } else {
        db = match sqlite3_open(fname) {
            Ok(db) => db,
            Err(msg) => {
                eprintln!("{}: {}", fname, msg);
                std::process::exit(EXIT_OPEN);
            }
        };

        if let Err(e) = db.execute_batch("PRAGMA integrity_check;") {
            eprintln!("{}: integrity_check: {}", fname, e);
            std::process::exit(EXIT_SQLITE);
        }
    }

    if z < 0 {
        let mut within = 0;

        if !pipeline && !stats {
            state.json_write_hash();

            state.json_write_string("type");
            state.json_write_string("FeatureCollection");

            state.json_write_string("properties");
            state.json_write_hash();
            state.json_write_newline();

            let mut stmt2 =
                match Stmt::prepare(&db, c"SELECT name, value from metadata order by name;") {
                    Ok(stmt2) => stmt2,
                    Err(msg) => {
                        eprintln!("{}: select failed: {}", fname, msg);
                        std::process::exit(EXIT_SQLITE);
                    }
                };

            while stmt2.step() == ffi::SQLITE_ROW {
                let (name, value) = match (stmt2.column_text(0), stmt2.column_text(1)) {
                    (Some(name), Some(value)) => (
                        String::from_utf8_lossy(name).into_owned(),
                        String::from_utf8_lossy(value).into_owned(),
                    ),
                    _ => {
                        eprintln!("Corrupt mbtiles file: null metadata");
                        std::process::exit(EXIT_SQLITE);
                    }
                };

                if !exclude_meta.contains(&name) {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;

                    state.json_write_string(&name);
                    state.json_write_string(&value);
                }
            }

            state.json_write_newline();
            state.wantnl = false; // XXX
        }

        if stats {
            state.json_write_array();
            state.json_write_newline();
        }

        if !pipeline && !stats {
            state.json_end_hash();

            state.json_write_string("features");
            state.json_write_array();
            state.json_write_newline();
        }

        if isdir {
            within = 0;
            for tile in &tiles {
                if !pipeline && !stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }
                if stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }

                let fn_ = format!("{}/{}", fname, tile.path());
                let mut f = match File::open(&fn_) {
                    Ok(f) => f,
                    Err(e) => {
                        perror_io(&fn_, &e);
                        std::process::exit(EXIT_OPEN);
                    }
                };

                let mut s = Vec::new();
                let mut buf = [0u8; 2000];
                loop {
                    match f.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => s.extend_from_slice(&buf[..n]),
                    }
                }
                drop(f);

                handle(
                    &s,
                    tile.z as i32,
                    tile.x as u32,
                    tile.y as u32,
                    to_decode,
                    pipeline,
                    stats,
                    &mut state,
                    coordinate_mode,
                );
            }
        } else if is_pmtiles {
            within = 0;
            let map = pmtiles_map.as_ref().unwrap();

            for entry in &entries {
                if !pipeline && !stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }
                if stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }

                let s = &map[entry.offset as usize..entry.offset as usize + entry.length as usize];
                handle(
                    s,
                    entry.z as i32,
                    entry.x,
                    entry.y,
                    to_decode,
                    pipeline,
                    stats,
                    &mut state,
                    coordinate_mode,
                );
            }
        } else {
            let mut stmt = match Stmt::prepare(
                &db,
                c"SELECT tile_data, zoom_level, tile_column, tile_row from tiles where zoom_level between ? and ? order by zoom_level, tile_column, tile_row;",
            ) {
                Ok(stmt) => stmt,
                Err(msg) => {
                    eprintln!("{}: select failed: {}", fname, msg);
                    std::process::exit(EXIT_SQLITE);
                }
            };

            stmt.bind_int(1, minzoom);
            stmt.bind_int(2, maxzoom);

            within = 0;
            while stmt.step() == ffi::SQLITE_ROW {
                if !pipeline && !stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }
                if stats {
                    if within != 0 {
                        state.json_comma_newline();
                    }
                    within = 1;
                }

                let tz = stmt.column_int(1);
                let tx = stmt.column_int(2);
                let mut ty = stmt.column_int(3);

                if !(0..32).contains(&tz) {
                    eprintln!("Impossible zoom level {} in mbtiles", tz);
                    std::process::exit(EXIT_IMPOSSIBLE);
                }

                ty = ((1i64 << tz) - 1 - ty as i64) as i32;
                let s = match stmt.column_blob(0) {
                    Some(s) => s,
                    None => {
                        eprintln!("Corrupt mbtiles file: null entry in tiles table");
                        std::process::exit(EXIT_SQLITE);
                    }
                };

                handle(
                    s,
                    tz,
                    tx as u32,
                    ty as u32,
                    to_decode,
                    pipeline,
                    stats,
                    &mut state,
                    coordinate_mode,
                );
            }
        }

        if !pipeline && !stats {
            state.json_end_array();
            state.json_end_hash();
            state.json_write_newline();
        }
        if stats {
            state.json_end_array();
            state.json_write_newline();
        }
        if pipeline {
            state.json_write_newline();
        }
    } else {
        let mut handled = 0;
        while z >= 0 && handled == 0 {
            if is_pmtiles {
                let map = pmtiles_map.as_ref().unwrap();
                let (tile_offset, tile_length) = pmtiles_get_tile(map, z, x as i32, y as i32);
                if tile_length > 0 {
                    if z != oz {
                        eprintln!(
                            "{}: Warning: using tile {}/{}/{} instead of {}/{}/{}",
                            fname, z, x, y, oz, ox, oy
                        );
                    }
                    let s = &map[tile_offset as usize..tile_offset as usize + tile_length as usize];
                    handle(
                        s,
                        z,
                        x,
                        y,
                        to_decode,
                        pipeline,
                        stats,
                        &mut state,
                        coordinate_mode,
                    );
                    handled = 1;
                }
            } else {
                let mut stmt = match Stmt::prepare(
                    &db,
                    c"SELECT tile_data from tiles where zoom_level = ? and tile_column = ? and tile_row = ?;",
                ) {
                    Ok(stmt) => stmt,
                    Err(msg) => {
                        eprintln!("{}: select failed: {}", fname, msg);
                        std::process::exit(EXIT_SQLITE);
                    }
                };

                stmt.bind_int(1, z);
                stmt.bind_int(2, x as i32);
                stmt.bind_int(
                    3,
                    1i64.wrapping_shl(z as u32)
                        .wrapping_sub(1)
                        .wrapping_sub(y as i64) as i32,
                );

                while stmt.step() == ffi::SQLITE_ROW {
                    let s = match stmt.column_blob(0) {
                        Some(s) => s,
                        None => {
                            eprintln!("Corrupt mbtiles file: null entry in tiles table");
                            std::process::exit(EXIT_SQLITE);
                        }
                    };

                    if z != oz {
                        eprintln!(
                            "{}: Warning: using tile {}/{}/{} instead of {}/{}/{}",
                            fname, z, x, y, oz, ox, oy
                        );
                    }

                    handle(
                        s,
                        z,
                        x,
                        y,
                        to_decode,
                        pipeline,
                        stats,
                        &mut state,
                        coordinate_mode,
                    );
                    handled = 1;
                }
            }

            z -= 1;
            x /= 2;
            y /= 2;
        }
    }

    if let Err((_, e)) = db.close() {
        eprintln!("{}: could not close database: {}", fname, e);
        std::process::exit(EXIT_CLOSE);
    }
}

const fn opt(
    name: &'static str,
    has_arg: i32,
    flag: Option<&'static AtomicI32>,
    val: u8,
) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag,
        val: val as i32,
    }
}

static LONG_OPTIONS: [LongOption; 15] = [
    opt("Tiles to decode", 0, None, 0),
    opt("minimum-zoom", REQUIRED_ARGUMENT, None, b'Z'),
    opt("maximum-zoom", REQUIRED_ARGUMENT, None, b'z'),
    opt("layer", REQUIRED_ARGUMENT, None, b'l'),
    //
    opt("Output format", 0, None, 0),
    opt("projection", REQUIRED_ARGUMENT, None, b's'),
    opt("fractional-coordinates", NO_ARGUMENT, None, b'F'),
    opt("integer-coordinates", NO_ARGUMENT, None, b'I'),
    opt("tag-layer-and-zoom", NO_ARGUMENT, None, b'c'),
    opt("stats", NO_ARGUMENT, None, b'S'),
    //
    opt("Filtering the output", 0, None, 0),
    opt("include", REQUIRED_ARGUMENT, None, b'y'),
    opt("exclude-metadata-row", REQUIRED_ARGUMENT, None, b'x'),
    //
    opt("Ignoring errors in the input", 0, None, 0),
    opt("force", NO_ARGUMENT, None, b'f'),
];

fn usage(argv: &[String]) -> ! {
    let forms = ["[options] tileset", "[options] tileset zoom x y"];

    print_usage(&mut std::io::stderr(), &argv[0], &forms, &LONG_OPTIONS, &[]);
    eprintln!("\nThe tileset can be an .mbtiles or .pmtiles file or a directory of tiles,");
    eprintln!("or, if zoom/x/y is specified, a single .pbf tile.");
    std::process::exit(EXIT_ARGS);
}

pub fn main() {
    PROGRESS_TIME_ENABLED.store(false, Ordering::Relaxed);
    restore_sigpipe();

    let mut argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argc = argv.len();
    let mut to_decode = BTreeSet::new();
    let mut pipeline = false;
    let mut stats = false;
    let mut exclude_meta = BTreeSet::new();
    let mut coordinate_mode = 0;

    // the options above, with the usage message headings removed
    let real_long_options = strip_usage_headings(&LONG_OPTIONS);
    let getopt_str = getopt_string(&real_long_options);

    let mut g = Getopt::new();
    loop {
        let i = g.getopt_long(&mut argv, &getopt_str, &real_long_options, None);
        if i == -1 {
            break;
        }
        let optarg = g.optarg.clone().unwrap_or_default();

        match u8::try_from(i) {
            Ok(0) => {}

            Ok(b's') => set_projection_or_exit(&optarg),

            Ok(b'F') => coordinate_mode = 1,

            Ok(b'I') => coordinate_mode = 2,

            Ok(b'z') => MAXZOOM.store(atoi(&optarg), Ordering::Relaxed),

            Ok(b'Z') => MINZOOM.store(atoi(&optarg), Ordering::Relaxed),

            Ok(b'l') => {
                to_decode.insert(optarg);
            }

            Ok(b'c') => pipeline = true,

            Ok(b'S') => stats = true,

            Ok(b'f') => FORCE.store(true, Ordering::Relaxed),

            Ok(b'x') => {
                exclude_meta.insert(optarg);
            }

            Ok(b'y') => {
                INCLUDE_ATTR.write().unwrap().insert(optarg);
            }

            _ => usage(&argv),
        }
    }

    let optind = g.optind;
    if argc == optind + 4 {
        decode(
            &argv[optind],
            atoi(&argv[optind + 1]),
            atoi(&argv[optind + 2]) as u32,
            atoi(&argv[optind + 3]) as u32,
            &to_decode,
            pipeline,
            stats,
            &exclude_meta,
            coordinate_mode,
        );
    } else if argc == optind + 1 {
        decode(
            &argv[optind],
            -1,
            -1i32 as u32,
            -1i32 as u32,
            &to_decode,
            pipeline,
            stats,
            &exclude_meta,
            coordinate_mode,
        );
    } else {
        usage(&argv);
    }
}

// Helpers for C library and SQLite C API behaviour that the C++ relies on

/// `mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0)`. Returns the errno on failure.
fn mmap_file(f: &File, size: u64) -> Result<Mmap, i32> {
    if size == 0 {
        // mmap() refuses an empty mapping
        return Err(libc::EINVAL);
    }
    // SAFETY: the map is only read, and the file is not modified while it is mapped.
    unsafe { Mmap::map(f) }.map_err(|e| e.raw_os_error().unwrap_or(libc::EINVAL))
}

/// `strcmp(map, s) == 0` on a memory map, which reads as zeros past the end of the file.
fn c_streq(map: &[u8], s: &[u8]) -> bool {
    for i in 0..=s.len() {
        let a = map.get(i).copied().unwrap_or(0);
        let b = s.get(i).copied().unwrap_or(0);
        if a != b {
            return false;
        }
        if a == 0 {
            return true;
        }
    }
    true
}

/// Restores the default `SIGPIPE` action that a C program starts with, so that
/// writing to a closed pipe ends the program quietly instead of failing the write.
fn restore_sigpipe() {
    crate::platform::sigpipe_default();
}

/// `sqlite3_open(fname, &db)`. On failure, returns `sqlite3_errmsg(db)`, without
/// the file name that rusqlite adds to the message when the file can't be opened.
fn sqlite3_open(fname: &str) -> Result<Connection, String> {
    Connection::open_with_flags(
        fname,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| {
        let msg = e.to_string();
        match e {
            rusqlite::Error::SqliteFailure(err, Some(_))
                if err.code == rusqlite::ErrorCode::CannotOpen =>
            {
                match msg.strip_suffix(&format!(": {}", fname)) {
                    Some(m) => m.to_string(),
                    None => msg,
                }
            }
            _ => msg,
        }
    })
}

/// A prepared statement used through the SQLite C API, as the C++ does, so that
/// the column conversions are SQLite's own.
struct Stmt<'a> {
    stmt: *mut ffi::sqlite3_stmt,
    _db: std::marker::PhantomData<&'a Connection>,
}

impl<'a> Stmt<'a> {
    /// `sqlite3_prepare_v2`. On failure, returns `sqlite3_errmsg(db)`.
    fn prepare(db: &'a Connection, sql: &CStr) -> Result<Stmt<'a>, String> {
        let mut stmt = std::ptr::null_mut();
        // SAFETY: the handle stays valid while `db` is borrowed, and `sql` is NUL-terminated.
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                db.handle(),
                sql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            // SAFETY: sqlite3_errmsg returns a NUL-terminated string owned by the connection.
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db.handle())) };
            return Err(msg.to_string_lossy().into_owned());
        }
        Ok(Stmt {
            stmt,
            _db: std::marker::PhantomData,
        })
    }

    fn bind_int(&mut self, i: i32, v: i32) {
        // SAFETY: the statement is live until drop.
        unsafe {
            ffi::sqlite3_bind_int(self.stmt, i, v);
        }
    }

    fn step(&mut self) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_step(self.stmt) }
    }

    fn column_int(&self, i: i32) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_column_int(self.stmt, i) }
    }

    /// `sqlite3_column_text`, up to its NUL terminator, or `None` for NULL.
    fn column_text(&self, i: i32) -> Option<&[u8]> {
        // SAFETY: the text stays valid until the next step, which needs `&mut self`.
        unsafe {
            let p = ffi::sqlite3_column_text(self.stmt, i);
            if p.is_null() {
                None
            } else {
                Some(CStr::from_ptr(p as *const c_char).to_bytes())
            }
        }
    }

    /// `sqlite3_column_bytes` and `sqlite3_column_blob`, or `None` if the blob pointer is NULL.
    fn column_blob(&self, i: i32) -> Option<&[u8]> {
        // SAFETY: the blob stays valid until the next step, which needs `&mut self`.
        unsafe {
            let len = ffi::sqlite3_column_bytes(self.stmt, i);
            let p = ffi::sqlite3_column_blob(self.stmt, i);
            if p.is_null() {
                None
            } else {
                Some(std::slice::from_raw_parts(p as *const u8, len as usize))
            }
        }
    }
}

impl Drop for Stmt<'_> {
    fn drop(&mut self) {
        // SAFETY: the statement was prepared and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.stmt);
        }
    }
}
