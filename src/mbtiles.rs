//! Port of mbtiles.hpp and mbtiles.cpp: the MBTiles output, metadata and tilestats.
//!
//! The C++ passes `sqlite3 *` around and may pass NULL when writing a directory.
//! Here that is `Option<&Connection>`, and code that shares one database between
//! threads keeps it in a `Mutex`.
//!
//! SQL text is built with SQLite's own `sqlite3_mprintf` and run with
//! `sqlite3_exec`, as in the C++, so that `%Q` quoting, `%f` formatting and the
//! error messages come out the same. Prepared statements go through rusqlite.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::process::exit;

use rusqlite::Connection;
use rusqlite::ffi;

use crate::errors::{EXIT_CLOSE, EXIT_EXISTS, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_SQLITE};
use crate::globals::{
    MAX_TILESTATS_ATTRIBUTES, MAX_TILESTATS_SAMPLE_VALUES, MAX_TILESTATS_VALUES, get,
};
use crate::milo::dtoa_milo;
use crate::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_NULL, MVT_STRING};
use crate::serial::SerialVal;
use crate::text::{fnv1a, truncate16};
use crate::tile::Strategy;
use crate::version::VERSION;
use crate::write_json::JsonWriter;

#[derive(Clone, Debug)]
pub struct Tilestat {
    /// sorted
    pub sample_values: Vec<SerialVal>,
    pub min: f64,
    pub max: f64,
    pub type_: i32,
}

impl Default for Tilestat {
    fn default() -> Self {
        Tilestat {
            sample_values: Vec::new(),
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            type_: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LayermapEntry {
    pub id: usize,
    pub tilestats: BTreeMap<String, Tilestat>,
    pub minzoom: i32,
    pub maxzoom: i32,
    pub description: String,

    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
    /// keep for tilestats, even if no features directly here
    pub retain: usize,
}

impl LayermapEntry {
    pub fn new(id: usize) -> LayermapEntry {
        LayermapEntry {
            id,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub name: String,
    pub description: String,
    pub version: i32,
    pub type_: String,
    pub format: String,

    pub minzoom: i32,
    pub maxzoom: i32,

    pub minlat: f64,
    pub minlon: f64,
    pub maxlat: f64,
    pub maxlon: f64,
    /// antimeridian-aware
    pub minlat2: f64,
    pub minlon2: f64,
    pub maxlat2: f64,
    pub maxlon2: f64,

    pub center_lon: f64,
    pub center_lat: f64,
    pub center_z: i32,

    /// not written if empty
    pub attribution: String,

    pub generator: String,
    pub generator_options: String,

    /// not written if empty
    pub strategies_json: String,

    pub vector_layers_json: String,
    pub tilestats_json: String,
    pub decisions_json: String,
}

/// `s.c_str()`: the text up to the first NUL, as a C string.
pub fn c_str(s: &str) -> CString {
    let b = s.as_bytes();
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    CString::new(&b[..end]).unwrap()
}

/// Copies and frees a string returned by `sqlite3_mprintf`. A NULL (out of
/// memory) becomes an empty statement, which `sqlite3_exec` treats as a no-op.
///
/// # Safety
///
/// `p` must be NULL or a string from `sqlite3_mprintf` that nothing else frees.
pub unsafe fn sqlite3_take(p: *mut c_char) -> CString {
    if p.is_null() {
        return CString::default();
    }
    // SAFETY: the caller passes a NUL-terminated string from sqlite3_mprintf,
    // and it is freed exactly once, after it has been copied.
    unsafe {
        let s = CStr::from_ptr(p).to_owned();
        ffi::sqlite3_free(p as *mut c_void);
        s
    }
}

/// `sqlite3_mprintf(fmt, ...)`, returning an owned copy of the result. Each
/// argument must have the C type its conversion expects: `*const c_char` for
/// `%Q`, `c_int` for `%d` and `c_double` for `%f`.
macro_rules! sqlite3_mprintf {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        // SAFETY: the format is a C string literal, the caller passes arguments
        // of the C types its conversions expect, which outlive the call, and
        // the result is taken over by sqlite3_take.
        unsafe {
            $crate::mbtiles::sqlite3_take(rusqlite::ffi::sqlite3_mprintf(
                ($fmt as &std::ffi::CStr).as_ptr() $(, $arg)*
            ))
        }
    };
}
pub(crate) use sqlite3_mprintf;

/// `sqlite3_open`. On failure, the error is `sqlite3_errmsg` of the handle it
/// leaves behind.
pub fn sqlite3_open(filename: &str) -> Result<Connection, String> {
    let name = c_str(filename);
    let mut db: *mut ffi::sqlite3 = std::ptr::null_mut();
    // SAFETY: `name` is a C string and `db` receives the new handle.
    let rc = unsafe { ffi::sqlite3_open(name.as_ptr(), &mut db) };
    if rc != ffi::SQLITE_OK {
        // SAFETY: sqlite3_errmsg accepts the handle sqlite3_open left, even NULL.
        let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)) }
            .to_string_lossy()
            .into_owned();
        return Err(msg);
    }
    // SAFETY: `db` is a handle that sqlite3_open just opened, and nothing else owns it.
    unsafe { Connection::from_handle_owned(db) }.map_err(|e| e.to_string())
}

/// `sqlite3_exec` with no callback. On failure, the error is the message
/// `sqlite3_exec` returns through its `errmsg` argument.
pub fn sqlite3_exec(db: &Connection, sql: &CStr) -> Result<(), String> {
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: the handle stays open while `db` is borrowed, `sql` is a C string,
    // and the error message, if any, is freed after it has been copied.
    unsafe {
        let rc = ffi::sqlite3_exec(
            db.handle(),
            sql.as_ptr(),
            None,
            std::ptr::null_mut(),
            &mut err,
        );
        if rc != ffi::SQLITE_OK {
            let msg = if err.is_null() {
                "(null)".to_string()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            ffi::sqlite3_free(err as *mut c_void);
            return Err(msg);
        }
    }
    Ok(())
}

/// `sqlite3_errmsg`.
pub fn sqlite3_errmsg(db: &Connection) -> String {
    // SAFETY: the handle stays open while `db` is borrowed, and sqlite3_errmsg
    // returns a NUL-terminated string that stays valid until the next call.
    unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db.handle())) }
        .to_string_lossy()
        .into_owned()
}

pub fn mbtiles_open(dbname: &str, argv: &[String], forcetable: i32) -> Connection {
    let outdb = match sqlite3_open(dbname) {
        Ok(db) => db,
        Err(msg) => {
            eprintln!("{}: {}: {}", argv[0], dbname, msg);
            exit(EXIT_OPEN);
        }
    };

    if let Err(err) = sqlite3_exec(&outdb, c"PRAGMA synchronous=0") {
        eprintln!("{}: async: {}", argv[0], err);
        exit(EXIT_SQLITE);
    }
    if let Err(err) = sqlite3_exec(&outdb, c"PRAGMA locking_mode=EXCLUSIVE") {
        eprintln!("{}: async: {}", argv[0], err);
        exit(EXIT_SQLITE);
    }
    if let Err(err) = sqlite3_exec(&outdb, c"PRAGMA journal_mode=DELETE") {
        eprintln!("{}: async: {}", argv[0], err);
        exit(EXIT_SQLITE);
    }
    if let Err(err) = sqlite3_exec(&outdb, c"CREATE TABLE metadata (name text, value text);") {
        eprintln!(
            "{}: Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.",
            argv[0], dbname
        );
        eprintln!("{}: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }
    if let Err(err) = sqlite3_exec(&outdb, c"create unique index name on metadata (name);") {
        eprintln!("{}: index metadata: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }

    // "map" maps z/x/y coordinates to a content hash
    if let Err(err) = sqlite3_exec(
        &outdb,
        c"CREATE TABLE map (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_id TEXT);",
    ) {
        eprintln!("{}: create map table: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }
    if let Err(err) = sqlite3_exec(
        &outdb,
        c"CREATE UNIQUE INDEX map_index ON map (zoom_level, tile_column, tile_row);",
    ) {
        eprintln!("{}: create map index: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }

    // "images" maps a content hash to tile contents, per zoom level
    if let Err(err) = sqlite3_exec(
        &outdb,
        c"CREATE TABLE images (zoom_level integer, tile_data blob, tile_id text);",
    ) {
        eprintln!("{}: create images table: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }
    if let Err(err) = sqlite3_exec(
        &outdb,
        c"CREATE UNIQUE INDEX images_id ON images (zoom_level, tile_id);",
    ) {
        eprintln!("{}: create images index: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }

    // "tiles" is a view that retrieves content from "images"
    // via the content hash looked up from "map".
    if let Err(err) = sqlite3_exec(
        &outdb,
        c"CREATE VIEW tiles AS SELECT map.zoom_level AS zoom_level, map.tile_column AS tile_column, map.tile_row AS tile_row, images.tile_data AS tile_data FROM map JOIN images ON images.tile_id = map.tile_id and images.zoom_level = map.zoom_level;",
    ) {
        eprintln!("{}: create tiles view: {}", argv[0], err);
        if forcetable == 0 {
            exit(EXIT_EXISTS);
        }
    }

    outdb
}

pub fn mbtiles_write_tile(outdb: &Connection, z: i32, tx: i32, ty: i32, data: &[u8]) {
    let hash = fnv1a(data).to_string();

    // following https://github.com/mapbox/node-mbtiles/blob/master/lib/mbtiles.js

    let images = "replace into images (zoom_level, tile_id, tile_data) values (?, ?, ?)";
    let mut stmt = match outdb.prepare(images) {
        Ok(stmt) => stmt,
        Err(_) => {
            eprintln!("sqlite3 images prep failed");
            exit(EXIT_SQLITE);
        }
    };

    let _ = stmt.raw_bind_parameter(1, z);
    let _ = stmt.raw_bind_parameter(2, hash.as_str());
    let _ = stmt.raw_bind_parameter(3, data);

    if stmt.raw_execute().is_err() {
        eprintln!("sqlite3 images insert failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }
    if stmt.finalize().is_err() {
        eprintln!("sqlite3 images finalize failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }

    let map = "insert into map (zoom_level, tile_column, tile_row, tile_id) values (?, ?, ?, ?)";
    let mut stmt = match outdb.prepare(map) {
        Ok(stmt) => stmt,
        Err(_) => {
            eprintln!("sqlite3 map prep failed");
            exit(EXIT_SQLITE);
        }
    };

    let _ = stmt.raw_bind_parameter(1, z);
    let _ = stmt.raw_bind_parameter(2, tx);
    // PORT: `(1 << z) - 1 - ty` in int arithmetic; wrapping stands in for the
    // C++ overflow at z = 31 or beyond.
    let _ = stmt.raw_bind_parameter(
        3,
        1i32.wrapping_shl(z as u32).wrapping_sub(1).wrapping_sub(ty),
    );
    let _ = stmt.raw_bind_parameter(4, hash.as_str());

    if stmt.raw_execute().is_err() {
        eprintln!("sqlite3 map insert failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }
    if stmt.finalize().is_err() {
        eprintln!("sqlite3 finalize failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }
}

pub fn mbtiles_erase_zoom(outdb: &Connection, z: i32) {
    let query = "delete from map where zoom_level = ?";
    let mut stmt = match outdb.prepare(query) {
        Ok(stmt) => stmt,
        Err(_) => {
            eprintln!("sqlite3 delete map prep failed");
            exit(EXIT_SQLITE);
        }
    };

    let _ = stmt.raw_bind_parameter(1, z);
    if stmt.raw_execute().is_err() {
        eprintln!("sqlite3 delete map failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }
    if stmt.finalize().is_err() {
        eprintln!(
            "sqlite3 delete map finalize failed: {}",
            sqlite3_errmsg(outdb)
        );
        exit(EXIT_SQLITE);
    }

    let query = "delete from images where zoom_level = ?";
    let mut stmt = match outdb.prepare(query) {
        Ok(stmt) => stmt,
        Err(_) => {
            eprintln!("sqlite3 delete images prep failed");
            exit(EXIT_SQLITE);
        }
    };

    let _ = stmt.raw_bind_parameter(1, z);
    if stmt.raw_execute().is_err() {
        eprintln!("sqlite3 delete images failed: {}", sqlite3_errmsg(outdb));
        exit(EXIT_SQLITE);
    }
    if stmt.finalize().is_err() {
        eprintln!(
            "sqlite3 delete images finalize failed: {}",
            sqlite3_errmsg(outdb)
        );
        exit(EXIT_SQLITE);
    }
}

// serial_val::operator< and serial_val::operator!= are defined here in the C++.
// In Rust they are the `Ord` and `PartialEq` impls in serial.rs.

#[allow(clippy::explicit_counter_loop)]
fn tilestats(
    layermap1: &BTreeMap<String, LayermapEntry>,
    elements: usize,
    state: &mut JsonWriter<'_>,
) {
    // Consolidate layers/attributes whose names are truncated
    let lmv = std::slice::from_ref(layermap1);
    let layermap = merge_layermaps_trunc(lmv, true);

    state.nospace = true;
    state.json_write_hash();

    state.nospace = true;
    state.json_write_string("layerCount");
    state.nospace = true;
    state.json_write_unsigned(layermap.len() as u64);

    state.nospace = true;
    state.json_write_string("layers");
    state.nospace = true;
    state.json_write_array();

    for layer in &layermap {
        state.nospace = true;
        state.json_write_hash();

        state.nospace = true;
        state.json_write_string("layer");
        state.nospace = true;
        state.json_write_string(layer.0);

        state.nospace = true;
        state.json_write_string("count");
        state.nospace = true;
        state.json_write_unsigned((layer.1.points + layer.1.lines + layer.1.polygons) as u64);

        let mut geomtype = "Polygon";
        if layer.1.points >= layer.1.lines && layer.1.points >= layer.1.polygons {
            geomtype = "Point";
        } else if layer.1.lines >= layer.1.polygons && layer.1.lines >= layer.1.points {
            geomtype = "LineString";
        }

        state.nospace = true;
        state.json_write_string("geometry");
        state.nospace = true;
        state.json_write_string(geomtype);

        let mut attrib_count = layer.1.tilestats.len();
        if attrib_count > get(&MAX_TILESTATS_ATTRIBUTES) {
            attrib_count = get(&MAX_TILESTATS_ATTRIBUTES);
        }

        state.nospace = true;
        state.json_write_string("attributeCount");
        state.nospace = true;
        state.json_write_unsigned(attrib_count as u64);

        state.nospace = true;
        state.json_write_string("attributes");
        state.nospace = true;
        state.json_write_array();

        let mut attrs: usize = 0;
        for attribute in &layer.1.tilestats {
            if attrs == elements {
                break;
            }
            attrs += 1;

            state.nospace = true;
            state.json_write_hash();

            state.nospace = true;
            state.json_write_string("attribute");
            state.nospace = true;
            state.json_write_string(attribute.0);

            let mut val_count = attribute.1.sample_values.len();
            if val_count > get(&MAX_TILESTATS_SAMPLE_VALUES) {
                val_count = get(&MAX_TILESTATS_SAMPLE_VALUES);
            }

            state.nospace = true;
            state.json_write_string("count");
            state.nospace = true;
            state.json_write_unsigned(val_count as u64);

            let mut type_: i32 = 0;
            for s in &attribute.1.sample_values {
                type_ |= 1 << s.type_;
            }

            // No "null" because null attributes are dropped
            let type_str = if type_ == (1 << MVT_DOUBLE) {
                "number"
            } else if type_ == (1 << MVT_BOOL) {
                "boolean"
            } else if type_ == (1 << MVT_STRING) {
                "string"
            } else {
                "mixed"
            };

            state.nospace = true;
            state.json_write_string("type");
            state.nospace = true;
            state.json_write_string(type_str);

            state.nospace = true;
            state.json_write_string("values");
            state.nospace = true;
            state.json_write_array();

            let mut vals: usize = 0;
            for value in &attribute.1.sample_values {
                if vals == elements {
                    break;
                }

                if value.type_ == MVT_DOUBLE || value.type_ == MVT_BOOL {
                    vals += 1;

                    state.nospace = true;
                    state.json_write_stringified(&value.s);
                } else {
                    let trunc = truncate16(&value.s, 256);

                    if trunc.len() == value.s.len() {
                        vals += 1;

                        state.nospace = true;
                        state.json_write_string(&value.s);
                    }
                }
            }

            state.nospace = true;
            state.json_end_array();

            if (type_ & (1 << MVT_DOUBLE)) != 0 {
                state.nospace = true;
                state.json_write_string("min");
                state.nospace = true;
                state.json_write_number(attribute.1.min);

                state.nospace = true;
                state.json_write_string("max");
                state.nospace = true;
                state.json_write_number(attribute.1.max);
            }

            state.nospace = true;
            state.json_end_hash();
        }

        state.nospace = true;
        state.json_end_array();
        state.nospace = true;
        state.json_end_hash();
    }

    state.nospace = true;
    state.json_end_array();
    state.nospace = true;
    state.json_end_hash();
}

fn stringify_strategies(strategies: &[Strategy]) -> String {
    let mut out = String::new();
    let mut any = false;

    {
        let mut state = JsonWriter::new_string(&mut out);

        state.nospace = true;
        state.json_write_array();
        for strategy in strategies {
            state.nospace = true;
            state.json_write_hash();

            if strategy.dropped_by_rate > 0 {
                state.nospace = true;
                state.json_write_string("dropped_by_rate");
                state.nospace = true;
                state.json_write_number(strategy.dropped_by_rate as f64);
                any = true;
            }

            if strategy.dropped_by_gamma > 0 {
                state.nospace = true;
                state.json_write_string("dropped_by_gamma");
                state.nospace = true;
                state.json_write_number(strategy.dropped_by_gamma as f64);
                any = true;
            }

            if strategy.dropped_as_needed > 0 {
                state.nospace = true;
                state.json_write_string("dropped_as_needed");
                state.nospace = true;
                state.json_write_number(strategy.dropped_as_needed as f64);
                any = true;
            }

            if strategy.coalesced_as_needed > 0 {
                state.nospace = true;
                state.json_write_string("coalesced_as_needed");
                state.nospace = true;
                state.json_write_number(strategy.coalesced_as_needed as f64);
                any = true;
            }

            if strategy.detail_reduced > 0 {
                state.nospace = true;
                state.json_write_string("detail_reduced");
                state.nospace = true;
                state.json_write_number(strategy.detail_reduced as f64);
                any = true;
            }

            if strategy.tiny_polygons > 0 {
                state.nospace = true;
                state.json_write_string("tiny_polygons");
                state.nospace = true;
                state.json_write_number(strategy.tiny_polygons as f64);
                any = true;
            }

            if strategy.tile_size > 0 {
                state.nospace = true;
                state.json_write_string("tile_size_desired");
                state.nospace = true;
                state.json_write_number(strategy.tile_size as f64);
                any = true;
            }

            if strategy.feature_count > 0 {
                state.nospace = true;
                state.json_write_string("feature_count_desired");
                state.nospace = true;
                state.json_write_number(strategy.feature_count as f64);
                any = true;
            }

            if strategy.truncated_zooms > 0 {
                state.nospace = true;
                state.json_write_string("truncated_zooms");
                state.nospace = true;
                state.json_write_number(strategy.truncated_zooms as f64);
                any = true;
            }

            state.nospace = true;
            state.json_end_hash();
        }
        state.nospace = true;
        state.json_end_array();
    }

    if any { out } else { String::new() }
}

/// Runs one metadata `INSERT`. On failure, prints `what: <error>` and exits
/// unless `forcetable` is set.
fn exec_metadata(db: &Connection, sql: &CStr, what: &str, forcetable: bool) {
    if let Err(err) = sqlite3_exec(db, sql) {
        eprintln!("{}: {}", what, err);
        if !forcetable {
            exit(EXIT_SQLITE);
        }
    }
}

pub fn mbtiles_write_metadata(db: &Connection, m: &Metadata, forcetable: bool) {
    let name = c_str(&m.name);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('name', %Q);",
        name.as_ptr()
    );
    exec_metadata(db, &sql, "set name in metadata", forcetable);

    let description = c_str(&m.description);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('description', %Q);",
        description.as_ptr()
    );
    exec_metadata(db, &sql, "set description in metadata", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('version', %d);",
        m.version as c_int
    );
    exec_metadata(db, &sql, "set version ", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('minzoom', %d);",
        m.minzoom as c_int
    );
    exec_metadata(db, &sql, "set minzoom", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('maxzoom', %d);",
        m.maxzoom as c_int
    );
    exec_metadata(db, &sql, "set maxzoom", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('center', '%f,%f,%d');",
        m.center_lon as c_double,
        m.center_lat as c_double,
        m.center_z as c_int
    );
    exec_metadata(db, &sql, "set center", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('bounds', '%f,%f,%f,%f');",
        m.minlon as c_double,
        m.minlat as c_double,
        m.maxlon as c_double,
        m.maxlat as c_double
    );
    exec_metadata(db, &sql, "set bounds", forcetable);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('antimeridian_adjusted_bounds', '%f,%f,%f,%f');",
        m.minlon2 as c_double,
        m.minlat2 as c_double,
        m.maxlon2 as c_double,
        m.maxlat2 as c_double
    );
    exec_metadata(db, &sql, "set bounds", forcetable);

    let type_ = c_str(&m.type_);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('type', %Q);",
        type_.as_ptr()
    );
    exec_metadata(db, &sql, "set type", forcetable);

    if !m.attribution.is_empty() {
        let attribution = c_str(&m.attribution);
        let sql = sqlite3_mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('attribution', %Q);",
            attribution.as_ptr()
        );
        exec_metadata(db, &sql, "set attribution", forcetable);
    }

    let format = c_str(&m.format);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('format', %Q);",
        format.as_ptr()
    );
    exec_metadata(db, &sql, "set format", forcetable);

    let generator = c_str(&m.generator);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('generator', %Q);",
        generator.as_ptr()
    );
    exec_metadata(db, &sql, "set generator", forcetable);

    let generator_options = c_str(&m.generator_options);
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('generator_options', %Q);",
        generator_options.as_ptr()
    );
    exec_metadata(db, &sql, "set commandline", forcetable);

    if !m.strategies_json.is_empty() {
        let strategies = c_str(&m.strategies_json);
        let sql = sqlite3_mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('strategies', %Q);",
            strategies.as_ptr()
        );
        exec_metadata(db, &sql, "set strategies", forcetable);
    }

    if !m.decisions_json.is_empty() {
        let decisions = c_str(&m.decisions_json);
        let sql = sqlite3_mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('tippecanoe_decisions', %Q);",
            decisions.as_ptr()
        );
        exec_metadata(db, &sql, "set decisions", forcetable);
    }

    if !m.vector_layers_json.is_empty() || !m.tilestats_json.is_empty() {
        let mut json = String::new();
        {
            let mut state = JsonWriter::new_string(&mut json);
            state.nospace = true;
            state.json_write_hash();

            if !m.vector_layers_json.is_empty() {
                state.nospace = true;
                state.json_write_string("vector_layers");
                state.nospace = true;
                state.json_write_json(&m.vector_layers_json);

                if !m.tilestats_json.is_empty() {
                    state.nospace = true;
                    state.json_write_string("tilestats");
                    state.nospace = true;
                    state.json_write_json(&m.tilestats_json);
                }
            } else if !m.tilestats_json.is_empty() {
                state.nospace = true;
                state.json_write_string("tilestats");
                state.nospace = true;
                state.json_write_json(&m.tilestats_json);
            }

            state.nospace = true;
            state.json_end_hash();
        }

        let json = c_str(&json);
        let sql = sqlite3_mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('json', %Q);",
            json.as_ptr()
        );
        exec_metadata(db, &sql, "set json", forcetable);
    }
}

fn sixdig(val: f64) -> f64 {
    (val * 1e6).round() / 1e6
}

pub fn version_str() -> String {
    let mut s = VERSION.to_string();
    let build_info = option_env!("BUILD_INFO").unwrap_or("");
    if !build_info.is_empty() {
        s += " ";
        s += build_info;
    }
    s
}

#[allow(clippy::too_many_arguments)]
pub fn make_metadata(
    fname: &str,
    minzoom: i32,
    maxzoom: i32,
    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
    minlat2: f64,
    minlon2: f64,
    maxlat2: f64,
    maxlon2: f64,
    midlat: f64,
    midlon: f64,
    attribution: Option<&str>,
    layermap: &BTreeMap<String, LayermapEntry>,
    vector: bool,
    description: Option<&str>,
    do_tilestats: bool,
    attribute_descriptions: &BTreeMap<String, String>,
    program: &str,
    commandline: &str,
    strategies: &[Strategy],
    basezoom: i32,
    mut droprate: f64,
    retain_points_multiplier: i32,
) -> Metadata {
    let mut m = Metadata {
        name: fname.to_string(),
        description: description.unwrap_or(fname).to_string(),
        version: 2,
        type_: "overlay".to_string(),
        format: if vector { "pbf" } else { "png" }.to_string(),
        ..Default::default()
    };

    m.minzoom = minzoom;
    m.maxzoom = maxzoom;

    m.minlat = sixdig(minlat);
    m.minlon = sixdig(minlon);
    m.maxlat = sixdig(maxlat);
    m.maxlon = sixdig(maxlon);

    m.minlat2 = sixdig(minlat2);
    m.minlon2 = sixdig(minlon2);
    m.maxlat2 = sixdig(maxlat2);
    m.maxlon2 = sixdig(maxlon2);

    m.center_lat = sixdig(midlat);
    m.center_lon = sixdig(midlon);
    m.center_z = maxzoom;

    if let Some(attribution) = attribution {
        m.attribution = attribution.to_string();
    }

    m.generator = program.to_string() + " " + &version_str();
    m.generator_options = commandline.to_string();

    m.strategies_json = stringify_strategies(strategies);

    if droprate.is_infinite() {
        // JSON has no representation for infinity, so substitute a huge
        // finite value. The cast is explicit because LLONG_MAX itself is
        // not representable as a double and rounds up to 2^63.
        droprate = i64::MAX as f64;
    }
    if basezoom != maxzoom || droprate != 2.5 || retain_points_multiplier != 1 {
        m.decisions_json = "{".to_string()
            + "\"basezoom\":"
            + &dtoa_milo(basezoom as f64)
            + ","
            + "\"droprate\":"
            + &dtoa_milo(droprate)
            + ","
            + "\"retain_points_multiplier\":"
            + &retain_points_multiplier.to_string()
            + "}";
    }

    if vector {
        {
            let mut state = JsonWriter::new_string(&mut m.vector_layers_json);

            state.nospace = true;
            state.json_write_array();

            let lnames: Vec<&String> = layermap.keys().collect();

            for lname in &lnames {
                let ts = &layermap[*lname];
                state.nospace = true;
                state.json_write_hash();

                state.nospace = true;
                state.json_write_string("id");
                state.nospace = true;
                state.json_write_string(lname);

                state.nospace = true;
                state.json_write_string("description");
                state.nospace = true;
                state.json_write_string(&ts.description);

                state.nospace = true;
                state.json_write_string("minzoom");
                state.nospace = true;
                state.json_write_signed(ts.minzoom as i64);

                state.nospace = true;
                state.json_write_string("maxzoom");
                state.nospace = true;
                state.json_write_signed(ts.maxzoom as i64);

                state.nospace = true;
                state.json_write_string("fields");
                state.nospace = true;
                state.json_write_hash();

                let mut attribute_count: usize = 0;
                for j in &ts.tilestats {
                    state.nospace = true;
                    state.json_write_string(j.0);

                    match attribute_descriptions.get(j.0) {
                        None => {
                            let mut type_: i32 = 0;
                            for s in &j.1.sample_values {
                                type_ |= 1 << s.type_;
                            }

                            if type_ == (1 << MVT_DOUBLE) {
                                state.nospace = true;
                                state.json_write_string("Number");
                            } else if type_ == (1 << MVT_BOOL) {
                                state.nospace = true;
                                state.json_write_string("Boolean");
                            } else if type_ == (1 << MVT_STRING) {
                                state.nospace = true;
                                state.json_write_string("String");
                            } else {
                                state.nospace = true;
                                state.json_write_string("Mixed");
                            }
                        }
                        Some(f) => {
                            state.nospace = true;
                            state.json_write_string(f);
                        }
                    }

                    attribute_count += 1;
                    if attribute_count >= get(&MAX_TILESTATS_ATTRIBUTES) {
                        break;
                    }
                }

                state.nospace = true;
                state.json_end_hash();
                state.nospace = true;
                state.json_end_hash();
            }

            state.nospace = true;
            state.json_end_array();
        }

        {
            let elements = get(&MAX_TILESTATS_VALUES);
            let mut state = JsonWriter::new_string(&mut m.tilestats_json);

            if do_tilestats && elements > 0 {
                tilestats(layermap, elements, &mut state);
            }
        }
    }

    m
}

pub fn mbtiles_close(outdb: Connection, pgm: &str) {
    if let Err(err) = sqlite3_exec(&outdb, c"ANALYZE;") {
        eprintln!("{}: ANALYZE failed: {}", pgm, err);
        exit(EXIT_SQLITE);
    }
    if let Err((outdb, _)) = outdb.close() {
        eprintln!(
            "{}: could not close database: {}",
            pgm,
            sqlite3_errmsg(&outdb)
        );
        exit(EXIT_CLOSE);
    }
}

pub fn merge_layermaps(
    maps: &[BTreeMap<String, LayermapEntry>],
) -> BTreeMap<String, LayermapEntry> {
    merge_layermaps_trunc(maps, false)
}

/// `merge_layermaps(maps, trunc)`.
pub fn merge_layermaps_trunc(
    maps: &[BTreeMap<String, LayermapEntry>],
    trunc: bool,
) -> BTreeMap<String, LayermapEntry> {
    let mut out: BTreeMap<String, LayermapEntry> = BTreeMap::new();

    for map in maps {
        for (name, entry) in map {
            if entry.points + entry.lines + entry.polygons + entry.retain == 0 {
                continue;
            }

            let mut layername = name.clone();
            if trunc {
                layername = truncate16(&layername, 256);
            }

            if !out.contains_key(&layername) {
                let mut out_entry = LayermapEntry::new(out.len());
                out_entry.minzoom = entry.minzoom;
                out_entry.maxzoom = entry.maxzoom;
                out_entry.description = entry.description.clone();
                out.insert(layername.clone(), out_entry);
            }

            let out_entry = match out.get_mut(&layername) {
                Some(out_entry) => out_entry,
                None => {
                    eprintln!("Internal error merging layers");
                    exit(EXIT_IMPOSSIBLE);
                }
            };

            for (ts_name, ts) in &entry.tilestats {
                let mut attribname = ts_name.clone();
                if trunc {
                    attribname = truncate16(&attribname, 256);
                }

                match out_entry.tilestats.get_mut(&attribname) {
                    None => {
                        out_entry.tilestats.insert(attribname, ts.clone());
                    }
                    Some(ts2) => {
                        for val in &ts.sample_values {
                            let pt = ts2.sample_values.partition_point(|v| v < val);
                            if pt == ts2.sample_values.len() || ts2.sample_values[pt] != *val {
                                // not found
                                ts2.sample_values.insert(pt, val.clone());

                                if ts2.sample_values.len() > get(&MAX_TILESTATS_SAMPLE_VALUES) {
                                    ts2.sample_values.pop();
                                }
                            }
                        }

                        ts2.type_ |= ts.type_;

                        if ts.min < ts2.min {
                            ts2.min = ts.min;
                        }
                        if ts.max > ts2.max {
                            ts2.max = ts.max;
                        }
                    }
                }
            }

            if entry.minzoom < out_entry.minzoom {
                out_entry.minzoom = entry.minzoom;
            }
            if entry.maxzoom > out_entry.maxzoom {
                out_entry.maxzoom = entry.maxzoom;
            }

            out_entry.points += entry.points;
            out_entry.lines += entry.lines;
            out_entry.polygons += entry.polygons;
        }
    }

    out
}

pub fn add_to_tilestats(tilestats: &mut BTreeMap<String, Tilestat>, attrib: &str, val: &SerialVal) {
    if val.type_ == MVT_NULL {
        return;
    }

    if !tilestats.contains_key(attrib) {
        tilestats.insert(attrib.to_string(), Tilestat::default());
    }

    let tsa = match tilestats.get_mut(attrib) {
        Some(tsa) => tsa,
        None => {
            eprintln!("Can't happen (tilestats)");
            exit(EXIT_IMPOSSIBLE);
        }
    };

    if val.type_ == MVT_DOUBLE {
        let d = crate::cstd::atof(&val.s);

        if d < tsa.min {
            tsa.min = d;
        }
        if d > tsa.max {
            tsa.max = d;
        }
    }

    let pt = tsa.sample_values.partition_point(|v| v < val);
    if pt == tsa.sample_values.len() || tsa.sample_values[pt] != *val {
        // not found
        if tsa.sample_values.len() >= get(&MAX_TILESTATS_SAMPLE_VALUES) {
            if pt == tsa.sample_values.len() {
                // insertion point would be at the end,
                // and the list is already full, so do nothing
            } else {
                // bump the former last value, insert this one
                tsa.sample_values.insert(pt, val.clone());
                tsa.sample_values.pop();
            }
        } else {
            tsa.sample_values.insert(pt, val.clone());
        }
    }

    tsa.type_ |= 1 << val.type_;
}
