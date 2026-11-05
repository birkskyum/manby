//! Port of dirtiles.hpp and dirtiles.cpp: tiles in a directory tree.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::OsStrExt;
use std::process::exit;

use rusqlite::Connection;

use crate::cstd::{atoi, errno, perror_io, strerror};
use crate::errors::{
    EXIT_CLOSE, EXIT_EXISTS, EXIT_JSON, EXIT_OPEN, EXIT_SQLITE, EXIT_UNLINK, EXIT_WRITE,
};
use crate::jsonpull::{JsonPull, JsonType};
use crate::mbtiles::{Metadata, c_str_bytes, sqlite3_exec, sqlite3_mprintf, sqlite3_open};
use crate::write_json::JsonWriter;

/// `zxy`. Ordered by zoom, then x, then y reversed (for TMS). The extension
/// takes no part in comparisons, as in the C++.
#[derive(Clone, Debug)]
pub struct Zxy {
    pub z: i64,
    pub x: i64,
    pub y: i64,
    pub extension: String,
}

impl Zxy {
    pub fn new(z: i32, x: i32, y: i32) -> Zxy {
        Zxy {
            z: z as i64,
            x: x as i64,
            y: y as i64,
            extension: ".pbf".to_string(),
        }
    }

    pub fn path(&self) -> String {
        format!("{}/{}/{}{}", self.z, self.x, self.y, self.extension)
    }
}

impl PartialEq for Zxy {
    fn eq(&self, o: &Zxy) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Zxy {}

impl PartialOrd for Zxy {
    fn partial_cmp(&self, o: &Zxy) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Zxy {
    fn cmp(&self, o: &Zxy) -> Ordering {
        self.z
            .cmp(&o.z)
            .then(self.x.cmp(&o.x))
            .then(o.y.cmp(&self.y))
    }
}

/// `std::to_string(double)`, which formats like `%f`.
fn to_string_double(d: f64) -> String {
    if d.is_nan() {
        return "nan".to_string();
    }
    format!("{:.6}", d)
}

pub fn dir_read_tile(base: &str, tile: &Zxy) -> Vec<u8> {
    fs::read(base.to_string() + "/" + &tile.path()).unwrap_or_default()
}

pub fn dir_write_tile(outdir: &str, z: i32, tx: i32, ty: i32, pbf: &[u8]) {
    // Don't check mkdir error returns, since most of these calls to
    // mkdir will be creating directories that already exist.
    let _ = fs::create_dir(outdir);

    let curdir = outdir.to_string();
    let slash = "/";

    let mut newdir = curdir + slash + &z.to_string();
    let _ = fs::create_dir(&newdir);

    newdir = newdir + "/" + &tx.to_string();
    let _ = fs::create_dir(&newdir);

    newdir = newdir + "/" + &ty.to_string() + ".pbf";

    if fs::metadata(&newdir).is_ok() {
        eprintln!("Can't write tile to already existing {}", newdir);
        exit(EXIT_EXISTS);
    }

    let mut fp = match File::create(&newdir) {
        Ok(fp) => fp,
        Err(e) => {
            eprintln!("{}: {}", newdir, strerror(e.raw_os_error().unwrap_or(0)));
            exit(EXIT_WRITE);
        }
    };

    if let Err(e) = fp.write_all(pbf) {
        eprintln!("{}: {}", newdir, strerror(e.raw_os_error().unwrap_or(0)));
        exit(EXIT_WRITE);
    }

    let fd = fp.into_raw_fd();
    // SAFETY: the File gave up `fd`, so this is the only close of it.
    if unsafe { libc::close(fd) } != 0 {
        eprintln!("{}: {}", newdir, strerror(errno()));
        exit(EXIT_CLOSE);
    }
}

fn numeric(s: &[u8]) -> bool {
    if s.is_empty() {
        return false;
    }
    for &c in s {
        if !c.is_ascii_digit() {
            return false;
        }
    }
    true
}

fn pbfname(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }

    &s[i..] == b".pbf" || &s[i..] == b".mvt"
}

/// `atoi(dp->d_name)`. Only called on names that `numeric` or `pbfname`
/// accepted, which are ASCII.
fn atoi_name(s: &[u8]) -> i32 {
    atoi(&String::from_utf8_lossy(s))
}

pub fn check_dir(d: &str, argv: &[String], force: bool, forcetable: bool) {
    let _ = fs::create_dir(d);
    let meta = d.to_string() + "/" + "metadata.json";
    if force {
        let _ = fs::remove_file(&meta); // error OK since it may not exist;
    } else if fs::metadata(&meta).is_ok() {
        eprintln!(
            "{}: Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.",
            argv[0], d
        );
        eprintln!("{}: {}: file exists", argv[0], meta);
        if !forcetable {
            exit(EXIT_EXISTS);
        }
    }

    if forcetable {
        // Don't clear existing tiles
        return;
    }

    let tiles = enumerate_dirtiles(d, i32::MIN, i32::MAX);

    for tile in &tiles {
        let fn_ = d.to_string() + "/" + &tile.path();

        if force {
            if let Err(e) = fs::remove_file(&fn_) {
                perror_io(&fn_, &e);
                exit(EXIT_UNLINK);
            }
        } else {
            eprintln!("{}: file exists", fn_);
            exit(EXIT_EXISTS);
        }
    }
}

pub fn enumerate_dirtiles(fname: &str, minzoom: i32, maxzoom: i32) -> Vec<Zxy> {
    let mut tiles: Vec<Zxy> = Vec::new();

    if let Ok(d1) = fs::read_dir(fname) {
        // readdir() returning NULL on an error ends the loop in the C++.
        for dp in d1.map_while(Result::ok) {
            let d_name = dp.file_name();
            let d_name = d_name.as_bytes();
            if numeric(d_name) && atoi_name(d_name) >= minzoom && atoi_name(d_name) <= maxzoom {
                let z = fname.to_string() + "/" + &String::from_utf8_lossy(d_name);
                let tz = atoi_name(d_name);

                let d2 = match fs::read_dir(&z) {
                    Ok(d2) => d2,
                    Err(e) => {
                        perror_io(&z, &e);
                        exit(EXIT_OPEN);
                    }
                };

                for dp2 in d2.map_while(Result::ok) {
                    let d_name2 = dp2.file_name();
                    let d_name2 = d_name2.as_bytes();
                    if numeric(d_name2) {
                        let x = z.clone() + "/" + &String::from_utf8_lossy(d_name2);
                        let tx = atoi_name(d_name2);

                        let d3 = match fs::read_dir(&x) {
                            Ok(d3) => d3,
                            Err(e) => {
                                perror_io(&x, &e);
                                exit(EXIT_OPEN);
                            }
                        };

                        for dp3 in d3.map_while(Result::ok) {
                            let d_name3 = dp3.file_name();
                            let d_name3 = d_name3.as_bytes();
                            if pbfname(d_name3) {
                                let ty = atoi_name(d_name3);
                                let mut tile = Zxy::new(tz, tx, ty);
                                if d_name3.windows(4).any(|w| w == b".mvt") {
                                    tile.extension = ".mvt".to_string();
                                }

                                tiles.push(tile);
                            }
                        }
                    }
                }
            }
        }
    }

    tiles.sort();
    tiles
}

pub fn dir_erase_zoom(fname: &str, zoom: i32) {
    if let Ok(d1) = fs::read_dir(fname) {
        for dp in d1.map_while(Result::ok) {
            let d_name = dp.file_name();
            let d_name = d_name.as_bytes();
            if numeric(d_name) && atoi_name(d_name) == zoom {
                let z = fname.to_string() + "/" + &String::from_utf8_lossy(d_name);

                let d2 = match fs::read_dir(&z) {
                    Ok(d2) => d2,
                    Err(e) => {
                        perror_io(&z, &e);
                        exit(EXIT_OPEN);
                    }
                };

                for dp2 in d2.map_while(Result::ok) {
                    let d_name2 = dp2.file_name();
                    let d_name2 = d_name2.as_bytes();
                    if numeric(d_name2) {
                        let x = z.clone() + "/" + &String::from_utf8_lossy(d_name2);

                        let d3 = match fs::read_dir(&x) {
                            Ok(d3) => d3,
                            Err(e) => {
                                perror_io(&x, &e);
                                exit(EXIT_OPEN);
                            }
                        };

                        for dp3 in d3.map_while(Result::ok) {
                            let d_name3 = dp3.file_name();
                            let d_name3 = d_name3.as_bytes();
                            if pbfname(d_name3) {
                                let y = x.clone() + "/" + &String::from_utf8_lossy(d_name3);
                                if let Err(e) = fs::remove_file(&y) {
                                    perror_io(&y, &e);
                                    exit(EXIT_UNLINK);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn dirmeta2tmp(fname: &str) -> Connection {
    let db = match sqlite3_open("") {
        Ok(db) => db,
        Err(msg) => {
            eprintln!("Temporary db: {}", msg);
            exit(EXIT_SQLITE);
        }
    };
    if let Err(err) = sqlite3_exec(&db, c"CREATE TABLE metadata (name text, value text);") {
        eprintln!("Create metadata table: {}", err);
        exit(EXIT_SQLITE);
    }

    let mut name = fname.to_string();
    name += "/metadata.json";

    match File::open(&name) {
        Err(e) => perror_io(&name, &e),
        Ok(f) => {
            let mut jp = JsonPull::begin_file(Box::new(BufReader::new(f)));
            let o = match jp.read_tree() {
                Some(o) => o,
                None => {
                    eprintln!(
                        "{}: metadata parsing error: {}",
                        name,
                        jp.error.unwrap_or("(null)")
                    );
                    exit(EXIT_JSON);
                }
            };

            if o.root().type_() != JsonType::Hash {
                eprintln!("{}: bad metadata format", name);
                exit(EXIT_JSON);
            }

            for (key, value) in o.root().entries() {
                // Skip, not just warn: a metadata.json from another tool may
                // have a numeric minzoom or a nested object, and string()
                // asserts on the type.
                if key.type_() != JsonType::String || value.type_() != JsonType::String {
                    eprintln!("{}: non-string in metadata", name);
                    continue;
                }

                let k = c_str_bytes(key.raw_string_bytes());
                let v = c_str_bytes(value.raw_string_bytes());
                let sql = sqlite3_mprintf!(
                    c"INSERT INTO metadata (name, value) VALUES (%Q, %Q);",
                    k.as_ptr(),
                    v.as_ptr()
                );
                if let Err(err) = sqlite3_exec(&db, &sql) {
                    eprintln!("set {} in metadata: {}", k.to_string_lossy(), err);
                }
            }
        }
    }

    db
}

fn out(state: &mut JsonWriter<'_>, k: &str, v: &str) {
    state.json_comma_newline();
    state.json_write_string(k);
    state.json_write_string(v);
}

pub fn dir_write_metadata(outdir: &str, m: &Metadata) {
    let metadata = outdir.to_string() + "/metadata.json";

    if fs::metadata(&metadata).is_ok() {
        // Leave existing metadata in place with --allow-existing
    } else {
        let fp = match File::create(&metadata) {
            Ok(fp) => fp,
            Err(e) => {
                perror_io(&metadata, &e);
                exit(EXIT_OPEN);
            }
        };
        let mut fp = BufWriter::new(fp);

        {
            let mut state = JsonWriter::new_file(&mut fp);

            state.json_write_hash();
            state.json_write_newline();

            out(&mut state, "name", &m.name);
            out(&mut state, "description", &m.description);
            out(&mut state, "version", &m.version.to_string());
            out(&mut state, "minzoom", &m.minzoom.to_string());
            out(&mut state, "maxzoom", &m.maxzoom.to_string());
            out(
                &mut state,
                "center",
                &(to_string_double(m.center_lon)
                    + ","
                    + &to_string_double(m.center_lat)
                    + ","
                    + &m.center_z.to_string()),
            );
            out(
                &mut state,
                "bounds",
                &(to_string_double(m.minlon)
                    + ","
                    + &to_string_double(m.minlat)
                    + ","
                    + &to_string_double(m.maxlon)
                    + ","
                    + &to_string_double(m.maxlat)),
            );
            out(
                &mut state,
                "antimeridian_adjusted_bounds",
                &(to_string_double(m.minlon2)
                    + ","
                    + &to_string_double(m.minlat2)
                    + ","
                    + &to_string_double(m.maxlon2)
                    + ","
                    + &to_string_double(m.maxlat2)),
            );
            out(&mut state, "type", &m.type_);
            if !m.attribution.is_empty() {
                out(&mut state, "attribution", &m.attribution);
            }
            if !m.strategies_json.is_empty() {
                out(&mut state, "strategies", &m.strategies_json);
            }
            if !m.decisions_json.is_empty() {
                out(&mut state, "tippecanoe_decisions", &m.decisions_json);
            }
            out(&mut state, "format", &m.format);
            out(&mut state, "generator", &m.generator);
            out(&mut state, "generator_options", &m.generator_options);

            if !m.vector_layers_json.is_empty() || !m.tilestats_json.is_empty() {
                let mut json = "{".to_string();

                if !m.vector_layers_json.is_empty() {
                    json += "\"vector_layers\":";
                    json += &m.vector_layers_json;

                    if !m.tilestats_json.is_empty() {
                        json += ",\"tilestats\":";
                        json += &m.tilestats_json;
                    }
                } else if !m.tilestats_json.is_empty() {
                    json += "\"tilestats\":";
                    json += &m.tilestats_json;
                }

                json += "}";

                out(&mut state, "json", &json);
            }

            state.json_write_newline();
            state.json_end_hash();
            state.json_write_newline();
        }
        let _ = fp.flush();
    }
}
