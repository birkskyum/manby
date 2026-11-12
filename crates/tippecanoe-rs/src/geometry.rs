//! Port of geometry.hpp (the geometry types) and geometry.cpp.
//! The functions geometry.hpp declares but clip.cpp defines are in clip.rs.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};

use crate::clip::{
    clip, distance_from_line, douglas_peucker, get_area, pnpoly, remove_noop, simple_clip_poly_bbox,
};
use crate::errors::EXIT_IMPOSSIBLE;
use crate::globals::{GEOMETRY_SCALE, additional, get, prevent};
use crate::mvt::MvtTile;
use crate::options::{A_VISVALINGAM, P_SIMPLIFY_SHARED_NODES};
use crate::projection::{decode_index, encode_index, encode_vertex, tile2lonlat};
use crate::serial::{Node, deserialize_byte, deserialize_long_long, nodecmp};
use crate::visvalingam::visvalingam;

pub const VT_POINT: i32 = 1;
pub const VT_LINE: i32 = 2;
pub const VT_POLYGON: i32 = 3;

pub const VT_END: i8 = 0;
pub const VT_MOVETO: i8 = 1;
pub const VT_LINETO: i8 = 2;
pub const VT_CLOSEPATH: i8 = 7;

/// `draw`. The C++ packs `x` and `y` into 40-bit bitfields to keep the struct at
/// 16 bytes. Coordinates stay far inside that range, so plain `i64` is used here.
///
/// Equality and ordering look only at the coordinates, `y` first, as in the C++.
#[derive(Clone, Copy, Debug, Default)]
pub struct Draw {
    pub x: i64,
    pub op: i8,
    pub y: i64,
    pub necessary: i8,
}

impl Draw {
    pub fn new(op: i8, x: i64, y: i64) -> Draw {
        Draw {
            x,
            op,
            y,
            necessary: 0,
        }
    }
}

impl PartialEq for Draw {
    fn eq(&self, s: &Draw) -> bool {
        self.y == s.y && self.x == s.x
    }
}

impl Eq for Draw {}

impl PartialOrd for Draw {
    fn partial_cmp(&self, s: &Draw) -> Option<Ordering> {
        Some(self.cmp(s))
    }
}

impl Ord for Draw {
    fn cmp(&self, s: &Draw) -> Ordering {
        self.y.cmp(&s.y).then(self.x.cmp(&s.x))
    }
}

pub type DrawVec = Vec<Draw>;

#[derive(Clone, Debug, Default)]
pub struct InputTile {
    pub tile: Vec<u8>,
    pub z: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Default)]
pub struct SourceTile {
    pub tile: MvtTile,
    pub z: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ClipBbox {
    pub lon1: f64,
    pub lat1: f64,
    pub lon2: f64,
    pub lat2: f64,

    pub minx: i64,
    pub miny: i64,
    pub maxx: i64,
    pub maxy: i64,

    /// empty, or arbitrary clipping polygon
    pub dv: DrawVec,
}

/// The value a coordinate has after the C++ stores it in the 40-bit bitfield of
/// a `draw`. Only needed where a computed coordinate can be out of that range.
pub fn bitfield40(v: i64) -> i64 {
    v.wrapping_shl(24) >> 24
}

/// `std::llround`.
fn llround(v: f64) -> i64 {
    v.round() as i64
}

/// Reads a serialized geometry, advancing `meta` past it.
pub fn decode_geometry(
    meta: &mut &[u8],
    z: i32,
    tx: u32,
    ty: u32,
    bbox: &mut [i64; 4],
    initial_x: u32,
    initial_y: u32,
) -> DrawVec {
    let mut out = DrawVec::new();

    bbox[0] = i64::MAX;
    bbox[1] = i64::MAX;
    bbox[2] = i64::MIN;
    bbox[3] = i64::MIN;

    let mut wx = initial_x as i64;
    let mut wy = initial_y as i64;
    let geometry_scale = get(&GEOMETRY_SCALE);

    loop {
        let mut d = Draw {
            op: deserialize_byte(meta),
            ..Default::default()
        };

        if d.op == VT_END {
            break;
        }

        if d.op == VT_MOVETO || d.op == VT_LINETO {
            let dx = deserialize_long_long(meta);
            let dy = deserialize_long_long(meta);

            wx += dx * (1i32 << geometry_scale) as i64;
            wy += dy * (1i32 << geometry_scale) as i64;

            let mut wwx = wx;
            let mut wwy = wy;

            if z != 0 {
                wwx -= (tx << (32 - z)) as i64;
                wwy -= (ty << (32 - z)) as i64;
            }

            bbox[0] = std::cmp::min(wwx, bbox[0]);
            bbox[1] = std::cmp::min(wwy, bbox[1]);
            bbox[2] = std::cmp::max(wwx, bbox[2]);
            bbox[3] = std::cmp::max(wwy, bbox[3]);

            d.x = wwx;
            d.y = wwy;
        }

        out.push(d);
    }

    out
}

pub fn check_polygon(geom: &mut DrawVec) {
    *geom = remove_noop(std::mem::take(geom), VT_POLYGON, 0);

    let mut mp: snap_rounding::MultiPolygon = Vec::new();
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            if j >= i + 4 {
                let mut lr: snap_rounding::LinearRing = Vec::new();

                for g in &geom[i..j] {
                    lr.push(snap_rounding::Point { x: g.x, y: g.y });
                }

                if lr.len() >= 3 {
                    let p: snap_rounding::Polygon = vec![lr];
                    mp.push(p);
                }
            }

            i = j - 1;
        }
        i += 1;
    }

    let mp2 = snap_rounding::snap_round(mp.clone(), true, true);
    if mp != mp2 {
        eprintln!("Internal error: self-intersecting polygon");
    }

    let mut outer_start: usize = usize::MAX;
    let mut outer_len: usize = 0;

    for i in 0..geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            let area = get_area(geom, i, j);

            if area > 0.0 {
                outer_start = i;
                outer_len = j - i;
            } else {
                for k in i..j {
                    if pnpoly(geom, outer_start, outer_len, geom[k].x, geom[k].y) == 0 {
                        let mut on_edge = false;

                        let mut l = outer_start;
                        while l < outer_start.wrapping_add(outer_len) {
                            if geom[k].x == geom[l].x || geom[k].y == geom[l].y {
                                on_edge = true;
                                break;
                            }
                            l += 1;
                        }

                        if !on_edge {
                            eprintln!(
                                "{},{} at {} not in outer ring ({} to {})",
                                geom[k].x,
                                geom[k].y,
                                k as i64,
                                outer_start as i64,
                                outer_start.wrapping_add(outer_len) as i64
                            );
                        }
                    }
                }
            }
        }
    }
}

pub fn quick_check(bbox: &[i64; 4], z: i32, buffer: i64) -> i32 {
    let mut min: i64 = 0;
    let mut area: i64 = 1i64 << (32 - z);

    // bbox entirely within the tile proper
    if bbox[0] > min && bbox[1] > min && bbox[2] < area && bbox[3] < area {
        return 1;
    }

    min -= buffer * area / 256;
    area += buffer * area / 256;

    // bbox entirely within the tile, including its buffer
    if bbox[0] > min && bbox[1] > min && bbox[2] < area && bbox[3] < area {
        return 3;
    }

    // bbox entirely outside the tile
    if bbox[0] > area || bbox[1] > area {
        return 0;
    }
    if bbox[2] < min || bbox[3] < min {
        return 0;
    }

    // some overlap of edge
    2
}

pub fn point_within_tile(x: i64, y: i64, z: i32) -> bool {
    // No adjustment for buffer, because the point must be
    // strictly within the tile to appear exactly once

    let area: i64 = 1i64 << (32 - z);

    x >= 0 && y >= 0 && x < area && y < area
}

// If any line segment crosses a tile boundary, add a node there
// that cannot be simplified away, to prevent the edge of any
// feature from jumping abruptly at the tile boundary.
pub fn impose_tile_boundaries(geom: &DrawVec, extent: i64) -> DrawVec {
    let mut out = DrawVec::new();

    for i in 0..geom.len() {
        if i > 0
            && geom[i].op == VT_LINETO
            && (geom[i - 1].op == VT_MOVETO || geom[i - 1].op == VT_LINETO)
        {
            let mut x1 = geom[i - 1].x;
            let mut y1 = geom[i - 1].y;

            let mut x2 = geom[i].x;
            let mut y2 = geom[i].y;

            let c = clip(&mut x1, &mut y1, &mut x2, &mut y2, 0, 0, extent, extent);

            if c > 1 {
                // clipped
                if x1 != geom[i - 1].x || y1 != geom[i - 1].y {
                    out.push(Draw::new(VT_LINETO, x1, y1));
                    let last = out.len() - 1;
                    out[last].necessary = 1;
                }
                if x2 != geom[i].x || y2 != geom[i].y {
                    out.push(Draw::new(VT_LINETO, x2, y2));
                    let last = out.len() - 1;
                    out[last].necessary = 1;
                }
            }
        }

        out.push(geom[i]);
    }

    out
}

#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
pub fn simplify_lines(
    geom: &mut DrawVec,
    z: i32,
    tx: i32,
    ty: i32,
    detail: i32,
    mark_tile_bounds: bool,
    simplification: f64,
    retain: usize,
    shared_nodes: &DrawVec,
    shared_nodes_map: &[Node],
    nodepos: usize,
    shared_nodes_bloom: &[u8],
) -> DrawVec {
    let res: i32 = 1i32.wrapping_shl((32 - detail - z) as u32);
    let area: i64 = 1i64.wrapping_shl((32 - z) as u32);

    for i in 0..geom.len() {
        if geom[i].op == VT_MOVETO {
            geom[i].necessary = 1;
        } else if geom[i].op == VT_LINETO {
            geom[i].necessary = 0;
            // if this is actually the endpoint, not an intermediate point,
            // it will be marked as necessary below
        } else {
            geom[i].necessary = 1;
        }

        if prevent(P_SIMPLIFY_SHARED_NODES) != 0 {
            // This is kind of weird, because we have two lists of shared nodes to look through:
            // * the drawvec, which is nodes that were introduced during clipping to the tile edge,
            //   and which are in local tile coordinates
            // * the shared_nodes_map, which was made globally before tiling began, and which
            //   is in global quadkey coordinates.
            // To look through the latter, we need to offset and encode the coordinates
            // of the feature we are simplifying.

            let pt = shared_nodes.partition_point(|n| *n < geom[i]);
            if pt != shared_nodes.len() && shared_nodes[pt] == geom[i] {
                geom[i].necessary = 1;
            }

            if nodepos > 0 {
                // offset to global
                let mut d = geom[i];
                if z != 0 {
                    d.x += tx as i64 * (1i64 << (32 - z));
                    d.y += ty as i64 * (1i64 << (32 - z));
                }

                let n = Node {
                    index: encode_vertex(d.x as u32, d.y as u32),
                };
                let mut bloom_ix = (n.index % (shared_nodes_bloom.len() as u64 * 8)) as usize;
                let bloom_mask: u8 = 1 << (bloom_ix & 7);
                bloom_ix >>= 3;

                if shared_nodes_bloom[bloom_ix] & bloom_mask != 0 {
                    let count = nodepos / std::mem::size_of::<Node>();
                    let map = &shared_nodes_map[..count.min(shared_nodes_map.len())];
                    if map.binary_search_by(|probe| nodecmp(probe, &n)).is_ok() {
                        geom[i].necessary = 1;
                    }
                }
            }
        }
    }

    if mark_tile_bounds {
        *geom = impose_tile_boundaries(geom, area);
    }

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            geom[i].necessary = 1;
            geom[j - 1].necessary = 1;

            // empirical mapping from douglas-peucker simplifications
            // to visvalingam simplifications that yield similar
            // output sizes
            let sim = simplification * (0.1596 * z as f64 + 0.878);
            let mut scale = (res as f64 * sim) * (res as f64 * sim);
            scale = (1.002 * scale.ln() + 0.3043).exp();

            if j - i > 1 {
                if additional(A_VISVALINGAM) != 0 {
                    visvalingam(geom, i, j, scale, retain);
                } else {
                    douglas_peucker(
                        geom,
                        i as i32,
                        (j - i) as i32,
                        res as f64 * simplification,
                        2,
                        retain,
                        prevent(P_SIMPLIFY_SHARED_NODES) != 0,
                    );
                }
            }
            i = j - 1;
        }
        i += 1;
    }

    let mut out = 0;
    for i in 0..geom.len() {
        if geom[i].necessary != 0 {
            geom[out] = geom[i];
            out += 1;
        }
    }
    geom.truncate(out);
    geom.clone()
}

pub fn reorder_lines(geom: &DrawVec) -> DrawVec {
    // Only reorder simple linestrings with a single moveto

    if geom.is_empty() {
        return geom.clone();
    }

    for i in 0..geom.len() {
        if geom[i].op == VT_MOVETO {
            if i != 0 {
                // moveto is not at the start, so it is not simple
                return geom.clone();
            }
        } else if geom[i].op == VT_LINETO {
            if i == 0 {
                // lineto is at the start: can't happen
                return geom.clone();
            }
        } else {
            // something other than moveto or lineto: can't happen
            return geom.clone();
        }
    }

    // Reorder anything that goes up and to the left
    // instead of down and to the right
    // so that it will coalesce better

    let l1 = encode_index(geom[0].x as u32, geom[0].y as u32);
    let l2 = encode_index(geom[geom.len() - 1].x as u32, geom[geom.len() - 1].y as u32);

    if l1 > l2 {
        let mut out = DrawVec::new();
        for i in 0..geom.len() {
            out.push(geom[geom.len() - 1 - i]);
        }
        out[0].op = VT_MOVETO;
        if out.len() > 1 {
            let last = out.len() - 1;
            out[last].op = VT_LINETO;
        }
        return out;
    }

    geom.clone()
}

/// geometry.cpp has this function under `#if 0` (it refers to a
/// `P_SIMPLIFY_EDGE_NODES` option that no longer exists), so the C++ build has
/// no definition and nothing can call it.
pub fn chop_polygon(geoms: &mut Vec<DrawVec>) -> Vec<DrawVec> {
    let _ = (geoms, simple_clip_poly_bbox);
    unreachable!("chop_polygon is compiled out of geometry.cpp");
}

pub fn stairstep(geom: &mut DrawVec, z: i32, detail: i32) -> DrawVec {
    let mut out = DrawVec::new();
    let scale = 1i32.wrapping_shl((32 - detail - z) as u32) as f64;

    for g in geom.iter_mut() {
        g.x = (g.x as f64 / scale).round() as i64;
        g.y = (g.y as f64 / scale).round() as i64;
    }

    for g in geom.iter() {
        if g.op == VT_MOVETO {
            out.push(*g);
        } else if !out.is_empty() {
            let mut x0 = out[out.len() - 1].x;
            let mut y0 = out[out.len() - 1].y;
            let mut x1 = g.x;
            let mut y1 = g.y;
            let mut swap = false;

            if y0 < y1 {
                swap = true;
                std::mem::swap(&mut x0, &mut x1);
                std::mem::swap(&mut y0, &mut y1);
            }

            let mut xx = x0;
            let mut yy = y0;
            let dx = (x1 - x0).abs();
            let sx: i64 = if x0 < x1 { 1 } else { -1 };
            let dy = (y1 - y0).abs();
            let sy: i64 = if y0 < y1 { 1 } else { -1 };
            let mut err = (if dx > dy { dx } else { -dy }) / 2;
            let mut last = -1;

            let mut tmp = DrawVec::new();
            tmp.push(Draw::new(VT_LINETO, xx, yy));

            while xx != x1 || yy != y1 {
                let e2 = err;

                if e2 > -dx {
                    err -= dy;
                    xx += sx;
                    if last == 1 {
                        let n = tmp.len() - 1;
                        tmp[n] = Draw::new(VT_LINETO, xx, yy);
                    } else {
                        tmp.push(Draw::new(VT_LINETO, xx, yy));
                    }
                    last = 1;
                }
                if e2 < dy {
                    err += dx;
                    yy += sy;
                    if last == 2 {
                        let n = tmp.len() - 1;
                        tmp[n] = Draw::new(VT_LINETO, xx, yy);
                    } else {
                        tmp.push(Draw::new(VT_LINETO, xx, yy));
                    }
                    last = 2;
                }
            }

            if swap {
                for j in (1..=tmp.len()).rev() {
                    out.push(tmp[j - 1]);
                }
            } else {
                for t in &tmp {
                    out.push(*t);
                }
            }

            // out.push_back(draw(VT_LINETO, xx, yy));
        } else {
            eprintln!("Can't happen: stairstepping lineto with no moveto");
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }

    for o in out.iter_mut() {
        o.x = bitfield40(o.x.wrapping_mul(1i32.wrapping_shl((32 - detail - z) as u32) as i64));
        o.y = bitfield40(o.y.wrapping_mul(1i32.wrapping_shl((32 - detail - z) as u32) as i64));
    }

    out
}

// https://github.com/Turfjs/turf/blob/master/packages/turf-center-of-mass/index.ts
//
// The MIT License (MIT)
//
// Copyright (c) 2019 Morgan Herlocker
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software is furnished to do so,
// subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
// FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
// COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
// IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
/// `centerOfMass`.
pub fn center_of_mass(dv: &DrawVec, start: usize, end: usize, centre: Draw) -> Draw {
    let mut coords: Vec<Draw> = Vec::new();
    for d in &dv[start..end] {
        coords.push(*d);
    }

    // First, we neutralize the feature (set it around coordinates [0,0]) to prevent rounding errors
    // We take any point to translate all the points around 0
    let translation = centre;
    let mut sx: f64 = 0.0;
    let mut sy: f64 = 0.0;
    let mut s_area: f64 = 0.0;

    let mut neutralized_points: Vec<Draw> = Vec::new();
    for c in &coords {
        neutralized_points.push(Draw::new(
            c.op,
            bitfield40(c.x - translation.x),
            bitfield40(c.y - translation.y),
        ));
    }

    for i in 0..coords.len() - 1 {
        // pi is the current point
        let pi = neutralized_points[i];
        let xi = pi.x as f64;
        let yi = pi.y as f64;

        // pj is the next point (pi+1)
        let pj = neutralized_points[i + 1];
        let xj = pj.x as f64;
        let yj = pj.y as f64;

        // a is the common factor to compute the signed area and the final coordinates
        let a = xi * yj - xj * yi;

        // sArea is the sum used to compute the signed area
        s_area += a;

        // sx and sy are the sums used to compute the final coordinates
        sx += (xi + xj) * a;
        sy += (yi + yj) * a;
    }

    // Shape has no area: fallback on turf.centroid
    if s_area == 0.0 {
        centre
    } else {
        // Compute the signed area, and factorize 1/6A
        let area = s_area * 0.5;
        let area_factor = 1.0 / (6.0 * area);

        // Compute the final coordinates, adding back the values that have been neutralized
        Draw::new(
            VT_MOVETO,
            bitfield40((translation.x as f64 + area_factor * sx) as i64),
            bitfield40((translation.y as f64 + area_factor * sy) as i64),
        )
    }
}

pub fn center_of_mass_mp(dv: &DrawVec) -> Draw {
    let mut ringx: f64 = 0.0;
    let mut ringy: f64 = 0.0;
    // size_t, which is 64 bits where tippecanoe runs
    let mut ringcount: u64 = 0;

    let mut i = 0;
    while i < dv.len() {
        if dv[i].op == VT_MOVETO {
            let mut xsum = dv[i].x as f64;
            let mut ysum = dv[i].y as f64;
            let mut count: isize = 1;
            let mut j = i + 1;
            while j < dv.len() {
                if dv[j].op != VT_LINETO {
                    break;
                } else {
                    xsum += dv[j].x as f64;
                    ysum += dv[j].y as f64;
                    count += 1;
                }
                j += 1;
            }

            let area = get_area(dv, i, j);
            let centroid = Draw::new(
                VT_MOVETO,
                bitfield40(llround(xsum / count as f64)),
                bitfield40(llround(ysum / count as f64)),
            );

            let center = center_of_mass(dv, i, j, centroid);
            ringx += center.x as f64 * area;
            ringy += center.y as f64 * area;
            // size_t += double converts the sum back to size_t
            ringcount = (ringcount as f64 + area) as u64;

            i = j - 1;
        }
        i += 1;
    }

    Draw::new(
        VT_MOVETO,
        bitfield40((ringx / ringcount as f64) as i64),
        bitfield40((ringy / ringcount as f64) as i64),
    )
}

pub fn label_goodness(dv: &DrawVec, x: i64, y: i64) -> f64 {
    let mut nesting = 0;

    let mut i = 0;
    while i < dv.len() {
        if dv[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < dv.len() {
                if dv[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            // if it's inside the ring, and it's an outer ring,
            // we are nested more; if it's an inner ring, we are
            // nested less.
            if pnpoly(dv, i, j - i, x, y) != 0 {
                if get_area(dv, i, j) >= 0.0 {
                    nesting += 1;
                } else {
                    nesting -= 1;
                }
            }

            i = j - 1;
        }
        i += 1;
    }

    if nesting < 1 {
        return 0.0; // outside the polygon is as bad as it gets
    }

    let mut closest = f64::INFINITY; // closest distance to the border

    for i in 0..dv.len() {
        let dx = (dv[i].x - x) as f64;
        let dy = (dv[i].y - y) as f64;
        let mut dist = (dx * dx + dy * dy).sqrt();
        if dist < closest {
            closest = dist;
        }

        if i > 0 && dv[i].op == VT_LINETO {
            dist = distance_from_line(x, y, dv[i - 1].x, dv[i - 1].y, dv[i].x, dv[i].y);
            if dist < closest {
                closest = dist;
            }
        }
    }

    closest
}

#[derive(Clone, Copy, Debug)]
struct Sorty {
    x: i64,
    y: i64,
}

/// `sorty_sorter(kind)`: the C++ comparator is a strict "less than" on a pair of
/// derived keys, so this returns the matching `Ordering`.
fn sorty_sorter(kind: usize, a: &Sorty, b: &Sorty) -> Ordering {
    let (xa, ya, xb, yb);

    if kind == 0 {
        // Y first
        xa = a.x;
        ya = a.y;

        xb = b.x;
        yb = b.y;
    } else if kind == 1 {
        // X first
        xa = a.y;
        ya = a.x;

        xb = b.y;
        yb = b.x;
    } else if kind == 2 {
        // diagonal
        xa = a.x + a.y;
        ya = a.x - a.y;

        xb = b.x + b.y;
        yb = b.x - b.y;
    } else {
        // other diagonal
        xa = a.x - a.y;
        ya = a.x + a.y;

        xb = b.x - b.y;
        yb = b.x + b.y;
    }

    ya.cmp(&yb).then(xa.cmp(&xb))
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    x: i64,
    y: i64,
    dist: f64,
}

/// `candidate::operator<`: largest distance sorts first
fn candidate_cmp(a: &Candidate, c: &Candidate) -> Ordering {
    if a.dist > c.dist {
        Ordering::Less
    } else if c.dist > a.dist {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

// Generate a label point for a polygon feature.
//
// A good label point will be near the center of the feature and far from any border.
//
// Polylabel is supposed to be able to do this optimally, but can be quite slow
// and sometimes still produces some odd results.
//
// The centroid is often off-center because edges with many curves will be
// weighted higher than edges with straight lines.
//
// Turf's center-of-mass algorithm generally does a good job, but can sometimes
// find a point that is outside the bounds of the polygon or quite close to the edge.
//
// So prefer the center of mass, but if it produces something too close to the border
// or outside the polygon, try a series of gridded points within the feature's bounding box
// until something works well, or if nothing does after several iterations, use the
// least-bad option.

pub fn polygon_to_anchor(geom: &DrawVec) -> DrawVec {
    let mut start: usize = 0;
    let mut end: usize = 0;
    let mut best_area: u64 = 0;
    let mut points: Vec<Sorty> = Vec::new();

    // find the largest outer ring, which will be the best thing
    // to label if we can do it.

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }

                points.push(Sorty {
                    x: geom[j].x,
                    y: geom[j].y,
                });
                j += 1;
            }

            let area = get_area(geom, i, j);
            if area > best_area as f64 {
                start = i;
                end = j;
                best_area = area as u64;
            }

            i = j - 1;
        }
        i += 1;
    }

    // If there are no outer rings, don't generate a label point

    if best_area > 0 {
        let mut xsum: i64 = 0;
        let mut ysum: i64 = 0;
        let mut count: usize = 0;
        let mut xmin = i64::MAX;
        let mut ymin = i64::MAX;
        let mut xmax = i64::MIN;
        let mut ymax = i64::MIN;

        // Calculate centroid and bounding box of biggest ring.
        // start + 1 to exclude the first point, which is duplicated as the last
        for g in &geom[start + 1..end] {
            xsum += g.x;
            ysum += g.y;
            count += 1;

            xmin = std::cmp::min(xmin, g.x);
            ymin = std::cmp::min(ymin, g.y);
            xmax = std::cmp::max(xmax, g.x);
            ymax = std::cmp::max(ymax, g.y);
        }

        if count > 0 {
            // We want label points that are at least a moderate distance from
            // the edge of the feature. The threshold for what is too close
            // is derived from the area of the feature.

            let radius = (best_area as f64 / std::f64::consts::PI).sqrt();
            let goodness_threshold = radius / 5.0;

            // First choice: Turf's center of mass.

            // long long / size_t is an unsigned division in the C++
            let centroid = Draw::new(
                VT_MOVETO,
                bitfield40(((xsum as u64) / (count as u64)) as i64),
                bitfield40(((ysum as u64) / (count as u64)) as i64),
            );
            let mut d = center_of_mass(geom, start, end, centroid);
            let mut goodness = label_goodness(geom, d.x, d.y);
            let mut kind = "mass";

            if goodness < goodness_threshold {
                // Label is too close to the border or outside it,
                // so try some other possible points. Sort the vertices
                // both by Y and X coordinate and then by diagonals,
                // and walk through each set
                // in sorted order. Adjacent pairs of coordinates should
                // tend to bounce back and forth between rings, so the
                // midpoint of each pair will hopefully be somewhere in the
                // interior of the polygon.

                let mut candidates: Vec<Candidate> = Vec::new();

                for pass in 0..4 {
                    points.sort_by(|a, b| sorty_sorter(pass, a, b));

                    for i in 1..points.len() {
                        let dx = (points[i].x - points[i - 1].x) as f64;
                        let dy = (points[i].y - points[i - 1].y) as f64;

                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist > 2.0 * goodness_threshold {
                            candidates.push(Candidate {
                                x: (points[i].x + points[i - 1].x) / 2,
                                y: (points[i].y + points[i - 1].y) / 2,
                                dist,
                            });
                        }
                    }
                }

                // Now sort the accumulate list of segment midpoints by the lengths
                // of the segments. Starting from the longest
                // segment, if we find one whose midpoint is inside the polygon and
                // far enough from any edge to be good enough, stop looking.

                candidates.sort_by(candidate_cmp);
                // only check the top 50 stride midpoints, since this list can be quite large
                let mut i = 0;
                while i < candidates.len() && i < 50 {
                    let maybe_goodness = label_goodness(geom, candidates[i].x, candidates[i].y);

                    if maybe_goodness > goodness {
                        d.x = candidates[i].x;
                        d.y = candidates[i].y;

                        goodness = maybe_goodness;
                        kind = "diagonal";
                        if goodness > goodness_threshold {
                            break;
                        }
                    }
                    i += 1;
                }
            }

            // We may still not have anything decent, so the next thing to look at
            // is points from gridding the bounding box of the largest ring.

            if goodness < goodness_threshold {
                let mut sub: i64 = 2;
                while sub < 32 && (xmax - xmin) > 2 * sub && (ymax - ymin) > 2 * sub {
                    for x in 1..sub {
                        for y in 1..sub {
                            let maybe = Draw::new(
                                VT_MOVETO,
                                xmin + x * (xmax - xmin) / sub,
                                ymin + y * (ymax - ymin) / sub,
                            );

                            let maybe_goodness = label_goodness(geom, maybe.x, maybe.y);
                            if maybe_goodness > goodness {
                                // better than the previous
                                d = maybe;
                                goodness = maybe_goodness;
                                kind = "grid";
                            }
                        }
                    }

                    if goodness > goodness_threshold {
                        break;
                    }
                    sub *= 2;
                }

                // There is nothing really good. Is the centroid maybe better?
                // If not, we're stuck with whatever the best we found was.
                let maybe_goodness = label_goodness(geom, centroid.x, centroid.y);
                if maybe_goodness > goodness {
                    d = centroid;
                    goodness = maybe_goodness;
                    kind = "centroid";
                }

                if goodness <= 0.0 {
                    let (lon, lat) = tile2lonlat(d.x, d.y, 32);

                    static WARNED: AtomicI64 = AtomicI64::new(0);
                    if WARNED.fetch_add(1, AtomicOrdering::Relaxed) < 10 {
                        eprintln!(
                            "could not find good label point: {} {:.6},{:.6}",
                            kind, lat, lon
                        );
                    }
                }
            }

            let dv = vec![d];
            return dv;
        }
    }

    DrawVec::new()
}

pub fn checkerboard_anchors(geom: &DrawVec, tx: i32, ty: i32, z: i32, label_point: u64) -> DrawVec {
    let mut out = DrawVec::new();

    // anchor point in world coordinates
    let (wx, wy) = decode_index(label_point);
    let wx = wx as i64;
    let wy = wy as i64;

    // upper left of tile in world coordinates
    let mut tx1: i64 = 0;
    let mut ty1: i64 = 0;
    // lower right of tile in world coordinates;
    let mut tx2: i64 = 1i64 << 32; // , ty2 = 1LL << 32;
    if z != 0 {
        tx1 = (tx as i64) << (32 - z);
        ty1 = (ty as i64) << (32 - z);

        tx2 = ((tx + 1) as i64) << (32 - z);
        // ty2 = (long long) (ty + 1) << (32 - z);
    }

    // upper left of feature in world coordinates
    let mut bx1 = i64::MAX;
    let mut by1 = i64::MAX;
    // lower right of feature in world coordinates;
    let mut bx2 = i64::MIN;
    let mut by2 = i64::MIN;

    for g in geom {
        bx1 = std::cmp::min(bx1, g.x + tx1);
        by1 = std::cmp::min(by1, g.y + ty1);

        bx2 = std::cmp::max(bx2, g.x + tx1);
        by2 = std::cmp::max(by2, g.y + ty1);
    }

    if bx1 > bx2 || by1 > by2 {
        return out;
    }

    // labels repeat every 0.3 tiles at z0
    let mut spiral_dist = 0.3;
    if z > 0 {
        // only every ~6 tiles by the time we get to z15
        spiral_dist *= ((z as f64).ln() * 1.2).exp();
    }

    let label_spacing = (spiral_dist * (tx2 - tx1) as f64) as i64;

    // These divide long longs, so floor() and ceil() see the truncated quotient
    let x1 = ((std::cmp::min(bx1 - wx, bx2 - wx) / label_spacing) as f64).floor() as i64;
    let x2 = ((std::cmp::max(bx1 - wx, bx2 - wx) / label_spacing) as f64).ceil() as i64;

    let y1 = ((std::cmp::min(by1 - wy, by2 - wy) / label_spacing) as f64 - 0.5).floor() as i64;
    let y2 = ((std::cmp::max(by1 - wy, by2 - wy) / label_spacing) as f64).ceil() as i64;

    for lx in x1..=x2 {
        for ly in y1..=y2 {
            let x = lx * label_spacing + wx;
            let mut y = ly * label_spacing + wy;

            if (lx as u64 & 1) == 1 {
                y += label_spacing / 2;
            }

            if x < bx1 || x > bx2 || y < by1 || y > by2 {
                continue;
            }

            // If it's the central label, it's the best we've got,
            // so accept it in any case. If it's from the outer spiral,
            // don't use it if it's too close to a border.
            if lx == 0 && ly == 0 {
                out.push(Draw::new(VT_MOVETO, x - tx1, y - ty1));
                break;
            } else {
                let tilesize = (1i64 << (32 - z)) as f64;
                let goodness_threshold = tilesize / 100.0;
                if label_goodness(geom, x - tx1, y - ty1) > goodness_threshold {
                    out.push(Draw::new(VT_MOVETO, x - tx1, y - ty1));
                    break;
                }
            }
        }
    }

    out
}

/// Port of the parts of mapbox/geometry/snap_rounding.hpp that `check_polygon`
/// uses: `snap_round` for `multi_polygon<long long>`. Nothing else in
/// tippecanoe includes that header.
mod snap_rounding {
    use std::collections::BTreeSet;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Point {
        pub x: i64,
        pub y: i64,
    }

    pub type LinearRing = Vec<Point>;
    pub type Polygon = Vec<LinearRing>;
    pub type MultiPolygon = Vec<Polygon>;

    /// `T / T` for `long long`. Division by zero is undefined in the C++, and
    /// gives 0 on arm64, so do the same instead of panicking.
    fn div(a: i64, b: i64) -> i64 {
        if b == 0 { 0 } else { a.wrapping_div(b) }
    }

    fn add_vertical(
        intermediate: usize,
        which_end: usize,
        into: usize,
        segments: &mut Vec<Vec<Point>>,
        again: &mut bool,
        nexts: &mut Vec<usize>,
    ) {
        *again = true;
        let dv = vec![segments[intermediate][which_end], segments[into][1]];
        segments.push(dv);
        segments[into][1] = segments[intermediate][which_end];
        nexts.push(nexts[into]);
        nexts[into] = nexts.len() - 1;
    }

    fn add_horizontal(
        intermediate: usize,
        which_end: usize,
        into: usize,
        segments: &mut Vec<Vec<Point>>,
        again: &mut bool,
        nexts: &mut Vec<usize>,
    ) {
        *again = true;

        let s = &segments[intermediate];
        let x = s[which_end].x;
        let y = s[0].y.wrapping_add(div(
            (s[which_end].x.wrapping_sub(s[0].x)).wrapping_mul(s[1].y.wrapping_sub(s[0].y)),
            s[1].x.wrapping_sub(s[0].x),
        ));
        let d = Point { x, y };

        let dv = vec![d, segments[into][1]];
        segments.push(dv);
        segments[into][1] = d;
        nexts.push(nexts[into]);
        nexts[into] = nexts.len() - 1;
    }

    fn warn(segments: &[Vec<Point>], a: usize, b: usize, do_warn: bool) {
        if do_warn {
            eprintln!(
                "{},{} to {},{} intersects {},{} to {},{}",
                segments[a][0].x,
                segments[a][0].y,
                segments[a][1].x,
                segments[a][1].y,
                segments[b][0].x,
                segments[b][0].y,
                segments[b][1].x,
                segments[b][1].y
            );
        }
    }

    fn check_intersection(
        segments: &mut Vec<Vec<Point>>,
        a: usize,
        b: usize,
        again: &mut bool,
        nexts: &mut Vec<usize>,
        do_warn: bool,
        endpoint_ok: bool,
    ) {
        let s10_x = segments[a][1].x.wrapping_sub(segments[a][0].x);
        let s10_y = segments[a][1].y.wrapping_sub(segments[a][0].y);
        let s32_x = segments[b][1].x.wrapping_sub(segments[b][0].x);
        let s32_y = segments[b][1].y.wrapping_sub(segments[b][0].y);

        // http://stackoverflow.com/questions/563198/how-do-you-detect-where-two-line-segments-intersect
        let denom = s10_x
            .wrapping_mul(s32_y)
            .wrapping_sub(s32_x.wrapping_mul(s10_y));

        if denom == 0 {
            // They are parallel or collinear. Find out if they are collinear.
            // http://www.cpsc.ucalgary.ca/~marina/papers/Segment_intersection.ps

            let (a0, a1, b0) = (segments[a][0], segments[a][1], segments[b][0]);
            let ccw =
                a0.x.wrapping_mul(a1.y)
                    .wrapping_add(a1.x.wrapping_mul(b0.y))
                    .wrapping_add(b0.x.wrapping_mul(a0.y))
                    .wrapping_sub(a0.x.wrapping_mul(b0.y))
                    .wrapping_sub(a1.x.wrapping_mul(a0.y))
                    .wrapping_sub(b0.x.wrapping_mul(a1.y));

            if ccw == 0 {
                if segments[a][0].x == segments[a][1].x {
                    // Vertical

                    let (amin, amax, bmin, bmax);
                    if segments[a][0].y < segments[a][1].y {
                        amin = segments[a][0].y;
                        amax = segments[a][1].y;
                    } else {
                        amin = segments[a][1].y;
                        amax = segments[a][0].y;
                    }
                    if segments[b][0].y < segments[b][1].y {
                        bmin = segments[b][0].y;
                        bmax = segments[b][1].y;
                    } else {
                        bmin = segments[b][1].y;
                        bmax = segments[b][0].y;
                    }

                    // All of these transformations preserve verticality so we can check multiple cases
                    if segments[b][0].y > amin && segments[b][0].y < amax {
                        // B0 is in A
                        warn(segments, a, b, do_warn);
                        add_vertical(b, 0, a, segments, again, nexts);
                    }
                    if segments[b][1].y > amin && segments[b][1].y < amax {
                        // B1 is in A
                        warn(segments, a, b, do_warn);
                        add_vertical(b, 1, a, segments, again, nexts);
                    }
                    if segments[a][0].y > bmin && segments[a][0].y < bmax {
                        // A0 is in B
                        warn(segments, a, b, do_warn);
                        add_vertical(a, 0, b, segments, again, nexts);
                    }
                    if segments[a][1].y > bmin && segments[a][1].y < bmax {
                        // A1 is in B
                        warn(segments, a, b, do_warn);
                        add_vertical(a, 1, b, segments, again, nexts);
                    }
                } else {
                    // Horizontal or diagonal

                    let (amin, amax, bmin, bmax);
                    if segments[a][0].x < segments[a][1].x {
                        amin = segments[a][0].x;
                        amax = segments[a][1].x;
                    } else {
                        amin = segments[a][1].x;
                        amax = segments[a][0].x;
                    }
                    if segments[b][0].x < segments[b][1].x {
                        bmin = segments[b][0].x;
                        bmax = segments[b][1].x;
                    } else {
                        bmin = segments[b][1].x;
                        bmax = segments[b][0].x;
                    }

                    // Don't check multiples, because rounding may corrupt collinearity
                    if segments[b][0].x > amin && segments[b][0].x < amax {
                        // B0 is in A
                        add_horizontal(b, 0, a, segments, again, nexts);
                        warn(segments, a, b, do_warn);
                    } else if segments[b][1].x > amin && segments[b][1].x < amax {
                        // B1 is in A
                        add_horizontal(b, 1, a, segments, again, nexts);
                        warn(segments, a, b, do_warn);
                    } else if segments[a][0].x > bmin && segments[a][0].x < bmax {
                        // A0 is in B
                        warn(segments, a, b, do_warn);
                        add_horizontal(a, 0, b, segments, again, nexts);
                    } else if segments[a][1].x > bmin && segments[a][1].x < bmax {
                        // A1 is in B
                        warn(segments, a, b, do_warn);
                        add_horizontal(a, 1, b, segments, again, nexts);
                    }
                }
            }
        } else {
            // Neither parallel nor collinear, so may intersect at a single point

            let s02_x = segments[a][0].x.wrapping_sub(segments[b][0].x);
            let s02_y = segments[a][0].y.wrapping_sub(segments[b][0].y);

            // long double is the same as double on arm64 macOS
            let s = (s10_x
                .wrapping_mul(s02_y)
                .wrapping_sub(s10_y.wrapping_mul(s02_x))) as f64
                / denom as f64;
            let t = (s32_x
                .wrapping_mul(s02_y)
                .wrapping_sub(s32_y.wrapping_mul(s02_x))) as f64
                / denom as f64;

            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&s) {
                let x = (segments[a][0].x as f64 + t * s10_x as f64).round() as i64;
                let y = (segments[a][0].y as f64 + t * s10_y as f64).round() as i64;

                if (t > 0.0 && t < 1.0 && s > 0.0 && s < 1.0) || !endpoint_ok {
                    if (0.0..=1.0).contains(&t)
                        && (x != segments[a][0].x || y != segments[a][0].y)
                        && (x != segments[a][1].x || y != segments[a][1].y)
                    {
                        warn(segments, a, b, do_warn);
                        // splitting a
                        let dv = vec![Point { x, y }, segments[a][1]];
                        segments.push(dv);
                        segments[a][1] = Point { x, y };
                        nexts.push(nexts[a]);
                        nexts[a] = nexts.len() - 1;
                        *again = true;
                    }

                    if (0.0..=1.0).contains(&s)
                        && (x != segments[b][0].x || y != segments[b][0].y)
                        && (x != segments[b][1].x || y != segments[b][1].y)
                    {
                        // splitting b
                        warn(segments, a, b, do_warn);
                        let dv = vec![Point { x, y }, segments[b][1]];
                        segments.push(dv);
                        segments[b][1] = Point { x, y };
                        nexts.push(nexts[b]);
                        nexts[b] = nexts.len() - 1;
                        *again = true;
                    }
                }
            }
        }
    }

    fn partition(
        segs: &[Vec<Point>],
        subset: &[usize],
        direction: i32,
        possible: &mut BTreeSet<(usize, usize)>,
    ) {
        let mut points: Vec<i64> = Vec::new();

        // List of X or Y midpoints of edges, so we can find the median

        if direction == 0 {
            for &s in subset {
                points.push(segs[s][0].x.wrapping_add(segs[s][1].x) / 2);
            }
        } else {
            for &s in subset {
                points.push(segs[s][0].y.wrapping_add(segs[s][1].y) / 2);
            }
        }

        if points.is_empty() {
            return;
        }

        let mid = points.len() / 2;
        let median = *points.select_nth_unstable(mid).1;

        // Partition into sets that are above or below, or to the left or to the right of, the median.
        // Segments that cross the median appear in both.

        let mut one: Vec<usize> = Vec::new();
        let mut two: Vec<usize> = Vec::new();

        if direction == 0 {
            for &s in subset {
                if segs[s][0].x <= median || segs[s][1].x <= median {
                    one.push(s);
                }
                if segs[s][0].x >= median || segs[s][1].x >= median {
                    two.push(s);
                }
            }
        } else {
            for &s in subset {
                if segs[s][0].y <= median || segs[s][1].y <= median {
                    one.push(s);
                }
                if segs[s][0].y >= median || segs[s][1].y >= median {
                    two.push(s);
                }
            }
        }

        if one.len() >= subset.len() || two.len() >= subset.len() {
            for i in 0..subset.len() {
                for j in i + 1..subset.len() {
                    possible.insert((subset[i], subset[j]));
                }
            }
        } else {
            // By experiment, stopping at 10 is a little faster than either 5 or 20

            if one.len() < 10 {
                for i in 0..one.len() {
                    for j in i + 1..one.len() {
                        possible.insert((one[i], one[j]));
                    }
                }
            } else {
                partition(segs, &one, (direction == 0) as i32, possible);
            }

            if two.len() < 10 {
                for i in 0..two.len() {
                    for j in i + 1..two.len() {
                        possible.insert((two[i], two[j]));
                    }
                }
            } else {
                partition(segs, &two, (direction == 0) as i32, possible);
            }
        }
    }

    fn intersect_segments(
        mut segments: Vec<Vec<Point>>,
        nexts: &mut Vec<usize>,
        do_warn: bool,
        endpoint_ok: bool,
    ) -> Vec<Vec<Point>> {
        let mut again = true;

        while again {
            again = false;

            let mut possible: BTreeSet<(usize, usize)> = BTreeSet::new();

            let subset: Vec<usize> = (0..segments.len()).collect();

            partition(&segments, &subset, 0, &mut possible);

            for &(first, second) in &possible {
                check_intersection(
                    &mut segments,
                    first,
                    second,
                    &mut again,
                    nexts,
                    do_warn,
                    endpoint_ok,
                );
            }
        }

        segments
    }

    pub fn snap_round(mut geom: MultiPolygon, do_warn: bool, endpoint_ok: bool) -> MultiPolygon {
        let mut segments: Vec<Vec<Point>> = Vec::new();
        let mut nexts: Vec<usize> = Vec::new();
        let mut ring_starts: Vec<Vec<usize>> = Vec::new();

        // Crunch out any 0-length segments
        for polygon in geom.iter_mut() {
            for ring in polygon.iter_mut() {
                let mut k = ring.len() as isize - 1;
                while k > 0 {
                    let ku = k as usize;
                    if ring[ku] == ring[ku - 1] {
                        ring.remove(ku);
                    }
                    k -= 1;
                }
            }
        }

        for (i, polygon) in geom.iter().enumerate() {
            ring_starts.push(Vec::new());

            for ring in polygon {
                let s = ring.len();

                if s > 1 {
                    ring_starts[i].push(segments.len());
                    let first = nexts.len();

                    for k in 0..s - 1 {
                        segments.push(vec![ring[k], ring[k + 1]]);
                        nexts.push(nexts.len() + 1);
                    }

                    // Fabricate a point if ring was not closed
                    if ring[0] != ring[s - 1] {
                        segments.push(vec![ring[s - 1], ring[0]]);
                        nexts.push(nexts.len() + 1);
                    }

                    // Last point of ring points back to first
                    let last = nexts.len() - 1;
                    nexts[last] = first;
                }
            }
        }

        segments = intersect_segments(segments, &mut nexts, do_warn, endpoint_ok);

        let mut mp: MultiPolygon = Vec::new();
        for (i, starts) in ring_starts.iter().enumerate() {
            mp.push(Vec::new());

            for (j, &start) in starts.iter().enumerate() {
                mp[i].push(Vec::new());

                let mut k = start;
                loop {
                    mp[i][j].push(segments[k][0]);
                    k = nexts[k];
                    if k == start {
                        break;
                    }
                }

                mp[i][j].push(segments[start][0]);
            }
        }

        mp
    }
}

/// Helpers shared by the geometry tests of several modules.
#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    /// Parses `op x y` triples into a drawvec.
    pub fn dv(s: &str) -> DrawVec {
        let v: Vec<i64> = s.split_whitespace().map(|t| t.parse().unwrap()).collect();
        v.chunks(3)
            .map(|c| Draw::new(c[0] as i8, c[1], c[2]))
            .collect()
    }

    /// Formats a drawvec as `op x y necessary` quadruples, the way the C++
    /// harness that produced the expected values prints them.
    pub fn show(dv: &DrawVec) -> String {
        dv.iter()
            .map(|d| format!("{} {} {} {}", d.op, d.x, d.y, d.necessary))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// The expected values in these tests come from the C++ functions, built with
// -ffp-contract=off and run on the same input.
#[cfg(test)]
mod tests {
    use super::test_util::{dv, show};
    use super::*;

    #[test]
    fn decode_geometry_reads_deltas() {
        let bytes: Vec<u8> = vec![
            0x01, 0x02, 0x04, 0x02, 0x01, 0x06, 0x06, 0x02, 0x03, 0x05, 0x01, 0x02, 0x03, 0x04,
            0x07, 0x00,
        ];
        let mut meta: &[u8] = &bytes;
        let mut bbox = [0i64; 4];
        let out = decode_geometry(&mut meta, 3, 2, 5, &mut bbox, 1000, 2000);
        assert_eq!(
            show(&out),
            "1 -1073740823 -2684352558 0 2 -1073740824 -2684352555 0 6 0 0 0 2 -1073740826 -2684352558 0 1 -1073740825 -2684352560 0 4 0 0 0 7 0 0 0"
        );
        assert_eq!(bbox, [-1073740826, -2684352560, -1073740823, -2684352555]);
        assert_eq!(bytes.len() - meta.len(), 16);
    }

    #[test]
    fn quick_check_classifies_bboxes() {
        assert_eq!(quick_check(&[10, 10, 1000, 1000], 22, 5), 1);
        assert_eq!(quick_check(&[-10, 10, 1000, 1000], 22, 5), 3);
        assert_eq!(quick_check(&[-10, 10, 1000, 1100], 22, 5), 2);
        assert_eq!(quick_check(&[2000, 10, 3000, 1000], 22, 5), 0);
        assert!(point_within_tile(0, 1023, 22));
        assert!(!point_within_tile(0, 1024, 22));
    }

    #[test]
    fn simplify_lines_douglas_peucker() {
        let mut g =
            dv("1 0 0 2 1000 300 2 2000 -200 2 3000 5000 2 4000 4900 2 5000 0 2 6000 100 2 7000 0");
        let out = simplify_lines(
            &mut g,
            10,
            0,
            0,
            12,
            true,
            1.0,
            0,
            &DrawVec::new(),
            &[],
            0,
            &[],
        );
        assert_eq!(
            show(&out),
            "1 0 0 1 2 1600 0 1 2 2038 0 1 2 3000 5000 1 2 5000 0 1 2 7000 0 1"
        );
        assert_eq!(show(&g), show(&out));
    }

    #[test]
    fn reorder_lines_reverses_up_and_left() {
        let g = dv("1 900 900 2 500 600 2 300 100 2 10 20");
        assert_eq!(
            show(&reorder_lines(&g)),
            "1 10 20 0 2 300 100 0 2 500 600 0 2 900 900 0"
        );
    }

    #[test]
    fn stairstep_follows_pixels() {
        let mut g = dv("1 0 0 2 5000 3000 2 2000 9000 2 0 0");
        let out = stairstep(&mut g, 10, 12);
        assert_eq!(show(&g), "1 0 0 0 2 5 3 0 2 2 9 0 2 0 0 0");
        assert_eq!(
            show(&out),
            "1 0 0 0 2 0 0 0 2 0 1024 0 2 2048 1024 0 2 2048 2048 0 2 4096 2048 0 2 4096 3072 0 2 5120 3072 0 2 5120 3072 0 2 5120 4096 0 2 4096 4096 0 2 4096 6144 0 2 3072 6144 0 2 3072 8192 0 2 2048 8192 0 2 2048 9216 0 2 2048 9216 0 2 2048 7168 0 2 1024 7168 0 2 1024 3072 0 2 0 3072 0 2 0 0 0"
        );
    }

    #[test]
    fn center_of_mass_of_two_squares() {
        let g =
            dv("1 0 0 2 100 0 2 100 100 2 0 100 2 0 0 1 200 0 2 400 0 2 400 400 2 200 400 2 200 0");
        assert_eq!(show(&vec![center_of_mass_mp(&g)]), "1 272 183 0");
    }

    #[test]
    fn polygon_to_anchor_avoids_the_hollow_of_a_u() {
        // The center of mass of this U shape is in its hollow, so the label
        // point comes from one of the fallbacks.
        let g = dv(
            "1 0 0 2 3000 0 2 3000 3000 2 2000 3000 2 2000 1000 2 1000 1000 2 1000 3000 2 0 3000 2 0 0",
        );
        assert_eq!(show(&polygon_to_anchor(&g)), "1 2500 1500 0");
    }

    #[test]
    fn checkerboard_anchors_in_a_tile() {
        let g = dv("1 0 0 2 536870912 0 2 536870912 536870912 2 0 536870912 2 0 0");
        assert_eq!(
            show(&checkerboard_anchors(&g, 2, 3, 3, 13835058055282163712)),
            "1 471824789 235912394 0"
        );
    }

    #[test]
    fn bitfield40_wraps_like_the_cpp_bitfield() {
        assert_eq!(bitfield40(5), 5);
        assert_eq!(bitfield40(-5), -5);
        assert_eq!(bitfield40(1 << 39), -(1 << 39));
        assert_eq!(bitfield40(66851165 * 65536), -16888561664);
    }
}
