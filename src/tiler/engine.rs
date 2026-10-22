//! The order of work: zoom levels, the passes tippecanoe makes over a zoom
//! level when a tile needs a stricter threshold, and the parallelism.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use rayon::prelude::*;
use tippecanoe::errors::{EXIT_ARGS, EXIT_IMPOSSIBLE, EXIT_INCOMPLETE};
use tippecanoe::globals::MAX_ZOOM;
use tippecanoe::jsonpull::JsonRef;
use tippecanoe::mbtiles::{LayermapEntry, merge_layermaps};
use tippecanoe::mvt::{MVT_NULL, MvtValue, stringified_to_mvt_value};
use tippecanoe::options::*;
use tippecanoe::tile::Strategy;

use super::encode::{ValueCodes, key_slot};
use super::make::{MakeParams, MakeResult, Thresholds, dropping_features, make_tile, reuse};
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
    /// The encodings of the values, for writing tiles.
    pub codes: ValueCodes,
    /// Each key's slot in tippecanoe's key dedup cache.
    pub key_slots: Vec<u32>,
    /// How many threads the work inside one tile may use.
    pub threads: usize,
    /// For `TileOptions::timing`: attempts, compressions, bytes compressed
    /// and nanoseconds spent compressing.
    pub counters: [std::sync::atomic::AtomicU64; 4],
    pub started: std::time::Instant,
}

impl<'a> Ctx<'a> {
    pub fn new(store: &'a FeatureStore, o: &'a TileOptions, threads: usize) -> Ctx<'a> {
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

        let codes = ValueCodes::new(&values);
        let key_slots = store.keys.iter().map(|k| key_slot(k)).collect();

        Ctx {
            codes,
            key_slots,
            threads,
            counters: Default::default(),
            started: std::time::Instant::now(),
            store,
            o,
            values,
            value_null,
            layer_rank,
            filter: o.filter.as_ref().map(|t| t.root()),
        }
    }
}

fn unsupported(ctx: &Ctx<'_>) -> Option<&'static str> {
    let store = ctx.store;
    if ctx.o.a(A_VARIABLE_DEPTH_PYRAMID)
        && (store.pool_offsets.len() != store.attrs.len()
            || store.segment_layers.is_empty()
            || store.initial_x.is_empty())
    {
        return Some("variable depth pyramids (-at) without the store's serialized layout");
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

/// The first tile's input: every feature of the store.
pub fn root_input(ctx: &Ctx<'_>) -> Option<TileInput> {
    let store = ctx.store;
    let o = ctx.o;
    if store.features.is_empty() {
        return None;
    }
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
    Some(TileInput {
        z: o.iz,
        x: o.ix,
        y: o.iy,
        feats,
        geom: Vec::new(),
        root: true,
        skip_unless_dropping: false,
        complexity: Some(o.estimated_complexity),
    })
}

/// How many zoom levels the reading may run ahead of the last finished level.
const LOOKAHEAD: usize = 3;

enum Task {
    Read {
        level: usize,
        input: TileInput,
        rp: ReadParams,
    },
    Make {
        level: usize,
        idx: usize,
        pass: usize,
        rt: Arc<ReadTile>,
        mp: MakeParams,
        prev: Option<Box<MakeResult>>,
    },
}

struct Ready {
    key: (usize, u8, u64),
    task: Task,
}

impl PartialEq for Ready {
    fn eq(&self, o: &Ready) -> bool {
        self.key == o.key
    }
}

impl Eq for Ready {}

impl PartialOrd for Ready {
    fn partial_cmp(&self, o: &Ready) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Ready {
    fn cmp(&self, o: &Ready) -> std::cmp::Ordering {
        self.key.cmp(&o.key)
    }
}

enum Event {
    Read {
        level: usize,
        tile: ReadTile,
    },
    Made {
        level: usize,
        idx: usize,
        pass: usize,
        result: MakeResult,
    },
}

/// The maximum zoom during a pass, and each tile's z, x, y and vertex count.
type PassTiles = (i32, Vec<(i32, u32, u32, i64)>);

/// One zoom level: the tiles that tippecanoe would read from one zoom's
/// temporary files.
struct Level {
    z: i32,
    /// tippecanoe's zoom counter when it starts on this level.
    loop_z: i32,
    nextzoom: Option<i32>,
    threads: usize,
    child_shards: i32,
    max_zoom_increment: i32,
    /// Inputs not read yet.
    queued: Vec<TileInput>,
    reading: usize,
    /// For tippecanoe's count of temporary files with data: each input's
    /// parent tile and its own position.
    origins: Vec<((u32, u32), (u32, u32))>,
    tiles: Vec<Arc<ReadTile>>,
    /// With `-at`: each tile's children, until it is known whether the tile
    /// writes them.
    held: Vec<Vec<TileInput>>,
    /// With `-at`: whether each tile's children must not try to stop early,
    /// from the pass in which the tile was first made.
    children_must_go_on: Vec<Option<bool>>,
    /// Whether any pass made a tile rather than skipping them all.
    wrote: bool,
    inputs_complete: bool,
    makes_started: bool,
    pass: usize,
    th: Thresholds,
    results: Vec<Option<MakeResult>>,
    outstanding: usize,
    /// Per pass: the maximum zoom then and each tile's z, x, y and vertex count.
    history: Vec<PassTiles>,
    tilestats: Vec<BTreeMap<String, LayermapEntry>>,
    zoom_tile_size: usize,
    zoom_feature_count: usize,
    strategy: Strategy,
    /// The zoom counter after the last pass, where the strategies go.
    strategy_z: i32,
    err: bool,
    done: bool,
}

impl Level {
    fn new(z: i32, loop_z: i32, o: &TileOptions) -> Level {
        Level {
            z,
            loop_z,
            nextzoom: None,
            threads: 1,
            child_shards: 0,
            max_zoom_increment: 0,
            queued: Vec::new(),
            reading: 0,
            origins: Vec::new(),
            tiles: Vec::new(),
            held: Vec::new(),
            children_must_go_on: Vec::new(),
            wrote: false,
            inputs_complete: false,
            makes_started: false,
            pass: 0,
            th: Thresholds {
                gamma: o.gamma,
                mingap: 0,
                minextent: 0,
                mindrop_sequence: 0,
                minattribute: if o.drop_by_attribute_descending {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                },
            },
            results: Vec::new(),
            outstanding: 0,
            history: Vec::new(),
            tilestats: Vec::new(),
            zoom_tile_size: 0,
            zoom_feature_count: 0,
            strategy: Strategy::default(),
            strategy_z: z,
            err: false,
            done: false,
        }
    }

    fn all_read(&self) -> bool {
        self.inputs_complete && self.queued.is_empty() && self.reading == 0
    }
}

struct Driver<'d, 's> {
    ctx: &'s Ctx<'s>,
    sink: &'s dyn TileSink,
    scope: &'d rayon::Scope<'s>,
    tx: Sender<Event>,
    /// Tasks waiting for a thread, shallower levels first.
    ready: BinaryHeap<Reverse<Ready>>,
    seq: u64,
    /// How many tasks the pool holds at once: enough that no thread waits for
    /// the coordinator, few enough that shallower levels keep their priority.
    /// Tasks never wait on rayon work inside, so a queued task cannot be
    /// picked up by a thread in the middle of another.
    max_in_flight: usize,
    levels: Vec<Level>,
    maxzoom: i32,
    extend_zooms_max: i64,
    finalized: usize,
    in_flight: usize,
    largest_written: i32,
    stopped: bool,
    most: i64,
    stats: TileStats,
    tilestats: Vec<BTreeMap<String, LayermapEntry>>,
    error: Option<Error>,
    timing: bool,
    started: std::time::Instant,
    /// `-at`: levels are made one after the other, since whether a tile is
    /// made depends on the outcome of its parent's level.
    variable_depth: bool,
}

/// Runs the tiling: a coordinator on the calling thread that hands the reading
/// and making of tiles to the pool and applies tippecanoe's zoom-level rules
/// to the results in zoom order.
pub fn drive<'s>(
    ctx: &'s Ctx<'s>,
    root: Option<TileInput>,
    sink: &'s dyn TileSink,
    scope: &rayon::Scope<'s>,
    threads: usize,
) -> crate::Result<TileStats> {
    let o = ctx.o;
    if let Some(what) = unsupported(ctx) {
        return Err(Error::new(
            EXIT_ARGS,
            format!("manby does not support {} yet", what),
        ));
    }

    let (tx, rx) = channel::<Event>();
    let mut d = Driver {
        ctx,
        sink,
        scope,
        tx,
        ready: BinaryHeap::new(),
        seq: 0,
        max_in_flight: threads.max(1) * 4,
        levels: Vec::new(),
        maxzoom: o.maxzoom,
        extend_zooms_max: o.extend_zooms_max,
        finalized: 0,
        in_flight: 0,
        largest_written: -1,
        stopped: false,
        most: 0,
        stats: TileStats::default(),
        tilestats: Vec::new(),
        error: None,
        timing: o.timing,
        started: std::time::Instant::now(),
        variable_depth: o.a(A_VARIABLE_DEPTH_PYRAMID),
    };

    if let Some(root) = root {
        let mut level = Level::new(o.iz, o.iz, o);
        level.origins.push(((root.x, root.y), (root.x, root.y)));
        level.queued.push(root);
        level.inputs_complete = true;
        d.levels.push(level);
        d.level_inputs_complete(0);
    }

    loop {
        d.pump();
        if d.in_flight == 0 {
            break;
        }
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break,
        };
        d.in_flight -= 1;
        if d.error.is_some() {
            d.ready.clear();
            continue;
        }
        match event {
            Event::Read { level, tile } => d.on_read(level, tile),
            Event::Made {
                level,
                idx,
                pass,
                result,
            } => d.on_made(level, idx, pass, result),
        }
    }

    if let Some(e) = d.error {
        return Err(e);
    }

    let mut stats = d.stats;
    if !stats.incomplete && stats.strategies.len() < (d.maxzoom + 1) as usize {
        stats
            .strategies
            .resize((d.maxzoom + 1) as usize, Strategy::default());
    }
    let mut maxzoom = d.maxzoom;
    if stats.incomplete {
        maxzoom = stats.maxzoom;
    } else if d.largest_written >= 0 && maxzoom > d.largest_written {
        maxzoom = d.largest_written;
    }
    stats.maxzoom = maxzoom;
    stats.layermap = merge_tilestats(d.tilestats);
    Ok(stats)
}

impl<'d, 's> Driver<'d, 's> {
    fn enqueue(&mut self, level: usize, kind: u8, task: Task) {
        self.seq += 1;
        self.ready.push(Reverse(Ready {
            key: (level, kind, self.seq),
            task,
        }));
    }

    /// Hands ready tasks to the pool while threads are free.
    fn pump(&mut self) {
        while self.in_flight < self.max_in_flight {
            let Some(Reverse(ready)) = self.ready.pop() else {
                break;
            };
            self.in_flight += 1;
            let ctx = self.ctx;
            let tx = self.tx.clone();
            match ready.task {
                Task::Read { level, input, rp } => {
                    self.scope.spawn(move |_| {
                        let tile = read_tile(ctx, input, &rp);
                        let _ = tx.send(Event::Read { level, tile });
                    });
                }
                Task::Make {
                    level,
                    idx,
                    pass,
                    rt,
                    mp,
                    prev,
                } => {
                    self.scope.spawn(move |_| {
                        let result = match prev.and_then(|prev| reuse(ctx, &rt, *prev, &mp.th)) {
                            Some(reused) => reused,
                            None => make_tile(ctx, &rt, &mp),
                        };
                        let _ = tx.send(Event::Made {
                            level,
                            idx,
                            pass,
                            result,
                        });
                    });
                }
            }
        }
    }

    fn spawn_read(&mut self, k: usize, input: TileInput) {
        let level = &mut self.levels[k];
        let rp = ReadParams {
            nextzoom: level.nextzoom.unwrap(),
            maxzoom: self.maxzoom,
            extend_zooms_max: self.extend_zooms_max,
        };
        level.reading += 1;
        self.enqueue(
            k,
            0,
            Task::Read {
                level: k,
                input,
                rp,
            },
        );
    }

    fn spawn_make(&mut self, k: usize, idx: usize) {
        self.spawn_make_after(k, idx, None);
    }

    /// Queues the making of tile `idx` of level `k` in its current pass;
    /// `prev` is its result from the pass before, which may still hold.
    fn spawn_make_after(&mut self, k: usize, idx: usize, prev: Option<Box<MakeResult>>) {
        let o = self.ctx.o;
        let level = &mut self.levels[k];
        let pass = level.pass;
        if level.tiles[idx].skip_unless_dropping
            && !dropping_features(&level.th, o.drop_by_attribute_descending)
        {
            level.results[idx] = Some(skipped(level.th));
            return;
        }
        let loop_z = if pass == 0 { level.loop_z } else { level.z };
        let simplification = if loop_z == self.maxzoom && o.maxzoom_simplification > 0.0 {
            o.maxzoom_simplification
        } else {
            o.simplification
        };
        let mp = MakeParams {
            maxzoom: self.maxzoom,
            simplification,
            th: level.th,
        };
        level.outstanding += 1;
        let rt = level.tiles[idx].clone();
        self.enqueue(
            k,
            1,
            Task::Make {
                level: k,
                idx,
                pass,
                rt,
                mp,
                prev,
            },
        );
    }

    /// Spawns the reads that are waiting, if the level may read now.
    fn release_reads(&mut self, k: usize) {
        if self.stopped || k >= self.finalized + LOOKAHEAD || self.levels[k].nextzoom.is_none() {
            return;
        }
        let queued = std::mem::take(&mut self.levels[k].queued);
        for input in queued {
            self.spawn_read(k, input);
        }
    }

    fn makes_allowed(&self, k: usize) -> bool {
        !self.stopped && self.levels[k].z <= self.maxzoom
    }

    fn start_makes(&mut self, k: usize) {
        if self.levels[k].makes_started || !self.makes_allowed(k) {
            return;
        }
        self.levels[k].makes_started = true;
        let n = self.levels[k].tiles.len();
        self.levels[k].results.resize_with(n, || None);
        for idx in 0..n {
            self.spawn_make(k, idx);
        }
    }

    /// All the inputs of level `k` are known: work out what tippecanoe's
    /// thread count would be, and from it the next zoom.
    fn level_inputs_complete(&mut self, k: usize) {
        let o = self.ctx.o;
        let level = &mut self.levels[k];
        level.inputs_complete = true;

        let useful_threads = if k == 0 {
            1
        } else {
            let prev = &self.levels[k - 1];
            let threads = prev.threads;
            let cs = prev.child_shards as u32;
            let mzi = prev.max_zoom_increment as u32;
            let mut parents: Vec<(u32, u32)> =
                self.levels[k].origins.iter().map(|(p, _)| *p).collect();
            parents.sort();
            parents.dedup();
            let mut shards = std::collections::BTreeSet::new();
            for (parent, child) in &self.levels[k].origins {
                let slot = parents.binary_search(parent).unwrap() % threads;
                let j = (child.0.wrapping_shl(mzi) | (child.1 & ((1u32 << mzi) - 1)))
                    & cs.wrapping_sub(1);
                shards.insert(slot * cs as usize + j as usize);
            }
            shards.len()
        };

        let level = &mut self.levels[k];
        let _ = &level;
        level.threads = tippecanoe_threads(o, useful_threads);
        level.child_shards = (o.temp_files / level.threads) as i32;
        level.max_zoom_increment = ((level.child_shards as f64).ln() / 4f64.ln()) as i32;
        if level.child_shards < 4 || level.max_zoom_increment < 1 {
            self.error = Some(Error::new(
                EXIT_IMPOSSIBLE,
                format!(
                    "Internal error: {} shards, max zoom increment {}",
                    level.child_shards, level.max_zoom_increment
                ),
            ));
            return;
        }
        if (((level.child_shards - 1) << 1) & level.child_shards) != level.child_shards {
            self.error = Some(Error::new(
                EXIT_IMPOSSIBLE,
                format!(
                    "Internal error: {} shards not a power of 2",
                    level.child_shards
                ),
            ));
            return;
        }
        if level.nextzoom.is_none() {
            let mut nextzoom = level.z + 1;
            if nextzoom < o.minzoom {
                if level.z + level.max_zoom_increment > o.minzoom {
                    nextzoom = o.minzoom;
                } else {
                    nextzoom = level.z + level.max_zoom_increment;
                }
            }
            level.nextzoom = Some(nextzoom);
        }

        self.release_reads(k);
        self.start_makes(k);
        self.check_children_complete(k);
        self.check_pass(k);
    }

    /// The next level's inputs are complete once this level is all read.
    fn check_children_complete(&mut self, k: usize) {
        if self.variable_depth || !self.levels[k].all_read() {
            return;
        }
        if k + 1 < self.levels.len() && !self.levels[k + 1].inputs_complete {
            self.level_inputs_complete(k + 1);
        }
    }

    fn on_read(&mut self, k: usize, mut tile: ReadTile) {
        let o = self.ctx.o;
        self.levels[k].reading -= 1;
        let children = std::mem::take(&mut tile.children);
        let parent = (tile.x, tile.y);
        let idx = self.levels[k].tiles.len();
        self.levels[k].tiles.push(Arc::new(tile));

        if self.variable_depth {
            self.levels[k].held.push(children);
            self.levels[k].children_must_go_on.push(None);
        } else if !children.is_empty() {
            if k + 1 == self.levels.len() {
                let z = children[0].z;
                let mut level = Level::new(z, self.levels[k].z + 1, o);
                if z + 1 >= o.minzoom {
                    level.nextzoom = Some(z + 1);
                }
                self.levels.push(level);
                self.start_makes(k + 1);
            }
            let next = &mut self.levels[k + 1];
            for child in children {
                next.origins.push((parent, (child.x, child.y)));
                next.queued.push(child);
            }
            self.release_reads(k + 1);
        }

        if self.levels[k].makes_started {
            self.levels[k].results.push(None);
            self.spawn_make(k, idx);
        }

        self.check_children_complete(k);
        self.check_pass(k);
    }

    fn on_made(&mut self, k: usize, idx: usize, pass: usize, result: MakeResult) {
        let level = &mut self.levels[k];
        debug_assert_eq!(pass, level.pass);
        level.outstanding -= 1;
        if level.children_must_go_on.len() > idx && level.children_must_go_on[idx].is_none() {
            level.children_must_go_on[idx] = Some(result.children_must_go_on);
        }
        level.results[idx] = Some(result);
        self.check_pass(k);
    }

    fn check_pass(&mut self, k: usize) {
        let level = &self.levels[k];
        if level.done
            || !level.makes_started
            || !level.all_read()
            || level.outstanding > 0
            || level.results.len() != level.tiles.len()
        {
            return;
        }
        self.complete_pass(k);
    }

    /// Every tile of level `k` has been made in this pass: apply tippecanoe's
    /// rules for the zoom level.
    fn complete_pass(&mut self, k: usize) {
        let o = self.ctx.o;
        let maxzoom = self.maxzoom;
        let level = &mut self.levels[k];

        let mut order: Vec<usize> = (0..level.tiles.len()).collect();
        order.sort_by_key(|&i| (level.tiles[i].x, level.tiles[i].y));

        let mut z = if level.pass == 0 {
            level.loop_z
        } else {
            level.z
        };
        let mut again = false;
        let mut extend_zooms = false;
        let mut strategy = Strategy::default();
        let mut history = Vec::with_capacity(order.len());
        let mut error: Option<String> = None;

        for &i in &order {
            let rt = &level.tiles[i];
            let r = level.results[i].as_mut().unwrap();
            history.push((rt.z, rt.x, rt.y, r.len));
            if r.skipped {
                continue;
            }
            level.wrote = true;
            if rt.z > z {
                z = rt.z;
            }

            if r.len < 0 {
                level.err = true;
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
                level.tilestats.push(std::mem::take(&mut r.tilestats));
            }

            let th = &mut level.th;
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
            level.zoom_tile_size = level.zoom_tile_size.max(r.tile_size_out);
            level.zoom_feature_count = level.zoom_feature_count.max(r.feature_count_out);

            if r.still_dropping {
                extend_zooms = true;
            }
        }
        level.history.push((maxzoom, history));

        if let Some(message) = error {
            self.error = Some(Error::new(EXIT_INCOMPLETE, message));
            return;
        }

        if extend_zooms
            && (o.a(A_EXTEND_ZOOMS) || self.extend_zooms_max > 0)
            && z == self.maxzoom
            && self.maxzoom < MAX_ZOOM
        {
            self.maxzoom += 1;
            if self.extend_zooms_max > 0 {
                self.extend_zooms_max -= 1;
            }
        }

        let level = &mut self.levels[k];
        strategy.tile_size = level.zoom_tile_size;
        strategy.feature_count = level.zoom_feature_count;
        level.strategy = strategy;
        level.strategy_z = z;

        if self.timing {
            use std::sync::atomic::Ordering::Relaxed;
            let c: Vec<u64> = self
                .ctx
                .counters
                .iter()
                .map(|c| c.swap(0, Relaxed))
                .collect();
            eprintln!(
                "{:.3}s: z{} pass {} done: {} tiles; since last: {} attempts, {} compressions of {} MB taking {:.3}s",
                self.started.elapsed().as_secs_f64(),
                level.z,
                level.pass,
                level.tiles.len(),
                c[0],
                c[1],
                c[2] >> 20,
                c[3] as f64 / 1e9
            );
        }

        if again {
            level.pass += 1;
            let prevs: Vec<Option<Box<MakeResult>>> = level
                .results
                .iter_mut()
                .map(|r| r.take().map(Box::new))
                .collect();
            for (idx, prev) in prevs.into_iter().enumerate() {
                self.spawn_make_after(k, idx, prev);
            }
            // tiles that are skipped are done at once
            self.check_pass(k);
            return;
        }

        level.done = true;
        if self.variable_depth {
            self.release_children(k);
        }
        self.try_finalize();

        // a level beyond the old maximum zoom may now be made
        for j in 0..self.levels.len() {
            if self.levels[j].tiles.len() + self.levels[j].reading + self.levels[j].queued.len() > 0
                && self.levels[j].inputs_complete
            {
                self.start_makes(j);
                self.check_pass(j);
            }
        }
    }

    /// With `-at`, once level `k` is done: the children of the tiles that
    /// wrote theirs become the next level, those of tiles that stopped early
    /// to be made only if their level drops features.
    fn release_children(&mut self, k: usize) {
        let o = self.ctx.o;
        if self.levels[k].err {
            return;
        }
        let level = &mut self.levels[k];
        let mut order: Vec<usize> = (0..level.tiles.len()).collect();
        order.sort_by_key(|&i| (level.tiles[i].x, level.tiles[i].y));

        let mut next: Vec<((u32, u32), TileInput)> = Vec::new();
        for &i in &order {
            let r = level.results[i].as_ref().unwrap();
            if r.skipped {
                continue;
            }
            let rt = &level.tiles[i];
            let must_go_on = level.children_must_go_on[i].unwrap_or(false);
            for mut child in std::mem::take(&mut level.held[i]) {
                child.skip_unless_dropping = r.truncated && child.z == rt.z + 1;
                if must_go_on {
                    // large enough not to try to stop early
                    child.complexity = Some(1i64 << 32);
                }
                next.push(((rt.x, rt.y), child));
            }
        }
        level.held = Vec::new();
        if next.is_empty() {
            return;
        }

        debug_assert_eq!(k + 1, self.levels.len());
        let z = next[0].1.z;
        let mut nl = Level::new(z, self.levels[k].z + 1, o);
        if z + 1 >= o.minzoom {
            nl.nextzoom = Some(z + 1);
        }
        for (parent, child) in next {
            nl.origins.push((parent, (child.x, child.y)));
            nl.queued.push(child);
        }
        self.levels.push(nl);
        self.level_inputs_complete(k + 1);
    }

    /// Writes out the finished levels in zoom order.
    fn try_finalize(&mut self) {
        while self.finalized < self.levels.len() && self.levels[self.finalized].done {
            if self.stopped {
                return;
            }
            let k = self.finalized;
            if let Err(e) = self.finalize(k) {
                self.error = Some(e);
                return;
            }
            self.finalized += 1;
            for j in self.finalized..self.levels.len() {
                self.release_reads(j);
            }
        }

        // levels deeper than the maximum zoom are never made once every level
        // that could extend the maximum zoom is done
        if self.finalized < self.levels.len()
            && !self.levels[self.finalized].done
            && self.levels[self.finalized].z > self.maxzoom
        {
            self.stopped = true;
        }
    }

    fn finalize(&mut self, k: usize) -> crate::Result<()> {
        let level = &mut self.levels[k];

        self.most = 0;
        for (pass_maxzoom, tiles) in &level.history {
            for &(z, x, y, len) in tiles {
                if z == *pass_maxzoom {
                    let a: u64 = ((x as u64) << 32) | y as u64;
                    let b: u64 = ((self.stats.midx as u64) << 32) | self.stats.midy as u64;
                    if len > self.most || (len == self.most && a < b) {
                        self.stats.midx = x;
                        self.stats.midy = y;
                        self.most = len;
                    }
                }
            }
        }

        let z = level.strategy_z;
        if z as usize >= self.stats.strategies.len() {
            self.stats
                .strategies
                .resize(z as usize + 1, Strategy::default());
        }
        self.stats.strategies[z as usize] = level.strategy;
        if level.wrote && level.z > self.largest_written {
            self.largest_written = level.z;
        }

        let mut order: Vec<usize> = (0..level.tiles.len()).collect();
        order.sort_by_key(|&i| (level.tiles[i].x, level.tiles[i].y));
        for &i in &order {
            let rt = &level.tiles[i];
            if let Some(r) = &level.results[i]
                && let Some(tile) = &r.tile
            {
                self.sink.write_tile(rt.z as u8, rt.x, rt.y, tile)?;
                self.stats.tiles += 1;
            }
        }

        self.tilestats.append(&mut level.tilestats);
        if self.tilestats.len() > 64 {
            let merged = merge_tilestats(std::mem::take(&mut self.tilestats));
            self.tilestats.push(merged);
        }

        if level.err {
            self.stopped = true;
            self.stats.incomplete = true;
            self.stats.maxzoom = level.z - 1;
        }

        // free the level's memory
        level.tiles = Vec::new();
        level.results = Vec::new();
        Ok(())
    }
}

/// The result for a tile skipped because its parent stopped early: tippecanoe
/// reads past it and counts it as one vertex.
fn skipped(th: Thresholds) -> MakeResult {
    MakeResult {
        len: 1,
        tile: None,
        strategy: None,
        th_out: th,
        tile_size_out: 0,
        feature_count_out: 0,
        still_dropping: false,
        tilestats: BTreeMap::new(),
        error: None,
        reusable: None,
        truncated: false,
        children_must_go_on: false,
        skipped: true,
    }
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
