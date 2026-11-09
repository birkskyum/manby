//! The tilesets tile-join reads: PMTiles archives, MBTiles databases and
//! directories of tiles, each read in tile-join's order (zoom, then x, then
//! TMS y), with the same messages and exit statuses when they cannot be read.
//!
//! - **PMTiles.** The directories are read at the start. The tiles of each
//!   batch are read from a mapping of the archive made for that batch, so
//!   that the pages of tiles already done leave the process's memory with
//!   it. Reading tiles one at a time with `pread` instead takes longer, since
//!   the reads of one file do not overlap (152,000 random reads take 0.1 s
//!   on 1 thread and 0.15 s on 18).
//! - **MBTiles.** A thread runs tile-join's query and hands the rows over in
//!   chunks, so that reading overlaps with the work on the tiles before them.
//! - **Directories.** The files are listed at the start and read when needed.

use std::ffi::{CStr, c_char};
use std::fs::File;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};

use memmap2::Mmap;
use rusqlite::{Connection, OpenFlags, ffi};
use tippecanoe::errors::{EXIT_FAILURE, EXIT_JSON, EXIT_MEMORY, EXIT_OPEN, EXIT_SQLITE};
use tippecanoe::jsonpull::{JsonPull, JsonType};
use tippecanoe::pmtiles::{self, EntryV3, HeaderV3};

use crate::{Error, Result};

/// Where a tile's data is.
#[derive(Clone, Debug)]
pub enum Loc {
    /// In the PMTiles archive.
    Range { offset: u64, len: u32 },
    /// Read already (MBTiles rows, overzoomed tiles).
    Data(Arc<[u8]>),
    /// A file of the directory, `.pbf` or `.mvt`.
    File { mvt: bool },
}

/// A tile's data, borrowed from the archive where it can be.
pub enum TileData<'a> {
    Borrowed(&'a [u8]),
    Shared(Arc<[u8]>),
}

impl std::ops::Deref for TileData<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            TileData::Borrowed(b) => b,
            TileData::Shared(b) => b,
        }
    }
}

impl TileData<'_> {
    pub fn shared(self) -> Arc<[u8]> {
        match self {
            TileData::Borrowed(b) => Arc::from(b),
            TileData::Shared(b) => b,
        }
    }
}

/// A tile as the reader comes to it.
#[derive(Clone, Debug)]
pub struct TileRef {
    pub z: i64,
    pub x: i64,
    pub y: i64,
    pub loc: Loc,
}

impl TileRef {
    /// The size of the tile's data, where it is known without reading it.
    pub fn size(&self) -> u64 {
        match &self.loc {
            Loc::Range { len, .. } => *len as u64,
            Loc::Data(d) => d.len() as u64,
            Loc::File { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pmtiles,
    Mbtiles,
    Dir,
}

pub struct Source {
    /// The name as given, which messages and the metadata use.
    pub name: String,
    pub kind: Kind,
    /// The tileset says tippecanoe or tile-join made it (see `key.rs`).
    pub tippecanoe_made: bool,
    /// PMTiles: the archive, for reading tiles, and its mapping, for directories.
    file: Option<File>,
    map: Option<Mmap>,
    /// MBTiles: the database tile-join opened, for metadata and overzoom.
    db: Option<Mutex<Connection>>,
    /// MBTiles: the rows still to come.
    rows: Option<Mutex<Receiver<Vec<TileRef>>>>,
    /// PMTiles and directories: all the tiles, until taken.
    tiles: Mutex<Option<Vec<TileRef>>>,
}

fn sqlite_error(message: String) -> Error {
    Error::new(EXIT_SQLITE, message)
}

/// The exit status of a C++ program that aborts.
const EXIT_ABORT: i32 = 134;

/// What tippecanoe-rs prints for an exception that ends the program, which
/// it then aborts.
fn uncaught(type_name: &str, what: &str) -> Error {
    let message = if cfg!(target_os = "macos") {
        format!("libc++abi: terminating due to uncaught exception of type {type_name}: {what}")
    } else {
        format!("terminate called after throwing an instance of '{type_name}'\n  what():  {what}")
    };
    Error::new(EXIT_ABORT, message)
}

/// tile-join's `sqlite3_open`, which creates the database if it is missing,
/// with SQLite's message for a failure, without the file name rusqlite adds.
fn sqlite3_open(fname: &str) -> std::result::Result<Connection, String> {
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

/// A statement used through SQLite's C API, as tile-join does, so that the
/// column conversions are SQLite's own.
pub struct RawStmt {
    stmt: *mut ffi::sqlite3_stmt,
}

// SAFETY: a statement is used by one thread at a time, with its connection.
unsafe impl Send for RawStmt {}

impl RawStmt {
    pub fn prepare(db: &Connection, sql: &CStr) -> std::result::Result<RawStmt, String> {
        let mut stmt = std::ptr::null_mut();
        // SAFETY: the handle is valid while `db` is borrowed, and `sql` is NUL-terminated.
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
        Ok(RawStmt { stmt })
    }

    pub fn bind_int(&mut self, i: i32, v: i32) {
        // SAFETY: the statement is live until drop.
        unsafe {
            ffi::sqlite3_bind_int(self.stmt, i, v);
        }
    }

    pub fn step(&mut self) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_step(self.stmt) }
    }

    pub fn column_int(&self, i: i32) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_column_int(self.stmt, i) }
    }

    /// `sqlite3_column_text`, up to its NUL, or None for NULL.
    pub fn column_text(&self, i: i32) -> Option<&[u8]> {
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

    pub fn column_blob(&self, i: i32) -> &[u8] {
        // SAFETY: the blob stays valid until the next step, which needs `&mut self`.
        unsafe {
            let p = ffi::sqlite3_column_blob(self.stmt, i);
            let len = ffi::sqlite3_column_bytes(self.stmt, i);
            if p.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(p as *const u8, len as usize)
            }
        }
    }
}

impl Drop for RawStmt {
    fn drop(&mut self) {
        // SAFETY: the statement was prepared and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.stmt);
        }
    }
}

const TILES_QUERY: &CStr =
    c"SELECT zoom_level, tile_column, tile_row, tile_data from tiles order by zoom_level, tile_column, tile_row;";

/// Rows per chunk handed over by an MBTiles reader thread.
const CHUNK_ROWS: usize = 4096;

impl Source {
    /// Opens `fname` as tile-join's `tileset_reader` does: a directory, a
    /// `.pmtiles` archive, or else an MBTiles database. Only the tiles of
    /// zooms `minzoom` to `maxzoom` are listed for PMTiles and directories,
    /// as tile-join lists them.
    pub fn open(fname: &str, minzoom: i32, maxzoom: i32) -> Result<Source> {
        // S_IFDIR is 16 bits on macOS and 32 on Linux
        #[allow(clippy::unnecessary_cast)]
        let is_dir =
            std::fs::metadata(fname).is_ok_and(|st| (st.mode() & libc::S_IFDIR as u32) != 0);
        if is_dir {
            let tiles = enumerate_dirtiles(fname, minzoom, maxzoom)?;
            let generator = dir_generator(fname);
            Ok(Source {
                name: fname.to_string(),
                kind: Kind::Dir,
                tippecanoe_made: tippecanoe_generator(generator.as_deref()),
                file: None,
                map: None,
                db: None,
                rows: None,
                tiles: Mutex::new(Some(tiles)),
            })
        } else if tippecanoe::pmtiles_file::pmtiles_has_suffix(fname) {
            Source::open_pmtiles(fname, minzoom, maxzoom)
        } else {
            Source::open_mbtiles(fname)
        }
    }

    fn open_pmtiles(fname: &str, minzoom: i32, maxzoom: i32) -> Result<Source> {
        let mmap_error = |errno: i32| {
            Error::new(
                EXIT_MEMORY,
                format!("mmap in decode: {}", tippecanoe::cstd::strerror(errno)),
            )
        };
        let file = File::open(fname).map_err(|_| mmap_error(libc::EBADF))?;
        let len = file.metadata().map(|st| st.size()).unwrap_or(0);
        if len == 0 {
            return Err(mmap_error(libc::EINVAL));
        }
        // SAFETY: the archive is only read, and nothing writes it while it is mapped.
        let map = unsafe { Mmap::map(&file) }
            .map_err(|e| mmap_error(e.raw_os_error().unwrap_or(libc::EINVAL)))?;

        let header = read_header(&map)?;
        let mut entries = Vec::new();
        collect_entries(
            &map,
            &header,
            header.root_dir_offset,
            header.root_dir_bytes,
            &mut entries,
        )?;
        // pmtiles::entries_tms: a stable sort by zoom, x, then y reversed
        entries.sort_by(|a: &TileRef, b: &TileRef| {
            a.z.cmp(&b.z).then(a.x.cmp(&b.x)).then(b.y.cmp(&a.y))
        });
        entries.retain(|e| e.z >= minzoom as i64 && e.z <= maxzoom as i64);

        let generator = pmtiles_metadata(&map, &header)
            .ok()
            .and_then(|o| json_string(&o, "generator"));
        Ok(Source {
            name: fname.to_string(),
            kind: Kind::Pmtiles,
            tippecanoe_made: tippecanoe_generator(generator.as_deref()),
            file: Some(file),
            map: Some(map),
            db: None,
            rows: None,
            tiles: Mutex::new(Some(entries)),
        })
    }

    fn open_mbtiles(fname: &str) -> Result<Source> {
        let db = sqlite3_open(fname).map_err(|msg| sqlite_error(format!("{}: {}", fname, msg)))?;
        if let Err(e) = db.execute_batch("PRAGMA integrity_check;") {
            return Err(sqlite_error(format!("{}: integrity_check: {}", fname, e)));
        }
        RawStmt::prepare(&db, TILES_QUERY)
            .map_err(|msg| sqlite_error(format!("{}: select failed: {}", fname, msg)))?;
        let generator = metadata_text(&db, c"SELECT value from metadata where name = 'generator'")
            .map(|g| String::from_utf8_lossy(&g).into_owned());

        // The rows are read on a thread of their own, on a connection of its own.
        let (sender, receiver) = sync_channel::<Vec<TileRef>>(4);
        let path = fname.to_string();
        std::thread::Builder::new()
            .name("mbtiles reader".to_string())
            .spawn(move || {
                let Ok(db) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                else {
                    return;
                };
                let Ok(mut stmt) = RawStmt::prepare(&db, TILES_QUERY) else {
                    return;
                };
                let mut chunk = Vec::with_capacity(CHUNK_ROWS);
                // As in tile-join, anything but a row ends the tiles.
                while stmt.step() == ffi::SQLITE_ROW {
                    let zoom = stmt.column_int(0) as i64;
                    let tms_y = stmt.column_int(2);
                    chunk.push(TileRef {
                        z: zoom,
                        x: stmt.column_int(1) as i64,
                        y: 1i64
                            .wrapping_shl(zoom as u32)
                            .wrapping_sub(1)
                            .wrapping_sub(tms_y as i64),
                        loc: Loc::Data(Arc::from(stmt.column_blob(3))),
                    });
                    if chunk.len() == CHUNK_ROWS
                        && sender
                            .send(std::mem::replace(
                                &mut chunk,
                                Vec::with_capacity(CHUNK_ROWS),
                            ))
                            .is_err()
                    {
                        return;
                    }
                }
                if !chunk.is_empty() {
                    let _ = sender.send(chunk);
                }
            })
            .map_err(|e| Error::new(EXIT_FAILURE, format!("mbtiles reader: {}", e)))?;

        Ok(Source {
            name: fname.to_string(),
            kind: Kind::Mbtiles,
            tippecanoe_made: tippecanoe_generator(generator.as_deref()),
            file: None,
            map: None,
            db: Some(Mutex::new(db)),
            rows: Some(Mutex::new(receiver)),
            tiles: Mutex::new(None),
        })
    }

    /// The next tiles in reading order, or None after the last.
    pub fn next_chunk(&self) -> Option<Vec<TileRef>> {
        if let Some(tiles) = self.tiles.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Some(tiles);
        }
        let rows = self.rows.as_ref()?;
        rows.lock().unwrap_or_else(|p| p.into_inner()).recv().ok()
    }

    /// The data of tile `t`, as tile-join's reader gives it: empty for a
    /// missing file of a directory.
    pub fn data(&self, t: &TileRef) -> Result<TileData<'_>> {
        self.data_in(t, None)
    }

    /// A mapping of a PMTiles archive for reading a batch of tiles with
    /// [`Source::data_in`]. Its pages leave the process's memory with it.
    pub fn batch_map(&self) -> Option<Mmap> {
        let file = self.file.as_ref()?;
        // SAFETY: the archive is only read, and nothing writes it while it is mapped.
        unsafe { Mmap::map(file) }.ok()
    }

    /// The data of tile `t`, from `map` if it is a mapping of this archive.
    pub fn data_in<'a>(&'a self, t: &TileRef, map: Option<&'a Mmap>) -> Result<TileData<'a>> {
        match &t.loc {
            Loc::Data(d) => Ok(TileData::Shared(d.clone())),
            Loc::Range { offset, len } if map.is_some() => {
                let start = *offset as usize;
                match map.and_then(|m| m.get(start..start.saturating_add(*len as usize))) {
                    Some(b) => Ok(TileData::Borrowed(b)),
                    None => Err(Error::new(
                        EXIT_FAILURE,
                        format!(
                            "{}: tile {}/{}/{} is past the end of the archive",
                            self.name, t.z, t.x, t.y
                        ),
                    )),
                }
            }
            Loc::Range { offset, len } => {
                let file = self
                    .file
                    .as_ref()
                    .expect("a PMTiles source has its archive");
                let mut buf = vec![0u8; *len as usize];
                file.read_exact_at(&mut buf, *offset).map_err(|_| {
                    Error::new(
                        EXIT_FAILURE,
                        format!(
                            "{}: tile {}/{}/{} is past the end of the archive",
                            self.name, t.z, t.x, t.y
                        ),
                    )
                })?;
                Ok(TileData::Shared(buf.into()))
            }
            Loc::File { mvt } => {
                let path = format!(
                    "{}/{}/{}/{}{}",
                    self.name,
                    t.z,
                    t.x,
                    t.y,
                    if *mvt { ".mvt" } else { ".pbf" }
                );
                Ok(TileData::Shared(
                    std::fs::read(path).unwrap_or_default().into(),
                ))
            }
        }
    }

    /// tile-join's `get_tile`: the data of z/x/y, looked up directly, for
    /// overzooming. Empty if there is no such tile.
    pub fn get_tile(&self, z: i64, x: i64, y: i64) -> Result<Vec<u8>> {
        match self.kind {
            Kind::Mbtiles => {
                let db = self
                    .db
                    .as_ref()
                    .expect("an MBTiles source has its database");
                let db = db.lock().unwrap_or_else(|p| p.into_inner());
                let mut q = RawStmt::prepare(
                    &db,
                    c"SELECT tile_data from tiles where zoom_level = ? and tile_column = ? and tile_row = ?;",
                )
                .map_err(|msg| sqlite_error(format!("{}: select failed: {}", self.name, msg)))?;
                q.bind_int(1, z as i32);
                q.bind_int(2, x as i32);
                q.bind_int(
                    3,
                    1i64.wrapping_shl(z as u32).wrapping_sub(1).wrapping_sub(y) as i32,
                );
                if q.step() == ffi::SQLITE_ROW {
                    return Ok(q.column_blob(0).to_vec());
                }
                Ok(Vec::new())
            }
            Kind::Pmtiles => {
                let map = self.map.as_ref().expect("a PMTiles source has its mapping");
                let (offset, len) = get_tile(map, z as u8, x as u32, y as u32)?;
                if len == 0 {
                    return Ok(Vec::new());
                }
                let end = offset as usize + len as usize;
                if end > map.len() {
                    return Err(Error::new(
                        EXIT_FAILURE,
                        format!(
                            "{}: tile {}/{}/{} is past the end of the archive",
                            self.name, z, x, y
                        ),
                    ));
                }
                Ok(map[offset as usize..end].to_vec())
            }
            Kind::Dir => {
                let t = TileRef {
                    z,
                    x,
                    y,
                    loc: Loc::File { mvt: false },
                };
                Ok(self.data(&t)?.to_vec())
            }
        }
    }

    /// The metadata as tile-join reads it when it closes a reader: the
    /// database itself, or a temporary one that tile-join fills from the
    /// PMTiles header and JSON or from metadata.json.
    pub fn metadata_db(&self) -> Result<MetadataDb<'_>> {
        match self.kind {
            Kind::Mbtiles => {
                let db = self
                    .db
                    .as_ref()
                    .expect("an MBTiles source has its database");
                Ok(MetadataDb::Borrowed(
                    db.lock().unwrap_or_else(|p| p.into_inner()),
                ))
            }
            Kind::Pmtiles => {
                let map = self.map.as_ref().expect("a PMTiles source has its mapping");
                let header = read_header(map)?;
                // the checks pmtilesmeta2tmp makes before it reads the
                // metadata, so that it has no reason to exit
                if header.internal_compression != pmtiles::COMPRESSION_NONE
                    && header.internal_compression != pmtiles::COMPRESSION_GZIP
                {
                    return Err(Error::new(
                        EXIT_OPEN,
                        format!(
                            "Unknown or unsupported pmtiles compression: {}",
                            header.internal_compression
                        ),
                    ));
                }
                match pmtiles_metadata(map, &header) {
                    Ok(_) => {}
                    Err(None) => {
                        return Err(Error::new(
                            EXIT_JSON,
                            format!("{}: bad metadata format", self.name),
                        ));
                    }
                    Err(Some(e)) => {
                        return Err(Error::new(
                            EXIT_JSON,
                            format!("{}: metadata parsing error: {}", self.name, e),
                        ));
                    }
                }
                Ok(MetadataDb::Owned(
                    tippecanoe::pmtiles_file::pmtilesmeta2tmp(&self.name, map),
                ))
            }
            Kind::Dir => {
                let name = self.name.clone() + "/metadata.json";
                if let Ok(f) = File::open(&name) {
                    let mut jp = JsonPull::begin_file(Box::new(std::io::BufReader::new(f)));
                    match jp.read_tree() {
                        None => {
                            return Err(Error::new(
                                EXIT_JSON,
                                format!(
                                    "{}: metadata parsing error: {}",
                                    name,
                                    jp.error.unwrap_or("(null)")
                                ),
                            ));
                        }
                        Some(o) if o.root().type_() != JsonType::Hash => {
                            return Err(Error::new(
                                EXIT_JSON,
                                format!("{}: bad metadata format", name),
                            ));
                        }
                        Some(_) => {}
                    }
                }
                Ok(MetadataDb::Owned(tippecanoe::dirtiles::dirmeta2tmp(
                    &self.name,
                )))
            }
        }
    }
}

pub enum MetadataDb<'a> {
    Borrowed(std::sync::MutexGuard<'a, Connection>),
    Owned(Connection),
}

impl MetadataDb<'_> {
    pub fn conn(&self) -> &Connection {
        match self {
            MetadataDb::Borrowed(c) => c,
            MetadataDb::Owned(c) => c,
        }
    }
}

/// The first value a metadata query finds, as text.
pub fn metadata_text(db: &Connection, sql: &CStr) -> Option<Vec<u8>> {
    let mut stmt = RawStmt::prepare(db, sql).ok()?;
    if stmt.step() != ffi::SQLITE_ROW {
        return None;
    }
    stmt.column_text(0).map(|s| s.to_vec())
}

/// The first value a metadata query finds, as `sqlite3_column_int` reads it.
pub fn metadata_int(db: &Connection, sql: &CStr) -> Option<i32> {
    let mut stmt = RawStmt::prepare(db, sql).ok()?;
    if stmt.step() != ffi::SQLITE_ROW {
        return None;
    }
    Some(stmt.column_int(0))
}

/// Whether the `generator` in a tileset's metadata names tippecanoe or
/// tile-join, which store their tiles as zlib compresses them.
fn tippecanoe_generator(generator: Option<&str>) -> bool {
    let Some(g) = generator else {
        return false;
    };
    let program = g.split(' ').next().unwrap_or("");
    program == "tippecanoe" || program == "tile-join"
}

fn dir_generator(fname: &str) -> Option<String> {
    let f = File::open(format!("{}/metadata.json", fname)).ok()?;
    let mut jp = JsonPull::begin_file(Box::new(std::io::BufReader::new(f)));
    let o = jp.read_tree()?;
    json_string(&o, "generator")
}

fn json_string(o: &tippecanoe::jsonpull::JsonTree, key: &str) -> Option<String> {
    if o.root().type_() != JsonType::Hash {
        return None;
    }
    let v = o.root().hash_get(key)?;
    if v.type_() != JsonType::String {
        return None;
    }
    Some(v.string().to_string())
}

/// The JSON metadata of a PMTiles archive, parsed. The error is the parser's
/// message, or None if the JSON is not an object.
fn pmtiles_metadata(
    map: &[u8],
    header: &HeaderV3,
) -> std::result::Result<tippecanoe::jsonpull::JsonTree, Option<String>> {
    let start = header.json_metadata_offset as usize;
    let end = start.saturating_add(header.json_metadata_bytes as usize);
    let json_s = map
        .get(start..end)
        .ok_or(Some("past the end of the archive".to_string()))?;
    let mut json = Vec::new();
    if header.internal_compression == pmtiles::COMPRESSION_GZIP {
        let mut d = flate2::Decompress::new_gzip(15);
        json.reserve(json_s.len() * 8 + 1024);
        loop {
            let before = d.total_in() as usize;
            match d.decompress_vec(&json_s[before..], &mut json, flate2::FlushDecompress::None) {
                Ok(flate2::Status::StreamEnd) => break,
                Ok(_) if json.len() == json.capacity() => json.reserve(json.capacity()),
                Ok(_) if d.total_in() as usize == before => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    } else {
        json = json_s.to_vec();
    }
    let end = json.iter().position(|&c| c == 0).unwrap_or(json.len());
    let mut jp = JsonPull::begin_string(&json[..end]);
    match jp.read_tree() {
        Some(o) if o.root().type_() == JsonType::Hash => Ok(o),
        Some(_) => Err(None),
        None => Err(Some(jp.error.unwrap_or("(null)").to_string())),
    }
}

/// `pmtiles::deserialize_header`, with its exceptions as errors.
fn read_header(map: &[u8]) -> Result<HeaderV3> {
    let mut s = vec![0u8; 127];
    let n = map.len().min(127);
    s[..n].copy_from_slice(&map[..n]);
    if &s[0..7] != b"PMTiles" {
        return Err(uncaught(
            "pmtiles::pmtiles_magic_number_exception",
            "pmtiles magic number exception",
        ));
    }
    if s[7] != 0x3 {
        return Err(uncaught(
            "pmtiles::pmtiles_version_exception",
            "pmtiles version: must be 3",
        ));
    }
    Ok(pmtiles::deserialize_header(&s))
}

/// pmtiles.hpp's `decode_varint`.
fn decode_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut val: u64 = 0;
    let mut shift = 0;
    loop {
        let Some(&b) = data.get(*pos) else {
            return Err(uncaught(
                "pmtiles::end_of_buffer_exception",
                "end of buffer exception",
            ));
        };
        *pos += 1;
        val |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(val);
        }
        shift += 7;
        if shift >= 70 {
            return Err(uncaught(
                "pmtiles::varint_too_long_exception",
                "varint too long exception",
            ));
        }
    }
}

/// `pmtiles::deserialize_directory`.
fn deserialize_directory(d: &[u8]) -> Result<Vec<EntryV3>> {
    let mut t = 0;
    let n = decode_varint(d, &mut t)? as usize;
    let mut result = vec![EntryV3::default(); n.min(d.len())];
    if n > d.len() {
        // more entries than bytes cannot be read
        return Err(uncaught(
            "pmtiles::end_of_buffer_exception",
            "end of buffer exception",
        ));
    }
    let mut last_id: u64 = 0;
    for e in result.iter_mut() {
        last_id = last_id.wrapping_add(decode_varint(d, &mut t)?);
        e.tile_id = last_id;
    }
    for e in result.iter_mut() {
        e.run_length = decode_varint(d, &mut t)? as u32;
    }
    for e in result.iter_mut() {
        e.length = decode_varint(d, &mut t)? as u32;
    }
    for i in 0..result.len() {
        let tmp = decode_varint(d, &mut t)?;
        result[i].offset = if i > 0 && tmp == 0 {
            result[i - 1]
                .offset
                .wrapping_add(result[i - 1].length as u64)
        } else {
            tmp.wrapping_sub(1)
        };
    }
    if t != d.len() {
        return Err(Error::new(
            EXIT_FAILURE,
            "Error: malformed pmtiles directory",
        ));
    }
    Ok(result)
}

/// A directory of the archive, decompressed.
fn directory(map: &[u8], header: &HeaderV3, offset: u64, len: u64) -> Result<Vec<EntryV3>> {
    let raw = map
        .get(offset as usize..(offset as usize).saturating_add(len as usize))
        .ok_or_else(|| uncaught("std::out_of_range", "pmtiles directory past the end"))?;
    match header.internal_compression {
        pmtiles::COMPRESSION_NONE => deserialize_directory(raw),
        pmtiles::COMPRESSION_GZIP => {
            let mut out = Vec::new();
            if tippecanoe::mvt::decompress(raw, &mut out) == 0 {
                // tippecanoe-rs goes on with what came out
            }
            deserialize_directory(&out)
        }
        _ => Err(uncaught(
            "std::runtime_error",
            "Unknown or unsupported compression.",
        )),
    }
}

/// `pmtiles::collect_entries`.
fn collect_entries(
    map: &[u8],
    h: &HeaderV3,
    offset: u64,
    len: u64,
    out: &mut Vec<TileRef>,
) -> Result<()> {
    for entry in directory(map, h, offset, len)? {
        if entry.run_length == 0 {
            collect_entries(
                map,
                h,
                h.leaf_dirs_offset + entry.offset,
                entry.length as u64,
                out,
            )?;
        } else {
            for id in entry.tile_id..entry.tile_id + entry.run_length as u64 {
                let (z, x, y) = tileid_to_zxy(id)?;
                out.push(TileRef {
                    z: z as i64,
                    x: x as i64,
                    y: y as i64,
                    loc: Loc::Range {
                        offset: h.tile_data_offset + entry.offset,
                        len: entry.length,
                    },
                });
            }
        }
    }
    Ok(())
}

/// `pmtiles::tileid_to_zxy`, with its overflow as an error.
fn tileid_to_zxy(id: u64) -> Result<(u8, u32, u32)> {
    let mut acc: u64 = 0;
    for z in 0u8..32 {
        let n = (1u64 << z) * (1u64 << z);
        if acc + n > id {
            let t = pmtiles::tileid_to_zxy(id);
            return Ok((t.z, t.x, t.y));
        }
        acc += n;
    }
    Err(uncaught(
        "std::overflow_error",
        "tile zoom exceeds 64-bit limit",
    ))
}

/// `pmtiles::find_tile`, including that it narrows the difference of tile
/// ids to an int.
fn find_tile(entries: &[EntryV3], tile_id: u64) -> EntryV3 {
    let mut m: i32 = 0;
    let mut n: i32 = (entries.len() as i32).wrapping_sub(1);
    while m <= n {
        let k = (n + m) >> 1;
        let cmp = tile_id.wrapping_sub(entries[k as usize].tile_id) as i32;
        if cmp > 0 {
            m = k + 1;
        } else if cmp < 0 {
            n = k - 1;
        } else {
            return entries[k as usize];
        }
    }
    if n >= 0 {
        if entries[n as usize].run_length == 0 {
            return entries[n as usize];
        }
        if tile_id.wrapping_sub(entries[n as usize].tile_id) < entries[n as usize].run_length as u64
        {
            return entries[n as usize];
        }
    }
    EntryV3::new(0, 0, 0, 0)
}

/// `pmtiles::get_tile`: the offset and length of z/x/y, or (0, 0).
fn get_tile(map: &[u8], z: u8, x: u32, y: u32) -> Result<(u64, u32)> {
    if z > 31 {
        return Err(uncaught(
            "std::overflow_error",
            "tile zoom exceeds 64-bit limit",
        ));
    }
    if x > (1u32 << z).wrapping_sub(1) || y > (1u32 << z).wrapping_sub(1) {
        return Err(uncaught(
            "std::overflow_error",
            "tile x/y outside zoom level bounds",
        ));
    }
    let tile_id = pmtiles::zxy_to_tileid(z, x, y);
    let h = read_header(map)?;
    let mut dir_offset = h.root_dir_offset;
    let mut dir_length = h.root_dir_bytes as u32;
    for _depth in 0..=3 {
        let entries = directory(map, &h, dir_offset, dir_length as u64)?;
        let entry = find_tile(&entries, tile_id);
        if entry.length > 0 {
            if entry.run_length > 0 {
                return Ok((h.tile_data_offset + entry.offset, entry.length));
            }
            dir_offset = h.leaf_dirs_offset + entry.offset;
            dir_length = entry.length;
        } else {
            return Ok((0, 0));
        }
    }
    Ok((0, 0))
}

fn numeric(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|c| c.is_ascii_digit())
}

fn pbfname(s: &[u8]) -> bool {
    let i = s
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(s.len());
    &s[i..] == b".pbf" || &s[i..] == b".mvt"
}

fn atoi_name(s: &[u8]) -> i32 {
    tippecanoe::cstd::atoi(&String::from_utf8_lossy(s))
}

/// `enumerate_dirtiles`, returning errors instead of exiting: the tiles of
/// the zooms `minzoom` to `maxzoom`, sorted by zoom, x and y reversed.
fn enumerate_dirtiles(fname: &str, minzoom: i32, maxzoom: i32) -> Result<Vec<TileRef>> {
    use std::os::unix::ffi::OsStrExt;
    let open_error = |path: &str, e: std::io::Error| {
        Error::new(
            EXIT_OPEN,
            format!("{}: {}", path, crate::options::os_message(&e)),
        )
    };
    let mut tiles: Vec<(i64, i64, i64, bool)> = Vec::new();
    if let Ok(d1) = std::fs::read_dir(fname) {
        for dp in d1.map_while(std::result::Result::ok) {
            let name = dp.file_name();
            let name = name.as_bytes();
            if !(numeric(name) && atoi_name(name) >= minzoom && atoi_name(name) <= maxzoom) {
                continue;
            }
            let z = fname.to_string() + "/" + &String::from_utf8_lossy(name);
            let tz = atoi_name(name);
            let d2 = std::fs::read_dir(&z).map_err(|e| open_error(&z, e))?;
            for dp2 in d2.map_while(std::result::Result::ok) {
                let name2 = dp2.file_name();
                let name2 = name2.as_bytes();
                if !numeric(name2) {
                    continue;
                }
                let x = z.clone() + "/" + &String::from_utf8_lossy(name2);
                let tx = atoi_name(name2);
                let d3 = std::fs::read_dir(&x).map_err(|e| open_error(&x, e))?;
                for dp3 in d3.map_while(std::result::Result::ok) {
                    let name3 = dp3.file_name();
                    let name3 = name3.as_bytes();
                    if pbfname(name3) {
                        let mvt = name3.windows(4).any(|w| w == b".mvt");
                        tiles.push((tz as i64, tx as i64, atoi_name(name3) as i64, mvt));
                    }
                }
            }
        }
    }
    // Zxy's order, which leaves out the extension; the sort is stable.
    tiles.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
    Ok(tiles
        .into_iter()
        .map(|(z, x, y, mvt)| TileRef {
            z,
            x,
            y,
            loc: Loc::File { mvt },
        })
        .collect())
}
