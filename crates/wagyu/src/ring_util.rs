//! Port of `ring_util.hpp`.

use crate::active_bound_list::ActiveBoundList;
use crate::bound::{Bound, BoundPtr};
use crate::config::{EdgeSide, Error, Result};
use crate::edge::{Edge, is_horizontal};
use crate::geometry::{BoundingBox, Point, PointF64};
use crate::point::{NIL, PointNode, PointPtr, RingPtr, pos_is, same_pos};
use crate::ring::{
    RingManager, area_from_point, create_new_point, create_new_point_before, create_new_ring, reverse_ring,
    ring_is_hole, ring1_replaces_ring2,
};
use crate::util::{greater_than_or_equal, llround, std_max, std_min, value_is_zero, values_are_equal};

pub(crate) fn set_hole_state(bnd: BoundPtr, active_bounds: &ActiveBoundList, bnds: &[Bound], rings: &mut RingManager) {
    let mut bnd_itr = active_bounds
        .iter()
        .rposition(|&b| b == bnd)
        .expect("set_hole_state: bound is not in the active bound list");
    let mut bnd_tmp: BoundPtr = NIL;
    // Find first non line ring to the left of current bound.
    while bnd_itr > 0 {
        bnd_itr -= 1;
        let b = active_bounds[bnd_itr];
        if b == NIL {
            continue;
        }
        if bnds[b].ring != NIL {
            if bnd_tmp == NIL {
                bnd_tmp = b;
            } else if bnds[bnd_tmp].ring == bnds[b].ring {
                bnd_tmp = NIL;
            }
        }
    }
    let r = bnds[bnd].ring;
    if bnd_tmp == NIL {
        rings.rings[r].parent = NIL;
        rings.children.push(r);
    } else {
        let p = bnds[bnd_tmp].ring;
        rings.rings[r].parent = p;
        rings.rings[p].children.push(r);
    }
}

pub(crate) fn update_current_hp_itr(scanline_y: i64, rings: &mut RingManager) {
    while rings.hot_pixels[rings.current_hp_itr].y > scanline_y {
        rings.current_hp_itr += 1;
    }
}

fn round_towards_min(val: f64) -> i64 {
    // 0.5 rounds to 0
    // 0.0 rounds to 0
    // -0.5 rounds to -1
    let half = val.floor() + 0.5;
    if values_are_equal(val, half) { val.floor() as i64 } else { llround(val) }
}

pub(crate) fn round_towards_max(val: f64) -> i64 {
    // 0.5 rounds to 1
    // 0.0 rounds to 0
    // -0.5 rounds to 0
    let half = val.floor() + 0.5;
    if values_are_equal(val, half) { val.ceil() as i64 } else { llround(val) }
}

#[inline]
fn get_edge_min_x(edge: &Edge, current_y: i64) -> i64 {
    if is_horizontal(edge) {
        if edge.bot.x < edge.top.x { edge.bot.x } else { edge.top.x }
    } else if edge.dx > 0.0 {
        if current_y == edge.top.y {
            edge.top.x
        } else {
            let lower_range_y = (current_y - edge.bot.y) as f64 - 0.5;
            let return_val = edge.bot.x as f64 + edge.dx * lower_range_y;
            round_towards_min(return_val)
        }
    } else if current_y == edge.bot.y {
        edge.bot.x
    } else {
        let return_val = edge.bot.x as f64 + edge.dx * ((current_y - edge.bot.y) as f64 + 0.5);
        round_towards_min(return_val)
    }
}

#[inline]
fn get_edge_max_x(edge: &Edge, current_y: i64) -> i64 {
    if is_horizontal(edge) {
        if edge.bot.x > edge.top.x { edge.bot.x } else { edge.top.x }
    } else if edge.dx < 0.0 {
        if current_y == edge.top.y {
            edge.top.x
        } else {
            let lower_range_y = (current_y - edge.bot.y) as f64 - 0.5;
            let return_val = edge.bot.x as f64 + edge.dx * lower_range_y;
            round_towards_max(return_val)
        }
    } else if current_y == edge.bot.y {
        edge.bot.x
    } else {
        let return_val = edge.bot.x as f64 + edge.dx * ((current_y - edge.bot.y) as f64 + 0.5);
        round_towards_max(return_val)
    }
}

/// Inserts the hot pixel at `itr` into the ring of `bnd` unless it repeats the end point.
/// Shared body of the two `hot_pixel_set_*` loops.
fn hot_pixel_insert(hp: Point, bnd: &Bound, rings: &mut RingManager) {
    let r = bnd.ring;
    let op = rings.rings[r].points;
    let to_front = bnd.side == EdgeSide::Left;
    if to_front && pos_is(&rings.points, hp, op) {
        return;
    } else if !to_front && pos_is(&rings.points, hp, rings.points[op].prev) {
        return;
    }
    let new_point = create_new_point_before(r, hp, op, rings);
    if to_front {
        rings.rings[r].points = new_point;
    }
}

fn hot_pixel_set_left_to_right(
    y: i64,
    start_x: i64,
    end_x: i64,
    bnd: &Bound,
    rings: &mut RingManager,
    itr: usize,
    end: usize,
    add_end_point: bool,
) {
    let mut x_min = get_edge_min_x(bnd.ce(), y);
    x_min = x_min.max(start_x);
    let mut x_max = get_edge_max_x(bnd.ce(), y);
    x_max = x_max.min(end_x);
    for k in itr..end {
        let hp = rings.hot_pixels[k];
        if hp.x < x_min {
            continue;
        }
        if hp.x > x_max {
            break;
        }
        if !add_end_point && hp.x == end_x {
            continue;
        }
        hot_pixel_insert(hp, bnd, rings);
    }
}

/// `itr` and `end` are reverse iterators given by their `base()`: the loop visits
/// `hot_pixels[itr - 1]` down to `hot_pixels[end]`.
fn hot_pixel_set_right_to_left(
    y: i64,
    start_x: i64,
    end_x: i64,
    bnd: &Bound,
    rings: &mut RingManager,
    itr: usize,
    end: usize,
    add_end_point: bool,
) {
    let mut x_min = get_edge_min_x(bnd.ce(), y);
    x_min = x_min.max(end_x);
    let mut x_max = get_edge_max_x(bnd.ce(), y);
    x_max = x_max.min(start_x);
    for k in (end..itr).rev() {
        let hp = rings.hot_pixels[k];
        if hp.x > x_max {
            continue;
        }
        if hp.x < x_min {
            break;
        }
        if !add_end_point && hp.x == end_x {
            continue;
        }
        hot_pixel_insert(hp, bnd, rings);
    }
}

pub(crate) fn sort_hot_pixels(rings: &mut RingManager) {
    // hot_pixel_sorter is a strict total order, and std::unique drops the equal points,
    // so any sort gives the same result as std::sort.
    rings.hot_pixels.sort_unstable_by(|pt1, pt2| pt2.y.cmp(&pt1.y).then(pt1.x.cmp(&pt2.x)));
    rings.hot_pixels.dedup();
}

pub(crate) fn insert_hot_pixels_in_path(
    bnd: BoundPtr,
    end_pt: Point,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    add_end_point: bool,
) {
    if end_pt == bnds[bnd].last_point {
        return;
    }

    let start_y = bnds[bnd].last_point.y;
    let start_x = bnds[bnd].last_point.x;
    let end_y = end_pt.y;
    let end_x = end_pt.x;

    let mut itr = rings.current_hp_itr;
    while rings.hot_pixels[itr].y <= start_y && itr != 0 {
        itr -= 1;
    }
    if start_x > end_x {
        while itr != rings.hot_pixels.len() {
            if rings.hot_pixels[itr].y > start_y {
                itr += 1;
                continue;
            }
            if rings.hot_pixels[itr].y < end_y {
                break;
            }
            let y = rings.hot_pixels[itr].y;
            let last_itr = itr;
            while itr != rings.hot_pixels.len() && rings.hot_pixels[itr].y == y {
                itr += 1;
            }
            let first_itr = itr;
            let add_end_point_itr = y != end_pt.y || add_end_point;
            hot_pixel_set_right_to_left(y, start_x, end_x, &bnds[bnd], rings, first_itr, last_itr, add_end_point_itr);
        }
    } else {
        while itr != rings.hot_pixels.len() {
            if rings.hot_pixels[itr].y > start_y {
                itr += 1;
                continue;
            }
            if rings.hot_pixels[itr].y < end_y {
                break;
            }
            let y = rings.hot_pixels[itr].y;
            let first_itr = itr;
            while itr != rings.hot_pixels.len() && rings.hot_pixels[itr].y == y {
                itr += 1;
            }
            let last_itr = itr;
            let add_end_point_itr = y != end_pt.y || add_end_point;
            hot_pixel_set_left_to_right(y, start_x, end_x, &bnds[bnd], rings, first_itr, last_itr, add_end_point_itr);
        }
    }
    bnds[bnd].last_point = end_pt;
}

pub(crate) fn add_to_hot_pixels(pt: Point, rings: &mut RingManager) {
    rings.hot_pixels.push(pt);
}

fn add_first_point(bnd: BoundPtr, active_bounds: &ActiveBoundList, pt: Point, bnds: &mut [Bound], rings: &mut RingManager) {
    let r = create_new_ring(rings);
    bnds[bnd].ring = r;
    let p = create_new_point(r, pt, rings);
    rings.rings[r].points = p;
    set_hole_state(bnd, active_bounds, bnds, rings);
    bnds[bnd].last_point = pt;
}

pub(crate) fn add_point_to_ring(bnd: BoundPtr, pt: Point, bnds: &mut [Bound], rings: &mut RingManager) {
    debug_assert!(bnds[bnd].ring != NIL);
    // Handle hot pixels
    insert_hot_pixels_in_path(bnd, pt, bnds, rings, false);

    // bnd.ring->points is the 'Left-most' point & bnd.ring->points->prev is the
    // 'Right-most'
    let r = bnds[bnd].ring;
    let op = rings.rings[r].points;
    let to_front = bnds[bnd].side == EdgeSide::Left;
    if to_front && pos_is(&rings.points, pt, op) {
        return;
    } else if !to_front && pos_is(&rings.points, pt, rings.points[op].prev) {
        return;
    }
    let before = rings.rings[r].points;
    let new_point = create_new_point_before(r, pt, before, rings);
    if to_front {
        rings.rings[r].points = new_point;
    }
}

pub(crate) fn add_point(bnd: BoundPtr, active_bounds: &ActiveBoundList, pt: Point, bnds: &mut [Bound], rings: &mut RingManager) {
    if bnds[bnd].ring == NIL {
        add_first_point(bnd, active_bounds, pt, bnds, rings);
    } else {
        add_point_to_ring(bnd, pt, bnds, rings);
    }
}

pub(crate) fn add_local_minimum_point(
    b1: BoundPtr,
    b2: BoundPtr,
    active_bounds: &ActiveBoundList,
    pt: Point,
    bnds: &mut [Bound],
    rings: &mut RingManager,
) {
    if is_horizontal(bnds[b2].ce()) || (bnds[b1].ce().dx > bnds[b2].ce().dx) {
        add_point(b1, active_bounds, pt, bnds, rings);
        bnds[b2].last_point = pt;
        bnds[b2].ring = bnds[b1].ring;
        bnds[b1].side = EdgeSide::Left;
        bnds[b2].side = EdgeSide::Right;
    } else {
        add_point(b2, active_bounds, pt, bnds, rings);
        bnds[b1].last_point = pt;
        bnds[b1].ring = bnds[b2].ring;
        bnds[b1].side = EdgeSide::Right;
        bnds[b2].side = EdgeSide::Left;
    }
}

#[inline]
fn get_dx(pt1: &PointNode, pt2: &PointNode) -> f64 {
    if pt1.y == pt2.y {
        f64::INFINITY
    } else {
        (pt2.x - pt1.x) as f64 / (pt2.y - pt1.y) as f64
    }
}

fn first_is_bottom_point(pts: &[PointNode], btm_pt1: PointPtr, btm_pt2: PointPtr) -> bool {
    let mut p = pts[btm_pt1].prev;
    while same_pos(pts, p, btm_pt1) && (p != btm_pt1) {
        p = pts[p].prev;
    }
    let dx1p = get_dx(&pts[btm_pt1], &pts[p]).abs();

    p = pts[btm_pt1].next;
    while same_pos(pts, p, btm_pt1) && (p != btm_pt1) {
        p = pts[p].next;
    }
    let dx1n = get_dx(&pts[btm_pt1], &pts[p]).abs();

    p = pts[btm_pt2].prev;
    while same_pos(pts, p, btm_pt2) && (p != btm_pt2) {
        p = pts[p].prev;
    }
    let dx2p = get_dx(&pts[btm_pt2], &pts[p]).abs();

    p = pts[btm_pt2].next;
    while same_pos(pts, p, btm_pt2) && (p != btm_pt2) {
        p = pts[p].next;
    }
    let dx2n = get_dx(&pts[btm_pt2], &pts[p]).abs();

    if values_are_equal(std_max(dx1p, dx1n), std_max(dx2p, dx2n))
        && values_are_equal(std_min(dx1p, dx1n), std_min(dx2p, dx2n))
    {
        let mut s = 0;
        let mut bbox = BoundingBox::default();
        area_from_point(pts, btm_pt1, &mut s, &mut bbox) > 0.0 // if otherwise identical use orientation
    } else {
        (greater_than_or_equal(dx1p, dx2p) && greater_than_or_equal(dx1p, dx2n))
            || (greater_than_or_equal(dx1n, dx2p) && greater_than_or_equal(dx1n, dx2n))
    }
}

fn get_bottom_point(pts: &[PointNode], mut pp: PointPtr) -> PointPtr {
    let mut dups: PointPtr = NIL;
    let mut p = pts[pp].next;
    while p != pp {
        if pts[p].y > pts[pp].y {
            pp = p;
            dups = NIL;
        } else if pts[p].y == pts[pp].y && pts[p].x <= pts[pp].x {
            if pts[p].x < pts[pp].x {
                dups = NIL;
                pp = p;
            } else if pts[p].next != pp && pts[p].prev != pp {
                dups = p;
            }
        }
        p = pts[p].next;
    }
    if dups != NIL {
        // there appears to be at least 2 vertices at bottom_point so ...
        while dups != p {
            if !first_is_bottom_point(pts, p, dups) {
                pp = dups;
            }
            dups = pts[dups].next;
            while !same_pos(pts, dups, pp) {
                dups = pts[dups].next;
            }
        }
    }
    pp
}

fn get_lower_most_ring(ring1: RingPtr, ring2: RingPtr, manager: &mut RingManager) -> RingPtr {
    // work out which polygon fragment has the correct hole state ...
    if manager.rings[ring1].bottom_point == NIL {
        manager.rings[ring1].bottom_point = get_bottom_point(&manager.points, manager.rings[ring1].points);
    }
    if manager.rings[ring2].bottom_point == NIL {
        manager.rings[ring2].bottom_point = get_bottom_point(&manager.points, manager.rings[ring2].points);
    }
    let pts = &manager.points;
    let pt1 = manager.rings[ring1].bottom_point;
    let pt2 = manager.rings[ring2].bottom_point;
    if pts[pt1].y > pts[pt2].y {
        ring1
    } else if pts[pt1].y < pts[pt2].y {
        ring2
    } else if pts[pt1].x < pts[pt2].x {
        ring1
    } else if pts[pt1].x > pts[pt2].x {
        ring2
    } else if pts[pt1].next == pt1 {
        ring2
    } else if pts[pt2].next == pt2 || first_is_bottom_point(pts, pt1, pt2) {
        ring1
    } else {
        ring2
    }
}

fn ring1_child_below_ring2(manager: &RingManager, mut ring1: RingPtr, ring2: RingPtr) -> bool {
    loop {
        ring1 = manager.rings[ring1].parent;
        if ring1 == ring2 {
            return true;
        }
        if ring1 == NIL {
            return false;
        }
    }
}

pub(crate) fn update_points_ring(ring: RingPtr, manager: &mut RingManager) {
    let start = manager.rings[ring].points;
    let mut op = start;
    loop {
        manager.points[op].ring = ring;
        op = manager.points[op].prev;
        if op == start {
            break;
        }
    }
}

fn append_ring(b1: BoundPtr, b2: BoundPtr, active_bounds: &ActiveBoundList, bnds: &mut [Bound], manager: &mut RingManager) {
    // get the start and ends of both output polygons ...
    let out_rec1 = bnds[b1].ring;
    let out_rec2 = bnds[b2].ring;

    let keep_ring;
    let keep_bound;
    let remove_ring;
    let remove_bound;
    if ring1_child_below_ring2(manager, out_rec1, out_rec2) {
        keep_ring = out_rec2;
        keep_bound = b2;
        remove_ring = out_rec1;
        remove_bound = b1;
    } else if ring1_child_below_ring2(manager, out_rec2, out_rec1) {
        keep_ring = out_rec1;
        keep_bound = b1;
        remove_ring = out_rec2;
        remove_bound = b2;
    } else if out_rec1 == get_lower_most_ring(out_rec1, out_rec2, manager) {
        keep_ring = out_rec1;
        keep_bound = b1;
        remove_ring = out_rec2;
        remove_bound = b2;
    } else {
        keep_ring = out_rec2;
        keep_bound = b2;
        remove_ring = out_rec1;
        remove_bound = b1;
    }

    // get the start and ends of both output polygons and
    // join b2 poly onto b1 poly and delete pointers to b2 ...

    let pts = &mut manager.points;
    let p1_lft = manager.rings[keep_ring].points;
    let p1_rt = pts[p1_lft].prev;
    let p2_lft = manager.rings[remove_ring].points;
    let p2_rt = pts[p2_lft].prev;

    // join b2 poly onto b1 poly and delete pointers to b2 ...
    if bnds[keep_bound].side == EdgeSide::Left {
        if bnds[remove_bound].side == EdgeSide::Left {
            // z y x a b c
            reverse_ring(pts, p2_lft);
            pts[p2_lft].next = p1_lft;
            pts[p1_lft].prev = p2_lft;
            pts[p1_rt].next = p2_rt;
            pts[p2_rt].prev = p1_rt;
            manager.rings[keep_ring].points = p2_rt;
        } else {
            // x y z a b c
            pts[p2_rt].next = p1_lft;
            pts[p1_lft].prev = p2_rt;
            pts[p2_lft].prev = p1_rt;
            pts[p1_rt].next = p2_lft;
            manager.rings[keep_ring].points = p2_lft;
        }
    } else if bnds[remove_bound].side == EdgeSide::Right {
        // a b c z y x
        reverse_ring(pts, p2_lft);
        pts[p1_rt].next = p2_rt;
        pts[p2_rt].prev = p1_rt;
        pts[p2_lft].next = p1_lft;
        pts[p1_lft].prev = p2_lft;
    } else {
        // a b c x y z
        pts[p1_rt].next = p2_lft;
        pts[p2_lft].prev = p1_rt;
        pts[p1_lft].prev = p2_rt;
        pts[p2_rt].next = p1_lft;
    }

    manager.rings[keep_ring].bottom_point = NIL;
    let keep_is_hole = ring_is_hole(&manager.rings, keep_ring);
    let remove_is_hole = ring_is_hole(&manager.rings, remove_ring);

    manager.rings[remove_ring].points = NIL;
    manager.rings[remove_ring].bottom_point = NIL;
    if keep_is_hole != remove_is_hole {
        let keep_parent = manager.rings[keep_ring].parent;
        ring1_replaces_ring2(keep_parent, remove_ring, manager);
    } else {
        ring1_replaces_ring2(keep_ring, remove_ring, manager);
    }

    update_points_ring(keep_ring, manager);

    // nb: safe because we only get here via AddLocalMaxPoly
    bnds[keep_bound].ring = NIL;
    bnds[remove_bound].ring = NIL;

    for &b in active_bounds.iter() {
        if b == NIL {
            continue;
        }
        if bnds[b].ring == remove_ring {
            bnds[b].ring = keep_ring;
            bnds[b].side = bnds[keep_bound].side;
            break; // Not sure why there is a break here but was transfered logic from angus
        }
    }
}

pub(crate) fn add_local_maximum_point(
    b1: BoundPtr,
    b2: BoundPtr,
    pt: Point,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    active_bounds: &ActiveBoundList,
) {
    insert_hot_pixels_in_path(b2, pt, bnds, rings, false);
    add_point(b1, active_bounds, pt, bnds, rings);
    if bnds[b1].ring == bnds[b2].ring {
        bnds[b1].ring = NIL;
        bnds[b2].ring = NIL;
        // I am not certain that order is important here?
    } else if rings.rings[bnds[b1].ring].ring_index < rings.rings[bnds[b2].ring].ring_index {
        append_ring(b1, b2, active_bounds, bnds, rings);
    } else {
        append_ring(b2, b1, active_bounds, bnds, rings);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointInPolygonResult {
    OnPolygon = -1,
    InsidePolygon = 0,
    OutsidePolygon = 1,
}

use PointInPolygonResult::{InsidePolygon, OnPolygon, OutsidePolygon};

#[inline]
fn toggle(result: PointInPolygonResult) -> PointInPolygonResult {
    // Switch between point outside polygon and point inside
    // polygon
    if result == OutsidePolygon { InsidePolygon } else { OutsidePolygon }
}

fn point_in_polygon(pt: Point, mut op: PointPtr, pts: &[PointNode]) -> PointInPolygonResult {
    // returns 0 if false, +1 if true, -1 if pt ON polygon boundary
    let mut result = OutsidePolygon;
    let start_op = op;
    loop {
        let o = &pts[op];
        let n = &pts[o.next];
        if n.y == pt.y && ((n.x == pt.x) || (o.y == pt.y && ((n.x > pt.x) == (o.x < pt.x)))) {
            return OnPolygon;
        }
        if (o.y < pt.y) != (n.y < pt.y) {
            if o.x >= pt.x {
                if n.x > pt.x {
                    result = toggle(result);
                } else {
                    let d = (o.x - pt.x) as f64 * (n.y - pt.y) as f64 - (n.x - pt.x) as f64 * (o.y - pt.y) as f64;
                    if value_is_zero(d) {
                        return OnPolygon;
                    }
                    if (d > 0.0) == (n.y > o.y) {
                        result = toggle(result);
                    }
                }
            } else if n.x > pt.x {
                let d = (o.x - pt.x) as f64 * (n.y - pt.y) as f64 - (n.x - pt.x) as f64 * (o.y - pt.y) as f64;
                if value_is_zero(d) {
                    return OnPolygon;
                }
                if (d > 0.0) == (n.y > o.y) {
                    result = toggle(result);
                }
            }
        }
        op = o.next;
        if start_op == op {
            break;
        }
    }
    result
}

fn point_in_polygon_f64(pt: PointF64, mut op: PointPtr, pts: &[PointNode]) -> PointInPolygonResult {
    // returns 0 if false, +1 if true, -1 if pt ON polygon boundary
    let mut result = OutsidePolygon;
    let start_op = op;
    loop {
        let o = &pts[op];
        let n = &pts[o.next];
        let op_x = o.x as f64;
        let op_y = o.y as f64;
        let op_next_x = n.x as f64;
        let op_next_y = n.y as f64;
        if values_are_equal(op_next_y, pt.y)
            && (values_are_equal(op_next_x, pt.x)
                || (values_are_equal(op_y, pt.y) && ((op_next_x > pt.x) == (op_x < pt.x))))
        {
            return OnPolygon;
        }
        if (op_y < pt.y) != (op_next_y < pt.y) {
            if greater_than_or_equal(op_x, pt.x) {
                if op_next_x > pt.x {
                    result = toggle(result);
                } else {
                    let d = (op_x - pt.x) * (op_next_y - pt.y) - (op_next_x - pt.x) * (op_y - pt.y);
                    if value_is_zero(d) {
                        return OnPolygon;
                    }
                    if (d > 0.0) == (op_next_y > op_y) {
                        result = toggle(result);
                    }
                }
            } else if op_next_x > pt.x {
                let d = (op_x - pt.x) * (op_next_y - pt.y) - (op_next_x - pt.x) * (op_y - pt.y);
                if value_is_zero(d) {
                    return OnPolygon;
                }
                if (d > 0.0) == (op_next_y > op_y) {
                    result = toggle(result);
                }
            }
        }
        op = o.next;
        if start_op == op {
            break;
        }
    }
    result
}

fn is_convex(edge: PointPtr, manager: &mut RingManager) -> bool {
    let pts = &manager.points;
    let prev = &pts[pts[edge].prev];
    let next = &pts[pts[edge].next];
    let e = &pts[edge];
    let v1x = e.x - prev.x;
    let v1y = e.y - prev.y;
    let v2x = next.x - e.x;
    let v2y = next.y - e.y;
    // PORT: wrapping products, see `util::slopes_equal`.
    let cross = v1x.wrapping_mul(v2y).wrapping_sub(v2x.wrapping_mul(v1y));
    let ring = e.ring;
    if cross < 0 && manager.area(ring) > 0.0 {
        true
    } else {
        cross > 0 && manager.area(ring) < 0.0
    }
}

fn centroid_of_points(edge: PointPtr, pts: &[PointNode]) -> PointF64 {
    let prev = &pts[pts[edge].prev];
    let next = &pts[pts[edge].next];
    let e = &pts[edge];
    PointF64 {
        x: (prev.x + e.x + next.x) as f64 / 3.0,
        y: (prev.y + e.y + next.y) as f64 / 3.0,
    }
}

fn inside_or_outside_special(first_pt: PointPtr, other_poly: PointPtr, manager: &mut RingManager) -> Result<PointInPolygonResult> {
    // We are going to loop through all the points
    // of the original triangle. The goal is to find a convex edge
    // that with its next and previous forms a triangle with its centroid
    // that is within the first ring. Then we will check the other polygon
    // to see if it is within this polygon.
    let mut itr = first_pt;
    loop {
        if is_convex(itr, manager) {
            let pt = centroid_of_points(itr, &manager.points);
            if InsidePolygon == point_in_polygon_f64(pt, first_pt, &manager.points) {
                return Ok(point_in_polygon_f64(pt, other_poly, &manager.points));
            }
        }
        itr = manager.points[itr].next;
        if itr == first_pt {
            break;
        }
    }

    Err(Error::new("Could not find a point within the polygon to test"))
}

fn box2_contains_box1(box1: &BoundingBox, box2: &BoundingBox) -> bool {
    box2.max.x >= box1.max.x && box2.max.y >= box1.max.y && box2.min.x <= box1.min.x && box2.min.y <= box1.min.y
}

pub(crate) fn poly2_contains_poly1(ring1: RingPtr, ring2: RingPtr, manager: &mut RingManager) -> Result<bool> {
    if !box2_contains_box1(&manager.rings[ring1].bbox, &manager.rings[ring2].bbox) {
        return Ok(false);
    }
    if manager.area(ring2).abs() < manager.area(ring1).abs() {
        return Ok(false);
    }
    let pts = &manager.points;
    let outpt1 = pts[manager.rings[ring1].points].next;
    let outpt2 = pts[manager.rings[ring2].points].next;
    let mut op = outpt1;
    loop {
        // nb: PointInPolygon returns 0 if false, +1 if true, -1 if pt on polygon
        let res = point_in_polygon(pts[op].pt(), outpt2, pts);
        if res != OnPolygon {
            return Ok(res == InsidePolygon);
        }
        op = pts[op].next;
        if op == outpt1 {
            break;
        }
    }
    let res = inside_or_outside_special(outpt1, outpt2, manager)?;
    Ok(res == InsidePolygon)
}
