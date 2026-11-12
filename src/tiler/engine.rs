//! The order of work: zoom levels, the passes tippecanoe makes over a zoom
//! level when a tile needs a stricter threshold, and the parallelism.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use tippecanoe::errors::{EXIT_ARGS, EXIT_IMPOSSIBLE, EXIT_INCOMPLETE};
use tippecanoe::globals::MAX_ZOOM;
use tippecanoe::mbtiles::{LayermapEntry, merge_layermaps};
use tippecanoe::mvt::{MVT_NULL, MvtValue, stringified_to_mvt_value};
use tippecanoe::options::*;
use tippecanoe::tile::Strategy;

use super::encode::{ValueCodes, key_slot};
use super::make::{MakeParams, MakeResult, Thresholds, dropping_features, make_tile};
use super::read::{Parked, ReadParams, TileInput, read_tile, store_inputs};
use super::util::Helpers;
use super::{TileOptions, TileStats};
use crate::buf::Buf;
use crate::error::Error;
use crate::mem::{Place, Rest, Spool};
use crate::sink::TileSink;
use crate::store::FeatureStore;
use rayon::prelude::*;

/// What every tile of a run shares.
pub struct Ctx<'a> {
    pub store: &'a FeatureStore,
    pub o: &'a TileOptions,
    pub value_null: Buf<bool>,
    /// The position of each layer when the layers are sorted by name.
    pub layer_rank: Vec<u32>,
    pub filter: Option<&'a crate::filter::Filter>,
    /// The encodings of the values, for writing tiles.
    pub codes: ValueCodes,
    /// Each key's slot in tippecanoe's key dedup cache.
    pub key_slots: Vec<u32>,
    /// `--accumulate-attribute`'s operation for each of the store's keys.
    pub key_accum: Vec<Option<tippecanoe::attribute::AttributeOp>>,
    /// With `--accumulate-attribute`, each value as `atof` reads it.
    pub value_num: Buf<f64>,
    /// The threads that help with the work inside one tile.
    pub helpers: Arc<Helpers>,
    /// The memory for tile data that waits: inputs, and tiles not written yet.
    pub rest: Arc<Rest>,
    /// For `TileOptions::timing`: attempts, compressions, bytes compressed
    /// and nanoseconds spent compressing.
    pub counters: [std::sync::atomic::AtomicU64; 4],
    /// Set when a task fails, so that the others stop at their next attempt.
    pub failed: std::sync::atomic::AtomicBool,
    pub started: std::time::Instant,
}

impl<'a> Ctx<'a> {
    /// The store's value `i` as a tile value.
    pub fn value(&self, i: u32) -> MvtValue {
        let v = self.store.values.get(i);
        stringified_to_mvt_value(v.type_, v.s)
    }

    pub fn new(store: &'a FeatureStore, o: &'a TileOptions, helpers: Arc<Helpers>) -> Ctx<'a> {
        let value_null = store.values.iter().map(|v| v.type_ == MVT_NULL).collect();

        let mut order: Vec<usize> = (0..store.layers.len()).collect();
        order.sort_by(|&a, &b| store.layers[a].name.cmp(&store.layers[b].name));
        let mut layer_rank = vec![0u32; store.layers.len()];
        for (rank, &i) in order.iter().enumerate() {
            layer_rank[i] = rank as u32;
        }

        let codes = ValueCodes::new(&store.values);
        let key_slots = store.keys.iter().map(|k| key_slot(k)).collect();
        let key_accum = store.keys.iter().map(|k| o.accumulate_op(k)).collect();
        let value_num = if !o.accumulates() {
            Buf::new()
        } else {
            let mut nums = Buf::with_capacity(store.values.len());
            nums.resize(store.values.len(), 0.0);
            nums.par_chunks_mut(1 << 14)
                .enumerate()
                .for_each(|(c, part)| {
                    for (k, n) in part.iter_mut().enumerate() {
                        *n = super::accum::to_double(store.values.get(((c << 14) + k) as u32).s);
                    }
                });
            nums
        };

        Ctx {
            codes,
            key_slots,
            key_accum,
            value_num,
            helpers,
            rest: Rest::new(o.rest_memory),
            counters: Default::default(),
            failed: Default::default(),
            started: std::time::Instant::now(),
            store,
            o,
            value_null,
            layer_rank,
            filter: o.filter.as_ref(),
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
    Some(TileInput::root(
        o.iz,
        o.ix,
        o.iy,
        store.features.len(),
        o.estimated_complexity,
    ))
}

/// How many tiles may be made at once within the memory budget, each
/// holding up to a full tile's features while it is made.
fn tiles_at_once(o: &TileOptions) -> usize {
    let per_tile = o.max_tile_features as u64 * 200 + o.max_tile_size as u64 * 8;
    (o.memory_budget / 2 / per_tile.max(1)).clamp(2, usize::MAX as u64) as usize
}

/// Why a level's spools are there when they are used.
const LIVE: &str = "a level's spools live until it is finalized";

/// How many zoom levels the tiles may run ahead of the last finished level.
const LOOKAHEAD: usize = 3;

/// A tile whose reading and making takes about this much memory or more
/// counts as big. Big tiles run together only as far as their memory
/// stays within `TileOptions::big_memory`.
const BIG_TASK: u64 = 16 << 20;

/// Reading a tile and making it in one pass of its zoom level. The first
/// read also writes the tile's features out for the next zoom; later passes
/// read the tile again from its input, as tippecanoe does, rather than keep
/// whole zoom levels of candidates in memory.
struct Task {
    level: usize,
    idx: usize,
    pass: usize,
    /// About how much memory the task takes, if it counts as big.
    big: u64,
    /// Where the tile and its children wait until they are needed.
    spool: Arc<Spool>,
    child_spool: Arc<Spool>,
    input: Arc<TileInput>,
    rp: ReadParams,
    complexity: Option<i64>,
    mp: MakeParams,
    /// Whether to make the tile once it is read: not if another shard does.
    make: bool,
}

/// What a task reports: the first read of a tile as soon as it is done, so
/// that the children can start while the tile is being made, then the tile.
enum Done {
    Read {
        level: usize,
        idx: usize,
        children: Vec<TileInput>,
        complexity: i64,
    },
    Made {
        level: usize,
        idx: usize,
        pass: usize,
        big: u64,
        result: Box<MakeResult>,
        /// The tile's `Reuse` record, if it is reusable, followed by the
        /// tile, if one is written, in the level's spool.
        made: Option<Place>,
        /// The record's length, or 0.
        record: u8,
        tile: bool,
    },
    Failed(Error),
}

/// A tile of a zoom level. A level keeps one for each of its tiles until it
/// is written, which at high zooms of large polygons is millions, so it is
/// small and holds no pointers, and the level keeps them in blocks of
/// `Buf`s, which the memory budget can move to a file. The input waits in
/// the level's spool (or, kept whole, in `Level::whole`), and so do the
/// tile and what another pass needs to know of how it was made.
///
/// Its fields leave no gaps, so that blocks of them can be stored as bytes.
#[derive(Clone, Copy)]
#[repr(C)]
struct Slot {
    /// The input, and once the tile is read its complexity.
    input: Parked,
    /// Where the made bytes are in the level's spool: the `Reuse` record,
    /// of `record` bytes, and then the tile, with `TILE`. A `Place` packed
    /// small.
    made_at: u64,
    made_len: u32,
    made_shard: u8,
    record: u8,
    flags: u16,
}

/// The tile has been read, and so has written its children.
const READ: u16 = 1;
/// The current pass has made the tile (or skipped it, or reused it).
const MADE: u16 = 1 << 1;
const SKIPPED: u16 = 1 << 2;
const TRUNCATED: u16 = 1 << 3;
/// A higher threshold cannot change what the pass made of the tile: the
/// `Reuse` record at the start of its made bytes says under which.
const REUSABLE: u16 = 1 << 4;
/// The made bytes hold a tile, after the record if there is one.
const TILE: u16 = 1 << 6;
/// With `-at`: whether the tile's children must not try to stop early is
/// known, from the pass in which the tile was first made, and what it is.
const MUST_GO_ON_KNOWN: u16 = 1 << 7;
const MUST_GO_ON: u16 = 1 << 8;
/// The input is kept whole, in `Level::whole`.
const WHOLE: u16 = 1 << 9;
/// Which of the four tiles below at the next zoom the tile wrote children
/// for, one bit each, as `2 * (y & 1) + (x & 1)` of the child, when they
/// went to the next level.
const CHILDREN: u32 = 10;

const _: () = assert!(std::mem::size_of::<Slot>() == 56);

impl Slot {
    fn made(&self) -> Place {
        Place::with_shard_byte(self.made_at, self.made_len as u64, self.made_shard)
    }
}

/// A level's slots, in blocks, so that adding to millions of them never
/// moves them all, and each block is a vector that the memory budget can
/// move to a file. A level that may never be made, beyond the maximum zoom,
/// puts its full blocks in its spool until it starts.
#[derive(Default)]
struct Slots {
    blocks: Vec<Block>,
    len: usize,
}

enum Block {
    Here(Buf<Slot>),
    /// A full block, stored as bytes.
    Away(Place),
}

const BLOCK: usize = 1 << 16;

impl Slots {
    fn push(&mut self, s: Slot) {
        if self.len.is_multiple_of(BLOCK) {
            self.blocks.push(Block::Here(Buf::new()));
        }
        match self.blocks.last_mut() {
            Some(Block::Here(b)) => b.push(s),
            _ => unreachable!("a block is stored only once full"),
        }
        self.len += 1;
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn iter(&self) -> impl Iterator<Item = &Slot> {
        self.blocks.iter().flat_map(|b| match b {
            Block::Here(b) => b.iter(),
            Block::Away(_) => panic!("{}", AWAY),
        })
    }

    /// Stores the full blocks in `spool`.
    fn put_away(&mut self, spool: &Arc<Spool>) -> crate::Result<()> {
        for b in &mut self.blocks {
            if let Block::Here(slots) = b
                && slots.len() == BLOCK
            {
                // SAFETY: a slot is plain data without gaps between its
                // fields, so its bytes are all initialized
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        slots.as_ptr() as *const u8,
                        slots.len() * std::mem::size_of::<Slot>(),
                    )
                };
                let place = spool
                    .store(bytes)?
                    .place_in(spool)
                    .expect("stored in the spool");
                *b = Block::Away(place);
            }
        }
        Ok(())
    }

    /// Takes back the blocks stored in `spool`.
    fn bring_back(&mut self, spool: &Arc<Spool>) -> crate::Result<()> {
        for b in &mut self.blocks {
            if let Block::Away(place) = b {
                let stored = place.stored(spool, 0);
                let bytes = stored.load()?;
                let size = std::mem::size_of::<Slot>();
                let mut slots = Buf::with_capacity(BLOCK);
                for c in bytes.chunks_exact(size) {
                    // SAFETY: the bytes are those of slots, each its own
                    // `size` of them, read unaligned
                    slots.push(unsafe { std::ptr::read_unaligned(c.as_ptr() as *const Slot) });
                }
                debug_assert_eq!(slots.len(), BLOCK);
                *b = Block::Here(slots);
            }
        }
        Ok(())
    }
}

const AWAY: &str = "a level's slots are back in memory once it starts";

impl std::ops::Index<usize> for Slots {
    type Output = Slot;
    fn index(&self, i: usize) -> &Slot {
        match &self.blocks[i / BLOCK] {
            Block::Here(b) => &b[i % BLOCK],
            Block::Away(_) => panic!("{}", AWAY),
        }
    }
}

impl std::ops::IndexMut<usize> for Slots {
    fn index_mut(&mut self, i: usize) -> &mut Slot {
        match &mut self.blocks[i / BLOCK] {
            Block::Here(b) => &mut b[i % BLOCK],
            Block::Away(_) => panic!("{}", AWAY),
        }
    }
}

/// The thresholds and counts under which a tile stands for the next pass
/// too, as a higher threshold cannot change it, and its `MakeResult::len`.
struct Reuse {
    len: i64,
    th: Thresholds,
    gap: u64,
    drop_sequence: u64,
    strategy: Option<Strategy>,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(b: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let c = b[*pos];
        *pos += 1;
        v |= ((c & 0x7f) as u64) << shift;
        if c < 0x80 {
            return v;
        }
        shift += 7;
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

impl Reuse {
    /// The record, in varints: most of it is zero or the largest value,
    /// and millions of tiles write one.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        let th = &self.th;
        put_varint(&mut out, zigzag(self.len));
        put_varint(&mut out, th.gamma.to_bits());
        put_varint(&mut out, th.mingap);
        put_varint(&mut out, zigzag(th.minextent));
        put_varint(&mut out, th.mindrop_sequence);
        if th.minattribute == f64::NEG_INFINITY {
            out.push(0);
        } else if th.minattribute == f64::INFINITY {
            out.push(1);
        } else {
            out.push(2);
            out.extend_from_slice(&th.minattribute.to_bits().to_le_bytes());
        }
        put_varint(&mut out, self.gap.wrapping_add(1));
        put_varint(&mut out, self.drop_sequence.wrapping_add(1));
        match &self.strategy {
            None => out.push(0),
            Some(s) => {
                out.push(1);
                for f in [
                    s.dropped_by_rate,
                    s.dropped_by_gamma,
                    s.dropped_as_needed,
                    s.coalesced_as_needed,
                    s.detail_reduced,
                    s.tiny_polygons,
                    s.truncated_zooms,
                    s.tile_size,
                    s.feature_count,
                ] {
                    put_varint(&mut out, f as u64);
                }
            }
        }
        out
    }

    fn decode(b: &[u8]) -> Reuse {
        let mut pos = 0;
        let len = unzigzag(get_varint(b, &mut pos));
        let gamma = f64::from_bits(get_varint(b, &mut pos));
        let mingap = get_varint(b, &mut pos);
        let minextent = unzigzag(get_varint(b, &mut pos));
        let mindrop_sequence = get_varint(b, &mut pos);
        pos += 1;
        let minattribute = match b[pos - 1] {
            0 => f64::NEG_INFINITY,
            1 => f64::INFINITY,
            _ => {
                pos += 8;
                f64::from_bits(u64::from_le_bytes(b[pos - 8..pos].try_into().unwrap()))
            }
        };
        let gap = get_varint(b, &mut pos).wrapping_sub(1);
        let drop_sequence = get_varint(b, &mut pos).wrapping_sub(1);
        pos += 1;
        let strategy = (b[pos - 1] == 1).then(|| {
            let mut f = || get_varint(b, &mut pos) as usize;
            Strategy {
                dropped_by_rate: f(),
                dropped_by_gamma: f(),
                dropped_as_needed: f(),
                coalesced_as_needed: f(),
                detail_reduced: f(),
                tiny_polygons: f(),
                truncated_zooms: f(),
                tile_size: f(),
                feature_count: f(),
            }
        });
        debug_assert_eq!(pos, b.len());
        Reuse {
            len,
            th: Thresholds {
                gamma,
                mingap,
                minextent,
                mindrop_sequence,
                minattribute,
            },
            gap,
            drop_sequence,
            strategy,
        }
    }
}

/// The totals of a pass over a level, which its tiles add to as they are
/// made. They do not depend on the order the tiles come in.
struct Totals {
    /// tippecanoe's zoom counter.
    z: i32,
    /// The thresholds for another pass.
    next: Thresholds,
    strategy: Strategy,
    extend_zooms: bool,
    /// The message of the first tile in tile order that failed with one.
    error: Option<((u32, u32), String)>,
    /// The tile with the most vertices, and of those the first in x and y
    /// order: its vertex count, x and y.
    top: Option<(i64, u32, u32)>,
}

/// Whether tile `a`, as `Totals::top` has it, comes before `b`.
fn above(a: (i64, u32, u32), b: (i64, u32, u32)) -> bool {
    a.0 > b.0 || (a.0 == b.0 && (a.1, a.2) < (b.1, b.2))
}

/// The one of `a` and `b` that comes first.
fn top_of(a: Option<(i64, u32, u32)>, b: (i64, u32, u32)) -> (i64, u32, u32) {
    match a {
        Some(a) if !above(b, a) => a,
        _ => b,
    }
}

impl Totals {
    fn new(z: i32, th: Thresholds) -> Totals {
        Totals {
            z,
            next: th,
            strategy: Strategy::default(),
            extend_zooms: false,
            error: None,
            top: None,
        }
    }
}

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
    slots: Slots,
    /// The tiles waiting for a thread, in the order they were queued, which
    /// is the order of their slots, and with each the maximum zoom and
    /// `extend_zooms_max` when it was queued: from each tile in
    /// `spawn_limits` on, the index into `Driver::limits` there.
    queue: VecDeque<u32>,
    spawn_limits: Vec<(u32, u16)>,
    /// The inputs kept whole rather than in the spool: the first tile's, and
    /// those whose bytes went to the memory budget's file as they were read.
    whole: Vec<Arc<TileInput>>,
    /// With `-at`: the tiles' children, until it is known whether each tile
    /// writes them.
    held: HashMap<u32, Vec<TileInput>>,
    /// Where the level's inputs and tiles wait, and where its tiles write
    /// their children: the next level's inputs. Each spool goes, with its
    /// memory and its temporary file, when the level whose inputs it holds
    /// is finalized.
    spool: Option<Arc<Spool>>,
    child_spool: Option<Arc<Spool>>,
    /// How many tiles wait for the level to start: all of them until it
    /// starts, the tiles from 0 on.
    waiting: usize,
    /// Tiles not read yet.
    unread: usize,
    /// Tiles without a result in the current pass.
    missing: usize,
    inputs_complete: bool,
    started: bool,
    pass: usize,
    th: Thresholds,
    totals: Totals,
    outstanding: usize,
    /// Of the tiles of the passes made at the maximum zoom then, the one with
    /// the most vertices (`Totals::top`).
    top: Option<(i64, u32, u32)>,
    tilestats: Vec<BTreeMap<String, LayermapEntry>>,
    zoom_tile_size: usize,
    zoom_feature_count: usize,
    strategy: Strategy,
    /// The zoom counter after the last pass, where the strategies go.
    strategy_z: i32,
    err: bool,
    done: bool,
    /// Whether any pass made a tile rather than skipping them all.
    wrote: bool,
}

impl Level {
    fn new(z: i32, loop_z: i32, o: &TileOptions, spool: Arc<Spool>, rest: &Arc<Rest>) -> Level {
        let th = Thresholds {
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
        Level {
            z,
            loop_z,
            nextzoom: None,
            threads: 1,
            child_shards: 0,
            max_zoom_increment: 0,
            slots: Slots::default(),
            queue: VecDeque::new(),
            spawn_limits: Vec::new(),
            whole: Vec::new(),
            held: HashMap::new(),
            spool: Some(spool),
            child_spool: Some(Spool::new(&o.tmpdir, rest)),
            waiting: 0,
            unread: 0,
            missing: 0,
            inputs_complete: false,
            started: false,
            pass: 0,
            th,
            totals: Totals::new(loop_z, th),
            outstanding: 0,
            top: None,
            tilestats: Vec::new(),
            zoom_tile_size: 0,
            zoom_feature_count: 0,
            strategy: Strategy::default(),
            strategy_z: z,
            err: false,
            done: false,
            wrote: false,
        }
    }

    /// Adds what a pass made of tile `idx` to the pass's totals.
    fn fold(&mut self, idx: usize, mut r: MakeResult, descending: bool) {
        let z = self.z;
        let slot = &mut self.slots[idx];
        let (x, y) = (slot.input.x, slot.input.y);
        let t = &mut self.totals;
        t.top = Some(top_of(t.top, (r.len, x, y)));
        // what of the result another pass needs: the `Reuse` is in the made
        // bytes, from `run_task`, or from an earlier pass if the tile was
        // reused
        slot.flags &= !(SKIPPED | TRUNCATED | REUSABLE);
        slot.flags |= MADE;
        if r.skipped {
            slot.flags |= SKIPPED;
        }
        if r.truncated {
            slot.flags |= TRUNCATED;
        }
        if r.reusable.is_some() {
            debug_assert!(slot.record > 0);
            slot.flags |= REUSABLE;
        }
        if r.skipped {
            return;
        }
        self.wrote = true;
        t.z = t.z.max(z);

        if r.len < 0 {
            self.err = true;
            if let Some(m) = r.error.take()
                && t.error.as_ref().is_none_or(|(at, _)| (x, y) < *at)
            {
                t.error = Some(((x, y), m));
            }
        }

        if let Some(s) = &r.strategy {
            let a = &mut t.strategy;
            a.dropped_by_rate += s.dropped_by_rate;
            a.dropped_by_gamma += s.dropped_by_gamma;
            a.dropped_as_needed += s.dropped_as_needed;
            a.coalesced_as_needed += s.coalesced_as_needed;
            a.detail_reduced += s.detail_reduced;
            a.tiny_polygons += s.tiny_polygons;
            a.truncated_zooms += s.truncated_zooms;
        }

        if !r.tilestats.is_empty() {
            self.tilestats.push(std::mem::take(&mut r.tilestats));
            // merged as they come, as merging does not depend on the order
            if self.tilestats.len() > 64 {
                let merged = merge_tilestats(std::mem::take(&mut self.tilestats));
                self.tilestats.push(merged);
            }
        }

        let (n, out) = (&mut t.next, r.th_out);
        n.gamma = n.gamma.max(out.gamma);
        n.mingap = n.mingap.max(out.mingap);
        n.minextent = n.minextent.max(out.minextent);
        n.mindrop_sequence = n.mindrop_sequence.max(out.mindrop_sequence);
        if descending {
            n.minattribute = n.minattribute.min(out.minattribute);
        } else {
            n.minattribute = n.minattribute.max(out.minattribute);
        }
        self.zoom_tile_size = self.zoom_tile_size.max(r.tile_size_out);
        self.zoom_feature_count = self.zoom_feature_count.max(r.feature_count_out);
        if r.still_dropping {
            t.extend_zooms = true;
        }
    }

    /// The tiles in tippecanoe's order within a zoom.
    fn order(&self) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.slots.len() as u32).collect();
        order.sort_unstable_by_key(|&i| {
            let s = &self.slots[i as usize];
            (s.input.x, s.input.y)
        });
        order
    }

    /// Records that tile `idx` wrote a child at `x`, `y` at the next zoom.
    fn wrote_child(&mut self, idx: usize, x: u32, y: u32) {
        let s = &mut self.slots[idx];
        debug_assert_eq!((x >> 1, y >> 1), (s.input.x, s.input.y));
        s.flags |= 1 << (CHILDREN + 2 * (y & 1) + (x & 1));
    }

    /// The input of tile `idx`.
    fn input(&self, idx: usize) -> Arc<TileInput> {
        let slot = &self.slots[idx];
        if slot.flags & WHOLE != 0 {
            self.whole[slot.input.index()].clone()
        } else {
            Arc::new(slot.input.input(self.z, self.spool.as_ref().expect(LIVE)))
        }
    }

    /// About how much memory reading and making tile `idx` takes.
    fn working_set(&self, idx: usize, store: &FeatureStore, o: &TileOptions) -> u64 {
        let slot = &self.slots[idx];
        if slot.flags & WHOLE != 0 {
            self.whole[slot.input.index()].working_set(store, o)
        } else {
            slot.input.working_set(o)
        }
    }
}

struct Driver<'d, 's> {
    ctx: &'s Ctx<'s>,
    sink: &'s dyn TileSink,
    scope: &'d rayon::Scope<'s>,
    tx: Sender<Done>,
    /// How many tasks the pool holds at once: enough that no thread waits for
    /// the coordinator, few enough that shallower levels keep their priority.
    /// Tasks never wait on rayon work inside, so a queued task cannot be
    /// picked up by a thread in the middle of another.
    max_in_flight: usize,
    levels: Vec<Level>,
    maxzoom: i32,
    extend_zooms_max: i64,
    /// Each maximum zoom and `extend_zooms_max` there has been, for
    /// `Level::spawn_limits`.
    limits: Vec<(i32, i64)>,
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
    /// The estimated memory of the big tiles running, and its limit.
    running_big: u64,
    big_memory: u64,
    /// `--shard`: the tiles this run reads and makes, and what it found.
    shard: Option<crate::shard::Range>,
    tiling: super::ShardTiling,
}

/// Runs the tiling: a coordinator on the calling thread that hands the tiles
/// to the pool and applies tippecanoe's zoom-level rules to the results in
/// zoom order.
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

    let (tx, rx) = channel::<Done>();
    let mut d = Driver {
        ctx,
        sink,
        scope,
        tx,
        max_in_flight: (threads.max(1) * 4).min(tiles_at_once(o)),
        levels: Vec::new(),
        maxzoom: o.maxzoom,
        extend_zooms_max: o.extend_zooms_max,
        limits: vec![(o.maxzoom, o.extend_zooms_max)],
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
        running_big: 0,
        big_memory: o.big_memory as u64,
        shard: o.shard,
        tiling: super::ShardTiling::default(),
    };

    let root = root.filter(|r| o.shard.is_none_or(|s| s.keeps(r.z, r.x, r.y)));
    if let Some(root) = root {
        d.levels.push(Level::new(
            o.iz,
            o.iz,
            o,
            Spool::new(&o.tmpdir, &ctx.rest),
            &ctx.rest,
        ));
        d.add_input(0, root);
        d.level_inputs_complete(0);
    }

    loop {
        d.pump();
        if d.in_flight == 0 {
            break;
        }
        let done = match recv_soon(&rx) {
            Some(e) => e,
            None => break,
        };
        if let Done::Made { .. } | Done::Failed(_) = done {
            d.in_flight -= 1;
        }
        if d.error.is_some() {
            for l in &mut d.levels {
                l.queue.clear();
            }
            continue;
        }
        match done {
            Done::Read {
                level,
                idx,
                children,
                complexity,
            } => d.on_read(level, idx, children, complexity),
            Done::Made {
                level,
                idx,
                pass,
                big,
                result,
                made,
                record,
                tile,
            } => {
                d.running_big -= big;
                d.on_made(level, idx, pass, *result, made, record, tile)
            }
            Done::Failed(e) => d.error = Some(e),
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
    if d.shard.is_some() {
        let mut t = d.tiling;
        t.tiles = stats.tiles;
        t.maxzoom = d.maxzoom;
        t.largest_written = d.largest_written;
        t.incomplete = stats.incomplete;
        t.incomplete_maxzoom = if stats.incomplete { stats.maxzoom } else { 0 };
        stats.shard = Some(t);
    }
    Ok(stats)
}

impl<'d, 's> Driver<'d, 's> {
    /// Hands ready tasks to the pool while threads are free, from the
    /// shallowest levels first.
    fn pump(&mut self) {
        let mut deferred: Vec<(usize, u32)> = Vec::new();
        while self.in_flight < self.max_in_flight {
            let Some(k) = (0..self.levels.len()).find(|&k| !self.levels[k].queue.is_empty()) else {
                break;
            };
            let idx = self.levels[k].queue.pop_front().unwrap();
            let ws = self.levels[k].working_set(idx as usize, self.ctx.store, self.ctx.o);
            let big = if ws >= BIG_TASK.min(self.big_memory / 64) {
                ws
            } else {
                0
            };
            if big > 0 && self.running_big > 0 && self.running_big + big > self.big_memory {
                deferred.push((k, idx));
                continue;
            }
            self.running_big += big;
            self.in_flight += 1;
            let ctx = self.ctx;
            let tx = self.tx.clone();
            let t = self.task(k, idx as usize, big);
            self.scope.spawn(move |_| {
                // a task that panics fails the run; its message is printed
                // already
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_task(ctx, t, &tx)
                }))
                .unwrap_or_else(|_| Err(Error::new(EXIT_IMPOSSIBLE, String::new())));
                if done.is_err() {
                    ctx.failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let _ = tx.send(done.unwrap_or_else(Done::Failed));
            });
        }
        // back at the front of their levels, in their order
        for (k, idx) in deferred.into_iter().rev() {
            self.levels[k].queue.push_front(idx);
        }
    }

    /// Adds a tile to level `k`.
    fn add_input(&mut self, k: usize, input: TileInput) {
        let level = &mut self.levels[k];
        debug_assert_eq!(input.z, level.z);
        let idx = level.slots.len();
        let (parked, flags) = match input.park(level.spool.as_ref().expect(LIVE)) {
            Some(p) => (p, 0),
            None => {
                let p = Parked::stand_in(&input, level.whole.len());
                level.whole.push(Arc::new(input));
                (p, WHOLE)
            }
        };
        level.slots.push(Slot {
            input: parked,
            made_at: 0,
            made_len: 0,
            made_shard: 0,
            record: 0,
            flags,
        });
        level.unread += 1;
        level.missing += 1;
        if level.started {
            self.spawn_tile(k, idx);
        } else {
            debug_assert_eq!(level.waiting, idx);
            level.waiting += 1;
            // a level beyond the maximum zoom may never be made, and so
            // waits in its spool
            if level.waiting.is_multiple_of(BLOCK)
                && level.z > self.maxzoom
                && k > 0
                && self.levels[k - 1].z + 1 == self.levels[k].z
            {
                let level = &mut self.levels[k];
                if let Err(e) = level.slots.put_away(level.spool.as_ref().expect(LIVE)) {
                    self.error = Some(e);
                }
            }
        }
    }

    /// Starts level `k` if it may start: its next zoom is known, its tiles
    /// may be made, and it is not too far ahead of the finished levels.
    fn try_start(&mut self, k: usize) {
        let level = &self.levels[k];
        if level.started
            || self.stopped
            || k >= self.finalized + LOOKAHEAD
            || level.nextzoom.is_none()
            || level.z > self.maxzoom
        {
            return;
        }
        let level = &mut self.levels[k];
        level.started = true;
        if let Err(e) = level.slots.bring_back(level.spool.as_ref().expect(LIVE)) {
            self.error = Some(e);
            return;
        }
        for idx in 0..std::mem::take(&mut self.levels[k].waiting) {
            self.spawn_tile(k, idx);
        }
    }

    /// Queues the reading and making of tile `idx` of level `k` in the
    /// level's current pass, or skips it.
    fn spawn_tile(&mut self, k: usize, idx: usize) {
        let o = self.ctx.o;
        let level = &mut self.levels[k];
        if level.slots[idx].input.skip_unless_dropping
            && !dropping_features(&level.th, o.drop_by_attribute_descending)
        {
            let th = level.th;
            level.fold(idx, skipped(th), o.drop_by_attribute_descending);
            level.missing -= 1;
            return;
        }
        level.outstanding += 1;
        // a pass queues its tiles in the order of their slots
        debug_assert!(level.queue.back().is_none_or(|&b| (b as usize) < idx));
        let limits = (self.limits.len() - 1) as u16;
        if level.spawn_limits.last().is_none_or(|l| l.1 != limits) {
            level.spawn_limits.push((idx as u32, limits));
        }
        level.queue.push_back(idx as u32);
    }

    /// The task of a queued tile. Nothing it is made from changes while the
    /// tile waits, apart from the maximum zoom, which `spawn_limits` keeps
    /// as it was: a level's thresholds and its tiles' reads change only once
    /// its pass is done.
    fn task(&self, k: usize, idx: usize, big: u64) -> Task {
        let o = self.ctx.o;
        let level = &self.levels[k];
        let pass = level.pass;
        let sl = &level.spawn_limits;
        let from = sl.partition_point(|&(i, _)| i as usize <= idx);
        let (maxzoom, extend_zooms_max) = self.limits[sl[from - 1].1 as usize];
        let slot = &level.slots[idx];
        let loop_z = if pass == 0 { level.loop_z } else { level.z };
        let simplification = if loop_z == maxzoom && o.maxzoom_simplification > 0.0 {
            o.maxzoom_simplification
        } else {
            o.simplification
        };
        Task {
            level: k,
            idx,
            pass,
            big,
            spool: level.spool.clone().expect(LIVE),
            child_spool: level.child_spool.clone().expect(LIVE),
            input: level.input(idx),
            rp: ReadParams {
                nextzoom: level.nextzoom.unwrap(),
                maxzoom,
                extend_zooms_max,
                children: slot.flags & READ == 0,
            },
            // a whole input keeps its own; the complexity is the slot's
            complexity: if slot.flags & WHOLE != 0 {
                slot.input.complexity()
            } else {
                None
            },
            mp: MakeParams {
                maxzoom,
                simplification,
                th: level.th,
            },
            make: self.shard.is_none_or(|r| r.makes(level.z)),
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
            // tippecanoe's temporary files with data: those of each parent,
            // numbered in x and y order, and of each child's position
            let shard = |parent: usize, x: u32, y: u32| {
                let j = (x.wrapping_shl(mzi) | (y & ((1u32 << mzi) - 1))) & cs.wrapping_sub(1);
                (parent % threads) * cs as usize + j as usize
            };
            let mut shards = std::collections::BTreeSet::new();
            if self.levels[k].z == prev.z + 1 {
                // from the parents, which know which children they wrote,
                // with x and y in 24 bits each and the children below
                debug_assert!(prev.z <= MAX_ZOOM);
                let mut parents: Vec<u64> = prev
                    .slots
                    .iter()
                    .filter(|s| (s.flags >> CHILDREN) & 0xf != 0)
                    .map(|s| {
                        ((s.input.x as u64) << 36)
                            | ((s.input.y as u64) << 8)
                            | ((s.flags >> CHILDREN) & 0xf) as u64
                    })
                    .collect();
                parents.sort_unstable();
                // the count matters only up to tippecanoe's other limits
                let enough = o.cpus.min(o.temp_files / 4);
                for (rank, p) in parents.iter().enumerate() {
                    let (x, y) = ((p >> 36) as u32, ((p >> 8) & 0xff_ffff) as u32);
                    for c in 0..4u32 {
                        if p & (1 << c) != 0 {
                            shards.insert(shard(rank, 2 * x + (c & 1), 2 * y + (c >> 1)));
                        }
                    }
                    if shards.len() >= enough {
                        break;
                    }
                }
            } else {
                // each tile's parent is the tile above it in the level before
                let dz = (self.levels[k].z - prev.z) as u32;
                let slots = &self.levels[k].slots;
                let mut parents: Vec<(u32, u32)> = slots
                    .iter()
                    .map(|s| (s.input.x >> dz, s.input.y >> dz))
                    .collect();
                parents.sort_unstable();
                parents.dedup();
                for s in slots.iter() {
                    let (x, y) = (s.input.x, s.input.y);
                    let rank = parents.binary_search(&(x >> dz, y >> dz)).unwrap();
                    shards.insert(shard(rank, x, y));
                }
            }
            shards.len()
        };

        let level = &mut self.levels[k];
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

        self.try_start(k);
        self.check_children_complete(k);
        self.check_pass(k);
    }

    /// The next level's inputs are complete once this level is all read.
    fn check_children_complete(&mut self, k: usize) {
        let level = &self.levels[k];
        if self.variable_depth || !level.inputs_complete || level.unread > 0 {
            return;
        }
        if k + 1 < self.levels.len() && !self.levels[k + 1].inputs_complete {
            self.level_inputs_complete(k + 1);
        }
    }

    fn on_read(&mut self, k: usize, idx: usize, children: Vec<TileInput>, complexity: i64) {
        let o = self.ctx.o;
        let level = &mut self.levels[k];
        let slot = &mut level.slots[idx];
        slot.flags |= READ;
        slot.input.set_complexity(complexity);
        level.unread -= 1;
        if self.variable_depth {
            level.held.insert(idx as u32, children);
        } else if !children.is_empty() {
            if k + 1 == self.levels.len() {
                let z = children[0].z;
                let spool = self.levels[k].child_spool.clone().expect(LIVE);
                let mut next = Level::new(z, self.levels[k].z + 1, o, spool, &self.ctx.rest);
                if z + 1 >= o.minzoom {
                    next.nextzoom = Some(z + 1);
                }
                self.levels.push(next);
                self.try_start(k + 1);
            }
            let level = &mut self.levels[k];
            if children[0].z == level.z + 1 {
                for c in &children {
                    level.wrote_child(idx, c.x, c.y);
                }
            }
            for child in children {
                if self
                    .shard
                    .is_none_or(|s| s.keeps(child.z, child.x, child.y))
                {
                    self.add_input(k + 1, child);
                }
            }
        }
        self.check_children_complete(k);
    }

    #[allow(clippy::too_many_arguments)]
    fn on_made(
        &mut self,
        k: usize,
        idx: usize,
        pass: usize,
        result: MakeResult,
        made: Option<Place>,
        record: u8,
        tile: bool,
    ) {
        let level = &mut self.levels[k];
        debug_assert_eq!(pass, level.pass);
        level.outstanding -= 1;
        level.missing -= 1;
        let slot = &mut level.slots[idx];
        let made = made.unwrap_or_default();
        slot.made_at = made.at;
        slot.made_len = made.len as u32;
        slot.made_shard = made.shard_byte();
        slot.record = record;
        slot.flags &= !TILE;
        if tile {
            slot.flags |= TILE;
        }
        if slot.flags & MUST_GO_ON_KNOWN == 0 {
            slot.flags |= MUST_GO_ON_KNOWN;
            if result.children_must_go_on {
                slot.flags |= MUST_GO_ON;
            }
        }
        level.fold(idx, result, self.ctx.o.drop_by_attribute_descending);
        self.check_pass(k);
    }

    fn check_pass(&mut self, k: usize) {
        let level = &self.levels[k];
        if level.done
            || !level.started
            || !level.inputs_complete
            || level.waiting > 0
            || level.missing > 0
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
        let fresh = Totals::new(level.z, level.th);
        let totals = std::mem::replace(&mut level.totals, fresh);
        let z = totals.z;
        let th = level.th;
        // With --as-needed-per-tile, each tile keeps the thresholds it raised
        // for itself, and the level is not made again with the highest.
        let next = if o.as_needed_per_tile {
            th
        } else {
            totals.next
        };
        let again = next.gamma > th.gamma
            || next.mingap > th.mingap
            || next.minextent > th.minextent
            || next.mindrop_sequence > th.mindrop_sequence
            || next.minattribute != th.minattribute;
        level.th = next;
        level.totals.next = next;
        let extend_zooms = totals.extend_zooms;
        let mut strategy = totals.strategy;
        if level.z == maxzoom
            && let Some(t) = totals.top
        {
            level.top = Some(top_of(level.top, t));
        }
        let error = totals.error.map(|(_, m)| m);

        if let Some(message) = error {
            self.error = Some(Error::new(EXIT_INCOMPLETE, message));
            return;
        }

        if let Some(r) = self.shard
            && level.z >= r.zoom
        {
            let extends = extend_zooms
                && (o.a(A_EXTEND_ZOOMS) || self.extend_zooms_max > 0)
                && z == self.maxzoom
                && self.maxzoom < MAX_ZOOM;
            if (again || extends) && self.tiling.again.last() != Some(&level.z) {
                self.tiling.again.push(level.z);
            }
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
            self.limits.push((self.maxzoom, self.extend_zooms_max));
        }

        let level = &mut self.levels[k];
        strategy.tile_size = level.zoom_tile_size;
        strategy.feature_count = level.zoom_feature_count;
        level.strategy = strategy;
        level.strategy_z = z;

        let level = &mut self.levels[k];
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
                level.slots.len(),
                c[0],
                c[1],
                c[2] >> 20,
                c[3] as f64 / 1e9
            );
        }

        if again {
            level.pass += 1;
            level.spawn_limits.clear();
            let th = level.th;
            let filters = o.prefilter.is_some() || o.postfilter.is_some();
            let reusable = !self.variable_depth && !filters;
            let mut record = [0u8; 255];
            for idx in 0..self.levels[k].slots.len() {
                let level = &mut self.levels[k];
                let slot = &mut level.slots[idx];
                let flags = slot.flags;
                slot.flags &= !MADE;
                level.missing += 1;
                let reuse = if reusable && flags & MADE != 0 && flags & REUSABLE != 0 {
                    let spool = level.spool.as_ref().expect(LIVE);
                    let slot = &level.slots[idx];
                    let record = &mut record[..slot.record as usize];
                    if let Err(e) = slot.made().read(spool, record) {
                        self.error = Some(e);
                        return;
                    }
                    let r = Reuse::decode(record);
                    Some((r.len, r))
                } else {
                    None
                };
                let reuse = reuse.filter(|(_, r)| {
                    r.th.gamma == th.gamma
                        && r.th.minextent == th.minextent
                        && r.th.minattribute == th.minattribute
                        && r.gap >= th.mingap
                        && r.drop_sequence > th.mindrop_sequence
                });
                match reuse {
                    // a higher threshold leaves the tile as it is
                    Some((len, r)) => {
                        let result = MakeResult {
                            len,
                            strategy: r.strategy,
                            reusable: Some(th),
                            reuse_gap: r.gap,
                            reuse_drop_sequence: r.drop_sequence,
                            ..skipped(th)
                        };
                        let result = MakeResult {
                            skipped: false,
                            ..result
                        };
                        level.fold(idx, result, o.drop_by_attribute_descending);
                        level.missing -= 1;
                    }
                    None => self.spawn_tile(k, idx),
                }
            }
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
            if !self.levels[j].slots.is_empty() && self.levels[j].inputs_complete {
                self.try_start(j);
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
        let order = self.levels[k].order();
        let level = &mut self.levels[k];

        let mut next: Vec<TileInput> = Vec::new();
        let z = level.z;
        for &i in &order {
            let i = i as usize;
            let flags = level.slots[i].flags;
            debug_assert!(flags & MADE != 0);
            if flags & SKIPPED != 0 {
                continue;
            }
            let must_go_on = flags & MUST_GO_ON != 0;
            for mut child in level.held.remove(&(i as u32)).unwrap_or_default() {
                child.skip_unless_dropping = flags & TRUNCATED != 0 && child.z == z + 1;
                if must_go_on {
                    // large enough not to try to stop early
                    child.complexity = Some(1i64 << 32);
                }
                if child.z == z + 1 {
                    level.wrote_child(i, child.x, child.y);
                }
                next.push(child);
            }
        }
        if next.is_empty() {
            return;
        }

        debug_assert_eq!(k + 1, self.levels.len());
        let z = next[0].z;
        let spool = self.levels[k].child_spool.clone().expect(LIVE);
        let mut nl = Level::new(z, self.levels[k].z + 1, o, spool, &self.ctx.rest);
        if z + 1 >= o.minzoom {
            nl.nextzoom = Some(z + 1);
        }
        self.levels.push(nl);
        for child in next {
            self.add_input(k + 1, child);
        }
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
                self.try_start(j);
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
        let t0 = std::time::Instant::now();
        let r = self.finalize1(k);
        if self.timing {
            eprintln!(
                "{:.3}s: finalize z{} took {:.3}s",
                self.started.elapsed().as_secs_f64(),
                self.levels[k].z,
                t0.elapsed().as_secs_f64()
            );
        }
        r
    }

    fn finalize1(&mut self, k: usize) -> crate::Result<()> {
        let order = self.levels[k].order();
        let level = &mut self.levels[k];

        // tippecanoe's tile with the most vertices at the maximum zoom, and
        // of those the first in x and y order; with none above zero, the
        // first of those with zero and the one found before
        self.most = 0;
        if let Some((len, x, y)) = level.top {
            let a: u64 = ((x as u64) << 32) | y as u64;
            let b: u64 = ((self.stats.midx as u64) << 32) | self.stats.midy as u64;
            if len > self.most || (len == self.most && a < b) {
                self.stats.midx = x;
                self.stats.midy = y;
                self.most = len;
            }
        }

        if self.shard.is_some() {
            let t = &mut self.tiling;
            t.strategies
                .push((level.z, level.strategy_z, level.strategy));
            if let Some((len, x, y)) = level.top {
                t.tops.push((level.z, len, x, y));
            }
            if !level.tilestats.is_empty() {
                t.layermaps
                    .push((level.z, merge_tilestats(level.tilestats.clone())));
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

        let spool = level.spool.clone().expect(LIVE);
        for &i in &order {
            let slot = &level.slots[i as usize];
            if slot.flags & TILE != 0 {
                let stored = slot.made().stored(&spool, slot.record as u64);
                let bytes = stored.load_sequential()?;
                self.sink
                    .write_tile(level.z as u8, slot.input.x, slot.input.y, &bytes)?;
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
        level.slots = Slots::default();
        level.spawn_limits = Vec::new();
        level.whole = Vec::new();
        level.held = HashMap::new();
        if let Some(spool) = level.spool.take() {
            for l in &mut self.levels {
                if l.child_spool
                    .as_ref()
                    .is_some_and(|c| Arc::ptr_eq(c, &spool))
                {
                    l.child_spool = None;
                }
            }
        }
        Ok(())
    }
}

/// How long the coordinator waits for the next result before it sleeps.
#[cfg(not(target_family = "wasm"))]
const COORDINATOR_SPIN: std::time::Duration = std::time::Duration::from_micros(200);

/// The next result, waited for awake for a moment before sleeping. Results
/// come every few microseconds while small tiles are being made, and each
/// time the coordinator sleeps, waking it costs more than that (much more
/// in a virtual machine, where it wakes a virtual processor), while the
/// pool runs out of tasks.
#[cfg(not(target_family = "wasm"))]
fn recv_soon(rx: &Receiver<Done>) -> Option<Done> {
    let started = std::time::Instant::now();
    loop {
        match rx.try_recv() {
            Ok(done) => return Some(done),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) if started.elapsed() > COORDINATOR_SPIN => {
                return rx.recv().ok();
            }
            Err(TryRecvError::Empty) => std::hint::spin_loop(),
        }
    }
}

/// The next result. Without threads the tasks wait in this thread's queue,
/// so it runs them until one is done.
#[cfg(target_family = "wasm")]
fn recv_soon(rx: &Receiver<Done>) -> Option<Done> {
    loop {
        match rx.try_recv() {
            Ok(done) => return Some(done),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {
                if rayon::yield_now() != Some(rayon::Yield::Executed) {
                    return rx.recv().ok();
                }
            }
        }
    }
}

/// Reads a tile, reporting the first read at once, and makes it. Its children
/// and the tile itself go into the spools while they wait.
fn run_task(ctx: &Ctx<'_>, t: Task, tx: &Sender<Done>) -> crate::Result<Done> {
    let mut rt = read_tile(ctx, &t.input, &t.rp, t.complexity)?;
    if t.rp.children {
        let mut children = std::mem::take(&mut rt.children);
        store_inputs(&mut children, &t.child_spool)?;
        let _ = tx.send(Done::Read {
            level: t.level,
            idx: t.idx,
            children,
            complexity: rt.complexity,
        });
    }
    if !t.make {
        return Ok(Done::Made {
            level: t.level,
            idx: t.idx,
            pass: t.pass,
            big: t.big,
            result: Box::new(skipped(t.mp.th)),
            made: None,
            record: 0,
            tile: false,
        });
    }
    let mut result = make_tile(ctx, &rt, &t.mp);
    drop(rt);
    let tile = result.tile.take();
    // what another pass needs to know of the tile waits with it
    let record = result.reusable.map(|th| {
        Reuse {
            len: result.len,
            th,
            gap: result.reuse_gap,
            drop_sequence: result.reuse_drop_sequence,
            strategy: result.strategy,
        }
        .encode()
    });
    let made = if tile.is_some() || record.is_some() {
        let parts = [
            record.as_deref().unwrap_or(&[]),
            tile.as_deref().unwrap_or(&[]),
        ];
        if parts[0].len() + parts[1].len() > u32::MAX as usize {
            return Err(Error::new(
                EXIT_IMPOSSIBLE,
                format!(
                    "tile {}/{}/{} is too large",
                    t.input.z, t.input.x, t.input.y
                ),
            ));
        }
        let stored = t.spool.store_parts(&parts)?;
        Some(stored.place_in(&t.spool).expect("stored in the spool"))
    } else {
        None
    };
    Ok(Done::Made {
        level: t.level,
        idx: t.idx,
        pass: t.pass,
        big: t.big,
        result: Box::new(result),
        made,
        record: record.map_or(0, |r| r.len() as u8),
        tile: tile.is_some(),
    })
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
        reuse_gap: u64::MAX,
        reuse_drop_sequence: u64::MAX,
        truncated: false,
        children_must_go_on: false,
        skipped: true,
    }
}

/// Merges the attribute statistics of several tiles. Every entry came from
/// tiling, so each stays marked as one to keep, as tippecanoe's tiling
/// segments are until the final merge.
pub(crate) fn merge_tilestats(
    maps: Vec<BTreeMap<String, LayermapEntry>>,
) -> BTreeMap<String, LayermapEntry> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_stay_small() {
        // a level keeps one per tile until it is written: 18 million at z15
        // for the largest Canadian census areas
        assert!(std::mem::size_of::<Slot>() <= 56);
    }

    #[test]
    fn reuse_records_come_back() {
        let th = |minattribute| Thresholds {
            gamma: 1.5,
            mingap: 0,
            minextent: -7,
            mindrop_sequence: u64::MAX - 3,
            minattribute,
        };
        let strategy = Strategy {
            dropped_as_needed: 3,
            tile_size: 123_456,
            feature_count: 789,
            ..Strategy::default()
        };
        for (len, th, gap, drop_sequence, strategy) in [
            (0, th(f64::NEG_INFINITY), u64::MAX, u64::MAX, None),
            (-1, th(f64::INFINITY), 0, 1, Some(strategy)),
            (
                i64::MAX,
                th(-2.25),
                12_345,
                u64::MAX - 1,
                Some(Strategy::default()),
            ),
        ] {
            let r = Reuse {
                len,
                th,
                gap,
                drop_sequence,
                strategy,
            };
            let bytes = r.encode();
            assert!(bytes.len() <= u8::MAX as usize);
            let back = Reuse::decode(&bytes);
            assert_eq!(back.len, len);
            assert_eq!(back.th, th);
            assert_eq!(back.gap, gap);
            assert_eq!(back.drop_sequence, drop_sequence);
            assert_eq!(
                back.strategy.map(|s| format!("{:?}", s)),
                strategy.map(|s| format!("{:?}", s))
            );
        }
    }

    #[test]
    fn slots_in_blocks() {
        let mut slots = Slots::default();
        let slot = |i: u32| {
            let input = TileInput::root(0, i, i, 1, 0);
            Slot {
                input: Parked::stand_in(&input, i as usize),
                made_at: i as u64,
                made_len: 0,
                made_shard: 0,
                record: 0,
                flags: 0,
            }
        };
        for i in 0..(BLOCK as u32 * 2 + 5) {
            slots.push(slot(i));
        }
        assert_eq!(slots.len(), BLOCK * 2 + 5);
        assert_eq!(slots.blocks.len(), 3);
        slots[BLOCK + 1].made_at = 7;
        assert_eq!(slots[BLOCK + 1].made_at, 7);
        assert!(
            slots
                .iter()
                .enumerate()
                .all(|(i, s)| s.input.index() == i && (i == BLOCK + 1 || s.made_at == i as u64))
        );
    }
}
