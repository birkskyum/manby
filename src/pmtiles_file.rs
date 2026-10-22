//! Port of pmtiles_file.hpp and pmtiles_file.cpp: PMTiles output and input.

use rusqlite::Connection;

use crate::mbtiles::Metadata;
use crate::pmtiles::EntryZxy;

pub fn pmtiles_has_suffix(filename: &str) -> bool {
    todo!("pmtiles_file::pmtiles_has_suffix")
}

pub fn check_pmtiles(filename: &str, argv: &[String], forcetable: bool) {
    todo!("pmtiles_file::check_pmtiles")
}

pub fn mbtiles_map_image_to_pmtiles(
    fname: &str,
    m: Metadata,
    tile_compression: bool,
    quiet: bool,
    quiet_progress: bool,
) {
    todo!("pmtiles_file::mbtiles_map_image_to_pmtiles")
}

/// `pmtiles_map` is the whole archive, memory-mapped.
pub fn pmtiles_entries_tms(pmtiles_map: &[u8], minzoom: i32, maxzoom: i32) -> Vec<EntryZxy> {
    todo!("pmtiles_file::pmtiles_entries_tms")
}

/// Returns the offset and length of the tile in the archive, or (0, 0).
pub fn pmtiles_get_tile(pmtiles_map: &[u8], z: i32, x: i32, y: i32) -> (u64, u32) {
    todo!("pmtiles_file::pmtiles_get_tile")
}

pub fn pmtilesmeta2tmp(fname: &str, pmtiles_map: &[u8]) -> Connection {
    todo!("pmtiles_file::pmtilesmeta2tmp")
}
