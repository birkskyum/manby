//! The order of work: zoom levels, the passes tippecanoe makes over a zoom
//! level when a tile needs a stricter threshold, and the parallelism.

use std::collections::BTreeMap;

use rayon::prelude::*;
use tippecanoe::errors::{EXIT_ARGS, EXIT_IMPOSSIBLE, EXIT_INCOMPLETE};
use tippecanoe::globals::MAX_ZOOM;
use tippecanoe::jsonpull::JsonRef;
use tippecanoe::mbtiles::{LayermapEntry, merge_layermaps};
use tippecanoe::mvt::{MVT_NULL, MvtValue, stringified_to_mvt_value};
use tippecanoe::options::*;
use tippecanoe::tile::Strategy;

use super::make::{MakeParams, MakeResult, Thresholds, make_tile};
use super::read::{Carried, ReadParams, ReadTile, TileInput, read_tile};
use super::{TileOptions, TileStats};
use crate::error::Error;
use crate::sink::TileSink;
use crate::store::FeatureStore;

/// What every tile of a run shares.
pub struct Ctx<'a> {
    pub store: &'a FeatureStore,
    pub o: &'a TileOptions,
    /// The store's attribute values as tile values.
    pub values: Vec<MvtValue>,
    pub value_null: Vec<bool>,
    /// The position of each layer when the layers are sorted by name.
    pub layer_rank: Vec<u32>,
    pub filter: Option<JsonRef<'a>>,
}

impl<'a> Ctx<'a> {
    fn new(store: &'a FeatureStore, o: &'a TileOptions) -> Ctx<'a> {
        let values: Vec<MvtValue> = store
            .values
            .par_iter()
            .with_min_len(4096)
            .map(|v| stringified_to_mvt_value(v.type_, &v.s))
            .collect();
        let value_null = store.values.iter().map(|v| v.type_ == MVT_NULL).collect();

        let mut order: Vec<usize> = (0..store.layers.len()).collect();
        order.sort_by(|&a, &b| store.layers[a].name.cmp(&store.layers[b].name));
        let mut layer_rank = vec![0u32; store.layers.len()];
        for (rank, &i) in order.iter().enumerate() {
            layer_rank[i] = rank as u32;
        }

        Ctx {
            store,
            o,
            values,
            value_null,
            layer_rank,
            filter: o.filter.as_ref().map(|t| t.root()),
        }
    }
}

fn unsupported(o: &TileOptions) -> Option<&'static str> {
    if o.a(A_VARIABLE_DEPTH_PYRAMID) {
        return Some("variable depth pyramids (-at)");
    }
    if o.a(A_DETECT_SHARED_BORDERS) {
        return Some("--detect-shared-borders");
    }
    if o.prefilter.is_some() {
        return Some("--prefilter");
    }
    if o.postfilter.is_some() {
        return Some("--postfilter");
    }
    None
}

/// How many threads tippecanoe would tile a zoom level with, given how many
/// of its temporary files have data.
fn tippecanoe_threads(o: &TileOptions, useful_threads: usize) -> usize {
    let mut threads = o.cpus;
    if threads > o.temp_files / 4 {
        threads = o.temp_files / 4;
    }
    if threads > useful_threads {
        threads = useful_threads;
    }
    for e in 0..30 {
        if threads >= (1usize << e) && threads < (1usize << (e + 1)) {
            threads = 1usize << e;
            break;
        }
    }
    if threads >= (1usize << 30) {
        threads = 1usize << 30;
    }
    threads.max(1)
}

pub fn run(store: &FeatureStore, o: &TileOptions, sink: &dyn TileSink) -> crate::Result<TileStats> {
    if let Some(what) = unsupported(o) {
        return Err(Error::new(
            EXIT_ARGS,
            format!("manby does not support {} yet", what),
        ));
    }

    let ctx = Ctx::new(store, o);
    let mut stats = TileStats::default();

    let mut maxzoom = o.maxzoom;
    let mut extend_zooms_max = o.extend_zooms_max;
    let mut largest_written: i32 = -1;
    let mut tilestats: Vec<BTreeMap<String, LayermapEntry>> = Vec::new();

    let mut level: Vec<TileInput> = Vec::new();
    if !store.features.is_empty() {
        let (ox, oy) = if o.iz != 0 {
            ((o.ix << (32 - o.iz)) as i64, (o.iy << (32 - o.iz)) as i64)
        } else {
            (0, 0)
        };
        let feats = store
            .features
            .par_iter()
            .enumerate()
            .with_min_len(4096)
            .map(|(i, f)| {
                let mut bbox = f.bbox;
                if bbox[0] <= bbox[2] {
                    bbox[0] -= ox;
                    bbox[1] -= oy;
                    bbox[2] -= ox;
                    bbox[3] -= oy;
                }
                Carried {
                    fi: i as u32,
                    gap: 0,
                    geom_start: f.geom_start,
                    geom_len: f.geom_len,
                    bbox,
                }
            })
            .collect();
        level.push(TileInput {
            z: o.iz,
            x: o.ix,
            y: o.iy,
            feats,
            geom: Vec::new(),
            root: true,
        });
    }

    // the temporary files with data at the next zoom, for tippecanoe's thread count
    let mut useful_threads: usize = 1;

    let mut z = o.iz;
    while z <= maxzoom {
        let mut most: i64 = 0;

        let threads = tippecanoe_threads(o, useful_threads);
        let child_shards = (o.temp_files / threads) as i32;
        let max_zoom_increment = ((child_shards as f64).ln() / 4f64.ln()) as i32;
        if !level.is_empty() {
            if child_shards < 4 || max_zoom_increment < 1 {
                return Err(Error::new(
                    EXIT_IMPOSSIBLE,
                    format!(
                        "Internal error: {} shards, max zoom increment {}",
                        child_shards, max_zoom_increment
                    ),
                ));
            }
            if (((child_shards - 1) << 1) & child_shards) != child_shards {
                return Err(Error::new(
                    EXIT_IMPOSSIBLE,
                    format!("Internal error: {} shards not a power of 2", child_shards),
                ));
            }
        }

        let level_z = level.first().map(|t| t.z).unwrap_or(z);
        let mut nextzoom = level_z + 1;
        if nextzoom < o.minzoom {
            if level_z + max_zoom_increment > o.minzoom {
                nextzoom = o.minzoom;
            } else {
                nextzoom = level_z + max_zoom_increment;
            }
        }

        let rp = ReadParams {
            nextzoom,
            maxzoom,
            extend_zooms_max,
        };
        let reads: Vec<ReadTile> = std::mem::take(&mut level)
            .into_par_iter()
            .map(|t| read_tile(&ctx, t, &rp))
            .collect();

        let mut err: i32 = i32::MAX;
        let mut error: Option<String> = None;

        let mut th = Thresholds {
            gamma: o.gamma,
            mingap: 0,
            minextent: 0,
            mindrop_sequence: 0,
            minattribute: if o.drop_by_attribute_descending {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            },
        };
        let mut zoom_tile_size: usize = 0;
        let mut zoom_feature_count: usize = 0;
        let mut results: Vec<MakeResult>;

        loop {
            let simplification = if z == maxzoom && o.maxzoom_simplification > 0.0 {
                o.maxzoom_simplification
            } else {
                o.simplification
            };
            let mp = MakeParams {
                maxzoom,
                simplification,
                th,
            };
            results = reads
                .par_iter()
                .map(|rt| make_tile(&ctx, rt, &mp))
                .collect();

            let mut again = false;
            let mut extend_zooms = false;
            let mut strategy = Strategy::default();

            for (rt, r) in reads.iter().zip(results.iter_mut()) {
                if rt.z > z {
                    z = rt.z;
                }
                if rt.z > largest_written {
                    largest_written = rt.z;
                }

                if rt.z == mp.maxzoom {
                    let a: u64 = ((rt.x as u64) << 32) | rt.y as u64;
                    let b: u64 = ((stats.midx as u64) << 32) | stats.midy as u64;
                    if r.len > most || (r.len == most && a < b) {
                        stats.midx = rt.x;
                        stats.midy = rt.y;
                        most = r.len;
                    }
                }

                if r.len < 0 {
                    err = rt.z - 1;
                    if error.is_none() {
                        error = r.error.take();
                    }
                }

                if let Some(s) = &r.strategy {
                    strategy.dropped_by_rate += s.dropped_by_rate;
                    strategy.dropped_by_gamma += s.dropped_by_gamma;
                    strategy.dropped_as_needed += s.dropped_as_needed;
                    strategy.coalesced_as_needed += s.coalesced_as_needed;
                    strategy.detail_reduced += s.detail_reduced;
                    strategy.tiny_polygons += s.tiny_polygons;
                    strategy.truncated_zooms += s.truncated_zooms;
                }

                if !r.tilestats.is_empty() {
                    tilestats.push(std::mem::take(&mut r.tilestats));
                }

                let out = r.th_out;
                if out.gamma > th.gamma {
                    th.gamma = out.gamma;
                    again = true;
                }
                if out.mingap > th.mingap {
                    th.mingap = out.mingap;
                    again = true;
                }
                if out.minextent > th.minextent {
                    th.minextent = out.minextent;
                    again = true;
                }
                if out.mindrop_sequence > th.mindrop_sequence {
                    th.mindrop_sequence = out.mindrop_sequence;
                    again = true;
                }
                let attr_propagate = if o.drop_by_attribute_descending {
                    out.minattribute < th.minattribute
                } else {
                    out.minattribute > th.minattribute
                };
                if attr_propagate {
                    th.minattribute = out.minattribute;
                    again = true;
                }
                zoom_tile_size = zoom_tile_size.max(r.tile_size_out);
                zoom_feature_count = zoom_feature_count.max(r.feature_count_out);

                if r.still_dropping {
                    extend_zooms = true;
                }
            }

            if let Some(message) = error {
                return Err(Error::new(EXIT_INCOMPLETE, message));
            }

            if extend_zooms
                && (o.a(A_EXTEND_ZOOMS) || extend_zooms_max > 0)
                && z == maxzoom
                && maxzoom < MAX_ZOOM
            {
                maxzoom += 1;
                if extend_zooms_max > 0 {
                    extend_zooms_max -= 1;
                }
            }

            if z as usize >= stats.strategies.len() {
                stats.strategies.resize(z as usize + 1, Strategy::default());
            }
            strategy.tile_size = zoom_tile_size;
            strategy.feature_count = zoom_feature_count;
            stats.strategies[z as usize] = strategy;

            if !again {
                break;
            }

            // the tiles of this zoom are made again; merge what this pass
            // added to the tilestats so it is not lost
            let merged = merge_tilestats(std::mem::take(&mut tilestats));
            if !merged.is_empty() {
                tilestats.push(merged);
            }
        }

        for (rt, r) in reads.iter().zip(results.iter()) {
            if let Some(tile) = &r.tile {
                sink.write_tile(rt.z as u8, rt.x, rt.y, tile)?;
                stats.tiles += 1;
            }
        }

        let merged = merge_tilestats(std::mem::take(&mut tilestats));
        if !merged.is_empty() {
            tilestats.push(merged);
        }

        // the next zoom's input, in tile order
        let mut next: Vec<TileInput> = Vec::new();
        let mut shards = std::collections::BTreeSet::new();
        let span_mask = (1u32 << max_zoom_increment) - 1;
        for (k, rt) in reads.into_iter().enumerate() {
            for child in rt.children {
                let j = ((child.x.wrapping_shl(max_zoom_increment as u32) | (child.y & span_mask))
                    & (child_shards as u32).wrapping_sub(1)) as usize;
                shards.insert((k % threads) * child_shards as usize + j);
                next.push(child);
            }
        }
        next.sort_by_key(|t| (t.x, t.y));
        useful_threads = shards.len();
        level = next;

        if err != i32::MAX {
            stats.incomplete = true;
            stats.maxzoom = err;
            stats.layermap = merge_tilestats(tilestats);
            return Ok(stats);
        }

        z += 1;
    }

    if largest_written >= 0 && maxzoom > largest_written {
        maxzoom = largest_written;
    }

    stats.maxzoom = maxzoom;
    stats.layermap = merge_tilestats(tilestats);
    Ok(stats)
}

/// Merges the attribute statistics of several tiles. Every entry came from
/// tiling, so each stays marked as one to keep, as tippecanoe's tiling
/// segments are until the final merge.
fn merge_tilestats(maps: Vec<BTreeMap<String, LayermapEntry>>) -> BTreeMap<String, LayermapEntry> {
    match maps.len() {
        0 => BTreeMap::new(),
        1 => maps.into_iter().next().unwrap(),
        _ => {
            let mut merged = merge_layermaps(&maps);
            for e in merged.values_mut() {
                e.retain = 1;
            }
            merged
        }
    }
}
