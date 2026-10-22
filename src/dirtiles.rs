//! Port of dirtiles.hpp and dirtiles.cpp: tiles in a directory tree.

use std::cmp::Ordering;

use rusqlite::Connection;

use crate::mbtiles::Metadata;

pub fn dir_write_tile(outdir: &str, z: i32, tx: i32, ty: i32, pbf: &[u8]) {
    todo!("dirtiles::dir_write_tile")
}

pub fn dir_erase_zoom(outdir: &str, z: i32) {
    todo!("dirtiles::dir_erase_zoom")
}

pub fn dir_write_metadata(outdir: &str, m: &Metadata) {
    todo!("dirtiles::dir_write_metadata")
}

pub fn check_dir(d: &str, argv: &[String], force: bool, forcetable: bool) {
    todo!("dirtiles::check_dir")
}

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

pub fn enumerate_dirtiles(fname: &str, minzoom: i32, maxzoom: i32) -> Vec<Zxy> {
    todo!("dirtiles::enumerate_dirtiles")
}

pub fn dirmeta2tmp(fname: &str) -> Connection {
    todo!("dirtiles::dirmeta2tmp")
}

pub fn dir_read_tile(base: &str, tile: &Zxy) -> Vec<u8> {
    todo!("dirtiles::dir_read_tile")
}
