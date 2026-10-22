//! Port of tile.hpp and tile.cpp: turning the sorted features into tiles, zoom by zoom.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use rusqlite::Connection;

use crate::attribute::AttributeOp;
use crate::jsonpull::JsonRef;
use crate::mbtiles::LayermapEntry;
use crate::serial::{Node, StringPoolRef};

#[derive(Debug, Default)]
pub struct AtomicStrategy {
    pub dropped_by_rate: AtomicUsize,
    pub dropped_by_gamma: AtomicUsize,
    pub dropped_as_needed: AtomicUsize,
    pub coalesced_as_needed: AtomicUsize,
    pub detail_reduced: AtomicUsize,
    pub tiny_polygons: AtomicUsize,
    pub truncated_zooms: AtomicUsize,
}

impl AtomicStrategy {
    pub fn add_from(&self, src: &Strategy) {
        todo!("AtomicStrategy::add_from")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Strategy {
    pub dropped_by_rate: usize,
    pub dropped_by_gamma: usize,
    pub dropped_as_needed: usize,
    pub coalesced_as_needed: usize,
    pub detail_reduced: usize,
    pub tiny_polygons: usize,
    pub truncated_zooms: usize,

    pub tile_size: usize,
    pub feature_count: usize,
}

impl Strategy {
    pub fn new(s: &AtomicStrategy, ts: usize, fc: usize) -> Strategy {
        Strategy {
            dropped_by_rate: s.dropped_by_rate.load(Ordering::Relaxed),
            dropped_by_gamma: s.dropped_by_gamma.load(Ordering::Relaxed),
            dropped_as_needed: s.dropped_as_needed.load(Ordering::Relaxed),
            coalesced_as_needed: s.coalesced_as_needed.load(Ordering::Relaxed),
            detail_reduced: s.detail_reduced.load(Ordering::Relaxed),
            tile_size: ts,
            feature_count: fc,
            tiny_polygons: s.tiny_polygons.load(Ordering::Relaxed),
            truncated_zooms: s.truncated_zooms.load(Ordering::Relaxed),
        }
    }
}

/// `geomfd` and `geom_size` hold one file descriptor and size per temporary
/// geometry file, as the C++ arrays do. `outdb` is `None` when writing a
/// directory. Returns the zoom level actually reached.
#[allow(clippy::too_many_arguments)]
pub fn traverse_zooms(
    geomfd: &mut [i32],
    geom_size: &mut [i64],
    global_stringpool: &StringPoolRef,
    midx: &AtomicU32,
    midy: &AtomicU32,
    maxzoom: &mut i32,
    minzoom: i32,
    outdb: Option<&Mutex<Connection>>,
    outdir: Option<&str>,
    buffer: i32,
    fname: &str,
    tmpdir: &str,
    gamma: f64,
    full_detail: i32,
    low_detail: i32,
    min_detail: i32,
    pool_off: &[i64],
    initial_x: &[u32],
    initial_y: &[u32],
    simplification: f64,
    maxzoom_simplification: f64,
    layermaps: &mut Vec<BTreeMap<String, LayermapEntry>>,
    prefilter: Option<&str>,
    postfilter: Option<&str>,
    attribute_accum: &HashMap<String, AttributeOp>,
    filter: Option<JsonRef<'_>>,
    strategies: &mut Vec<Strategy>,
    iz: i32,
    shared_nodes_map: &[Node],
    nodepos: usize,
    shared_nodes_bloom: &[u8],
    basezoom: i32,
    droprate: f64,
    unidecode_data: &[String],
    drop_by_attribute_as_needed_attribute: Option<&str>,
    drop_by_attribute_descending: bool,
) -> i32 {
    todo!("tile::traverse_zooms")
}

pub fn manage_gap(index: u64, previndex: &mut u64, scale: f64, gamma: f64, gap: &mut f64) -> i32 {
    todo!("tile::manage_gap")
}
