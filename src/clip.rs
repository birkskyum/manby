//! Port of clip.cpp: clipping, polygon cleaning with wagyu, line clipping with
//! Clipper2, simplification, and the overzoom machinery.
//!
//! C++ overloads get distinct names here: the bounding-box forms of
//! `simple_clip_poly`, `clip_point` and `clip_lines` end in `_bbox`, and the
//! `mvt_geometry` forms of `pnpoly` and `pnpoly_mp` end in `_mvt`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::process::exit;
use std::sync::Arc;

use clipper2::{ClipType, Clipper64, FillRule, Path64, Paths64, Point64};
use wagyu::{
    ClipType as WagyuClipType, FillType, LinearRing, MultiPolygon, Point, PolygonType, Wagyu,
};

use crate::attribute::{AttributeOp, preserve_attribute_mvt};
use crate::errors::{EXIT_FAILURE, EXIT_IMPOSSIBLE, EXIT_MVT, EXIT_PROTOBUF};
use crate::evaluator::evaluate_feature;
use crate::geometry::{
    ClipBbox, Draw, DrawVec, InputTile, SourceTile, VT_CLOSEPATH, VT_LINE, VT_LINETO, VT_MOVETO,
    VT_POINT, VT_POLYGON,
};
use crate::jsonpull::JsonRef;
use crate::mvt::{
    MVT_BOOL, MVT_CLOSEPATH, MVT_LINETO, MVT_MOVETO, MvtFeature, MvtGeometry, MvtLayer, MvtTile,
    MvtValue, compress, mvt_value_to_long_long,
};
use crate::serial::KeyPool;
use crate::text::starts_with;

/// `simple_clip_poly(geom, minx, miny, maxx, maxy, ax, ay, bx, by, edge_nodes, prevent_simplify_shared_nodes)`.
#[allow(clippy::too_many_arguments)]
pub fn simple_clip_poly_bbox_edges(
    geom: &mut DrawVec,
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    edge_nodes: &mut DrawVec,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    let mut out = DrawVec::new();
    if prevent_simplify_shared_nodes {
        *geom = remove_noop(std::mem::take(geom), VT_POLYGON, 0);
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

            let mut tmp: Vec<(f64, f64)> = Vec::new();
            for g in &geom[i..j] {
                let x = g.x as f64;
                let y = g.y as f64;
                tmp.push((x, y));
            }
            tmp = clip_poly1(
                &tmp,
                minx,
                miny,
                maxx,
                maxy,
                ax,
                ay,
                bx,
                by,
                edge_nodes,
                prevent_simplify_shared_nodes,
            );
            if !tmp.is_empty()
                && (tmp[0].0 != tmp[tmp.len() - 1].0 || tmp[0].1 != tmp[tmp.len() - 1].1)
            {
                eprintln!("Internal error: Polygon ring not closed");
                exit(EXIT_FAILURE);
            }
            for (k, p) in tmp.iter().enumerate() {
                if k == 0 {
                    out.push(Draw::new(VT_MOVETO, p.0.round() as i64, p.1.round() as i64));
                } else {
                    out.push(Draw::new(VT_LINETO, p.0.round() as i64, p.1.round() as i64));
                }
            }

            i = j - 1;
        } else {
            eprintln!("Unexpected operation in polygon {}", geom[i].op as i32);
            exit(EXIT_IMPOSSIBLE);
        }
        i += 1;
    }

    out
}

/// `simple_clip_poly(geom, minx, miny, maxx, maxy, prevent_simplify_shared_nodes)`.
pub fn simple_clip_poly_bbox(
    geom: &mut DrawVec,
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    let mut dv = DrawVec::new();
    simple_clip_poly_bbox_edges(
        geom,
        minx,
        miny,
        maxx,
        maxy,
        minx,
        miny,
        maxx,
        maxy,
        &mut dv,
        prevent_simplify_shared_nodes,
    )
}

/// `simple_clip_poly(geom, z, buffer, edge_nodes, prevent_simplify_shared_nodes)`.
pub fn simple_clip_poly(
    geom: &mut DrawVec,
    z: i32,
    buffer: i32,
    edge_nodes: &mut DrawVec,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    let area: i64 = 1i64 << (32 - z);
    let clip_buffer: i64 = buffer as i64 * area / 256;

    simple_clip_poly_bbox_edges(
        geom,
        -clip_buffer,
        -clip_buffer,
        area + clip_buffer,
        area + clip_buffer,
        0,
        0,
        area,
        area,
        edge_nodes,
        prevent_simplify_shared_nodes,
    )
}

pub fn clip_point(geom: &mut DrawVec, z: i32, buffer: i64) -> DrawVec {
    let mut min: i64 = 0;
    let mut area: i64 = 1i64 << (32 - z);

    min -= buffer * area / 256;
    area += buffer * area / 256;

    clip_point_bbox(geom, min, min, area, area)
}

pub fn clip_point_bbox(geom: &mut DrawVec, minx: i64, miny: i64, maxx: i64, maxy: i64) -> DrawVec {
    let mut out = DrawVec::new();

    for g in geom.iter() {
        if g.x >= minx && g.y >= miny && g.x <= maxx && g.y <= maxy {
            out.push(*g);
        }
    }

    out
}

pub fn clip_lines(geom: &mut DrawVec, z: i32, buffer: i64) -> DrawVec {
    let mut min: i64 = 0;
    let mut area: i64 = 1i64 << (32 - z);
    min -= buffer * area / 256;
    area += buffer * area / 256;

    clip_lines_bbox(geom, min, min, area, area)
}

pub fn clip_lines_bbox(geom: &mut DrawVec, minx: i64, miny: i64, maxx: i64, maxy: i64) -> DrawVec {
    let mut out = DrawVec::new();

    for i in 0..geom.len() {
        if i > 0
            && (geom[i - 1].op == VT_MOVETO || geom[i - 1].op == VT_LINETO)
            && geom[i].op == VT_LINETO
        {
            let mut x1 = geom[i - 1].x;
            let mut y1 = geom[i - 1].y;

            let mut x2 = geom[i].x;
            let mut y2 = geom[i].y;

            let c = clip(&mut x1, &mut y1, &mut x2, &mut y2, minx, miny, maxx, maxy);

            if c > 1 {
                // clipped
                out.push(Draw::new(VT_MOVETO, x1, y1));
                out.push(Draw::new(VT_LINETO, x2, y2));
                out.push(Draw::new(VT_MOVETO, geom[i].x, geom[i].y));
            } else if c == 1 {
                // unchanged
                out.push(geom[i]);
            } else {
                // clipped away entirely
                out.push(Draw::new(VT_MOVETO, geom[i].x, geom[i].y));
            }
        } else {
            out.push(geom[i]);
        }
    }

    out
}

const INSIDE: i32 = 0;
const LEFT: i32 = 1;
const RIGHT: i32 = 2;
const BOTTOM: i32 = 4;
const TOP: i32 = 8;

fn compute_out_code(x: i64, y: i64, xmin: i64, ymin: i64, xmax: i64, ymax: i64) -> i32 {
    let mut code = INSIDE;

    if x < xmin {
        code |= LEFT;
    } else if x > xmax {
        code |= RIGHT;
    }

    if y < ymin {
        code |= BOTTOM;
    } else if y > ymax {
        code |= TOP;
    }

    code
}

/// Clips the segment to the box. Returns 0 if it is outside, 1 if unchanged, 2 if clipped.
#[allow(clippy::too_many_arguments)]
pub fn clip(
    x0: &mut i64,
    y0: &mut i64,
    x1: &mut i64,
    y1: &mut i64,
    xmin: i64,
    ymin: i64,
    xmax: i64,
    ymax: i64,
) -> i32 {
    let mut outcode0 = compute_out_code(*x0, *y0, xmin, ymin, xmax, ymax);
    let mut outcode1 = compute_out_code(*x1, *y1, xmin, ymin, xmax, ymax);
    let mut accept = 0;
    let mut changed = 0;

    loop {
        if (outcode0 | outcode1) == 0 {
            // Bitwise OR is 0. Trivially accept and get out of loop
            accept = 1;
            break;
        } else if (outcode0 & outcode1) != 0 {
            // Bitwise AND is not 0. Trivially reject and get out of loop
            break;
        } else {
            // failed both tests, so calculate the line segment to clip
            // from an outside point to an intersection with clip edge
            let mut x = *x0;
            let mut y = *y0;

            // At least one endpoint is outside the clip rectangle; pick it.
            let outcode_out = if outcode0 != 0 { outcode0 } else { outcode1 };

            // XXX truncating division

            // The products can overflow for very long segments, which the
            // C++ leaves to wrap around, so they wrap here too.

            // Now find the intersection point;
            // use formulas y = y0 + slope * (x - x0), x = x0 + (1 / slope) * (y - y0)
            if outcode_out & TOP != 0 {
                // point is above the clip rectangle
                x = x0.wrapping_add((*x1 - *x0).wrapping_mul(ymax - *y0).wrapping_div(*y1 - *y0));
                y = ymax;
            } else if outcode_out & BOTTOM != 0 {
                // point is below the clip rectangle
                x = x0.wrapping_add((*x1 - *x0).wrapping_mul(ymin - *y0).wrapping_div(*y1 - *y0));
                y = ymin;
            } else if outcode_out & RIGHT != 0 {
                // point is to the right of clip rectangle
                y = y0.wrapping_add((*y1 - *y0).wrapping_mul(xmax - *x0).wrapping_div(*x1 - *x0));
                x = xmax;
            } else if outcode_out & LEFT != 0 {
                // point is to the left of clip rectangle
                y = y0.wrapping_add((*y1 - *y0).wrapping_mul(xmin - *x0).wrapping_div(*x1 - *x0));
                x = xmin;
            }

            // Now we move outside point to intersection point to clip
            // and get ready for next pass.
            if outcode_out == outcode0 {
                *x0 = x;
                *y0 = y;
                outcode0 = compute_out_code(*x0, *y0, xmin, ymin, xmax, ymax);
                changed = 1;
            } else {
                *x1 = x;
                *y1 = y;
                outcode1 = compute_out_code(*x1, *y1, xmin, ymin, xmax, ymax);
                changed = 1;
            }
        }
    }

    if accept == 0 { 0 } else { changed + 1 }
}

fn decode_clipped(t: &MultiPolygon, out: &mut DrawVec, scale: f64) {
    out.clear();

    for polygon in t {
        for (j, lr) in polygon.iter().enumerate() {
            let mut ring = DrawVec::new();

            for (k, p) in lr.iter().enumerate() {
                ring.push(Draw::new(
                    if k == 0 { VT_MOVETO } else { VT_LINETO },
                    (p.x as f64 / scale).round() as i64,
                    (p.y as f64 / scale).round() as i64,
                ));
            }

            if !ring.is_empty() && ring[ring.len() - 1] != ring[0] {
                eprintln!("Had to close ring");
                ring.push(Draw::new(VT_LINETO, ring[0].x, ring[0].y));
            }

            let area = get_area(&ring, 0, ring.len());

            if (j == 0 && area < 0.0) || (j != 0 && area > 0.0) {
                eprintln!("Ring area has wrong sign: {:.6} for {}", area, j);
                exit(EXIT_IMPOSSIBLE);
            }

            out.extend_from_slice(&ring);
        }
    }
}

pub fn clean_or_clip_poly(
    geom: &mut DrawVec,
    z: i32,
    buffer: i32,
    clip: bool,
    try_scaling: bool,
) -> DrawVec {
    *geom = remove_noop(std::mem::take(geom), VT_POLYGON, 0);
    let mut result: MultiPolygon = MultiPolygon::new();

    let mut scale = 16.0;
    if !try_scaling {
        scale = 1.0;
    }

    let mut again = true;
    while again {
        let mut wagyu = Wagyu::new();
        again = false;

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
                    let mut lr = LinearRing::new();

                    for g in &geom[i..j] {
                        lr.push(Point::new(
                            (g.x as f64 * scale) as i64,
                            (g.y as f64 * scale) as i64,
                        ));
                    }

                    if lr.len() >= 3 {
                        wagyu.add_ring(&lr, PolygonType::Subject);
                    }
                }

                i = j - 1;
            }
            i += 1;
        }

        if clip {
            let mut area: i64 = 0xFFFFFFFF;
            if z != 0 {
                area = 1i64 << (32 - z);
            }
            let clip_buffer: i64 = buffer as i64 * area / 256;

            let lr: LinearRing = vec![
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (area + clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (area + clip_buffer) as f64) as i64,
                    (scale * (area + clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (area + clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
            ];

            wagyu.add_ring(&lr, PolygonType::Clip);
        }

        result.clear();
        match wagyu.execute(WagyuClipType::Union, FillType::Positive, FillType::Positive) {
            Ok(r) => result = r,
            Err(e) => {
                let mut f = String::new();
                f.push_str(&format!("{}\n", e));
                eprintln!("{}", e);
                f.push('[');

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
                            if i != 0 {
                                f.push(',');
                            }
                            f.push('[');

                            for (k, g) in geom[i..j].iter().enumerate() {
                                if k != 0 {
                                    f.push(',');
                                }
                                f.push_str(&format!("[{},{}]", g.x, g.y));
                            }

                            f.push(']');
                        }

                        i = j - 1;
                    }
                    i += 1;
                }

                f.push(']');
                f.push_str("\n\n\n\n\n");

                if let Ok(mut file) = std::fs::File::create("/tmp/wagyu.log") {
                    let _ = file.write_all(f.as_bytes());
                }
                eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                exit(EXIT_IMPOSSIBLE);
            }
        }

        if scale != 1.0 {
            for outer in &result {
                for ring in outer {
                    for p in ring {
                        if p.x as f64 / scale != (p.x as f64 / scale).round()
                            || p.y as f64 / scale != (p.y as f64 / scale).round()
                        {
                            scale = 1.0;
                            again = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    let mut ret = DrawVec::new();
    decode_clipped(&result, &mut ret, scale);
    ret
}

pub fn clip_poly_poly(geom: &DrawVec, bounds: &DrawVec) -> DrawVec {
    let mut result: MultiPolygon = MultiPolygon::new();

    {
        let mut wagyu = Wagyu::new();

        let mut i = 0;
        while i < geom.len() {
            if geom[i].op == VT_MOVETO {
                let mut lr = LinearRing::new();
                lr.push(Point::new(geom[i].x, geom[i].y));

                let mut j = i + 1;
                while j < geom.len() {
                    if geom[j].op != VT_LINETO {
                        break;
                    }
                    lr.push(Point::new(geom[j].x, geom[j].y));
                    j += 1;
                }

                if lr.len() >= 4 {
                    wagyu.add_ring(&lr, PolygonType::Subject);
                }

                i = j - 1;
            }
            i += 1;
        }

        let mut i = 0;
        while i < bounds.len() {
            if bounds[i].op == VT_MOVETO {
                let mut lr = LinearRing::new();
                lr.push(Point::new(bounds[i].x, bounds[i].y));

                let mut j = i + 1;
                while j < bounds.len() {
                    if bounds[j].op != VT_LINETO {
                        break;
                    }
                    lr.push(Point::new(bounds[j].x, bounds[j].y));
                    j += 1;
                }

                if lr.len() >= 4 {
                    wagyu.add_ring(&lr, PolygonType::Clip);
                }

                i = j - 1;
            }
            i += 1;
        }

        result.clear();
        match wagyu.execute(
            WagyuClipType::Intersection,
            FillType::Positive,
            FillType::Positive,
        ) {
            Ok(r) => result = r,
            Err(_) => {
                eprintln!("Internal error: Polygon clipping failed");
                exit(EXIT_IMPOSSIBLE);
            }
        }
    }

    let mut ret = DrawVec::new();
    decode_clipped(&result, &mut ret, 1.0);
    ret
}

pub fn clip_point_poly(geom: &DrawVec, bounds: &DrawVec) -> DrawVec {
    let mut out = DrawVec::new();
    for p in geom {
        if pnpoly_mp(bounds, p.x, p.y) {
            out.push(*p);
        }
    }
    out
}

fn geom_to_clipper(geom: &DrawVec) -> Paths64 {
    let mut subject = Paths64::new();

    for i in 0..geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut path: Path64 = vec![Point64::new(geom[i].x, geom[i].y)];
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                path.push(Point64::new(geom[j].x, geom[j].y));
                j += 1;
            }
            subject.push(path);
        }
    }

    subject
}

fn clipper_to_geom(geom: &Paths64, out: &mut DrawVec) {
    for ring in geom {
        for (i, p) in ring.iter().enumerate() {
            out.push(Draw::new(
                if i == 0 { VT_MOVETO } else { VT_LINETO },
                p.x,
                p.y,
            ));
        }
    }
}

pub fn clip_lines_poly(geom: &DrawVec, region: &DrawVec) -> DrawVec {
    let subject = geom_to_clipper(geom);
    let clip = geom_to_clipper(region);

    let mut clipper = Clipper64::new();
    clipper.add_open_subject(&subject);
    clipper.add_clip(&clip);

    let mut solution = Paths64::new();
    let mut open_solution = Paths64::new();
    clipper.execute(
        ClipType::Intersection,
        FillRule::Positive,
        &mut solution,
        &mut open_solution,
    );

    let mut out = DrawVec::new();
    clipper_to_geom(&solution, &mut out);
    clipper_to_geom(&open_solution, &mut out);
    out
}

pub fn to_tile_scale(geom: &mut DrawVec, z: i32, detail: i32) {
    if 32 - detail - z < 0 {
        for g in geom.iter_mut() {
            g.x = (g.x as f64 * (1i64 << (-(32 - detail - z))) as f64).round() as i64;
            g.y = (g.y as f64 * (1i64 << (-(32 - detail - z))) as f64).round() as i64;
        }
    } else {
        for g in geom.iter_mut() {
            g.x = (g.x as f64 / (1i64 << (32 - detail - z)) as f64).round() as i64;
            g.y = (g.y as f64 / (1i64 << (32 - detail - z)) as f64).round() as i64;
        }
    }
}

pub fn from_tile_scale(geom: &DrawVec, z: i32, detail: i32) -> DrawVec {
    let mut out = DrawVec::new();
    for g in geom {
        let mut d = *g;
        d.x *= 1i64.wrapping_shl((32 - detail - z) as u32);
        d.y *= 1i64.wrapping_shl((32 - detail - z) as u32);
        out.push(d);
    }
    out
}

pub fn remove_noop(mut geom: DrawVec, type_: i32, shift: i32) -> DrawVec {
    // first pass: remove empty linetos

    let mut ox: i64 = 0;
    let mut oy: i64 = 0;
    let mut out = DrawVec::new();

    for g in &geom {
        let nx = (g.x as f64 / (1i64 << shift) as f64).round() as i64;
        let ny = (g.y as f64 / (1i64 << shift) as f64).round() as i64;

        if g.op == VT_LINETO && nx == ox && ny == oy {
            continue;
        }

        if g.op == VT_CLOSEPATH {
            out.push(*g);
        } else {
            /* moveto or lineto */
            out.push(*g);
            ox = nx;
            oy = ny;
        }
    }

    // second pass: remove unused movetos

    if type_ != VT_POINT {
        geom = std::mem::take(&mut out);

        let mut i = 0;
        while i < geom.len() {
            if geom[i].op == VT_MOVETO {
                if i + 1 >= geom.len() {
                    // followed by end-of-geometry: not needed
                    i += 1;
                    continue;
                }

                if geom[i + 1].op == VT_MOVETO {
                    // followed by another moveto: not needed
                    i += 1;
                    continue;
                }

                if geom[i + 1].op == VT_CLOSEPATH {
                    // followed by closepath: only possible after close_poly()
                    i += 1; // also remove unused closepath
                    i += 1;
                    continue;
                }
            }

            out.push(geom[i]);
            i += 1;
        }
    }

    // second pass: remove empty movetos

    if type_ == VT_LINE {
        geom = std::mem::take(&mut out);

        for i in 0..geom.len() {
            if i > 1
                && geom[i].op == VT_MOVETO
                && geom[i - 1].op == VT_LINETO
                && (geom[i - 1].x as f64 / (1i64 << shift) as f64).round()
                    == (geom[i].x as f64 / (1i64 << shift) as f64).round()
                && (geom[i - 1].y as f64 / (1i64 << shift) as f64).round()
                    == (geom[i].y as f64 / (1i64 << shift) as f64).round()
            {
                continue;
            }

            out.push(geom[i]);
        }
    }

    out
}

pub fn get_area_scaled(geom: &DrawVec, i: usize, j: usize) -> f64 {
    let max_exact_double = ((1i64 << 53) - 1) as f64;

    // keep scaling the geometry down until we can calculate its area without overflow
    let mut scale: i64 = 2;
    while scale < (1i64 << 30) {
        let bx = geom[i].x;
        let by = geom[i].y;
        let mut again = false;

        // https://en.wikipedia.org/wiki/Shoelace_formula
        let mut area: f64 = 0.0;
        for k in i..j {
            let next = geom[i + ((k - i + 1) % (j - i))];
            area += ((geom[k].x - bx) / scale) as f64 * ((next.y - by) / scale) as f64;
            if area.abs() >= max_exact_double {
                again = true;
                break;
            }
            area -= ((geom[k].y - by) / scale) as f64 * ((next.x - bx) / scale) as f64;
            if area.abs() >= max_exact_double {
                again = true;
                break;
            }
        }

        if again {
            scale *= 2;
            continue;
        } else {
            area /= 2.0;
            return area * scale as f64 * scale as f64;
        }
    }

    eprintln!("get_area_scaled: can't happen");
    exit(EXIT_IMPOSSIBLE);
}

pub fn get_area(geom: &DrawVec, i: usize, j: usize) -> f64 {
    let max_exact_double = ((1i64 << 53) - 1) as f64;

    // Coordinates in `geom` are 40-bit integers, so there is no good way
    // to multiply them without possible precision loss. Since they probably
    // do not use the full precision, shift them nearer to the origin so
    // their product is more likely to be exactly representable as a double.
    //
    // (In practice they are actually 34-bit integers: 32 bits for the
    // Mercator world plane, plus another two bits so features can stick
    // off either the left or right side. But that is still too many bits
    // for the product to fit either in a 64-bit long long or in a
    // double where the largest exact integer is 2^53.)
    //
    // If the intermediate calculation still exceeds 2^53, start trying to
    // recalculate the area by scaling down the geometry. This will not
    // produce as precise an area, but it will still be close, and the
    // sign will be correct, which is more important, since the sign
    // determines the winding order of the rings. We can then use that
    // sign with this generally more precise area calculation.

    let bx = geom[i].x;
    let by = geom[i].y;

    // https://en.wikipedia.org/wiki/Shoelace_formula
    let mut area: f64 = 0.0;
    let mut overflow = false;
    for k in i..j {
        let next = geom[i + ((k - i + 1) % (j - i))];
        area += (geom[k].x - bx) as f64 * (next.y - by) as f64;
        if area.abs() >= max_exact_double {
            overflow = true;
        }
        area -= (geom[k].y - by) as f64 * (next.x - bx) as f64;
        if area.abs() >= max_exact_double {
            overflow = true;
        }
    }
    area /= 2.0;

    if overflow {
        let scaled_area = get_area_scaled(geom, i, j);
        if (area < 0.0 && scaled_area > 0.0) || (area > 0.0 && scaled_area < 0.0) {
            area = -area;
        }
    }

    area
}

pub fn get_mp_area(geom: &DrawVec) -> f64 {
    let mut ret = 0.0;

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

            ret += get_area(geom, i, j);
            i = j - 1;
        }
        i += 1;
    }

    ret
}

pub fn close_poly(geom: &mut DrawVec) -> DrawVec {
    let mut out = DrawVec::new();

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

            if j - 1 > i && (geom[j - 1].x != geom[i].x || geom[j - 1].y != geom[i].y) {
                eprintln!("Internal error: polygon not closed");
            }

            for g in &geom[i..j - 1] {
                out.push(*g);
            }
            out.push(Draw::new(VT_CLOSEPATH, 0, 0));

            i = j - 1;
        }
        i += 1;
    }

    out
}

fn inside(d: (f64, f64), edge: i32, minx: i64, miny: i64, maxx: i64, maxy: i64) -> bool {
    match edge {
        0 => {
            // top
            d.1 > miny as f64
        }
        1 => {
            // right
            d.0 < maxx as f64
        }
        2 => {
            // bottom
            d.1 < maxy as f64
        }
        3 => {
            // left
            d.0 > minx as f64
        }
        _ => {
            eprintln!("internal error inside");
            exit(EXIT_FAILURE);
        }
    }
}

fn intersect(
    a: (f64, f64),
    b: (f64, f64),
    edge: i32,
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
) -> (f64, f64) {
    match edge {
        0 => {
            // top
            (
                a.0 + (b.0 - a.0) * (miny as f64 - a.1) / (b.1 - a.1),
                miny as f64,
            )
        }
        1 => {
            // right
            (
                maxx as f64,
                a.1 + (b.1 - a.1) * (maxx as f64 - a.0) / (b.0 - a.0),
            )
        }
        2 => {
            // bottom
            (
                a.0 + (b.0 - a.0) * (maxy as f64 - a.1) / (b.1 - a.1),
                maxy as f64,
            )
        }
        3 => {
            // left
            (
                minx as f64,
                a.1 + (b.1 - a.1) * (minx as f64 - a.0) / (b.0 - a.0),
            )
        }
        _ => {
            eprintln!("internal error intersecting");
            exit(EXIT_FAILURE);
        }
    }
}

/// The new point on a tile edge, as an edge node.
fn edge_node(p: (f64, f64)) -> Draw {
    Draw::new(VT_MOVETO, p.0.round() as i64, p.1.round() as i64)
}

// http://en.wikipedia.org/wiki/Sutherland%E2%80%93Hodgman_algorithm
#[allow(clippy::too_many_arguments)]
fn clip_poly1(
    geom: &[(f64, f64)],
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    edge_nodes: &mut DrawVec,
    prevent_simplify_shared_nodes: bool,
) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = geom.to_vec();

    for edge in 0..4 {
        if !out.is_empty() {
            let input = std::mem::take(&mut out);

            let mut s = input[input.len() - 1];

            for &e in &input {
                if !inside(s, edge, minx, miny, maxx, maxy) {
                    // was outside the buffer

                    if !inside(e, edge, minx, miny, maxx, maxy) {
                        // still outside the buffer
                    } else if !inside(e, edge, ax, ay, bx, by) {
                        // outside the tile but inside the buffer
                        out.push(intersect(s, e, edge, minx, miny, maxx, maxy)); // on buffer edge
                        out.push(e);
                    } else {
                        out.push(intersect(s, e, edge, minx, miny, maxx, maxy)); // on buffer edge
                        if prevent_simplify_shared_nodes {
                            out.push(intersect(s, e, edge, ax, ay, bx, by)); // on tile boundary
                            edge_nodes.push(edge_node(out[out.len() - 1]));
                        }
                        out.push(e);
                    }
                } else if !inside(s, edge, ax, ay, bx, by) {
                    // was inside the buffer but outside the tile edge

                    if !inside(e, edge, minx, miny, maxx, maxy) {
                        // now outside the buffer
                        out.push(intersect(s, e, edge, minx, miny, maxx, maxy)); // on buffer edge
                    } else if !inside(e, edge, ax, ay, bx, by) {
                        // still outside the tile edge but inside the buffer
                        out.push(e);
                    } else {
                        // now inside the tile
                        if prevent_simplify_shared_nodes {
                            out.push(intersect(s, e, edge, ax, ay, bx, by)); // on tile boundary
                            edge_nodes.push(edge_node(out[out.len() - 1]));
                        }
                        out.push(e);
                    }
                } else {
                    // was inside the tile

                    if !inside(e, edge, minx, miny, maxx, maxy) {
                        // now outside the buffer
                        if prevent_simplify_shared_nodes {
                            out.push(intersect(s, e, edge, ax, ay, bx, by)); // on tile boundary
                            edge_nodes.push(edge_node(out[out.len() - 1]));
                        }
                        out.push(intersect(s, e, edge, minx, miny, maxx, maxy)); // on buffer edge
                    } else if !inside(e, edge, ax, ay, bx, by) {
                        // now inside the buffer but outside the tile edge
                        if prevent_simplify_shared_nodes {
                            out.push(intersect(s, e, edge, ax, ay, bx, by)); // on tile boundary
                            edge_nodes.push(edge_node(out[out.len() - 1]));
                        }
                        out.push(e);
                    } else {
                        // still inside the tile
                        out.push(e);
                    }
                }

                s = e;
            }
        }
    }

    if !out.is_empty() {
        // If the polygon begins and ends outside the edge,
        // the starting and ending points will be left as the
        // places where it intersects the edge. Need to add
        // another point to close the loop.

        if out[0].0 != out[out.len() - 1].0 || out[0].1 != out[out.len() - 1].1 {
            out.push(out[0]);
        }

        if out.len() < 3 {
            // fprintf(stderr, "Polygon degenerated to a line segment\n");
            out.clear();
            return out;
        }
    }

    out
}

pub fn distance_from_line(
    point_x: i64,
    point_y: i64,
    seg_a_x: i64,
    seg_a_y: i64,
    seg_b_x: i64,
    seg_b_y: i64,
) -> f64 {
    let p2x = seg_b_x - seg_a_x;
    let p2y = seg_b_y - seg_a_y;

    // These calculations must be made in integers instead of floating point
    // to make them consistent between x86 and arm floating point implementations.
    //
    // Coordinates may be up to 34 bits, so their product is up to 68 bits,
    // making their sum up to 69 bits. Downshift before multiplying to keep them in range.
    let something = ((p2x / 4)
        .wrapping_mul(p2x / 8)
        .wrapping_add((p2y / 4).wrapping_mul(p2y / 8))) as f64
        * 32.0;
    // likewise
    let mut u = if 0.0 == something {
        0.0
    } else {
        (((point_x - seg_a_x) / 4)
            .wrapping_mul(p2x / 8)
            .wrapping_add(((point_y - seg_a_y) / 4).wrapping_mul(p2y / 8))) as f64
            * 32.0
            / something
    };

    #[allow(clippy::manual_clamp)]
    if u >= 1.0 {
        u = 1.0;
    } else if u <= 0.0 {
        u = 0.0;
    }

    let x = seg_a_x as f64 + u * p2x as f64;
    let y = seg_a_y as f64 + u * p2y as f64;

    let dx = x - point_x as f64;
    let dy = y - point_y as f64;

    ((dx * dx + dy * dy).sqrt() * 16.0).round() / 16.0
}

// https://github.com/Project-OSRM/osrm-backend/blob/733d1384a40f/Algorithms/DouglasePeucker.cpp
#[allow(clippy::too_many_arguments)]
pub fn douglas_peucker(
    geom: &mut DrawVec,
    start: i32,
    n: i32,
    e: f64,
    mut kept: usize,
    mut retain: usize,
    prevent_simplify_shared_nodes: bool,
) {
    let mut recursion_stack: Vec<i32> = Vec::new();
    let at = |k: i32| (start + k) as usize;

    if geom[at(0)].necessary == 0 || geom[at(n - 1)].necessary == 0 {
        eprintln!("endpoints not marked necessary");
        exit(EXIT_IMPOSSIBLE);
    }

    let mut prev = 0;
    for here in 1..n {
        if geom[at(here)].necessary != 0 {
            recursion_stack.push(prev);
            recursion_stack.push(here);
            prev = here;

            if prevent_simplify_shared_nodes && retain > 0 {
                retain -= 1;
            }
        }
    }
    // These segments are put on the stack from start to end,
    // independent of winding, so note that anything that uses
    // "retain" to force it to keep at least N points will
    // keep a different set of points when wound one way than
    // when wound the other way.

    while let Some(second) = recursion_stack.pop() {
        // pop next element
        let first = recursion_stack.pop().unwrap();

        let mut max_distance = -1.0;
        let mut farthest_element_index;

        // find index idx of element with max_distance
        if geom[at(first)] < geom[at(second)] {
            farthest_element_index = first;
            for i in first + 1..second {
                let temp_dist = distance_from_line(
                    geom[at(i)].x,
                    geom[at(i)].y,
                    geom[at(first)].x,
                    geom[at(first)].y,
                    geom[at(second)].x,
                    geom[at(second)].y,
                );

                let distance = temp_dist.abs();

                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance
                            && geom[at(i)] < geom[at(farthest_element_index)]))
                {
                    farthest_element_index = i;
                    max_distance = distance;
                }
            }
        } else {
            farthest_element_index = second;
            let mut i = second - 1;
            while i > first {
                let temp_dist = distance_from_line(
                    geom[at(i)].x,
                    geom[at(i)].y,
                    geom[at(second)].x,
                    geom[at(second)].y,
                    geom[at(first)].x,
                    geom[at(first)].y,
                );

                let distance = temp_dist.abs();

                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance
                            && geom[at(i)] < geom[at(farthest_element_index)]))
                {
                    farthest_element_index = i;
                    max_distance = distance;
                }
                i -= 1;
            }
        }

        if max_distance >= 0.0 {
            // mark idx as necessary
            geom[at(farthest_element_index)].necessary = 1;
            kept += 1;

            if geom[at(first)] < geom[at(second)] {
                if 1 < farthest_element_index - first {
                    recursion_stack.push(first);
                    recursion_stack.push(farthest_element_index);
                }
                if 1 < second - farthest_element_index {
                    recursion_stack.push(farthest_element_index);
                    recursion_stack.push(second);
                }
            } else {
                if 1 < second - farthest_element_index {
                    recursion_stack.push(farthest_element_index);
                    recursion_stack.push(second);
                }
                if 1 < farthest_element_index - first {
                    recursion_stack.push(first);
                    recursion_stack.push(farthest_element_index);
                }
            }
        }
    }
}

// cut-down version of simplify_lines(), not dealing with shared node preservation
fn simplify_lines_basic(
    geom: &mut DrawVec,
    z: i32,
    detail: i32,
    simplification: f64,
    retain: usize,
) -> DrawVec {
    let res: i32 = 1i32.wrapping_shl((32 - detail - z) as u32);

    for g in geom.iter_mut() {
        if g.op == VT_MOVETO {
            g.necessary = 1;
        } else if g.op == VT_LINETO {
            g.necessary = 0;
            // if this is actually the endpoint, not an intermediate point,
            // it will be marked as necessary below
        } else {
            g.necessary = 1;
        }
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

            if j - i > 1 {
                douglas_peucker(
                    geom,
                    i as i32,
                    (j - i) as i32,
                    res as f64 * simplification,
                    2,
                    retain,
                    false,
                );
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

#[allow(clippy::too_many_arguments)]
pub fn reduce_tiny_poly(
    geom: &DrawVec,
    z: i32,
    detail: i32,
    still_needs_simplification: &mut bool,
    reduced_away: &mut bool,
    accum_area: &mut f64,
    tiny_polygon_size: f64,
) -> DrawVec {
    let mut out = DrawVec::new();
    let pixel = 1i64.wrapping_shl((32 - detail - z) as u32) as f64 * tiny_polygon_size;

    let mut included_last_outer = false;
    *still_needs_simplification = false;
    *reduced_away = false;

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

            let area = get_area(geom, i, j);

            // XXX There is an ambiguity here: If the area of a ring is 0 and it is followed by holes,
            // we don't know whether the area-0 ring was a hole too or whether it was the outer ring
            // that these subsequent holes are somehow being subtracted from. I hope that if a polygon
            // was simplified down to nothing, its holes also became nothing.

            if area != 0.0 {
                // These are pixel coordinates, so area > 0 for the outer ring.
                // If the outer ring of a polygon was reduced to a pixel, its
                // inner rings must just have their area de-accumulated rather
                // than being drawn since we don't really know where they are.

                // i.e., this outer ring is small enough that we are including it
                // in a tiny polygon rather than letting it represent itself,
                // OR it is an inner ring and we haven't output an outer ring for it to be
                // cut out of, so we are just subtracting its area from the tiny polygon
                // rather than trying to deal with it geometrically
                if (area > 0.0 && area <= pixel * pixel) || (area < 0.0 && !included_last_outer) {
                    *accum_area += area;
                    *reduced_away = true;

                    if area > 0.0 && *accum_area > pixel * pixel {
                        // XXX use centroid;

                        let x = geom[i].x as f64;
                        let y = geom[i].y as f64;
                        out.push(Draw::new(
                            VT_MOVETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0 + pixel) as i64,
                            (y - pixel / 2.0) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0 + pixel) as i64,
                            (y - pixel / 2.0 + pixel) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0 + pixel) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0) as i64,
                        ));

                        *accum_area -= pixel * pixel;
                    }

                    if area > 0.0 {
                        included_last_outer = false;
                    }
                }
                // i.e., this ring is large enough that it gets to represent itself
                // or it is a tiny hole out of a real polygon, which we are still treating
                // as a real geometry because otherwise we can accumulate enough tiny holes
                // that we will drop the next several outer rings getting back up to 0.
                else {
                    let mut k = i;
                    while k < j && k < geom.len() {
                        out.push(geom[k]);
                        k += 1;
                    }

                    // which means that the overall polygon has a real geometry,
                    // which means that it gets to be simplified.
                    *still_needs_simplification = true;

                    if area > 0.0 {
                        included_last_outer = true;
                    }
                }
            } else {
                // area is 0: doesn't count as either having been reduced away,
                // since it was probably just degenerate from having been clipped,
                // or as needing simplification, since it produces no output.
            }

            i = j - 1;
        } else {
            eprintln!(
                "how did we get here with {} in {}?",
                geom[i].op as i32,
                geom.len() as i32
            );

            for g in geom {
                eprint!("{}/{}/{} ", g.op as i32, g.x, g.y);
            }
            eprintln!();

            out.push(geom[i]);
        }
        i += 1;
    }

    out
}

/* pnpoly:
Copyright (c) 1970-2003, Wm. Randolph Franklin

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimers.
Redistributions in binary form must reproduce the above copyright notice in the documentation and/or other materials provided with the distribution.
The name of W. Randolph Franklin may not be used to endorse or promote products derived from this Software without specific prior written permission.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/

pub fn pnpoly(vert: &DrawVec, start: usize, nvert: usize, testx: i64, testy: i64) -> i32 {
    let mut c = false;
    let mut i = 0;
    let mut j = nvert.wrapping_sub(1);
    while i < nvert {
        let vi = vert[i + start];
        let vj = vert[j + start];
        if ((vi.y > testy) != (vj.y > testy))
            && ((testx as f64)
                < (vj.x - vi.x).wrapping_mul(testy - vi.y) as f64 / (vj.y - vi.y) as f64
                    + vi.x as f64)
        {
            c = !c;
        }
        j = i;
        i += 1;
    }
    c as i32
}

pub fn pnpoly_mvt(
    vert: &[MvtGeometry],
    start: usize,
    nvert: usize,
    testx: i64,
    testy: i64,
) -> bool {
    let mut c = false;
    let mut i = 0;
    let mut j = nvert.wrapping_sub(1);
    while i < nvert {
        let vi = vert[i + start];
        let vj = vert[j + start];
        if ((vi.y > testy) != (vj.y > testy))
            && ((testx as f64)
                < (vj.x - vi.x).wrapping_mul(testy - vi.y) as f64 / (vj.y - vi.y) as f64
                    + vi.x as f64)
        {
            c = !c;
        }
        j = i;
        i += 1;
    }
    c
}

pub fn pnpoly_mp_mvt(geom: &[MvtGeometry], x: i64, y: i64) -> bool {
    // assumes rings are properly nested, so inside a hole matches twice
    let mut found = false;

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == MVT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != MVT_LINETO {
                    break;
                }
                j += 1;
            }

            found ^= pnpoly_mvt(geom, i, j - i, x, y);
            i = j - 1;
        }
        i += 1;
    }

    found
}

pub fn pnpoly_mp(geom: &DrawVec, x: i64, y: i64) -> bool {
    // assumes rings are properly nested, so inside a hole matches twice
    let mut found = false;

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

            found ^= pnpoly(geom, i, j - i, x, y) != 0;
            i = j - 1;
        }
        i += 1;
    }

    found
}

/// `overzoom(std::vector<input_tile> const &tiles, ...)`. Returns the encoded tile.
#[allow(clippy::too_many_arguments)]
pub fn overzoom(
    tiles: &[InputTile],
    nz: i32,
    nx: i32,
    ny: i32,
    detail: i32,
    buffer: i32,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    do_compress: bool,
    next_overzoomed_tiles: Option<&mut Vec<(u32, u32)>>,
    demultiply: bool,
    filter: Option<JsonRef<'_>>,
    preserve_input_order: bool,
    attribute_accum: &HashMap<String, AttributeOp>,
    unidecode_data: &[String],
    simplification: f64,
    tiny_polygon_size: f64,
    bins: &[MvtLayer],
    bin_by_id_list: &str,
    accumulate_numeric: &str,
    feature_limit: usize,
    clipbboxes: &[ClipBbox],
    deduplicate_by_id: bool,
) -> Vec<u8> {
    let mut decoded: Vec<SourceTile> = Vec::new();

    for t in tiles {
        let mut tile = MvtTile::default();

        // The C++ catches the exception protozero throws on malformed input
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut was_compressed = false;
            tile.decode(&t.tile, &mut was_compressed)
        }));
        match parsed {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Couldn't parse tile {}/{}/{}", t.z, t.x as u32, t.y as u32);
                exit(EXIT_MVT);
            }
            Err(_) => {
                eprintln!(
                    "PBF decoding error in tile {}/{}/{}",
                    t.z, t.x as u32, t.y as u32
                );
                exit(EXIT_PROTOBUF);
            }
        }

        decoded.push(SourceTile {
            tile,
            z: t.z,
            x: t.x,
            y: t.y,
        });
    }

    overzoom_source_tiles(
        &decoded,
        nz,
        nx,
        ny,
        detail,
        buffer,
        keep,
        exclude,
        exclude_prefix,
        do_compress,
        next_overzoomed_tiles,
        demultiply,
        filter,
        preserve_input_order,
        attribute_accum,
        unidecode_data,
        simplification,
        tiny_polygon_size,
        bins,
        bin_by_id_list,
        accumulate_numeric,
        feature_limit,
        clipbboxes,
        deduplicate_by_id,
    )
}

/// like a minimal serial_feature, but with mvt_feature-style attributes
struct TileFeature<'a> {
    geom: DrawVec,
    t: i32,
    has_id: bool,
    id: u64,
    tags: Vec<u32>,
    layer: &'a MvtLayer,
    seq: usize,
}

fn should_keep(
    key: &str,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
) -> bool {
    if keep.is_empty() || keep.contains(key) {
        if exclude.contains(key) {
            return false;
        }

        for prefix in exclude_prefix {
            if starts_with(key, prefix) {
                return false;
            }
        }

        return true;
    }

    false
}

fn handle_closepath_from_mvt(geom: &mut DrawVec) {
    // mvt geometries close polygons with a mvt_closepath operation
    // tippecanoe-internal geometries close polygons with a lineto to the initial point

    let mut last_open = 0;

    for i in 0..geom.len() {
        if geom[i].op as i32 == MVT_CLOSEPATH {
            geom[i] = Draw::new(MVT_LINETO as i8, geom[last_open].x, geom[last_open].y);
        } else if geom[i].op as i32 == MVT_MOVETO {
            last_open = i;
        }
    }
}

// returns true if a feature was output; false if it was clipped away
#[allow(clippy::too_many_arguments)]
fn feature_out(
    features: &[TileFeature],
    outlayer: &mut MvtLayer,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    attribute_accum: &HashMap<String, AttributeOp>,
    _accumulate_numeric: &str,
    key_pool: &mut KeyPool,
    buffer: i32,
    include_nonaggregate: bool,
    _clipbboxes: &[ClipBbox],
    _nz: i32,
    _nx: i32,
    _ny: i32,
    deduplicate_ids: Option<&mut BTreeSet<u64>>,
) -> bool {
    // Add geometry to output feature

    let mut geom = features[0].geom.clone();
    let t = features[0].t;

    let mut fix_polygons = false;
    if (buffer >= 0) && t == VT_POLYGON {
        fix_polygons = true;
    }

    if fix_polygons {
        handle_closepath_from_mvt(&mut geom);
    }

    if buffer >= 0 {
        if t == VT_LINE {
            geom = clip_lines(&mut geom, 32 - outlayer.detail(), buffer as i64);
        } else if t == VT_POLYGON {
            let mut dv = DrawVec::new();
            geom = simple_clip_poly(&mut geom, 32 - outlayer.detail(), buffer, &mut dv, false);
        } else if t == VT_POINT {
            geom = clip_point(&mut geom, 32 - outlayer.detail(), buffer as i64);
        }

        geom = remove_noop(geom, t, 0);
    }

    if fix_polygons {
        geom = clean_or_clip_poly(&mut geom, 0, 0, false, false);
        geom = close_poly(&mut geom);
    }

    let mut outfeature = MvtFeature {
        type_: features[0].t,
        ..Default::default()
    };
    for g in &geom {
        outfeature
            .geometry
            .push(MvtGeometry::new(g.op as i32, g.x, g.y));
    }

    // ID and attributes, if it didn't get clipped away

    if features[0].has_id
        && let Some(deduplicate_ids) = deduplicate_ids
    {
        if deduplicate_ids.contains(&features[0].id) {
            outfeature.geometry.clear();
        } else {
            deduplicate_ids.insert(features[0].id);
        }
    }

    if !outfeature.geometry.is_empty() {
        if features[0].has_id {
            outfeature.has_id = true;
            outfeature.id = features[0].id;
        }

        outfeature.seq = features[0].seq;

        if !attribute_accum.is_empty() {
            // convert the attributes of the output feature
            // from layer references to a vector so they can have
            // attributes from the other features of the
            // multiplier cluster accumulated onto them

            let mut full_keys: Vec<Arc<String>> = Vec::new();
            let mut full_values: Vec<MvtValue> = Vec::new();

            let f = &features[0];
            let mut i = 0;
            while i + 1 < f.tags.len() {
                let key = &f.layer.keys[f.tags[i] as usize];
                if should_keep(key, keep, exclude, exclude_prefix) {
                    if attribute_accum.contains_key(key) {
                        // this attribute has an accumulator, so convert it
                        full_keys.push(key_pool.pool(key));
                        full_values.push(f.layer.values[f.tags[i + 1] as usize].clone());
                    } else if include_nonaggregate {
                        // otherwise just tag it directly onto the output feature
                        outlayer.tag(
                            &mut outfeature,
                            key,
                            &f.layer.values[f.tags[i + 1] as usize],
                        );
                    }
                }
                i += 2;
            }

            // accumulate whatever attributes are specified to be accumulated
            // onto the feature that will survive into the output, from the
            // features that will not

            for fi in &features[1..] {
                let mut keys: BTreeSet<String> = BTreeSet::new();

                let mut j = 0;
                while j + 1 < fi.tags.len() {
                    let key = &fi.layer.keys[fi.tags[j] as usize];
                    if should_keep(key, keep, exclude, exclude_prefix) {
                        keys.insert(key.clone());
                    }
                    j += 2;
                }

                let mut j = 0;
                while j + 1 < fi.tags.len() {
                    let key = &fi.layer.keys[fi.tags[j] as usize];
                    if should_keep(key, keep, exclude, exclude_prefix)
                        && let Some(found) = attribute_accum.get(key)
                    {
                        let val = fi.layer.values[fi.tags[j + 1] as usize].clone();
                        preserve_attribute_mvt(
                            found,
                            key,
                            &val,
                            &mut full_keys,
                            &mut full_values,
                            key_pool,
                        );
                    }
                    j += 2;
                }
            }

            // convert the final attributes back to mvt_value
            // and tag them onto the output feature

            for i in 0..full_keys.len() {
                if should_keep(&full_keys[i], keep, exclude, exclude_prefix) {
                    outlayer.tag(&mut outfeature, &full_keys[i], &full_values[i]);
                }
            }
        } else if include_nonaggregate {
            let f = &features[0];
            let mut i = 0;
            while i + 1 < f.tags.len() {
                if should_keep(
                    &f.layer.keys[f.tags[i] as usize],
                    keep,
                    exclude,
                    exclude_prefix,
                ) {
                    outlayer.tag(
                        &mut outfeature,
                        &f.layer.keys[f.tags[i] as usize],
                        &f.layer.values[f.tags[i + 1] as usize],
                    );
                }
                i += 2;
            }
        }

        outlayer.features.push(outfeature);
        return true;
    }

    false
}

const RETAIN_POINTS_MULTIPLIER_FIRST: &str = "tippecanoe:retain_points_multiplier_first";
const RETAIN_POINTS_MULTIPLIER_SEQUENCE: &str = "tippecanoe:retain_points_multiplier_sequence";

/// `overzoom(std::vector<source_tile> const &tiles, ...)`. Returns the encoded tile.
#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
pub fn overzoom_source_tiles(
    tiles: &[SourceTile],
    nz: i32,
    nx: i32,
    ny: i32,
    detail: i32,
    buffer: i32,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    do_compress: bool,
    next_overzoomed_tiles: Option<&mut Vec<(u32, u32)>>,
    demultiply: bool,
    filter: Option<JsonRef<'_>>,
    preserve_input_order: bool,
    attribute_accum: &HashMap<String, AttributeOp>,
    unidecode_data: &[String],
    simplification: f64,
    tiny_polygon_size: f64,
    bins: &[MvtLayer],
    bin_by_id_list: &str,
    accumulate_numeric: &str,
    feature_limit: usize,
    clipbboxes: &[ClipBbox],
    deduplicate_by_id: bool,
) -> Vec<u8> {
    let detail_or_unspecified = detail;
    let mut outtile = MvtTile::default();
    let mut key_pool = KeyPool::default();

    // map from layer name to ids used in that layer
    let mut deduplicate_ids: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();

    for tile in tiles {
        for layer in &tile.tile.layers {
            let mut outlayer: Option<usize> = None;

            let mut det = detail_or_unspecified;
            if det <= 0 {
                det = ((layer.extent as f64).ln() / 2f64.ln()).round() as i32;
            }

            for i in 0..outtile.layers.len() {
                if outtile.layers[i].name == layer.name {
                    outlayer = Some(i);
                }
            }

            let outlayer = match outlayer {
                Some(o) => o,
                None => {
                    let newlayer = MvtLayer {
                        name: layer.name.clone(),
                        version: layer.version,
                        extent: 1i64 << det,
                        ..Default::default()
                    };

                    outtile.layers.push(newlayer);
                    outtile.layers.len() - 1
                }
            };

            let mut deduplicate_by_id_set: Option<&mut BTreeSet<u64>> = None;
            if deduplicate_by_id {
                deduplicate_by_id_set =
                    Some(deduplicate_ids.entry(layer.name.clone()).or_default());
            }

            let mut pending_tile_features: Vec<TileFeature> = Vec::new();
            let mut accum_area: f64 = 0.0;

            for feature_in in &layer.features {
                let mut feature = feature_in.clone();
                let mut geom = DrawVec::new();
                let t = feature.type_;

                // Convert feature geometry to world coordinates

                let tilesize: i64 = 1i64 << (32 - tile.z); // source tile size in world coordinates
                let mut ring_closure = Draw::new(0, 0, 0);
                let sametile = nz == tile.z
                    && nx == tile.x
                    && ny == tile.y
                    && outtile.layers[outlayer].extent >= layer.extent;

                for g in &feature.geometry {
                    if g.op == MVT_CLOSEPATH {
                        geom.push(ring_closure);
                    } else {
                        geom.push(Draw::new(
                            g.op as i8,
                            g.x * tilesize / layer.extent + tile.x as i64 * tilesize,
                            g.y * tilesize / layer.extent + tile.y as i64 * tilesize,
                        ));
                        if g.op == MVT_MOVETO {
                            ring_closure = geom[geom.len() - 1];
                            ring_closure.op = MVT_LINETO as i8;
                        }
                    }
                }

                // Now offset from world coordinates to output tile coordinates,
                // but retain world scale, because that is what tippecanoe zoom-oriented clipping expects

                let outtilesize: i64 = 1i64 << (32 - nz); // destination tile size in world coordinates
                for g in geom.iter_mut() {
                    g.x -= nx as i64 * outtilesize;
                    g.y -= ny as i64 * outtilesize;
                }

                {
                    // Clip to output tile

                    let mut xmin = i64::MAX;
                    let mut ymin = i64::MAX;
                    let mut xmax = i64::MIN;
                    let mut ymax = i64::MIN;

                    for g in &geom {
                        xmin = std::cmp::min(xmin, g.x);
                        ymin = std::cmp::min(ymin, g.y);
                        xmax = std::cmp::max(xmax, g.x);
                        ymax = std::cmp::max(ymax, g.y);
                    }

                    let b = outtilesize * buffer as i64 / 256;
                    if xmax < -b || ymax < -b || xmin > outtilesize + b || ymin > outtilesize + b {
                        // quick exclusion by bounding box
                        if deduplicate_by_id && feature.has_id {
                            deduplicate_by_id_set
                                .as_deref_mut()
                                .unwrap()
                                .insert(feature.id);
                        }
                        continue;
                    }

                    if t == VT_LINE {
                        geom = clip_lines(&mut geom, nz, buffer as i64);
                    } else if t == VT_POLYGON {
                        let mut dv = DrawVec::new();
                        geom = simple_clip_poly(&mut geom, nz, buffer, &mut dv, false);
                    } else if t == VT_POINT {
                        geom = clip_point(&mut geom, nz, buffer as i64);
                    }
                }

                if geom.is_empty() {
                    // clipped away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let mut flush_multiplier_cluster = false;
                if demultiply {
                    let mut i: isize = feature.tags.len() as isize - 2;
                    while i >= 0 {
                        let iu = i as usize;
                        if layer.keys[feature.tags[iu] as usize] == RETAIN_POINTS_MULTIPLIER_FIRST {
                            let v = &layer.values[feature.tags[iu + 1] as usize];
                            if v.type_ == MVT_BOOL && v.numeric_value.bool_value() {
                                flush_multiplier_cluster = true;
                                feature.tags.drain(iu..iu + 2);
                            }
                        } else if i < feature.tags.len() as isize
                            && layer.keys[feature.tags[iu] as usize]
                                == RETAIN_POINTS_MULTIPLIER_SEQUENCE
                        {
                            let v = &layer.values[feature.tags[iu + 1] as usize];
                            feature.seq = mvt_value_to_long_long(v) as usize;
                            feature.tags.drain(iu..iu + 2);
                        }
                        i -= 2;
                    }
                } else {
                    flush_multiplier_cluster = true;
                }

                if flush_multiplier_cluster && !pending_tile_features.is_empty() {
                    feature_out(
                        &pending_tile_features,
                        &mut outtile.layers[outlayer],
                        keep,
                        exclude,
                        exclude_prefix,
                        attribute_accum,
                        accumulate_numeric,
                        &mut key_pool,
                        -1,
                        true,
                        &[],
                        nz,
                        nx,
                        ny,
                        deduplicate_by_id_set.as_deref_mut(),
                    );
                    if outtile.layers[outlayer].features.len() >= feature_limit {
                        break;
                    }
                    pending_tile_features.clear();
                }

                let mut exclude_attributes: BTreeSet<String> = BTreeSet::new();
                if let Some(filter) = filter
                    && !evaluate_feature(
                        &feature,
                        layer,
                        filter,
                        &mut exclude_attributes,
                        nz,
                        unidecode_data,
                    )
                {
                    // filtered away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let mut still_need_simplification_after_reduction = false;
                if t == VT_POLYGON && tiny_polygon_size > 0.0 {
                    let mut simplified_away_by_reduction = false;

                    geom = reduce_tiny_poly(
                        &geom,
                        nz,
                        det,
                        &mut still_need_simplification_after_reduction,
                        &mut simplified_away_by_reduction,
                        &mut accum_area,
                        tiny_polygon_size,
                    );
                } else {
                    still_need_simplification_after_reduction = true;
                }

                if simplification > 0.0 && still_need_simplification_after_reduction {
                    if t == VT_POLYGON {
                        geom = simplify_lines_basic(&mut geom, nz, det, simplification, 4);
                    } else if t == VT_LINE {
                        geom = simplify_lines_basic(&mut geom, nz, det, simplification, 0);
                    }
                }

                // Scale to output tile extent

                to_tile_scale(&mut geom, nz, det);

                if !sametile {
                    // Clean geometries

                    geom = remove_noop(geom, t, 0);
                    if t == VT_POLYGON {
                        geom = clean_or_clip_poly(&mut geom, 0, 0, false, false);
                    }
                }

                if t == VT_POLYGON {
                    geom = close_poly(&mut geom);
                }

                if geom.is_empty() {
                    // simplified away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let tf = TileFeature {
                    geom,
                    t,
                    has_id: feature.has_id,
                    id: feature.id,
                    tags: std::mem::take(&mut feature.tags),
                    layer,
                    seq: feature.seq,
                };

                pending_tile_features.push(tf);
            }

            if !pending_tile_features.is_empty() {
                feature_out(
                    &pending_tile_features,
                    &mut outtile.layers[outlayer],
                    keep,
                    exclude,
                    exclude_prefix,
                    attribute_accum,
                    accumulate_numeric,
                    &mut key_pool,
                    -1,
                    true,
                    &[],
                    nz,
                    nx,
                    ny,
                    deduplicate_by_id_set,
                );
                pending_tile_features.clear();
                if outtile.layers[outlayer].features.len() >= feature_limit {
                    break;
                }
            }

            if preserve_input_order {
                outtile.layers[outlayer]
                    .features
                    .sort_by(|a, b| a.seq.cmp(&b.seq));
            }
        }
    }

    if let Some(next_overzoomed_tiles) = next_overzoomed_tiles {
        // will any child tiles have features in them?
        // find out recursively from the tile we just made.
        //
        // (yes, we should keep them instead of remaking them
        // later, but that first requires figuring out where to
        // keep them.)

        if !outtile.layers.is_empty() {
            for x in 0..2 {
                for y in 0..2 {
                    let st = SourceTile {
                        tile: outtile.clone(),
                        z: nz,
                        x: nx,
                        y: ny,
                    };

                    let sts = vec![st];

                    // feature_limit arg is 1, because we just care whether there are any overzoomed features
                    // left after clipping to the child tile, not about their actual content
                    let child = overzoom_source_tiles(
                        &sts,
                        nz + 1,
                        nx * 2 + x,
                        ny * 2 + y,
                        detail_or_unspecified,
                        buffer,
                        keep,
                        exclude,
                        exclude_prefix,
                        false,
                        None,
                        demultiply,
                        filter,
                        preserve_input_order,
                        attribute_accum,
                        unidecode_data,
                        simplification,
                        tiny_polygon_size,
                        bins,
                        bin_by_id_list,
                        accumulate_numeric,
                        1,
                        clipbboxes,
                        deduplicate_by_id,
                    );
                    if !child.is_empty() {
                        next_overzoomed_tiles.push(((nx * 2 + x) as u32, (ny * 2 + y) as u32));
                    }
                }
            }
        }
    }

    outtile.layers.retain(|layer| !layer.features.is_empty());

    if !outtile.layers.is_empty() {
        let pbf = outtile.encode();

        let mut compressed = Vec::new();
        if do_compress {
            compress(&pbf, &mut compressed, true);
        } else {
            compressed = pbf;
        }

        compressed
    } else {
        Vec::new()
    }
}

pub fn fix_polygon(geom: &DrawVec, use_winding: bool, reverse_winding: bool) -> DrawVec {
    let mut outer = 1;
    let mut out = DrawVec::new();

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_CLOSEPATH {
            outer = 1;
        } else if geom[i].op == VT_MOVETO {
            // Find the end of the ring

            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            // A polygon ring must contain at least three points
            // (and really should contain four). If this one does
            // not have any, avoid a division by zero trying to
            // calculate the centroid below.
            if j - i < 1 {
                i = j - 1;
                outer = 0;
                i += 1;
                continue;
            }

            // Make a temporary copy of the ring.
            // Close it if it isn't closed.

            let mut ring = DrawVec::new();
            for g in &geom[i..j] {
                ring.push(*g);
            }
            if j - i != 0 && (ring[0].x != ring[j - i - 1].x || ring[0].y != ring[j - i - 1].y) {
                ring.push(ring[0]);
            }

            // A polygon ring at this point should contain at least four points.
            // Flesh it out with some vertex copies if it doesn't.

            while ring.len() < 4 {
                ring.push(ring[0]);
            }

            // Reverse ring if winding order doesn't match
            // inner/outer expectation

            let mut reverse_ring = false;
            if use_winding {
                // GeoJSON winding is reversed from vector winding
                reverse_ring = true;
            } else if reverse_winding {
                // GeoJSON winding is reversed from vector winding
                reverse_ring = false;
            } else {
                let area = get_area(&ring, 0, ring.len());
                if ((area > 0.0) as i32) != outer {
                    reverse_ring = true;
                }
            }

            if reverse_ring {
                let mut tmp = DrawVec::new();
                for a in (0..ring.len()).rev() {
                    tmp.push(ring[a]);
                }
                ring = tmp;
            }

            // Now we are rotating the ring to make the first/last point
            // one that would be unlikely to be simplified away.

            // calculate centroid
            // a + 1 < size() because point 0 is duplicated at the end
            let mut xtotal: i64 = 0;
            let mut ytotal: i64 = 0;
            let mut count: i64 = 0;
            for r in &ring[..ring.len() - 1] {
                xtotal += r.x;
                ytotal += r.y;
                count += 1;
            }
            xtotal /= count;
            ytotal /= count;

            // figure out which point is furthest from the centroid
            let mut dist2: i64 = 0;
            let mut furthest: i64 = 0;
            for a in 0..ring.len() - 1 {
                // division by 16 because these are z0 coordinates and we need to avoid overflow
                let xd = (ring[a].x - xtotal) / 16;
                let yd = (ring[a].y - ytotal) / 16;
                let d2 = xd * xd + yd * yd;
                if d2 > dist2 || (d2 == dist2 && ring[a] < ring[furthest as usize]) {
                    dist2 = d2;
                    furthest = a as i64;
                }
            }

            // then figure out which point is furthest from *that*,
            // which will hopefully be a good origin point since it should be
            // at a far edge of the shape.
            let mut dist2b: i64 = 0;
            let mut furthestb: i64 = 0;
            for a in 0..ring.len() - 1 {
                // division by 16 because these are z0 coordinates and we need to avoid overflow
                let xd = (ring[a].x - ring[furthest as usize].x) / 16;
                let yd = (ring[a].y - ring[furthest as usize].y) / 16;
                let d2 = xd * xd + yd * yd;
                if d2 > dist2b || (d2 == dist2b && ring[a] < ring[furthestb as usize]) {
                    dist2b = d2;
                    furthestb = a as i64;
                }
            }

            // rotate ring so the furthest point is the duplicated one.
            // the idea is that simplification will then be more efficient,
            // never wasting the start and end points, which are always retained,
            // on a point that has little impact on the shape.

            // Copy ring into output, fixing the moveto/lineto ops if necessary because of
            // reversal or closing

            for a in 0..ring.len() {
                let a2 = (a + furthestb as usize) % (ring.len() - 1);

                if a == 0 {
                    out.push(Draw::new(VT_MOVETO, ring[a2].x, ring[a2].y));
                } else {
                    out.push(Draw::new(VT_LINETO, ring[a2].x, ring[a2].y));
                }
            }

            // Next ring or polygon begins on the non-lineto that ended this one
            // and is not an outer ring unless there is a terminator first

            i = j - 1;
            outer = 0;
        } else {
            eprintln!(
                "Internal error: polygon ring begins with {}, not moveto",
                geom[i].op as i32
            );
            exit(EXIT_IMPOSSIBLE);
        }
        i += 1;
    }

    out
}

pub fn line_is_too_small(geometry: &DrawVec, z: i32, detail: i32) -> bool {
    if geometry.is_empty() {
        return true;
    }

    let scale = 1i64.wrapping_shl((32 - detail - z) as u32) as f64;
    let mut x: i64 = 0;
    let mut y: i64 = 0;
    for g in geometry {
        if g.op == VT_MOVETO {
            x = (g.x as f64 / scale).round() as i64;
            y = (g.y as f64 / scale).round() as i64;
        } else {
            let xx = (g.x as f64 / scale).round() as i64;
            let yy = (g.y as f64 / scale).round() as i64;

            if xx != x || yy != y {
                return false;
            }
        }
    }

    true
}

pub fn coalesce_polygon(geom: &mut DrawVec, scale_up: bool) {
    // wagyu should be able to straightforwardly handle
    // anything under a few hundred thousand vertices
    if geom.len() < 100000 {
        *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
        return;
    }

    // These geometries were assembled in geometric order,
    // so sub-batches of them should hopefully union into
    // reasonable sets.
    //
    // Find the first outer ring after halfway point.

    let mut i = geom.len() / 2;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            if get_area(geom, i, j) > 0.0 {
                // If we have an outer ring, split there
                // and coalesce the two halves

                // Copy second half to new vector
                let mut geom2: Vec<Draw> = geom[i..].to_vec();

                // Resize vector to include only first half
                geom.truncate(i);

                // Clean each half individually
                coalesce_polygon(geom, scale_up);
                coalesce_polygon(&mut geom2, scale_up);

                // Copy second half back with first
                geom.extend_from_slice(&geom2);
                geom2.clear();

                // Clean the combined geometry
                *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
                return;
            }

            i = j - 1;
        }
        i += 1;
    }

    // Can't find a breakpoint; take what we can get.
    *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
}
