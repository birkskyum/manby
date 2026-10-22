//! Port of mbtiles.hpp and mbtiles.cpp: the MBTiles output, metadata and tilestats.
//!
//! The C++ passes `sqlite3 *` around and may pass NULL when writing a directory.
//! Here that is `Option<&Connection>`, and code that shares one database between
//! threads keeps it in a `Mutex`.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::serial::SerialVal;
use crate::tile::Strategy;

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

pub fn mbtiles_open(dbname: &str, argv: &[String], forcetable: i32) -> Connection {
    todo!("mbtiles::mbtiles_open")
}

pub fn mbtiles_write_tile(outdb: &Connection, z: i32, tx: i32, ty: i32, data: &[u8]) {
    todo!("mbtiles::mbtiles_write_tile")
}

pub fn mbtiles_erase_zoom(outdb: &Connection, z: i32) {
    todo!("mbtiles::mbtiles_erase_zoom")
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
    droprate: f64,
    retain_points_multiplier: i32,
) -> Metadata {
    todo!("mbtiles::make_metadata")
}

pub fn mbtiles_write_metadata(db: &Connection, m: &Metadata, forcetable: bool) {
    todo!("mbtiles::mbtiles_write_metadata")
}

pub fn mbtiles_close(outdb: Connection, pgm: &str) {
    todo!("mbtiles::mbtiles_close")
}

pub fn merge_layermaps(
    maps: &[BTreeMap<String, LayermapEntry>],
) -> BTreeMap<String, LayermapEntry> {
    todo!("mbtiles::merge_layermaps")
}

/// `merge_layermaps(maps, trunc)`.
pub fn merge_layermaps_trunc(
    maps: &[BTreeMap<String, LayermapEntry>],
    trunc: bool,
) -> BTreeMap<String, LayermapEntry> {
    todo!("mbtiles::merge_layermaps_trunc")
}

pub fn add_to_tilestats(tilestats: &mut BTreeMap<String, Tilestat>, attrib: &str, val: &SerialVal) {
    todo!("mbtiles::add_to_tilestats")
}

pub fn version_str() -> String {
    todo!("mbtiles::version_str")
}
