//! MBTiles, written by a single thread in large transactions.
//!
//! The database has tippecanoe's schema, pragmas, rows and metadata. Callers
//! hash their tiles on their own threads and hand them to the writer thread
//! over a bounded channel; the writer inserts them with prepared statements and
//! commits every so often instead of after every tile, as tippecanoe does.

use std::collections::HashSet;
use std::ffi::{CStr, c_double, c_int};
use std::fs;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use rusqlite::Connection;
use tippecanoe::errors::{EXIT_CLOSE, EXIT_EXISTS, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_SQLITE};
use tippecanoe::mbtiles::{Metadata, c_str, sqlite3_errmsg, sqlite3_exec, sqlite3_open};
use tippecanoe::text::fnv1a;
use tippecanoe::write_json::JsonWriter;

use super::{SinkSettings, TileSink, check_tile, tileset_metadata};
use crate::{Error, Result};

/// Tiles or bytes after which the writer commits and starts a new transaction.
const BATCH_TILES: usize = 100_000;
const BATCH_BYTES: usize = 256 << 20;

/// `sqlite3_mprintf(fmt, ...)`, returning an owned copy. Each argument must
/// have the C type its conversion expects.
macro_rules! mprintf {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        // SAFETY: the format is a C string literal, the arguments have the C
        // types its conversions expect and outlive the call, and sqlite3_take
        // takes over the result.
        unsafe {
            tippecanoe::mbtiles::sqlite3_take(rusqlite::ffi::sqlite3_mprintf(
                ($fmt as &CStr).as_ptr() $(, $arg)*
            ))
        }
    };
}

enum Message {
    Tile {
        z: u8,
        x: u32,
        y: u32,
        hash: u64,
        data: Vec<u8>,
    },
    Erase {
        z: u8,
        reply: SyncSender<Result<()>>,
    },
    Finish {
        metadata: Box<Metadata>,
    },
}

pub struct MbtilesSink {
    sender: Option<SyncSender<Message>>,
    writer: Option<JoinHandle<Result<()>>>,
    /// The writer's error, once it has stopped because of one.
    failure: Arc<Mutex<Option<(i32, String)>>>,
    tile_format: crate::mlt::TileFormat,
}

fn sqlite_error(message: String) -> Error {
    Error::new(EXIT_SQLITE, message)
}

impl MbtilesSink {
    /// Opens or creates `path` the way tippecanoe does for `-o x.mbtiles`
    /// (`mbtiles_open`), and starts the writer thread.
    pub fn create(path: &str, settings: &SinkSettings) -> Result<MbtilesSink> {
        if settings.force {
            let _ = fs::remove_file(path);
        }
        let db = mbtiles_open(path, settings)?;

        let (sender, receiver) = sync_channel(4096);
        let failure = Arc::new(Mutex::new(None));
        let writer_failure = failure.clone();
        let allow_existing = settings.allow_existing;
        let writer = std::thread::Builder::new()
            .name("mbtiles writer".to_string())
            .spawn(move || {
                let result = write_tiles(db, receiver, allow_existing);
                if let Err(e) = &result {
                    *writer_failure.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some((e.exit_code, e.message.clone()));
                }
                result
            })
            .map_err(|e| Error::new(EXIT_IMPOSSIBLE, format!("mbtiles writer thread: {}", e)))?;

        Ok(MbtilesSink {
            sender: Some(sender),
            writer: Some(writer),
            failure,
            tile_format: settings.tile_format,
        })
    }

    fn send(&self, message: Message) -> Result<()> {
        let sent = self.sender.as_ref().map(|s| s.send(message));
        match sent {
            Some(Ok(())) => Ok(()),
            _ => Err(self.writer_error()),
        }
    }

    fn writer_error(&self) -> Error {
        match &*self.failure.lock().unwrap_or_else(|p| p.into_inner()) {
            Some((code, message)) => Error::new(*code, message.clone()),
            None => Error::new(EXIT_IMPOSSIBLE, "mbtiles writer stopped"),
        }
    }
}

impl Drop for MbtilesSink {
    fn drop(&mut self) {
        // Without finish(), the writer commits what it has and closes.
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

impl TileSink for MbtilesSink {
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<()> {
        check_tile(z, x, y)?;
        self.send(Message::Tile {
            z,
            x,
            y,
            hash: fnv1a(data),
            data: data.to_vec(),
        })
    }

    fn erase_zoom(&self, z: u8) -> Result<()> {
        let (reply, answer) = sync_channel(1);
        self.send(Message::Erase { z, reply })?;
        answer.recv().unwrap_or_else(|_| Err(self.writer_error()))
    }

    fn finish(mut self: Box<Self>, metadata: &Metadata) -> Result<()> {
        let sent = self.send(Message::Finish {
            metadata: Box::new(tileset_metadata(metadata, self.tile_format).into_owned()),
        });
        self.sender.take();
        let joined = match self.writer.take() {
            Some(writer) => writer
                .join()
                .unwrap_or_else(|_| Err(Error::new(EXIT_IMPOSSIBLE, "mbtiles writer panicked"))),
            None => Ok(()),
        };
        sent.and(joined)
    }
}

/// tippecanoe's `mbtiles_open`, returning errors instead of exiting. With
/// `allow_existing`, the complaints about tables that already exist are
/// printed and the database is used as it is, as in tippecanoe.
pub fn mbtiles_open(path: &str, settings: &SinkSettings) -> Result<Connection> {
    let program = &settings.program;
    let db = sqlite3_open(path)
        .map_err(|msg| Error::new(EXIT_OPEN, format!("{}: {}: {}", program, path, msg)))?;

    for pragma in [
        c"PRAGMA synchronous=0",
        c"PRAGMA locking_mode=EXCLUSIVE",
        c"PRAGMA journal_mode=DELETE",
    ] {
        sqlite3_exec(&db, pragma)
            .map_err(|err| sqlite_error(format!("{}: async: {}", program, err)))?;
    }

    let creates: [(&CStr, &str); 7] = [
        (c"CREATE TABLE metadata (name text, value text);", ""),
        (c"create unique index name on metadata (name);", "index metadata"),
        // "map" maps z/x/y coordinates to a content hash
        (
            c"CREATE TABLE map (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_id TEXT);",
            "create map table",
        ),
        (
            c"CREATE UNIQUE INDEX map_index ON map (zoom_level, tile_column, tile_row);",
            "create map index",
        ),
        // "images" maps a content hash to tile contents, per zoom level
        (
            c"CREATE TABLE images (zoom_level integer, tile_data blob, tile_id text);",
            "create images table",
        ),
        (
            c"CREATE UNIQUE INDEX images_id ON images (zoom_level, tile_id);",
            "create images index",
        ),
        // "tiles" is a view that retrieves content from "images"
        // via the content hash looked up from "map".
        (
            c"CREATE VIEW tiles AS SELECT map.zoom_level AS zoom_level, map.tile_column AS tile_column, map.tile_row AS tile_row, images.tile_data AS tile_data FROM map JOIN images ON images.tile_id = map.tile_id and images.zoom_level = map.zoom_level;",
            "create tiles view",
        ),
    ];
    for (sql, what) in creates {
        if let Err(err) = sqlite3_exec(&db, sql) {
            let message = if what.is_empty() {
                format!(
                    "{}: Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.\n{}: {}",
                    program, path, program, err
                )
            } else {
                format!("{}: {}: {}", program, what, err)
            };
            if !settings.allow_existing {
                return Err(Error::new(EXIT_EXISTS, message));
            }
            eprintln!("{}", message);
        }
    }

    // A larger page cache makes the inserts faster; it is not stored in the file.
    sqlite3_exec(&db, c"PRAGMA cache_size=-262144")
        .map_err(|err| sqlite_error(format!("cache_size: {}", err)))?;

    Ok(db)
}

/// The writer thread: inserts tiles until `Finish`, then writes the metadata
/// and closes the database as `mbtiles_close` does.
fn write_tiles(db: Connection, receiver: Receiver<Message>, allow_existing: bool) -> Result<()> {
    let mut finish: Option<Box<Metadata>> = None;
    {
        let begin = |db: &Connection| {
            sqlite3_exec(db, c"BEGIN").map_err(|e| sqlite_error(format!("BEGIN: {}", e)))
        };
        let commit = |db: &Connection| {
            sqlite3_exec(db, c"COMMIT").map_err(|e| sqlite_error(format!("COMMIT: {}", e)))
        };

        let prepare = |sql: &str, what: &str| {
            db.prepare(sql)
                .map_err(|_| sqlite_error(format!("sqlite3 {} prep failed", what)))
        };
        let mut images = prepare(
            "replace into images (zoom_level, tile_id, tile_data) values (?, ?, ?)",
            "images",
        )?;
        let mut map = prepare(
            "insert into map (zoom_level, tile_column, tile_row, tile_id) values (?, ?, ?, ?)",
            "map",
        )?;
        let mut delete_map = prepare("delete from map where zoom_level = ?", "delete map")?;
        let mut delete_images =
            prepare("delete from images where zoom_level = ?", "delete images")?;

        // (zoom, hash) of the images this writer has stored, so that a tile
        // seen before at the same zoom does not replace its row again.
        let mut stored: HashSet<(u8, u64)> = HashSet::new();
        let mut tiles_in_batch = 0usize;
        let mut bytes_in_batch = 0usize;
        begin(&db)?;

        for message in receiver.iter() {
            match message {
                Message::Tile {
                    z,
                    x,
                    y,
                    hash,
                    data,
                } => {
                    let hash_text = hash.to_string();
                    if stored.insert((z, hash)) {
                        images
                            .execute(rusqlite::params![z as i32, hash_text, data])
                            .map_err(|_| {
                                sqlite_error(format!(
                                    "sqlite3 images insert failed: {}",
                                    sqlite3_errmsg(&db)
                                ))
                            })?;
                    }
                    let tile_row = ((1i64 << z) - 1 - y as i64) as i32;
                    map.execute(rusqlite::params![z as i32, x as i32, tile_row, hash_text])
                        .map_err(|_| {
                            sqlite_error(format!(
                                "sqlite3 map insert failed: {}",
                                sqlite3_errmsg(&db)
                            ))
                        })?;

                    tiles_in_batch += 1;
                    bytes_in_batch += data.len();
                    if tiles_in_batch >= BATCH_TILES || bytes_in_batch >= BATCH_BYTES {
                        commit(&db)?;
                        begin(&db)?;
                        tiles_in_batch = 0;
                        bytes_in_batch = 0;
                    }
                }
                Message::Erase { z, reply } => {
                    let erased = delete_map
                        .execute([z as i32])
                        .map_err(|_| {
                            sqlite_error(format!(
                                "sqlite3 delete map failed: {}",
                                sqlite3_errmsg(&db)
                            ))
                        })
                        .and_then(|_| {
                            delete_images.execute([z as i32]).map_err(|_| {
                                sqlite_error(format!(
                                    "sqlite3 delete images failed: {}",
                                    sqlite3_errmsg(&db)
                                ))
                            })
                        })
                        .map(|_| ());
                    stored.retain(|&(zoom, _)| zoom != z);
                    let failed = erased
                        .as_ref()
                        .err()
                        .map(|e| Error::new(e.exit_code, e.message.clone()));
                    let _ = reply.send(erased);
                    if let Some(e) = failed {
                        return Err(e);
                    }
                }
                Message::Finish { metadata } => {
                    finish = Some(metadata);
                    break;
                }
            }
        }
        commit(&db)?;
    }

    if let Some(m) = finish {
        mbtiles_write_metadata(&db, &m, allow_existing)?;
    }
    mbtiles_close(db)
}

/// tippecanoe's `mbtiles_close`.
fn mbtiles_close(db: Connection) -> Result<()> {
    sqlite3_exec(&db, c"ANALYZE;")
        .map_err(|err| sqlite_error(format!("ANALYZE failed: {}", err)))?;
    db.close().map_err(|(db, _)| {
        Error::new(
            EXIT_CLOSE,
            format!("could not close database: {}", sqlite3_errmsg(&db)),
        )
    })
}

/// Runs one metadata `INSERT`. On failure, the error is returned, or with
/// `forcetable` printed and passed over, as in tippecanoe.
fn exec_metadata(db: &Connection, sql: &CStr, what: &str, forcetable: bool) -> Result<()> {
    if let Err(err) = sqlite3_exec(db, sql) {
        let message = format!("{}: {}", what, err);
        if !forcetable {
            return Err(sqlite_error(message));
        }
        eprintln!("{}", message);
    }
    Ok(())
}

/// tippecanoe's `mbtiles_write_metadata`, returning errors instead of exiting.
pub fn mbtiles_write_metadata(db: &Connection, m: &Metadata, forcetable: bool) -> Result<()> {
    let name = c_str(&m.name);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('name', %Q);",
        name.as_ptr()
    );
    exec_metadata(db, &sql, "set name in metadata", forcetable)?;

    let description = c_str(&m.description);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('description', %Q);",
        description.as_ptr()
    );
    exec_metadata(db, &sql, "set description in metadata", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('version', %d);",
        m.version as c_int
    );
    exec_metadata(db, &sql, "set version ", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('minzoom', %d);",
        m.minzoom as c_int
    );
    exec_metadata(db, &sql, "set minzoom", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('maxzoom', %d);",
        m.maxzoom as c_int
    );
    exec_metadata(db, &sql, "set maxzoom", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('center', '%f,%f,%d');",
        m.center_lon as c_double,
        m.center_lat as c_double,
        m.center_z as c_int
    );
    exec_metadata(db, &sql, "set center", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('bounds', '%f,%f,%f,%f');",
        m.minlon as c_double,
        m.minlat as c_double,
        m.maxlon as c_double,
        m.maxlat as c_double
    );
    exec_metadata(db, &sql, "set bounds", forcetable)?;

    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('antimeridian_adjusted_bounds', '%f,%f,%f,%f');",
        m.minlon2 as c_double,
        m.minlat2 as c_double,
        m.maxlon2 as c_double,
        m.maxlat2 as c_double
    );
    exec_metadata(db, &sql, "set bounds", forcetable)?;

    let type_ = c_str(&m.type_);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('type', %Q);",
        type_.as_ptr()
    );
    exec_metadata(db, &sql, "set type", forcetable)?;

    if !m.attribution.is_empty() {
        let attribution = c_str(&m.attribution);
        let sql = mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('attribution', %Q);",
            attribution.as_ptr()
        );
        exec_metadata(db, &sql, "set attribution", forcetable)?;
    }

    let format = c_str(&m.format);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('format', %Q);",
        format.as_ptr()
    );
    exec_metadata(db, &sql, "set format", forcetable)?;

    let generator = c_str(&m.generator);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('generator', %Q);",
        generator.as_ptr()
    );
    exec_metadata(db, &sql, "set generator", forcetable)?;

    let generator_options = c_str(&m.generator_options);
    let sql = mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('generator_options', %Q);",
        generator_options.as_ptr()
    );
    exec_metadata(db, &sql, "set commandline", forcetable)?;

    if !m.strategies_json.is_empty() {
        let strategies = c_str(&m.strategies_json);
        let sql = mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('strategies', %Q);",
            strategies.as_ptr()
        );
        exec_metadata(db, &sql, "set strategies", forcetable)?;
    }

    if !m.decisions_json.is_empty() {
        let decisions = c_str(&m.decisions_json);
        let sql = mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('tippecanoe_decisions', %Q);",
            decisions.as_ptr()
        );
        exec_metadata(db, &sql, "set decisions", forcetable)?;
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
            }
            if !m.tilestats_json.is_empty() {
                state.nospace = true;
                state.json_write_string("tilestats");
                state.nospace = true;
                state.json_write_json(&m.tilestats_json);
            }

            state.nospace = true;
            state.json_end_hash();
        }

        let json = c_str(&json);
        let sql = mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('json', %Q);",
            json.as_ptr()
        );
        exec_metadata(db, &sql, "set json", forcetable)?;
    }
    Ok(())
}
