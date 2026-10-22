//! Reading a tile's input: what tippecanoe's `next_feature` does for every
//! feature of a tile, including writing the features out for the next zoom.
//!
//! tippecanoe does this again for every retry of a tile and every pass of a
//! zoom, but the results are always the same, so here it happens once and the
//! candidates are kept.

use std::collections::{BTreeSet, HashMap};

use flate2::{Compress, Compression, FlushCompress};
use tippecanoe::clip::{clip_lines, clip_point, remove_noop, simple_clip_poly};
use tippecanoe::evaluator::evaluate;
use tippecanoe::geometry::VT_END;
use tippecanoe::geometry::{
    Draw, DrawVec, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON, point_within_tile,
    quick_check,
};
use tippecanoe::mvt::{MVT_LINESTRING, MVT_POINT, MVT_POLYGON, MVT_STRING, MVT_UINT, MvtValue};
use tippecanoe::options::*;
use tippecanoe::projection::decode_index;
use tippecanoe::serial::{
    FEATURE_ADDED_FOR_MULTIPLIER_DENSITY, FEATURE_DROPPED, FEATURE_KEPT, serialize_byte,
    serialize_int, serialize_long_long, serialize_uint, serialize_ulong_long,
};
use tippecanoe::text::bit_reverse;

use super::engine::Ctx;
use super::util::{Chunked, par_ranges, shift_right};
use crate::mem::{Spool, Stored};
use crate::store::Attr;
use std::sync::Arc;
use tippecanoe::serial::SerialVal;

/// `pow(2, 64)`.
const TWO_TO_THE_64: f64 = 18446744073709551616.0;

/// How many input features one parallel job reads.
const CHUNK: usize = 16384;

/// How many features of a tile input a run holds at most, so that reading
/// can be split at run boundaries.
const RUN: u32 = 4096;

/// No feature: the run is the first of its input.
const NO_FEATURE: u32 = u32::MAX;

/// A run of features in a tile input.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// Where the run starts in `TileInput::data`.
    start: u64,
    count: u32,
    /// The features' vertices.
    vertices: u32,
    /// The feature before the run, or `NO_FEATURE`.
    prev: u32,
}

/// The input of one tile: what tippecanoe writes into the temporary file for
/// it. Only the clipped geometry and the gap change on the way down;
/// everything else comes from the store.
///
/// The features are kept compact, since the inputs of whole zoom levels wait
/// in memory. For each feature: its store index as the difference from the
/// one before it in the run, its gap, and its geometry, each vertex as the
/// op and the differences of its coordinates, divided by `2^shift`, from the
/// vertex before. Numbers are varints, signed ones zigzag.
pub struct TileInput {
    pub z: i32,
    pub x: u32,
    pub y: u32,
    /// The first tile reads every feature of the store.
    pub root: bool,
    data: Stored,
    runs: Vec<Run>,
    count: usize,
    vertices: u64,
    /// The last feature, which the next part's first run follows.
    last: u32,
    shift: u32,
    /// With `-at`: the parent stopped early, so the tile is made only once
    /// its zoom level drops features.
    pub skip_unless_dropping: bool,
    /// With `-at`: tippecanoe's `estimated_complexity` of the tile, if it is
    /// known without counting the bytes of its temporary file.
    pub complexity: Option<i64>,
}

impl TileInput {
    /// The first tile, which reads every feature of the store.
    pub fn root(z: i32, x: u32, y: u32, features: usize, complexity: i64) -> TileInput {
        TileInput {
            z,
            x,
            y,
            root: true,
            data: Stored::Mem(Vec::new()),
            runs: Vec::new(),
            count: features,
            vertices: 0,
            last: NO_FEATURE,
            shift: 0,
            skip_unless_dropping: false,
            complexity: Some(complexity),
        }
    }

    fn empty(z: i32, x: u32, y: u32, shift: u32) -> TileInput {
        TileInput {
            z,
            x,
            y,
            root: false,
            data: Stored::Mem(Vec::new()),
            runs: Vec::new(),
            count: 0,
            vertices: 0,
            last: NO_FEATURE,
            shift,
            skip_unless_dropping: false,
            complexity: None,
        }
    }

    /// The number of features.
    pub fn len(&self) -> usize {
        self.count
    }

    /// About how much memory reading and making the tile takes.
    pub fn working_set(&self, store: &crate::store::FeatureStore) -> u64 {
        let vertices = if self.root {
            store.geometry.len() as u64
        } else {
            self.vertices
        };
        vertices * 32 + self.count as u64 * 96
    }

    /// Appends a part that one read chunk of the parent wrote.
    fn append(&mut self, part: ChildPart) {
        let Stored::Mem(data) = &mut self.data else {
            unreachable!("inputs are complete before they are spooled");
        };
        let off = data.len() as u64;
        let mut first = true;
        for mut r in part.runs {
            r.start += off;
            if first {
                r.prev = self.last;
                first = false;
            }
            self.runs.push(r);
        }
        if data.is_empty() {
            *data = part.data;
        } else {
            data.extend_from_slice(&part.data);
        }
        self.count += part.count;
        self.vertices += part.vertices;
        self.last = part.last;
    }

    /// The runs and the features, from the input's bytes.
    fn layout<'a>(&'a self, bytes: &'a [u8]) -> Layout<'a> {
        if let Stored::Mem(_) = self.data {
            return Layout {
                runs: self.runs.clone(),
                data: bytes,
                shift: self.shift,
            };
        }
        let mut pos = 0;
        let n = get_varint(bytes, &mut pos) as usize;
        let mut runs = Vec::with_capacity(n);
        for _ in 0..n {
            let start = get_varint(bytes, &mut pos);
            let count = get_varint(bytes, &mut pos) as u32;
            let vertices = get_varint(bytes, &mut pos) as u32;
            let prev = (get_varint(bytes, &mut pos) as u32).wrapping_sub(1);
            runs.push(Run {
                start,
                count,
                vertices,
                prev,
            });
        }
        Layout {
            runs,
            data: &bytes[pos..],
            shift: self.shift,
        }
    }

    /// The bytes it is stored as: the runs, then the features.
    fn encode(&self, out: &mut Vec<u8>) {
        put_varint(out, self.runs.len() as u64);
        for r in &self.runs {
            put_varint(out, r.start);
            put_varint(out, r.count as u64);
            put_varint(out, r.vertices as u64);
            put_varint(out, r.prev.wrapping_add(1) as u64);
        }
        if let Stored::Mem(d) = &self.data {
            out.extend_from_slice(d);
        }
    }
}

/// An input's runs and features, ready to read.
struct Layout<'a> {
    runs: Vec<Run>,
    data: &'a [u8],
    shift: u32,
}

impl Layout<'_> {
    /// Splits the runs into at most `parts` groups of about equal feature counts.
    fn groups(&self, count: usize, parts: usize) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::with_capacity(parts);
        let mut first = 0;
        let mut seen = 0usize;
        for (i, r) in self.runs.iter().enumerate() {
            seen += r.count as usize;
            if seen * parts >= count * (out.len() + 1) && out.len() + 1 < parts {
                out.push(first..i + 1);
                first = i + 1;
            }
        }
        if first < self.runs.len() || out.is_empty() {
            out.push(first..self.runs.len());
        }
        out
    }

    fn cursor(&self, runs: std::ops::Range<usize>) -> Cursor<'_> {
        let pos = self
            .runs
            .get(runs.start)
            .map(|r| r.start as usize)
            .unwrap_or(0);
        Cursor {
            layout: self,
            run: runs.start,
            end: runs.end,
            left: self.runs.get(runs.start).map(|r| r.count).unwrap_or(0),
            pos,
            base: 0,
        }
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_zigzag(out: &mut Vec<u8>, v: i64) {
    put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn get_varint(data: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = data[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

fn get_zigzag(data: &[u8], pos: &mut usize) -> i64 {
    let v = get_varint(data, pos);
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

/// Stores the inputs in `spool`, in one piece.
pub fn store_inputs(inputs: &mut [TileInput], spool: &Arc<Spool>) -> crate::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let mut all = Vec::new();
    let mut ends = Vec::with_capacity(inputs.len());
    for t in inputs.iter() {
        t.encode(&mut all);
        ends.push(all.len());
    }
    let Stored::Spooled { at, .. } = spool.store(&all)? else {
        unreachable!();
    };
    let mut from = 0;
    for (t, end) in inputs.iter_mut().zip(ends) {
        t.data = Stored::Spooled {
            spool: spool.clone(),
            at: at + from as u64,
            len: end - from,
        };
        t.runs = Vec::new();
        from = end;
    }
    Ok(())
}

/// Reads the features of some runs of a tile input in order.
struct Cursor<'a> {
    layout: &'a Layout<'a>,
    run: usize,
    end: usize,
    left: u32,
    pos: usize,
    base: u32,
}

impl Cursor<'_> {
    /// The feature before the first run.
    fn prev(&self) -> Option<u32> {
        match self.layout.runs.get(self.run).map(|r| r.prev) {
            Some(NO_FEATURE) | None => None,
            Some(p) => Some(p),
        }
    }

    /// The next feature: its store index and gap. Its geometry goes into
    /// `geometry`, and the bounds of its points into `bbox`.
    fn next(&mut self, geometry: &mut DrawVec, bbox: &mut [i64; 4]) -> Option<(u32, u64)> {
        while self.left == 0 {
            self.run += 1;
            if self.run >= self.end {
                return None;
            }
            let r = self.layout.runs[self.run];
            self.left = r.count;
            self.pos = r.start as usize;
            self.base = 0;
        }
        self.left -= 1;
        let data = self.layout.data;
        let shift = self.layout.shift;
        let fi = (self.base as i64 + get_zigzag(data, &mut self.pos)) as u32;
        self.base = fi;
        let gap = get_varint(data, &mut self.pos);
        let n = get_varint(data, &mut self.pos) as usize;
        *bbox = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        let (mut x, mut y) = (0i64, 0i64);
        for _ in 0..n {
            let op = data[self.pos] as i8;
            self.pos += 1;
            if op == VT_MOVETO || op == VT_LINETO {
                x += get_zigzag(data, &mut self.pos);
                y += get_zigzag(data, &mut self.pos);
                let (wx, wy) = (x << shift, y << shift);
                bbox[0] = bbox[0].min(wx);
                bbox[1] = bbox[1].min(wy);
                bbox[2] = bbox[2].max(wx);
                bbox[3] = bbox[3].max(wy);
                geometry.push(Draw::new(op, wx, wy));
            } else {
                geometry.push(Draw::new(op, 0, 0));
            }
        }
        Some((fi, gap))
    }
}

/// A feature that survived reading: a candidate for the tile.
#[derive(Clone, Copy, Debug)]
pub struct Cand {
    pub fi: u32,
    pub dropped: i32,
    pub gap: u64,
    pub geom_start: u32,
    pub geom_len: u32,
    pub edge_start: u32,
    pub edge_len: u32,
    /// `NO_LIST` for the feature's own attributes without nulls, or an index
    /// into `ReadTile::attr_lists` when the filter excluded some.
    pub attrs: u32,
    /// `NO_LIST`, or an index into `ReadTile::full` for the full keys and
    /// values that a feature from the prefilter arrives with.
    pub full: u32,
}

pub const NO_LIST: u32 = u32::MAX;

/// What reading a tile produced.
pub struct ReadTile {
    pub z: i32,
    pub x: u32,
    pub y: u32,
    pub cands: Chunked<Cand>,
    pub geom: Chunked<Draw>,
    pub edges: Chunked<Draw>,
    pub attr_lists: Chunked<Vec<Attr>>,
    pub full: Vec<(Vec<Arc<String>>, Vec<SerialVal>)>,
    pub original_features: i64,
    pub unclipped_features: i64,
    pub minzoom_feature_pending: bool,
    /// The inputs of the tiles at the next zoom written, in tile order.
    pub children: Vec<TileInput>,
    pub skip_unless_dropping: bool,
    /// With `-at`: tippecanoe's `estimated_complexity`, as far as it matters.
    pub complexity: i64,
}

impl ReadTile {
    pub fn cand_geom(&self, c: &Cand) -> &[Draw] {
        self.geom.slice(c.geom_start as usize, c.geom_len as usize)
    }

    pub fn cand_edges(&self, c: &Cand) -> &[Draw] {
        self.edges.slice(c.edge_start as usize, c.edge_len as usize)
    }
}

/// Zoom-level values that reading needs.
#[derive(Clone, Copy)]
pub struct ReadParams {
    pub nextzoom: i32,
    pub maxzoom: i32,
    pub extend_zooms_max: i64,
    /// Whether to write the features out for the next zoom, which happens
    /// only the first time a tile is read.
    pub children: bool,
}

/// The features one read chunk writes for one child tile.
struct ChildPart {
    x: u32,
    y: u32,
    data: Vec<u8>,
    runs: Vec<Run>,
    count: usize,
    vertices: u64,
    /// The last feature written, and the one the deltas count from.
    last: u32,
    base: u32,
}

struct ChunkOut {
    cands: Vec<Cand>,
    geom: Vec<Draw>,
    edges: Vec<Draw>,
    attr_lists: Vec<Vec<Attr>>,
    children: Vec<ChildPart>,
    original_features: i64,
    unclipped_features: i64,
    minzoom_feature_pending: bool,
}

/// Reads one tile. `complexity` is the `-at` estimate if an earlier read
/// already counted it.
pub fn read_tile(
    ctx: &Ctx<'_>,
    input: &TileInput,
    rp: &ReadParams,
    complexity: Option<i64>,
) -> crate::Result<ReadTile> {
    let bytes = input.data.load()?;
    let layout = input.layout(&bytes);
    let complexity = match input.complexity.or(complexity) {
        Some(c) => c,
        None if ctx.o.a(A_VARIABLE_DEPTH_PYRAMID) => stream_complexity(ctx, input, &layout),
        None => 0,
    };
    let n = input.len();
    let parts = (n / CHUNK).clamp(1, ctx.helpers.width());
    let chunks: Vec<ChunkOut> = if input.root {
        par_ranges(&ctx.helpers, n, CHUNK, |r| {
            read_chunk(ctx, input, Source::Root(r), rp)
        })
    } else {
        let groups = layout.groups(n, parts);
        par_ranges(&ctx.helpers, groups.len(), 1, |r| {
            debug_assert_eq!(r.len(), 1);
            read_chunk(
                ctx,
                input,
                Source::Runs(&layout, groups[r.start].clone()),
                rp,
            )
        })
    };

    let mut out = ReadTile {
        z: input.z,
        x: input.x,
        y: input.y,
        cands: Chunked::default(),
        geom: Chunked::default(),
        edges: Chunked::default(),
        attr_lists: Chunked::default(),
        full: Vec::new(),
        original_features: 0,
        unclipped_features: 0,
        minzoom_feature_pending: false,
        children: Vec::new(),
        skip_unless_dropping: input.skip_unless_dropping,
        complexity,
    };

    let shift = child_shift(ctx, rp.nextzoom);
    if chunks.len() == 1 {
        let c = chunks.into_iter().next().unwrap();
        out.cands.append(c.cands);
        out.geom.append(c.geom);
        out.edges.append(c.edges);
        out.attr_lists.append(c.attr_lists);
        out.original_features = c.original_features;
        out.unclipped_features = c.unclipped_features;
        out.minzoom_feature_pending = c.minzoom_feature_pending;
        out.children = c
            .children
            .into_iter()
            .map(|p| {
                let mut t = TileInput::empty(rp.nextzoom, p.x, p.y, shift);
                t.append(p);
                t
            })
            .collect();
    } else {
        let mut children: Vec<TileInput> = Vec::new();
        for mut c in chunks {
            let goff = out.geom.len() as u32;
            let eoff = out.edges.len() as u32;
            let aoff = out.attr_lists.len() as u32;
            for cand in c.cands.iter_mut() {
                cand.geom_start += goff;
                cand.edge_start += eoff;
                if cand.attrs != NO_LIST {
                    cand.attrs += aoff;
                }
            }
            out.cands.append(c.cands);
            out.geom.append(c.geom);
            out.edges.append(c.edges);
            out.attr_lists.append(c.attr_lists);
            out.original_features += c.original_features;
            out.unclipped_features += c.unclipped_features;
            out.minzoom_feature_pending |= c.minzoom_feature_pending;

            for p in c.children {
                let pos = children.binary_search_by(|t| (t.x, t.y).cmp(&(p.x, p.y)));
                let i = match pos {
                    Ok(i) => i,
                    Err(i) => {
                        children.insert(i, TileInput::empty(rp.nextzoom, p.x, p.y, shift));
                        i
                    }
                };
                children[i].append(p);
            }
        }
        out.children = children;
    }

    assign_dropped(ctx, &mut out);
    Ok(out)
}

/// The coordinates of a child tile at `nextzoom` are whole multiples of the
/// geometry scale when its origin is; then they are kept divided by it.
fn child_shift(ctx: &Ctx<'_>, nextzoom: i32) -> u32 {
    let gs = ctx.o.geometry_scale;
    if gs > 0 && 32 - nextzoom >= gs {
        gs as u32
    } else {
        0
    }
}

/// tippecanoe's `estimated_complexity` of a tile that is not the first: the
/// bytes its parent wrote for it into the temporary file before the end
/// marker, the features compressed. Counting stops once the size is too big
/// for the tile to try to stop early, since then the exact size does not matter.
fn stream_complexity(ctx: &Ctx<'_>, input: &TileInput, layout: &Layout<'_>) -> i64 {
    let o = ctx.o;
    let store = ctx.store;
    let gs = o.geometry_scale;

    let mut header = Vec::new();
    serialize_int(&mut header, input.z);
    serialize_uint(&mut header, input.x);
    serialize_uint(&mut header, input.y);
    let base = 8 + header.len() as i64;
    let limit = (0.9 * o.max_tile_size as f64) as i64;
    let enough = |c: i64| (c as f64).mul_add(0.6693, -3.36e+04) as i64 >= limit;

    let (ox, oy) = if input.z != 0 {
        (
            (input.x << (32 - input.z)) as i64,
            (input.y << (32 - input.z)) as i64,
        )
    } else {
        (0, 0)
    };

    let mut zs = Compress::new(Compression::default(), true);
    let mut out = vec![0u8; 1 << 17];
    let mut pending: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut s: Vec<u8> = Vec::new();
    let mut deflate = |zs: &mut Compress, data: &[u8]| {
        let mut used = 0;
        while used < data.len() {
            let before = zs.total_in();
            zs.compress(&data[used..], &mut out, FlushCompress::None)
                .expect("deflate cannot fail with these settings");
            used += (zs.total_in() - before) as usize;
        }
    };

    let mut cursor = layout.cursor(0..layout.runs.len());
    let mut geom = DrawVec::new();
    let mut bbox = [0i64; 4];
    loop {
        geom.clear();
        let Some((fi, gap)) = cursor.next(&mut geom, &mut bbox) else {
            break;
        };
        let f = &store.features[fi as usize];
        let segment = f.segment as usize;

        s.clear();
        serialize_byte(&mut s, f.t);
        let mut layer: i64 = store.segment_layers[segment][f.layer as usize].wrapping_shl(7);
        layer |= ((f.label_point != 0) as i64) << 6;
        layer |= ((f.index() != 0) as i64) << 4;
        layer |= ((f.extent != 0) as i64) << 3;
        layer |= (f.has_id as i64) << 2;
        layer |= ((f.tippecanoe_minzoom != -1) as i64) << 1;
        layer |= (f.tippecanoe_maxzoom != -1) as i64;
        serialize_long_long(&mut s, layer);
        serialize_long_long(&mut s, f.seq as i64);
        if f.tippecanoe_minzoom != -1 {
            serialize_int(&mut s, f.tippecanoe_minzoom as i32);
        }
        if f.tippecanoe_maxzoom != -1 {
            serialize_int(&mut s, f.tippecanoe_maxzoom as i32);
        }
        if f.has_id {
            serialize_ulong_long(&mut s, f.id);
        }
        serialize_int(&mut s, f.segment as i32);

        let mut wx = shift_right(store.initial_x[segment] as i64, gs);
        let mut wy = shift_right(store.initial_y[segment] as i64, gs);
        for d in &geom {
            serialize_byte(&mut s, d.op);
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                // the parent wrote whole multiples of the geometry scale
                let x = (d.x + ox) >> gs;
                let y = (d.y + oy) >> gs;
                serialize_long_long(&mut s, x - wx);
                serialize_long_long(&mut s, y - wy);
                wx = x;
                wy = y;
            }
        }
        serialize_byte(&mut s, VT_END);

        if f.index() != 0 {
            serialize_ulong_long(&mut s, f.index());
            serialize_ulong_long(&mut s, gap);
        }
        if f.label_point != 0 {
            serialize_ulong_long(&mut s, f.label_point);
        }
        if f.extent != 0 {
            serialize_long_long(&mut s, f.extent);
        }
        let pool = &store.pool_offsets[f.attr_start as usize..][..f.attr_len as usize];
        serialize_long_long(&mut s, pool.len() as i64);
        for p in pool {
            serialize_long_long(&mut s, p.key);
            serialize_long_long(&mut s, p.value);
        }
        serialize_byte(&mut s, f.feature_minzoom);

        serialize_long_long(&mut pending, s.len() as i64);
        pending.extend_from_slice(&s);
        if pending.len() >= 1 << 16 {
            deflate(&mut zs, &pending);
            pending.clear();
            let c = base + zs.total_out() as i64;
            if enough(c) {
                return c;
            }
        }
    }
    deflate(&mut zs, &pending);
    base + zs.total_out() as i64
}

/// The multiplier and drop-rate decisions of `next_feature`, which depend on
/// the features before them, so they are made in order after the parallel part.
fn assign_dropped(ctx: &Ctx<'_>, rt: &mut ReadTile) {
    let o = ctx.o;
    let z = rt.z;
    let retain_points_multiplier = o.retain_points_multiplier;
    let extra_multiplier_zooms = (retain_points_multiplier as f64).ln() / o.droprate.ln();
    let pmdt = o.preserve_multiplier_density_threshold;

    // multiplier_state.count, by layer; every layer's count starts out absent
    let mut count: Vec<i32> = vec![-1; ctx.store.layers.len()];
    let mut prev_not_dropped_index: u64 = 0;

    for c in rt.cands.iter_mut() {
        let f = &ctx.store.features[c.fi as usize];
        let mut dropped;
        if f.tippecanoe_minzoom == -1 {
            dropped = FEATURE_DROPPED;

            let layer = f.layer as usize;
            if count[layer] < 0 {
                count[layer] = 0;
                dropped = FEATURE_KEPT; // the first feature in each tile is always kept
            }

            let feature_minzoom =
                f.feature_minzoom as f64 - (bit_reverse(f.index() >> 2) as f64 / TWO_TO_THE_64);

            if z as f64 >= feature_minzoom || dropped == FEATURE_KEPT {
                count[layer] = 0;
                dropped = FEATURE_KEPT;
            } else if z as f64 + extra_multiplier_zooms >= feature_minzoom
                && count[layer] + 1 < retain_points_multiplier
            {
                count[layer] += 1;
                dropped = count[layer];
            } else if pmdt > 0
                && f.index().wrapping_sub(prev_not_dropped_index)
                    > ((1i64 << (32 - z)) as u64 / pmdt)
                        .wrapping_mul((1i64 << (32 - z)) as u64 / pmdt)
            {
                dropped = FEATURE_ADDED_FOR_MULTIPLIER_DENSITY;
            } else {
                dropped = FEATURE_DROPPED;
            }
        } else {
            dropped = FEATURE_KEPT;
        }

        if dropped != FEATURE_DROPPED {
            prev_not_dropped_index = f.index();
        }

        c.dropped = dropped;
    }
}

/// The features a read chunk reads: a range of the store's for the first
/// tile, otherwise some runs of the input.
enum Source<'a> {
    Root(std::ops::Range<usize>),
    Runs(&'a Layout<'a>, std::ops::Range<usize>),
}

fn read_chunk(ctx: &Ctx<'_>, input: &TileInput, source: Source<'_>, rp: &ReadParams) -> ChunkOut {
    let o = ctx.o;
    let store = ctx.store;
    let z = input.z;
    let tx = input.x;
    let ty = input.y;
    let buffer = o.buffer;

    let mut out = ChunkOut {
        cands: Vec::new(),
        geom: Vec::new(),
        edges: Vec::new(),
        attr_lists: Vec::new(),
        children: Vec::new(),
        original_features: 0,
        unclipped_features: 0,
        minzoom_feature_pending: false,
    };

    // room for every feature and vertex, so that nothing grows by copying
    let (features, vertices) = match &source {
        Source::Root(r) => (
            r.len(),
            store.features[r.clone()]
                .iter()
                .map(|f| f.geom_len as usize)
                .sum(),
        ),
        Source::Runs(layout, r) => layout.runs[r.clone()].iter().fold((0, 0), |(f, v), run| {
            (f + run.count as usize, v + run.vertices as usize)
        }),
    };
    out.cands.reserve(features);
    out.geom.reserve(vertices);

    let (mut root, mut cursor) = match source {
        Source::Root(r) => (Some(r), None),
        Source::Runs(layout, r) => (None, Some(layout.cursor(r))),
    };

    // next_feature_state.previndex: the index of the feature read before
    let prev = match (&root, &cursor) {
        (Some(r), _) => r.start.checked_sub(1).map(|i| i as u32),
        (_, Some(c)) => c.prev(),
        _ => None,
    };
    let mut previndex: u64 = prev
        .map(|i| store.features[i as usize].index())
        .unwrap_or(0);

    let (ox_root, oy_root) = if input.root && z != 0 {
        ((tx << (32 - z)) as i64, (ty << (32 - z)) as i64)
    } else {
        (0, 0)
    };

    let mut geometry = DrawVec::new();
    let mut edge_nodes = DrawVec::new();

    loop {
        geometry.clear();
        let mut bbox = [0i64; 4];
        let (fi, mut gap) = if let Some(r) = root.as_mut() {
            let Some(i) = r.next() else {
                break;
            };
            (i as u32, 0)
        } else {
            match cursor.as_mut().unwrap().next(&mut geometry, &mut bbox) {
                Some(next) => next,
                None => break,
            }
        };
        let f = &store.features[fi as usize];

        if input.root {
            bbox = store.feature_bbox(f);
            if bbox[0] <= bbox[2] {
                bbox[0] -= ox_root;
                bbox[1] -= oy_root;
                bbox[2] -= ox_root;
                bbox[3] -= oy_root;
            }
            let src = store.feature_geometry(f);
            if ox_root == 0 && oy_root == 0 {
                geometry.extend(src.iter().map(|d| Draw::new(d.op, d.x, d.y)));
            } else {
                geometry.extend(src.iter().map(|d| {
                    if d.op == VT_MOVETO || d.op == VT_LINETO {
                        Draw::new(d.op, d.x - ox_root, d.y - oy_root)
                    } else {
                        Draw::new(d.op, 0, 0)
                    }
                }));
            }
        }

        out.original_features += 1;

        if gap == 0 && f.index() != previndex {
            let ox: i64 = (1i64 << (32 - z)).wrapping_mul(tx as i64);
            let oy: i64 = (1i64 << (32 - z)).wrapping_mul(ty as i64);

            let (wx1, wy1) = decode_index(previndex);

            for g in &geometry {
                let dx: i64 = wx1 as i64 - (g.x + ox);
                let dy: i64 = wy1 as i64 - (g.y + oy);

                let d = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) as u64;
                if d > gap {
                    gap = d;
                }
            }
        }
        previndex = f.index();

        edge_nodes.clear();
        if clip_to_tile(
            ctx,
            &mut geometry,
            &mut bbox,
            &mut edge_nodes,
            f.t,
            z,
            buffer as i64,
        ) {
            continue;
        }

        if !geometry.is_empty() {
            out.unclipped_features += 1;
        }

        if rp.children && (f.tippecanoe_maxzoom == -1 || f.tippecanoe_maxzoom as i32 >= rp.nextzoom)
        {
            rewrite(
                ctx,
                fi,
                gap,
                &geometry,
                &bbox,
                z,
                tx,
                ty,
                rp,
                &mut out.children,
            );
        }

        if z < o.minzoom {
            continue;
        }

        if f.tippecanoe_minzoom != -1 && z < f.tippecanoe_minzoom as i32 {
            // a leaf at z carries only z-visible content, so an excluded feature at
            // any deeper minzoom (even z + 1) must block leafing here
            out.minzoom_feature_pending = true;
            continue;
        }
        if f.tippecanoe_maxzoom != -1 && z > f.tippecanoe_maxzoom as i32 {
            continue;
        }

        let mut attrs = NO_LIST;
        if let Some(filter) = ctx.filter {
            match filter_feature(ctx, filter, fi, z) {
                None => continue,
                Some(None) => {}
                Some(Some(list)) => {
                    attrs = out.attr_lists.len() as u32;
                    out.attr_lists.push(list);
                }
            }
        }

        let geom_start = out.geom.len() as u32;
        out.geom.extend_from_slice(&geometry);
        let edge_start = out.edges.len() as u32;
        out.edges.extend_from_slice(&edge_nodes);

        out.cands.push(Cand {
            fi,
            dropped: FEATURE_DROPPED,
            gap,
            geom_start,
            geom_len: geometry.len() as u32,
            edge_start,
            edge_len: edge_nodes.len() as u32,
            attrs,
            full: NO_LIST,
        });
    }

    out.children.sort_by_key(|p| (p.x, p.y));
    out
}

/// Evaluates the `-j` filter. `None` if the feature is filtered out,
/// `Some(Some(attrs))` if some of its attributes are to be removed.
fn filter_feature(
    ctx: &Ctx<'_>,
    filter: tippecanoe::jsonpull::JsonRef<'_>,
    fi: u32,
    z: i32,
) -> Option<Option<Vec<Attr>>> {
    let store = ctx.store;
    let f = &store.features[fi as usize];
    let mut attributes: HashMap<String, MvtValue> = HashMap::new();
    let layername = &store.layers[f.layer as usize].name;
    let mut exclude_attributes: BTreeSet<String> = BTreeSet::new();

    for a in store.feature_attrs(f) {
        attributes
            .entry(store.keys[a.key as usize].clone())
            .or_insert_with(|| ctx.value(a.value));
    }

    if f.has_id {
        let mut v = MvtValue {
            type_: MVT_UINT,
            ..Default::default()
        };
        v.numeric_value.set_uint_value(f.id);
        attributes.entry("$id".to_string()).or_insert(v);
    }

    let mut v = MvtValue {
        type_: MVT_STRING,
        ..Default::default()
    };
    if f.t as i32 == MVT_POINT {
        v.set_string_value("Point");
    } else if f.t as i32 == MVT_LINESTRING {
        v.set_string_value("LineString");
    } else if f.t as i32 == MVT_POLYGON {
        v.set_string_value("Polygon");
    }
    attributes.entry("$type".to_string()).or_insert(v);

    let mut v2 = MvtValue {
        type_: MVT_UINT,
        ..Default::default()
    };
    v2.numeric_value.set_uint_value(z as u64);
    attributes.entry("$zoom".to_string()).or_insert(v2);

    if !evaluate(
        &attributes,
        layername,
        filter,
        &mut exclude_attributes,
        &ctx.o.unidecode_data,
    ) {
        return None;
    }

    if exclude_attributes.is_empty() {
        return Some(None);
    }

    Some(Some(
        store
            .feature_attrs(f)
            .iter()
            .filter(|a| {
                !ctx.value_null[a.value as usize]
                    && !exclude_attributes.contains(&store.keys[a.key as usize])
            })
            .copied()
            .collect(),
    ))
}

/// tippecanoe's `clip_to_tile`. Returns true if the feature is entirely
/// outside the tile by its bounding box alone.
fn clip_to_tile(
    ctx: &Ctx<'_>,
    geometry: &mut DrawVec,
    bbox: &mut [i64; 4],
    edge_nodes: &mut DrawVec,
    t: i8,
    z: i32,
    buffer: i64,
) -> bool {
    let o = ctx.o;
    let mut quick = quick_check(bbox, z, buffer);

    if z == 0
        && (bbox[0] <= (1i64 << 32) * buffer / 256
            || bbox[2] >= (1i64 << 32) - ((1i64 << 32) * buffer / 256))
    {
        // If the geometry extends off the edge of the world, concatenate on another copy
        // shifted by 360 degrees, and then make sure both copies get clipped down to size.

        let n = geometry.len();

        if bbox[0] <= (1i64 << 32) * buffer / 256 {
            for i in 0..n {
                let g = geometry[i];
                geometry.push(Draw::new(g.op, g.x + (1i64 << 32), g.y));
            }
        }

        if bbox[2] >= (1i64 << 32) - ((1i64 << 32) * buffer / 256) {
            for i in 0..n {
                let g = geometry[i];
                geometry.push(Draw::new(g.op, g.x - (1i64 << 32), g.y));
            }
        }

        bbox[0] = 0;
        bbox[2] = 1i64 << 32;

        quick = -1;
    }

    if quick == 0 {
        return true;
    }

    if quick != 1 || o.p(P_DUPLICATION) {
        let t = t as i32;
        let mut clipped = DrawVec::new();
        if t == VT_LINE {
            clipped = clip_lines(geometry, z, buffer);
        }
        if t == VT_POLYGON {
            clipped = simple_clip_poly(
                geometry,
                z,
                buffer as i32,
                edge_nodes,
                o.p(P_SIMPLIFY_SHARED_NODES),
            );
        }
        if t == VT_POINT {
            clipped = clip_point(geometry, z, buffer);
        }

        clipped = remove_noop(clipped, t, 0);

        if o.p(P_DUPLICATION) && z != 0 {
            if !point_within_tile((bbox[0] + bbox[2]) / 2, (bbox[1] + bbox[3]) / 2, z) {
                geometry.clear();
            }
        } else if o.p(P_CLIPPING) && z != 0 {
            if clipped.is_empty() {
                geometry.clear();
            }
        } else {
            *geometry = clipped;
        }
    }

    false
}

/// tippecanoe's `rewrite`: hands a copy of the feature to every child tile it
/// can touch, with its geometry as the child will read it back from the
/// temporary file (scaled down and back up by the geometry scale).
#[allow(clippy::too_many_arguments)]
fn rewrite(
    ctx: &Ctx<'_>,
    fi: u32,
    gap: u64,
    geometry: &DrawVec,
    bbox: &[i64; 4],
    z: i32,
    tx: u32,
    ty: u32,
    rp: &ReadParams,
    children: &mut Vec<ChildPart>,
) {
    let o = ctx.o;
    let nextzoom = rp.nextzoom;
    if geometry.is_empty()
        || !(nextzoom <= rp.maxzoom || o.a(A_EXTEND_ZOOMS) || rp.extend_zooms_max > 0)
    {
        return;
    }

    let buffer = o.buffer;
    let gs = o.geometry_scale;
    let span: i32 = 1 << (nextzoom - z);

    let mut bbox2 = [0i64; 4];
    for (b, &v) in bbox2.iter_mut().zip(bbox.iter()) {
        *b = v / 1i32.wrapping_shl((32 - nextzoom - 8) as u32) as i64;
    }
    bbox2[0] -= (buffer + 1) as i64;
    bbox2[1] -= (buffer + 1) as i64;
    bbox2[2] += buffer as i64;
    bbox2[3] += buffer as i64;

    for b in bbox2.iter_mut() {
        if *b < 0 {
            *b = 0;
        }
        if *b >= 256 * span as i64 {
            *b = 256 * (span as i64 - 1);
        }
        *b /= 256;
    }

    let mut sx: u32 = 0;
    let mut sy: u32 = 0;
    if z != 0 {
        sx = tx << (32 - z);
        sy = ty << (32 - z);
    }

    let scale = (1i32 << gs) as i64;
    let shift = child_shift(ctx, nextzoom);

    let mut xo = bbox2[0] as i32;
    while xo as i64 <= bbox2[2] {
        let mut yo = bbox2[1] as i32;
        while yo as i64 <= bbox2[3] {
            let cx: u32 = tx.wrapping_mul(span as u32).wrapping_add(xo as u32);
            let cy: u32 = ty.wrapping_mul(span as u32).wrapping_add(yo as u32);
            let cox = (cx << (32 - nextzoom)) as i64;
            let coy = (cy << (32 - nextzoom)) as i64;

            let i = match children.iter().position(|p| p.x == cx && p.y == cy) {
                Some(i) => i,
                None => {
                    children.push(ChildPart {
                        x: cx,
                        y: cy,
                        data: Vec::new(),
                        runs: Vec::new(),
                        count: 0,
                        vertices: 0,
                        last: NO_FEATURE,
                        base: 0,
                    });
                    children.len() - 1
                }
            };
            let part = &mut children[i];
            if part.runs.last().is_none_or(|r| r.count >= RUN) {
                part.runs.push(Run {
                    start: part.data.len() as u64,
                    count: 0,
                    vertices: 0,
                    prev: part.last,
                });
                part.base = 0;
            }
            let run = part.runs.last_mut().unwrap();
            run.count += 1;
            run.vertices = run.vertices.saturating_add(geometry.len() as u32);
            part.count += 1;
            part.vertices += geometry.len() as u64;
            put_zigzag(&mut part.data, fi as i64 - part.base as i64);
            part.base = fi;
            part.last = fi;
            put_varint(&mut part.data, gap);
            put_varint(&mut part.data, geometry.len() as u64);
            let (mut px, mut py) = (0i64, 0i64);
            for g in geometry {
                part.data.push(g.op as u8);
                if g.op == VT_MOVETO || g.op == VT_LINETO {
                    let x = (shift_right(g.x + sx as i64, gs) * scale - cox) >> shift;
                    let y = (shift_right(g.y + sy as i64, gs) * scale - coy) >> shift;
                    put_zigzag(&mut part.data, x - px);
                    put_zigzag(&mut part.data, y - py);
                    px = x;
                    py = y;
                }
            }

            yo += 1;
        }
        xo += 1;
    }
}
