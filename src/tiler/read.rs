//! Reading a tile's input: what tippecanoe's `next_feature` does for every
//! feature of a tile, including writing the features out for the next zoom.
//!
//! tippecanoe does this again for every retry of a tile and every pass of a
//! zoom, but the results are always the same, so here it happens once and the
//! candidates are kept.

use std::collections::{BTreeSet, HashMap};

use tippecanoe::clip::{clip_lines, clip_point, remove_noop, simple_clip_poly};
use tippecanoe::evaluator::evaluate;
use tippecanoe::geometry::{
    Draw, DrawVec, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON, point_within_tile,
    quick_check,
};
use tippecanoe::mvt::{MVT_LINESTRING, MVT_POINT, MVT_POLYGON, MVT_STRING, MVT_UINT, MvtValue};
use tippecanoe::options::*;
use tippecanoe::projection::decode_index;
use tippecanoe::serial::{FEATURE_ADDED_FOR_MULTIPLIER_DENSITY, FEATURE_DROPPED, FEATURE_KEPT};
use tippecanoe::text::bit_reverse;

use super::engine::Ctx;
use super::util::{par_ranges, shift_right};
use crate::store::Attr;
use std::sync::Arc;
use tippecanoe::serial::SerialVal;

/// `pow(2, 64)`.
const TWO_TO_THE_64: f64 = 18446744073709551616.0;

/// How many input features one parallel job reads.
const CHUNK: usize = 16384;

/// A feature on its way to a tile: what tippecanoe writes into the temporary
/// file for the tile. Only the clipped geometry and the gap change on the way
/// down; everything else comes from the store.
#[derive(Clone, Copy, Debug)]
pub struct Carried {
    pub fi: u32,
    pub gap: u64,
    /// Into the store's geometry for the first tile, otherwise into `TileInput::geom`.
    pub geom_start: u64,
    pub geom_len: u32,
    pub bbox: [i64; 4],
}

/// The input of one tile.
pub struct TileInput {
    pub z: i32,
    pub x: u32,
    pub y: u32,
    pub feats: Vec<Carried>,
    pub geom: Vec<Draw>,
    /// The first tile reads its geometry from the store.
    pub root: bool,
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
    pub cands: Vec<Cand>,
    pub geom: Vec<Draw>,
    pub edges: Vec<Draw>,
    pub attr_lists: Vec<Vec<Attr>>,
    pub full: Vec<(Vec<Arc<String>>, Vec<SerialVal>)>,
    pub original_features: i64,
    pub unclipped_features: i64,
    pub minzoom_feature_pending: bool,
    /// The inputs of the tiles at the next zoom written, in tile order.
    pub children: Vec<TileInput>,
}

impl ReadTile {
    pub fn cand_geom(&self, c: &Cand) -> &[Draw] {
        &self.geom[c.geom_start as usize..(c.geom_start + c.geom_len) as usize]
    }

    pub fn cand_edges(&self, c: &Cand) -> &[Draw] {
        &self.edges[c.edge_start as usize..(c.edge_start + c.edge_len) as usize]
    }
}

/// Zoom-level values that reading needs.
pub struct ReadParams {
    pub nextzoom: i32,
    pub maxzoom: i32,
    pub extend_zooms_max: i64,
}

struct ChildPart {
    x: u32,
    y: u32,
    feats: Vec<Carried>,
    geom: Vec<Draw>,
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

/// Reads one tile.
pub fn read_tile(ctx: &Ctx<'_>, input: TileInput, rp: &ReadParams) -> ReadTile {
    let n = input.feats.len();
    let chunks: Vec<ChunkOut> = par_ranges(n, CHUNK, ctx.threads, |r| {
        read_chunk(ctx, &input, r.start, r.end, rp)
    });

    let mut out = ReadTile {
        z: input.z,
        x: input.x,
        y: input.y,
        cands: Vec::new(),
        geom: Vec::new(),
        edges: Vec::new(),
        attr_lists: Vec::new(),
        full: Vec::new(),
        original_features: 0,
        unclipped_features: 0,
        minzoom_feature_pending: false,
        children: Vec::new(),
    };

    if chunks.len() == 1 {
        let c = chunks.into_iter().next().unwrap();
        out.cands = c.cands;
        out.geom = c.geom;
        out.edges = c.edges;
        out.attr_lists = c.attr_lists;
        out.original_features = c.original_features;
        out.unclipped_features = c.unclipped_features;
        out.minzoom_feature_pending = c.minzoom_feature_pending;
        out.children = c
            .children
            .into_iter()
            .map(|p| TileInput {
                z: rp.nextzoom,
                x: p.x,
                y: p.y,
                feats: p.feats,
                geom: p.geom,
                root: false,
            })
            .collect();
    } else {
        out.cands
            .reserve(chunks.iter().map(|c| c.cands.len()).sum());
        out.geom.reserve(chunks.iter().map(|c| c.geom.len()).sum());
        let mut children: Vec<TileInput> = Vec::new();
        for c in chunks {
            let goff = out.geom.len() as u32;
            let eoff = out.edges.len() as u32;
            let aoff = out.attr_lists.len() as u32;
            out.cands.extend(c.cands.into_iter().map(|mut cand| {
                cand.geom_start += goff;
                cand.edge_start += eoff;
                if cand.attrs != NO_LIST {
                    cand.attrs += aoff;
                }
                cand
            }));
            out.geom.extend(c.geom);
            out.edges.extend(c.edges);
            out.attr_lists.extend(c.attr_lists);
            out.original_features += c.original_features;
            out.unclipped_features += c.unclipped_features;
            out.minzoom_feature_pending |= c.minzoom_feature_pending;

            for p in c.children {
                let pos = children.binary_search_by(|t| (t.x, t.y).cmp(&(p.x, p.y)));
                let i = match pos {
                    Ok(i) => i,
                    Err(i) => {
                        children.insert(
                            i,
                            TileInput {
                                z: rp.nextzoom,
                                x: p.x,
                                y: p.y,
                                feats: Vec::new(),
                                geom: Vec::new(),
                                root: false,
                            },
                        );
                        i
                    }
                };
                let t = &mut children[i];
                let off = t.geom.len() as u64;
                t.feats.extend(p.feats.into_iter().map(|mut f| {
                    f.geom_start += off;
                    f
                }));
                t.geom.extend(p.geom);
            }
        }
        out.children = children;
    }

    assign_dropped(ctx, &mut out);
    out
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
                f.feature_minzoom as f64 - (bit_reverse(f.index >> 2) as f64 / TWO_TO_THE_64);

            if z as f64 >= feature_minzoom || dropped == FEATURE_KEPT {
                count[layer] = 0;
                dropped = FEATURE_KEPT;
            } else if z as f64 + extra_multiplier_zooms >= feature_minzoom
                && count[layer] + 1 < retain_points_multiplier
            {
                count[layer] += 1;
                dropped = count[layer];
            } else if pmdt > 0
                && f.index.wrapping_sub(prev_not_dropped_index)
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
            prev_not_dropped_index = f.index;
        }

        c.dropped = dropped;
    }
}

fn read_chunk(
    ctx: &Ctx<'_>,
    input: &TileInput,
    start: usize,
    end: usize,
    rp: &ReadParams,
) -> ChunkOut {
    let o = ctx.o;
    let store = ctx.store;
    let z = input.z;
    let tx = input.x;
    let ty = input.y;
    let buffer = o.buffer;

    let mut out = ChunkOut {
        cands: Vec::with_capacity(end - start),
        geom: Vec::new(),
        edges: Vec::new(),
        attr_lists: Vec::new(),
        children: Vec::new(),
        original_features: 0,
        unclipped_features: 0,
        minzoom_feature_pending: false,
    };

    // next_feature_state.previndex: the index of the feature read before
    let mut previndex: u64 = if start == 0 {
        0
    } else {
        store.features[input.feats[start - 1].fi as usize].index
    };

    let (ox_root, oy_root) = if input.root && z != 0 {
        ((tx << (32 - z)) as i64, (ty << (32 - z)) as i64)
    } else {
        (0, 0)
    };

    let mut geometry = DrawVec::new();
    let mut edge_nodes = DrawVec::new();

    for c in &input.feats[start..end] {
        let f = &store.features[c.fi as usize];

        geometry.clear();
        let mut bbox = c.bbox;
        if input.root {
            let src =
                &store.geometry[c.geom_start as usize..c.geom_start as usize + c.geom_len as usize];
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
        } else {
            geometry.extend_from_slice(
                &input.geom[c.geom_start as usize..c.geom_start as usize + c.geom_len as usize],
            );
        }

        out.original_features += 1;

        let mut gap = c.gap;
        if gap == 0 && f.index != previndex {
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
        previndex = f.index;

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

        if f.tippecanoe_maxzoom == -1 || f.tippecanoe_maxzoom as i32 >= rp.nextzoom {
            rewrite(
                ctx,
                c.fi,
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
            match filter_feature(ctx, filter, c.fi, z) {
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
            fi: c.fi,
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
            .or_insert_with(|| ctx.values[a.value as usize].clone());
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
                        feats: Vec::new(),
                        geom: Vec::new(),
                    });
                    children.len() - 1
                }
            };
            let part = &mut children[i];
            let geom_start = part.geom.len() as u64;
            let mut cb = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
            for g in geometry {
                if g.op == VT_MOVETO || g.op == VT_LINETO {
                    let x = shift_right(g.x + sx as i64, gs) * scale - cox;
                    let y = shift_right(g.y + sy as i64, gs) * scale - coy;
                    cb[0] = cb[0].min(x);
                    cb[1] = cb[1].min(y);
                    cb[2] = cb[2].max(x);
                    cb[3] = cb[3].max(y);
                    part.geom.push(Draw::new(g.op, x, y));
                } else {
                    part.geom.push(Draw::new(g.op, 0, 0));
                }
            }
            part.feats.push(Carried {
                fi,
                gap,
                geom_start,
                geom_len: geometry.len() as u32,
                bbox: cb,
            });

            yo += 1;
        }
        xo += 1;
    }
}
