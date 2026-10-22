//! PMTiles written directly, without an intermediate SQLite database.
//!
//! tippecanoe writes a PMTiles archive by filling an MBTiles database, then
//! reading it back in tile-id order (`mbtiles_map_image_to_pmtiles`). This
//! sink makes the same archive, byte for byte, without the database:
//!
//! - Tiles arrive from any thread. Each is hashed (xxh3-128) and goes to one
//!   of 64 shards by its hash, so threads rarely wait for each other, and
//!   identical tiles are stored once, as tippecanoe's conversion does.
//! - Distinct tile data stays in memory up to a budget; beyond it, it goes to
//!   an unlinked temporary file. Each shard collects its spilled tiles and
//!   writes them a megabyte at a time at a range it reserves, so writers
//!   neither share a file offset nor make a system call per tile.
//! - At `finish`, the tiles are sorted by tile id and the data is laid out in
//!   the order tippecanoe's conversion uses: each distinct tile where it is
//!   first used. Runs and directories are built as tippecanoe builds them,
//!   with the leaves compressed in parallel, and the archive is written once,
//!   the tile data in pieces written in parallel at their final offsets.
//!
//! Because the layout depends only on the set of tiles, the archive does not
//! depend on the order the tiles arrived in or on the number of threads.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasherDefault, Hasher};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use rayon::prelude::*;
use tippecanoe::errors::{EXIT_EXISTS, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_SQLITE, EXIT_WRITE};
use tippecanoe::mbtiles::Metadata;
use tippecanoe::pmtiles::{self, EntryV3, HeaderV3};
use tippecanoe::write_json::JsonWriter;

use super::{
    SinkSettings, TileSink, check_tile, first_tile_id, io_message, tileset_metadata,
    to_string_double,
};
use crate::compress::gzip;
use crate::{Error, Result};

/// The PMTiles tile type of MapLibre Tiles, added to the spec after
/// tippecanoe's `pmtiles.hpp`.
pub const TILETYPE_MLT: u8 = 0x6;

const SHARDS: usize = 64;
const CHUNK: usize = 256 << 10;
/// Bytes of tile data per parallel write at `finish`.
const PIECE: u64 = 1 << 20;
/// Bytes of spilled tile data a shard collects before writing them out.
const SPILL_BATCH: usize = 256 << 10;

/// Where one distinct tile's data is.
#[derive(Clone, Copy)]
struct Content {
    len: u32,
    place: Place,
    /// `chunk << 32 | offset` in the shard's chunks, the offset in the shard's
    /// spill batch, or the offset in the spill file.
    pos: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Memory,
    Batch,
    Spilled,
}

/// The keys are already uniform hashes, so they are their own hash.
#[derive(Default)]
struct HashHasher(u64);

impl Hasher for HashHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8) ^ b as u64;
        }
    }

    fn write_u128(&mut self, i: u128) {
        self.0 = i as u64;
    }
}

#[derive(Default)]
struct Shard {
    index: HashMap<u128, u32, BuildHasherDefault<HashHasher>>,
    contents: Vec<Content>,
    chunks: Vec<Vec<u8>>,
    /// (tile id, content number in this shard)
    tiles: Vec<(u64, u32)>,
    /// Spilled tiles not yet written to the spill file, and their content numbers.
    batch: Vec<u8>,
    batched: Vec<u32>,
}

impl Shard {
    fn store(&mut self, data: &[u8]) -> u64 {
        let fits = self
            .chunks
            .last()
            .is_some_and(|c| c.capacity() - c.len() >= data.len());
        if !fits {
            self.chunks.push(Vec::with_capacity(CHUNK.max(data.len())));
        }
        let chunk = self.chunks.len() - 1;
        let c = &mut self.chunks[chunk];
        let off = c.len();
        c.extend_from_slice(data);
        (chunk as u64) << 32 | off as u64
    }

    /// Appends the tile's data to `out`. Spilled data is read from the
    /// file rather than mapped, so that it stays out of the process's memory.
    fn data_into(
        &self,
        c: &Content,
        spill: Option<&File>,
        out: &mut Vec<u8>,
    ) -> std::io::Result<()> {
        let (from, to) = (c.pos as usize, c.pos as usize + c.len as usize);
        match c.place {
            Place::Spilled => {
                let at = out.len();
                out.resize(at + c.len as usize, 0);
                match spill {
                    Some(f) => f.read_exact_at(&mut out[at..], c.pos),
                    None => Err(std::io::Error::other("PMTiles spill file missing")),
                }
            }
            Place::Batch => {
                out.extend_from_slice(&self.batch[from..to]);
                Ok(())
            }
            Place::Memory => {
                let chunk = &self.chunks[(c.pos >> 32) as usize];
                let off = (c.pos & 0xFFFF_FFFF) as usize;
                out.extend_from_slice(&chunk[off..off + c.len as usize]);
                Ok(())
            }
        }
    }

    /// Takes the spill batch, reserving its range of the spill file, for the
    /// caller to write at the returned offset.
    fn take_batch(&mut self, spill_len: &AtomicU64) -> (Vec<u8>, u64) {
        let base = spill_len.fetch_add(self.batch.len() as u64, Ordering::Relaxed);
        for &n in &self.batched {
            let c = &mut self.contents[n as usize];
            c.pos += base;
            c.place = Place::Spilled;
        }
        self.batched.clear();
        (std::mem::take(&mut self.batch), base)
    }
}

pub struct PmtilesSink {
    path: PathBuf,
    file: File,
    tile_compression: bool,
    tile_format: crate::mlt::TileFormat,
    tmpdir: PathBuf,
    memory_budget: usize,
    shards: Vec<Mutex<Shard>>,
    in_memory: AtomicUsize,
    spill: OnceLock<std::result::Result<File, String>>,
    spill_len: AtomicU64,
    failed: AtomicBool,
}

impl PmtilesSink {
    /// Prepares `path` the way tippecanoe does for `-o x.pmtiles`.
    pub fn create(path: &str, settings: &SinkSettings) -> Result<PmtilesSink> {
        if settings.force {
            let _ = fs::remove_file(path);
        } else if fs::metadata(path).is_ok() {
            let mut message = format!(
                "Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.\n{}: {}: file exists",
                path, settings.program, path
            );
            if settings.allow_existing {
                message += &format!(
                    "\n{}: --allow-existing is not supported for pmtiles",
                    settings.program
                );
            }
            return Err(Error::new(EXIT_EXISTS, message));
        }

        // tippecanoe would create its SQLite database here, and SQLite
        // reports any failure to create it this way.
        let file = File::create(path).map_err(|_| {
            Error::new(EXIT_OPEN, format!("{}: unable to open database file", path))
        })?;

        Ok(PmtilesSink {
            path: PathBuf::from(path),
            file,
            tile_compression: settings.tile_compression,
            tile_format: settings.tile_format,
            tmpdir: settings.tmpdir.clone(),
            memory_budget: settings.memory_budget,
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            in_memory: AtomicUsize::new(0),
            spill: OnceLock::new(),
            spill_len: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        })
    }

    fn spill_file(&self) -> Result<&File> {
        self.spill
            .get_or_init(|| create_unlinked(&self.tmpdir))
            .as_ref()
            .map_err(|m| Error::new(EXIT_OPEN, m.clone()))
    }

    fn spill_write(&self, bytes: &[u8], at: u64) -> Result<()> {
        let written = self.spill_file().and_then(|f| {
            f.write_all_at(bytes, at)
                .map_err(|e| Error::new(EXIT_WRITE, io_message("PMTiles spill file", &e)))
        });
        if written.is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
        written
    }

    fn lock(&self, shard: usize) -> std::sync::MutexGuard<'_, Shard> {
        self.shards[shard]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A temporary file in `dir` that is already unlinked, as tippecanoe makes them.
fn create_unlinked(dir: &Path) -> std::result::Result<File, String> {
    for attempt in 0..100u32 {
        let name = dir.join(format!(
            "manby-pmtiles-{}-{}-{}",
            std::process::id(),
            attempt,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&name)
        {
            Ok(f) => {
                let _ = fs::remove_file(&name);
                return Ok(f);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_message(&name.to_string_lossy(), &e)),
        }
    }
    Err(format!("{}: cannot create a temporary file", dir.display()))
}

impl TileSink for PmtilesSink {
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<()> {
        check_tile(z, x, y)?;
        let len = u32::try_from(data.len()).map_err(|_| {
            Error::new(
                EXIT_IMPOSSIBLE,
                format!("tile {}/{}/{} is too large for PMTiles", z, x, y),
            )
        })?;
        let tile_id = pmtiles::zxy_to_tileid(z, x, y);
        let hash = xxhash_rust::xxh3::xxh3_128(data);

        let mut batch = None;
        {
            let mut shard = self.lock((hash >> 64) as usize % SHARDS);
            let n = match shard.index.get(&hash) {
                Some(&n) => n,
                None => {
                    let n = shard.contents.len() as u32;
                    let used = self.in_memory.fetch_add(data.len(), Ordering::Relaxed);
                    let content = if used + data.len() <= self.memory_budget {
                        let pos = shard.store(data);
                        Content {
                            len,
                            place: Place::Memory,
                            pos,
                        }
                    } else {
                        self.in_memory.fetch_sub(data.len(), Ordering::Relaxed);
                        let pos = shard.batch.len() as u64;
                        shard.batch.extend_from_slice(data);
                        shard.batched.push(n);
                        Content {
                            len,
                            place: Place::Batch,
                            pos,
                        }
                    };
                    shard.contents.push(content);
                    shard.index.insert(hash, n);
                    if shard.batch.len() >= SPILL_BATCH {
                        batch = Some(shard.take_batch(&self.spill_len));
                    }
                    n
                }
            };
            shard.tiles.push((tile_id, n));
        }

        if let Some((bytes, at)) = batch {
            self.spill_write(&bytes, at)?;
        }
        Ok(())
    }

    fn erase_zoom(&self, z: u8) -> Result<()> {
        if z > 31 {
            return Ok(());
        }
        let range = first_tile_id(z)..first_tile_id(z + 1);
        for i in 0..SHARDS {
            self.lock(i).tiles.retain(|t| !range.contains(&t.0));
        }
        Ok(())
    }

    fn finish(self: Box<Self>, metadata: &Metadata) -> Result<()> {
        let mut this = *self;
        if this.failed.load(Ordering::Relaxed) {
            return Err(Error::new(
                EXIT_WRITE,
                "PMTiles spill file: an earlier write failed",
            ));
        }
        let mut shards: Vec<Shard> = std::mem::take(&mut this.shards)
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner()))
            .collect();
        for shard in &mut shards {
            if !shard.batch.is_empty() {
                let (bytes, at) = shard.take_batch(&this.spill_len);
                this.spill_write(&bytes, at)?;
            }
        }
        let spill: Option<&File> = match this.spill.get() {
            Some(Ok(f)) => Some(f),
            _ => None,
        };

        let layout = Layout::new(&shards)?;
        let metadata = tileset_metadata(metadata, this.tile_format);
        let archive = Archive::new(&layout, &metadata, this.tile_compression);

        let write_error = |e: std::io::Error| {
            Error::new(EXIT_WRITE, io_message(&this.path.to_string_lossy(), &e))
        };
        let front = [
            &archive.header[..],
            &archive.root,
            &archive.json,
            &archive.leaves,
        ]
        .concat();
        this.file.write_all_at(&front, 0).map_err(write_error)?;

        // The tile data, in pieces written in parallel at their final offsets.
        let len = |&(s, n): &(u32, u32)| shards[s as usize].contents[n as usize].len as u64;
        let mut pieces: Vec<(usize, usize, u64, u64)> = Vec::new();
        let (mut start, mut at, mut bytes) = (0, front.len() as u64, 0u64);
        for (i, item) in layout.order.iter().enumerate() {
            if bytes >= PIECE {
                pieces.push((start, i, at, bytes));
                (start, at, bytes) = (i, at + bytes, 0);
            }
            bytes += len(item);
        }
        pieces.push((start, layout.order.len(), at, bytes));
        pieces
            .into_par_iter()
            .try_for_each(|(from, to, at, bytes)| {
                let mut buf = Vec::with_capacity(bytes as usize);
                for &(s, n) in &layout.order[from..to] {
                    let shard = &shards[s as usize];
                    shard.data_into(&shard.contents[n as usize], spill, &mut buf)?;
                }
                this.file.write_all_at(&buf, at)
            })
            .map_err(write_error)?;
        Ok(())
    }
}

/// The tiles in tile-id order, the directory entries tippecanoe's conversion
/// makes for them, and the order the distinct tiles are written in.
struct Layout {
    entries: Vec<EntryV3>,
    /// (shard, content number) of each distinct tile, in archive order.
    order: Vec<(u32, u32)>,
    addressed_tiles: u64,
    tile_data_bytes: u64,
}

impl Layout {
    fn new(shards: &[Shard]) -> Result<Layout> {
        let mut tiles: Vec<(u64, u64)> =
            Vec::with_capacity(shards.iter().map(|s| s.tiles.len()).sum());
        for (i, shard) in shards.iter().enumerate() {
            tiles.extend(
                shard
                    .tiles
                    .iter()
                    .map(|&(id, n)| (id, (i as u64) << 32 | n as u64)),
            );
        }
        tiles.par_sort_unstable();
        if let Some(w) = tiles.windows(2).find(|w| w[0].0 == w[1].0) {
            let t = pmtiles::tileid_to_zxy(w[0].0);
            return Err(Error::new(
                EXIT_SQLITE,
                format!(
                    "sqlite3 map insert failed: UNIQUE constraint failed: map.zoom_level, map.tile_column, map.tile_row (tile {}/{}/{} written twice)",
                    t.z, t.x, t.y
                ),
            ));
        }

        let mut offsets: Vec<Vec<u64>> = shards
            .iter()
            .map(|s| vec![u64::MAX; s.contents.len()])
            .collect();
        let mut entries: Vec<EntryV3> = Vec::with_capacity(tiles.len());
        let mut order: Vec<(u32, u32)> = Vec::new();
        let mut offset: u64 = 0;

        // As in mbtiles_map_image_to_pmtiles(), including that a run only
        // grows while the tile follows the run's first tile, so no run is
        // longer than 2.
        for &(tile_id, key) in &tiles {
            let (s, n) = ((key >> 32) as usize, (key & 0xFFFF_FFFF) as usize);
            let len = shards[s].contents[n].len;
            let at = offsets[s][n];
            if at != u64::MAX {
                match entries.last_mut() {
                    Some(last) if tile_id == last.tile_id + 1 && last.offset == at => {
                        last.run_length += 1;
                    }
                    _ => entries.push(EntryV3::new(tile_id, at, len, 1)),
                }
            } else {
                offsets[s][n] = offset;
                entries.push(EntryV3::new(tile_id, offset, len, 1));
                order.push((s as u32, n as u32));
                offset += len as u64;
            }
        }

        Ok(Layout {
            entries,
            order,
            addressed_tiles: tiles.len() as u64,
            tile_data_bytes: offset,
        })
    }
}

/// Everything in the archive before the tile data.
struct Archive {
    header: Vec<u8>,
    root: Vec<u8>,
    json: Vec<u8>,
    leaves: Vec<u8>,
}

impl Archive {
    fn new(layout: &Layout, m: &Metadata, tile_compression: bool) -> Archive {
        let (root, leaves) = make_root_leaves(&layout.entries);
        let json = metadata_to_pmtiles_json(m);

        let mut header = HeaderV3 {
            min_zoom: m.minzoom as u8,
            max_zoom: m.maxzoom as u8,
            min_lon_e7: (m.minlon * 10000000.0) as i32,
            min_lat_e7: (m.minlat * 10000000.0) as i32,
            max_lon_e7: (m.maxlon * 10000000.0) as i32,
            max_lat_e7: (m.maxlat * 10000000.0) as i32,
            center_zoom: m.center_z as u8,
            center_lon_e7: (m.center_lon * 10000000.0) as i32,
            center_lat_e7: (m.center_lat * 10000000.0) as i32,
            clustered: true,
            internal_compression: pmtiles::COMPRESSION_GZIP,
            tile_compression: if tile_compression {
                pmtiles::COMPRESSION_GZIP
            } else {
                pmtiles::COMPRESSION_NONE
            },
            tile_type: if m.format == "pbf" {
                pmtiles::TILETYPE_MVT
            } else if m.format == "png" {
                pmtiles::TILETYPE_PNG
            } else if m.format == "mlt" {
                TILETYPE_MLT
            } else {
                pmtiles::TILETYPE_UNKNOWN
            },
            root_dir_offset: 127,
            root_dir_bytes: root.len() as u64,
            ..Default::default()
        };
        header.json_metadata_offset = header.root_dir_offset + header.root_dir_bytes;
        header.json_metadata_bytes = json.len() as u64;
        header.leaf_dirs_offset = header.json_metadata_offset + header.json_metadata_bytes;
        header.leaf_dirs_bytes = leaves.len() as u64;
        header.tile_data_offset = header.leaf_dirs_offset + header.leaf_dirs_bytes;
        header.tile_data_bytes = layout.tile_data_bytes;
        header.addressed_tiles_count = layout.addressed_tiles;
        header.tile_entries_count = layout.entries.len() as u64;
        header.tile_contents_count = layout.order.len() as u64;

        Archive {
            header: header.serialize(),
            root,
            json,
            leaves,
        }
    }
}

/// tippecanoe's `make_root_leaves()` with gzip, making the same bytes, but
/// with the leaves compressed in parallel, and the trial of a root directory
/// without leaves made at the same time as the first try with leaves.
pub fn make_root_leaves(entries: &[EntryV3]) -> (Vec<u8>, Vec<u8>) {
    let (whole, mut built) = rayon::join(
        || gzip(&pmtiles::serialize_directory(entries), false),
        || build_root_leaves(entries, 4096),
    );
    if whole.len() <= 16384 - 127 {
        return (whole, Vec::new());
    }
    let mut leaf_size = 4096;
    while built.0.len() >= 16384 - 127 {
        leaf_size *= 2;
        built = build_root_leaves(entries, leaf_size);
    }
    built
}

/// tippecanoe's `build_root_leaves()` with gzip, the leaves compressed in parallel.
pub(crate) fn build_root_leaves(entries: &[EntryV3], leaf_size: usize) -> (Vec<u8>, Vec<u8>) {
    let leaves: Vec<Vec<u8>> = entries
        .par_chunks(leaf_size)
        .map(|leaf| gzip(&pmtiles::serialize_directory(leaf), false))
        .collect();
    let mut root_entries = Vec::with_capacity(leaves.len());
    let mut leaves_bytes = Vec::with_capacity(leaves.iter().map(|l| l.len()).sum());
    for (i, leaf) in leaves.iter().enumerate() {
        root_entries.push(EntryV3::new(
            entries[i * leaf_size].tile_id,
            leaves_bytes.len() as u64,
            leaf.len() as u32,
            0,
        ));
        leaves_bytes.extend_from_slice(leaf);
    }
    (
        gzip(&pmtiles::serialize_directory(&root_entries), false),
        leaves_bytes,
    )
}

fn out(state: &mut JsonWriter<'_>, k: &str, v: &str) {
    state.json_comma_newline();
    state.json_write_string(k);
    state.json_write_string(v);
}

/// tippecanoe's `metadata_to_pmtiles_json()`: the metadata JSON, gzipped.
pub fn metadata_to_pmtiles_json(m: &Metadata) -> Vec<u8> {
    let mut buf = String::new();
    {
        let mut state = JsonWriter::new_string(&mut buf);

        state.json_write_hash();
        state.json_write_newline();

        out(&mut state, "name", &m.name);
        out(&mut state, "format", &m.format);
        out(&mut state, "type", &m.type_);
        out(&mut state, "description", &m.description);
        out(&mut state, "version", &m.version.to_string());
        if !m.attribution.is_empty() {
            out(&mut state, "attribution", &m.attribution);
        }
        if !m.strategies_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("strategies");
            state.json_write_json(&m.strategies_json);
        }
        if !m.decisions_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("tippecanoe_decisions");
            state.json_write_json(&m.decisions_json);
        }

        out(&mut state, "generator", &m.generator);
        out(&mut state, "generator_options", &m.generator_options);

        let bounds2 = to_string_double(m.minlon2)
            + ","
            + &to_string_double(m.minlat2)
            + ","
            + &to_string_double(m.maxlon2)
            + ","
            + &to_string_double(m.maxlat2);
        out(&mut state, "antimeridian_adjusted_bounds", &bounds2);

        if !m.vector_layers_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("vector_layers");
            state.json_write_json(&m.vector_layers_json);
        }
        if !m.tilestats_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("tilestats");
            state.json_write_json(&m.tilestats_json);
        }

        state.json_write_newline();
        state.json_end_hash();
        state.json_write_newline();
    }
    gzip(buf.as_bytes(), false)
}
