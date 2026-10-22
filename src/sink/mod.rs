//! Where finished tiles go. See DESIGN.md.
//!
//! Each sink writes what tippecanoe-rs writes for the same tiles and metadata:
//! a PMTiles archive with the same bytes, an MBTiles database with the same
//! tables and rows, or a directory with the same files. [`open`] prepares the
//! output the way tippecanoe's `main` does for `-o`, `-e`, `-f` and `-F`.
//!
//! # Other cloud-native outputs worth adding
//!
//! - **MapLibre Tiles (MLT) in PMTiles.** PMTiles has no tile type for MLT
//!   yet, so this needs a type number agreed with the PMTiles spec, then the
//!   same writer with MLT-encoded tiles. MLT's column layout also compresses
//!   better, so the archive shrinks without gzip.
//! - **Straight to object storage.** The PMTiles writer already knows every
//!   offset before it writes a byte of tile data, so `finish` could stream
//!   the archive into an S3 or GCS multipart upload (5 MiB parts, uploaded
//!   in parallel) instead of a local file, with no local copy at all.
//! - **Brotli or zstd inside PMTiles.** The header already has codes for
//!   both; zstd at a low level decodes several times faster than gzip in
//!   browsers that support it, for archives that are not checked against
//!   tippecanoe.
//! - **Clustered spill without a copy.** When the tiler can emit tiles in
//!   tile-id order (it walks the quadtree, which is close), most tiles could go
//!   straight to their final place in the archive; APFS `clonefile` and Linux
//!   `copy_file_range` only help when whole files or block-aligned ranges move,
//!   which tile data in a PMTiles archive never is.
//! - **Versatiles or COMTiles.** Other single-file formats for HTTP range
//!   reads, which could share the dedup and ordering of the PMTiles writer.

pub mod dir;
pub mod mbtiles;
pub mod pmtiles;

use std::path::PathBuf;

use tippecanoe::mbtiles::Metadata;

use crate::options::Output;

/// Receives finished tiles from any number of threads, in any order.
pub trait TileSink: Send + Sync {
    /// `data` is the tile as it should be stored (compressed unless tile
    /// compression is off).
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> crate::Result<()>;

    /// Removes the tiles of zoom `z` already written, for tippecanoe's retries
    /// that redo a whole zoom level with a lower maximum zoom.
    fn erase_zoom(&self, z: u8) -> crate::Result<()>;

    /// Writes the metadata and closes the output.
    fn finish(self: Box<Self>, metadata: &Metadata) -> crate::Result<()>;
}

/// How to prepare and write the output.
#[derive(Clone, Debug)]
pub struct SinkSettings {
    /// `-f`: delete the existing tileset first.
    pub force: bool,
    /// `-F`: add to an existing MBTiles database or directory.
    pub allow_existing: bool,
    /// Tiles are gzipped (no `-pC`). Recorded in the PMTiles header.
    pub tile_compression: bool,
    /// `-t`: where temporary files go.
    pub tmpdir: PathBuf,
    /// PMTiles: bytes of distinct tile data kept in memory; the rest goes to
    /// a temporary file until `finish`.
    pub memory_budget: usize,
    /// `argv[0]`, which starts the second line of some messages, as in tippecanoe.
    pub program: String,
}

impl Default for SinkSettings {
    fn default() -> SinkSettings {
        SinkSettings {
            force: false,
            allow_existing: false,
            tile_compression: true,
            tmpdir: PathBuf::from("/tmp"),
            memory_budget: physical_memory() / 4,
            program: "tippecanoe".to_string(),
        }
    }
}

/// Bytes of RAM, or 4 GiB if the system will not say.
pub fn physical_memory() -> usize {
    // SAFETY: sysconf has no preconditions.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    if pages > 0 && page_size > 0 {
        (pages as usize).saturating_mul(page_size as usize)
    } else {
        4 << 30
    }
}

/// Prepares `output` and returns the sink that writes it.
pub fn open(output: &Output, settings: &SinkSettings) -> crate::Result<Box<dyn TileSink>> {
    match output {
        Output::Pmtiles(path) => Ok(Box::new(pmtiles::PmtilesSink::create(path, settings)?)),
        Output::Mbtiles(path) => Ok(Box::new(mbtiles::MbtilesSink::create(path, settings)?)),
        Output::Directory(path) => Ok(Box::new(dir::DirSink::create(path, settings)?)),
        Output::None => Err(crate::Error::new(
            tippecanoe::errors::EXIT_ARGS,
            "must specify -o out.mbtiles or -e directory",
        )),
    }
}

/// The tile ids of zoom `z` in a PMTiles archive: `first_tile_id(z)..first_tile_id(z + 1)`.
pub(crate) fn first_tile_id(z: u8) -> u64 {
    ((1u64 << (2 * z as u32)) - 1) / 3
}

/// Checks that z/x/y names a tile.
pub(crate) fn check_tile(z: u8, x: u32, y: u32) -> crate::Result<()> {
    if z > 31 || x >> z != 0 || y >> z != 0 {
        return Err(crate::Error::new(
            tippecanoe::errors::EXIT_IMPOSSIBLE,
            format!("tile {}/{}/{} is outside zoom level bounds", z, x, y),
        ));
    }
    Ok(())
}

/// `std::to_string(double)`, which formats like `%f`.
pub(crate) fn to_string_double(d: f64) -> String {
    if d.is_nan() {
        return "nan".to_string();
    }
    format!("{:.6}", d)
}

/// tippecanoe's `perror()` text for an I/O error.
pub(crate) fn io_message(path: &str, e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(errno) => format!("{}: {}", path, tippecanoe::cstd::strerror(errno)),
        None => format!("{}: {}", path, e),
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn tile_id_ranges() {
        for z in 0..=31u8 {
            let n = 1u32 << z;
            assert_eq!(
                first_tile_id(z),
                tippecanoe::pmtiles::zxy_to_tileid(z, 0, 0)
            );
            if z < 31 {
                assert_eq!(
                    first_tile_id(z + 1),
                    tippecanoe::pmtiles::zxy_to_tileid(z, n - 1, 0) + 1
                );
            }
        }
        assert!(check_tile(0, 0, 0).is_ok());
        assert!(check_tile(3, 7, 7).is_ok());
        assert!(check_tile(3, 8, 0).is_err());
        assert!(check_tile(32, 0, 0).is_err());
    }
}
