//! A directory of z/x/y.pbf files and metadata.json, as tippecanoe's `-e` writes.
//!
//! Tiles are written on the threads that make them, so the writes run in
//! parallel. Each tile file is created with `O_EXCL`, which refuses an
//! existing file in one step where tippecanoe uses `stat` and then `fopen`,
//! and the z and z/x directories are made once instead of for every tile.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::OsStrExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tippecanoe::errors::{EXIT_CLOSE, EXIT_EXISTS, EXIT_OPEN, EXIT_UNLINK, EXIT_WRITE};
use tippecanoe::mbtiles::Metadata;
use tippecanoe::write_json::JsonWriter;

use super::{SinkSettings, TileSink, check_tile, io_message, to_string_double};
use crate::{Error, Result};

const SHARDS: usize = 16;

pub struct DirSink {
    dir: String,
    /// Zoom directories already made.
    zooms: [AtomicBool; 32],
    /// z/x directories already made, sharded by x.
    columns: Vec<Mutex<HashSet<(u8, u32)>>>,
}

impl DirSink {
    /// Prepares `dir` the way tippecanoe's `check_dir` does for `-e`.
    pub fn create(dir: &str, settings: &SinkSettings) -> Result<DirSink> {
        let _ = fs::create_dir(dir);
        let meta = dir.to_string() + "/metadata.json";
        if settings.force {
            let _ = fs::remove_file(&meta); // error OK since it may not exist
        } else if fs::metadata(&meta).is_ok() {
            let message = format!(
                "Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.\n{}: {}: file exists",
                dir, settings.program, meta
            );
            if !settings.allow_existing {
                return Err(Error::new(EXIT_EXISTS, message));
            }
            eprintln!("{}: {}", settings.program, message);
        }

        if !settings.allow_existing {
            for tile in enumerate_dirtiles(dir, i32::MIN, i32::MAX)? {
                let path = dir.to_string() + "/" + &tile;
                if settings.force {
                    fs::remove_file(&path)
                        .map_err(|e| Error::new(EXIT_UNLINK, io_message(&path, &e)))?;
                } else {
                    return Err(Error::new(EXIT_EXISTS, format!("{}: file exists", path)));
                }
            }
        }

        Ok(DirSink {
            dir: dir.to_string(),
            zooms: std::array::from_fn(|_| AtomicBool::new(false)),
            columns: (0..SHARDS).map(|_| Mutex::default()).collect(),
        })
    }
}

impl TileSink for DirSink {
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<()> {
        check_tile(z, x, y)?;

        // Don't check mkdir error returns, since most of these calls to
        // mkdir will be creating directories that already exist.
        let zoom_dir = format!("{}/{}", self.dir, z);
        if !self.zooms[z as usize].load(Ordering::Relaxed) {
            let _ = fs::create_dir(&self.dir);
            let _ = fs::create_dir(&zoom_dir);
            self.zooms[z as usize].store(true, Ordering::Relaxed);
        }
        let column_dir = format!("{}/{}", zoom_dir, x);
        {
            let mut made = self.columns[x as usize % SHARDS]
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if made.insert((z, x)) {
                let _ = fs::create_dir(&column_dir);
            }
        }

        let path = format!("{}/{}.pbf", column_dir, y);
        let mut f = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::new(
                    EXIT_EXISTS,
                    format!("Can't write tile to already existing {}", path),
                ));
            }
            Err(e) => return Err(Error::new(EXIT_WRITE, io_message(&path, &e))),
        };
        f.write_all(data)
            .map_err(|e| Error::new(EXIT_WRITE, io_message(&path, &e)))?;
        let fd = f.into_raw_fd();
        // SAFETY: the File gave up `fd`, so this is the only close of it.
        if unsafe { libc::close(fd) } != 0 {
            let e = std::io::Error::last_os_error();
            return Err(Error::new(EXIT_CLOSE, io_message(&path, &e)));
        }
        Ok(())
    }

    fn erase_zoom(&self, z: u8) -> Result<()> {
        dir_erase_zoom(&self.dir, z as i32)
    }

    fn finish(self: Box<Self>, metadata: &Metadata) -> Result<()> {
        dir_write_metadata(&self.dir, metadata)
    }
}

fn numeric(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|c| c.is_ascii_digit())
}

fn pbfname(s: &[u8]) -> bool {
    let digits = s.iter().take_while(|c| c.is_ascii_digit()).count();
    &s[digits..] == b".pbf" || &s[digits..] == b".mvt"
}

fn atoi(s: &[u8]) -> i32 {
    tippecanoe::cstd::atoi(&String::from_utf8_lossy(s))
}

/// The entries of a directory, or none if it cannot be read (the top level
/// in tippecanoe), or an error (the levels below).
fn read_names(dir: &str, must: bool) -> Result<Vec<Vec<u8>>> {
    match fs::read_dir(dir) {
        Ok(d) => Ok(d
            .map_while(std::result::Result::ok)
            .map(|e| e.file_name().as_bytes().to_vec())
            .collect()),
        Err(_) if !must => Ok(Vec::new()),
        Err(e) => Err(Error::new(EXIT_OPEN, io_message(dir, &e))),
    }
}

/// tippecanoe's `enumerate_dirtiles`, returning the `z/x/y.ext` paths in its
/// order and errors instead of exiting.
pub fn enumerate_dirtiles(dir: &str, minzoom: i32, maxzoom: i32) -> Result<Vec<String>> {
    let mut tiles: Vec<(i64, i64, i64, String)> = Vec::new();
    for zname in read_names(dir, false)? {
        let tz = atoi(&zname);
        if !numeric(&zname) || tz < minzoom || tz > maxzoom {
            continue;
        }
        let z = format!("{}/{}", dir, String::from_utf8_lossy(&zname));
        for xname in read_names(&z, true)? {
            if !numeric(&xname) {
                continue;
            }
            let tx = atoi(&xname);
            let x = format!("{}/{}", z, String::from_utf8_lossy(&xname));
            for yname in read_names(&x, true)? {
                if pbfname(&yname) {
                    let ty = atoi(&yname);
                    let ext = if yname.windows(4).any(|w| w == b".mvt") {
                        ".mvt"
                    } else {
                        ".pbf"
                    };
                    tiles.push((
                        tz as i64,
                        tx as i64,
                        ty as i64,
                        format!("{}/{}/{}{}", tz, tx, ty, ext),
                    ));
                }
            }
        }
    }
    // z, then x, then y reversed (for TMS); stable, as in tippecanoe
    tiles.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
    Ok(tiles.into_iter().map(|t| t.3).collect())
}

/// tippecanoe's `dir_erase_zoom`, returning errors instead of exiting.
pub fn dir_erase_zoom(dir: &str, zoom: i32) -> Result<()> {
    for zname in read_names(dir, false)? {
        if !numeric(&zname) || atoi(&zname) != zoom {
            continue;
        }
        let z = format!("{}/{}", dir, String::from_utf8_lossy(&zname));
        for xname in read_names(&z, true)? {
            if !numeric(&xname) {
                continue;
            }
            let x = format!("{}/{}", z, String::from_utf8_lossy(&xname));
            for yname in read_names(&x, true)? {
                if pbfname(&yname) {
                    let y = format!("{}/{}", x, String::from_utf8_lossy(&yname));
                    fs::remove_file(&y).map_err(|e| Error::new(EXIT_UNLINK, io_message(&y, &e)))?;
                }
            }
        }
    }
    Ok(())
}

fn out(state: &mut JsonWriter<'_>, k: &str, v: &str) {
    state.json_comma_newline();
    state.json_write_string(k);
    state.json_write_string(v);
}

/// tippecanoe's `dir_write_metadata`, returning errors instead of exiting.
pub fn dir_write_metadata(dir: &str, m: &Metadata) -> Result<()> {
    let metadata = dir.to_string() + "/metadata.json";
    if fs::metadata(&metadata).is_ok() {
        // Leave existing metadata in place with --allow-existing
        return Ok(());
    }
    let file =
        File::create(&metadata).map_err(|e| Error::new(EXIT_OPEN, io_message(&metadata, &e)))?;
    let mut fp = BufWriter::new(file);
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
            &format!(
                "{},{},{}",
                to_string_double(m.center_lon),
                to_string_double(m.center_lat),
                m.center_z
            ),
        );
        out(
            &mut state,
            "bounds",
            &format!(
                "{},{},{},{}",
                to_string_double(m.minlon),
                to_string_double(m.minlat),
                to_string_double(m.maxlon),
                to_string_double(m.maxlat)
            ),
        );
        out(
            &mut state,
            "antimeridian_adjusted_bounds",
            &format!(
                "{},{},{},{}",
                to_string_double(m.minlon2),
                to_string_double(m.minlat2),
                to_string_double(m.maxlon2),
                to_string_double(m.maxlat2)
            ),
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
            } else {
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
    fp.flush()
        .map_err(|e| Error::new(EXIT_WRITE, io_message(&metadata, &e)))
}
