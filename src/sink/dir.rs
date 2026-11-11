//! A directory of z/x/y.pbf files and metadata.json, as tippecanoe's `-e` writes.
//!
//! Creating many small files is slow on most file systems and does not get
//! faster with many threads (on APFS, beyond about four), so tiles are handed
//! to a few writer threads instead of being written by the threads that make
//! them. Each writer owns a share of the z/x directories, which keeps writers
//! out of each other's directories and lets each make its directories once
//! without locking. Each tile file is created with `O_EXCL`, which refuses an
//! existing file in one step where tippecanoe uses `stat` and then `fopen`.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tippecanoe::errors::{
    EXIT_CLOSE, EXIT_EXISTS, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_UNLINK, EXIT_WRITE,
};
use tippecanoe::mbtiles::Metadata;
use tippecanoe::write_json::JsonWriter;

use super::{SinkSettings, TileSink, check_tile, io_message, tileset_metadata, to_string_double};
use crate::mlt::TileFormat;
use crate::{Error, Result};

enum Job {
    Tile {
        z: u8,
        x: u32,
        y: u32,
        data: Vec<u8>,
    },
    /// Answered once every tile sent before it is written.
    Barrier(SyncSender<()>),
}

type Failure = Arc<Mutex<Option<(i32, String)>>>;

pub struct DirSink {
    dir: String,
    tile_format: TileFormat,
    senders: Vec<SyncSender<Job>>,
    writers: Vec<JoinHandle<()>>,
    /// The first error a writer met.
    failure: Failure,
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
                "{}: Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.\n{}: {}: file exists",
                settings.program, dir, settings.program, meta
            );
            if !settings.allow_existing {
                return Err(Error::new(EXIT_EXISTS, message));
            }
            eprintln!("{}", message);
        }

        if !settings.allow_existing {
            for tile in enumerate_dirtiles(dir, i32::MIN, i32::MAX, settings.tile_format)? {
                let path = dir.to_string() + "/" + &tile;
                if settings.force {
                    fs::remove_file(&path)
                        .map_err(|e| Error::new(EXIT_UNLINK, io_message(&path, &e)))?;
                } else {
                    return Err(Error::new(EXIT_EXISTS, format!("{}: file exists", path)));
                }
            }
        }

        let failure: Failure = Arc::new(Mutex::new(None));
        let mut senders = Vec::new();
        let mut writers = Vec::new();
        for i in 0..settings.file_writers.max(1) {
            let (sender, receiver) = sync_channel(1024);
            let (dir, failure) = (dir.to_string(), failure.clone());
            let extension = settings.tile_format.extension();
            let writer = std::thread::Builder::new()
                .name(format!("tile files {}", i))
                .spawn(move || write_files(&dir, extension, receiver, &failure))
                .map_err(|e| Error::new(EXIT_IMPOSSIBLE, format!("tile file writer: {}", e)))?;
            senders.push(sender);
            writers.push(writer);
        }

        Ok(DirSink {
            dir: dir.to_string(),
            tile_format: settings.tile_format,
            senders,
            writers,
            failure,
        })
    }

    fn failed(&self) -> Option<Error> {
        self.failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|(code, message)| Error::new(*code, message.clone()))
    }

    /// Waits until every tile sent so far is written.
    fn flush(&self) -> Result<()> {
        let acks: Vec<_> = self
            .senders
            .iter()
            .filter_map(|s| {
                let (ack, done) = sync_channel(1);
                s.send(Job::Barrier(ack)).ok().map(|_| done)
            })
            .collect();
        for done in acks {
            let _ = done.recv();
        }
        self.failed().map_or(Ok(()), Err)
    }

    fn stop(&mut self) {
        self.senders.clear();
        for writer in self.writers.drain(..) {
            let _ = writer.join();
        }
    }
}

impl Drop for DirSink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One writer thread: writes the tiles it is sent until the sink closes.
fn write_files(dir: &str, extension: &str, receiver: Receiver<Job>, failure: &Failure) {
    let mut zooms = [false; 32];
    let mut columns: HashSet<(u8, u32)> = HashSet::new();
    for job in receiver.iter() {
        match job {
            Job::Tile { z, x, y, data } => {
                if failure.lock().unwrap_or_else(|p| p.into_inner()).is_some() {
                    continue;
                }
                if let Err(e) = write_file(dir, extension, z, x, y, &data, &mut zooms, &mut columns)
                {
                    let mut f = failure.lock().unwrap_or_else(|p| p.into_inner());
                    if f.is_none() {
                        *f = Some((e.exit_code, e.message));
                    }
                }
            }
            Job::Barrier(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

/// tippecanoe's `dir_write_tile`, returning errors instead of exiting.
#[allow(clippy::too_many_arguments)]
fn write_file(
    dir: &str,
    extension: &str,
    z: u8,
    x: u32,
    y: u32,
    data: &[u8],
    zooms: &mut [bool; 32],
    columns: &mut HashSet<(u8, u32)>,
) -> Result<()> {
    // Don't check mkdir error returns, since most of these calls to
    // mkdir will be creating directories that already exist.
    let zoom_dir = format!("{}/{}", dir, z);
    if !zooms[z as usize] {
        let _ = fs::create_dir(dir);
        let _ = fs::create_dir(&zoom_dir);
        zooms[z as usize] = true;
    }
    let column_dir = format!("{}/{}", zoom_dir, x);
    if columns.insert((z, x)) {
        let _ = fs::create_dir(&column_dir);
    }

    let path = format!("{}/{}{}", column_dir, y, extension);
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
    let fd = tippecanoe::platform::file_into_fd(f);
    // SAFETY: the File gave up `fd`, so this is the only close of it.
    if unsafe { libc::close(fd) } != 0 {
        let e = std::io::Error::last_os_error();
        return Err(Error::new(EXIT_CLOSE, io_message(&path, &e)));
    }
    Ok(())
}

impl TileSink for DirSink {
    /// Queues the tile for its writer. A failed write is reported by a later
    /// call, by `erase_zoom` or by `finish`.
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<()> {
        check_tile(z, x, y)?;
        if let Some(e) = self.failed() {
            return Err(e);
        }
        let column = (z as u64) << 32 | x as u64;
        let writer =
            (column.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize % self.senders.len();
        let job = Job::Tile {
            z,
            x,
            y,
            data: data.to_vec(),
        };
        self.senders[writer].send(job).map_err(|_| {
            self.failed()
                .unwrap_or_else(|| Error::new(EXIT_IMPOSSIBLE, "tile file writer stopped"))
        })
    }

    fn erase_zoom(&self, z: u8) -> Result<()> {
        self.flush()?;
        dir_erase_zoom(&self.dir, z as i32, self.tile_format)
    }

    fn finish(self: Box<Self>, metadata: &Metadata) -> Result<()> {
        self.finish_with(metadata, &[])
    }

    fn finish_with(
        mut self: Box<Self>,
        metadata: &Metadata,
        extra: &[(String, String)],
    ) -> Result<()> {
        self.stop();
        if let Some(e) = self.failed() {
            return Err(e);
        }
        write_metadata(
            &self.dir,
            &tileset_metadata(metadata, self.tile_format),
            extra,
        )
    }
}

fn numeric(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|c| c.is_ascii_digit())
}

/// Whether `s` names a tile file: digits, then `.pbf` or `.mvt` as in
/// tippecanoe, or `.mlt` when writing MapLibre Tiles.
fn tilename(s: &[u8], tile_format: TileFormat) -> bool {
    let digits = s.iter().take_while(|c| c.is_ascii_digit()).count();
    let ext = &s[digits..];
    ext == b".pbf" || ext == b".mvt" || (tile_format == TileFormat::Mlt && ext == b".mlt")
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
            .map(|e| e.file_name().as_encoded_bytes().to_vec())
            .collect()),
        Err(_) if !must => Ok(Vec::new()),
        Err(e) => Err(Error::new(EXIT_OPEN, io_message(dir, &e))),
    }
}

/// tippecanoe's `enumerate_dirtiles`, returning the `z/x/y.ext` paths in its
/// order and errors instead of exiting.
pub fn enumerate_dirtiles(
    dir: &str,
    minzoom: i32,
    maxzoom: i32,
    tile_format: TileFormat,
) -> Result<Vec<String>> {
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
                if tilename(&yname, tile_format) {
                    let ty = atoi(&yname);
                    let ext = if yname.windows(4).any(|w| w == b".mvt") {
                        ".mvt"
                    } else if yname.ends_with(b".mlt") {
                        ".mlt"
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
pub fn dir_erase_zoom(dir: &str, zoom: i32, tile_format: TileFormat) -> Result<()> {
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
                if tilename(&yname, tile_format) {
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
    write_metadata(dir, m, &[])
}

/// The metadata with more string keys at the end.
fn write_metadata(dir: &str, m: &Metadata, extra: &[(String, String)]) -> Result<()> {
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
        for (k, v) in extra {
            out(&mut state, k, v);
        }

        state.json_write_newline();
        state.json_end_hash();
        state.json_write_newline();
    }
    fp.flush()
        .map_err(|e| Error::new(EXIT_WRITE, io_message(&metadata, &e)))
}
