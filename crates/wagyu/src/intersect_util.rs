//! Port of `intersect_util.hpp`.

use crate::active_bound_list::{ActiveBoundList, is_even_odd_fill_type};
use crate::bound::{Bound, BoundPtr};
use crate::bubble_sort::bubble_sort;
use crate::config::{ClipType, Error, FillType, PolygonType, Result};
use crate::edge::{Edge, get_current_x, slopes_equal_edges};
use crate::geometry::{Point, PointF64};
use crate::intersect::{IntersectList, IntersectNode};
use crate::point::NIL;
use crate::ring::RingManager;
use crate::ring_util::{add_local_maximum_point, add_local_minimum_point, add_point, round_towards_max};
use crate::stable_sort::stable_sort;
use crate::util::values_are_equal;

fn intersect_list_sorter(node1: &IntersectNode, node2: &IntersectNode, bnds: &[Bound]) -> bool {
    if !values_are_equal(node2.pt.y, node1.pt.y) {
        node2.pt.y < node1.pt.y
    } else {
        (bnds[node2.bound1].winding_count2 + bnds[node2.bound2].winding_count2)
            > (bnds[node1.bound1].winding_count2 + bnds[node1.bound2].winding_count2)
    }
}

#[inline]
pub(crate) fn round_point(pt: PointF64) -> Point {
    Point::new(round_towards_max(pt.x), round_towards_max(pt.y))
}

#[inline]
fn swap_rings(b1: BoundPtr, b2: BoundPtr, bnds: &mut [Bound]) {
    let ring = bnds[b1].ring;
    bnds[b1].ring = bnds[b2].ring;
    bnds[b2].ring = ring;
}

#[inline]
fn swap_sides(b1: BoundPtr, b2: BoundPtr, bnds: &mut [Bound]) {
    let side = bnds[b1].side;
    bnds[b1].side = bnds[b2].side;
    bnds[b2].side = side;
}

pub(crate) fn get_edge_intersection(e1: &Edge, e2: &Edge, pt: &mut PointF64) -> bool {
    let p0_x = e1.bot.x as f64;
    let p0_y = e1.bot.y as f64;
    let p1_x = e1.top.x as f64;
    let p1_y = e1.top.y as f64;
    let p2_x = e2.bot.x as f64;
    let p2_y = e2.bot.y as f64;
    let p3_x = e2.top.x as f64;
    let p3_y = e2.top.y as f64;
    let s1_x = p1_x - p0_x;
    let s1_y = p1_y - p0_y;
    let s2_x = p3_x - p2_x;
    let s2_y = p3_y - p2_y;

    let s = (-s1_y * (p0_x - p2_x) + s1_x * (p0_y - p2_y)) / (-s2_x * s1_y + s1_x * s2_y);
    let t = (s2_x * (p0_y - p2_y) - s2_y * (p0_x - p2_x)) / (-s2_x * s1_y + s1_x * s2_y);

    if (0.0..=1.0).contains(&s) && (0.0..=1.0).contains(&t) {
        pt.x = p0_x + (t * s1_x);
        pt.y = p0_y + (t * s1_y);
        return true;
    }
    false
}

pub(crate) fn intersection_compare(b1: &Bound, b2: &Bound) -> bool {
    !(b1.current_x > b2.current_x && !slopes_equal_edges(b1.ce(), b2.ce()))
}

fn build_intersect_list(active_bounds: &mut ActiveBoundList, bnds: &[Bound], intersects: &mut IntersectList) -> Result<()> {
    bubble_sort(
        active_bounds,
        |b1, b2| intersection_compare(&bnds[b1], &bnds[b2]),
        |b1, b2| {
            // on_intersection_swap
            let mut pt = PointF64::default();
            if !get_edge_intersection(bnds[b1].ce(), bnds[b2].ce(), &mut pt) {
                return Err(Error::new("Trying to find intersection of lines that do not intersect"));
            }
            intersects.push(IntersectNode { bound1: b1, bound2: b2, pt });
            Ok(())
        },
    )
}

pub(crate) fn intersect_bounds(
    b1: BoundPtr,
    b2: BoundPtr,
    pt: Point,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
    rings: &mut RingManager,
    active_bounds: &ActiveBoundList,
    bnds: &mut [Bound],
) {
    let b1_contributing = bnds[b1].ring != NIL;
    let b2_contributing = bnds[b2].ring != NIL;

    // update winding counts...
    // assumes that b1 will be to the Right of b2 ABOVE the intersection
    if bnds[b1].poly_type == bnds[b2].poly_type {
        if is_even_odd_fill_type(&bnds[b1], subject_fill_type, clip_fill_type) {
            let wc = bnds[b1].winding_count;
            bnds[b1].winding_count = bnds[b2].winding_count;
            bnds[b2].winding_count = wc;
        } else {
            if bnds[b1].winding_count + bnds[b2].winding_delta as i32 == 0 {
                bnds[b1].winding_count = -bnds[b1].winding_count;
            } else {
                bnds[b1].winding_count += bnds[b2].winding_delta as i32;
            }
            if bnds[b2].winding_count - bnds[b1].winding_delta as i32 == 0 {
                bnds[b2].winding_count = -bnds[b2].winding_count;
            } else {
                bnds[b2].winding_count -= bnds[b1].winding_delta as i32;
            }
        }
    } else {
        if !is_even_odd_fill_type(&bnds[b2], subject_fill_type, clip_fill_type) {
            bnds[b1].winding_count2 += bnds[b2].winding_delta as i32;
        } else {
            bnds[b1].winding_count2 = if bnds[b1].winding_count2 == 0 { 1 } else { 0 };
        }
        if !is_even_odd_fill_type(&bnds[b1], subject_fill_type, clip_fill_type) {
            bnds[b2].winding_count2 -= bnds[b1].winding_delta as i32;
        } else {
            bnds[b2].winding_count2 = if bnds[b2].winding_count2 == 0 { 1 } else { 0 };
        }
    }

    let (b1_fill_type, b1_fill_type2) = if bnds[b1].poly_type == PolygonType::Subject {
        (subject_fill_type, clip_fill_type)
    } else {
        (clip_fill_type, subject_fill_type)
    };
    let (b2_fill_type, b2_fill_type2) = if bnds[b2].poly_type == PolygonType::Subject {
        (subject_fill_type, clip_fill_type)
    } else {
        (clip_fill_type, subject_fill_type)
    };

    let b1_wc = match b1_fill_type {
        FillType::Positive => bnds[b1].winding_count,
        FillType::Negative => -bnds[b1].winding_count,
        FillType::EvenOdd | FillType::NonZero => bnds[b1].winding_count.abs(),
    };
    let b2_wc = match b2_fill_type {
        FillType::Positive => bnds[b2].winding_count,
        FillType::Negative => -bnds[b2].winding_count,
        FillType::EvenOdd | FillType::NonZero => bnds[b2].winding_count.abs(),
    };
    if b1_contributing && b2_contributing {
        if (b1_wc != 0 && b1_wc != 1)
            || (b2_wc != 0 && b2_wc != 1)
            || (bnds[b1].poly_type != bnds[b2].poly_type && cliptype != ClipType::XOr)
        {
            add_local_maximum_point(b1, b2, pt, bnds, rings, active_bounds);
        } else {
            add_point(b1, active_bounds, pt, bnds, rings);
            add_point(b2, active_bounds, pt, bnds, rings);
            swap_sides(b1, b2, bnds);
            swap_rings(b1, b2, bnds);
        }
    } else if b1_contributing {
        if b2_wc == 0 || b2_wc == 1 {
            add_point(b1, active_bounds, pt, bnds, rings);
            bnds[b2].last_point = pt;
            swap_sides(b1, b2, bnds);
            swap_rings(b1, b2, bnds);
        }
    } else if b2_contributing {
        if b1_wc == 0 || b1_wc == 1 {
            bnds[b1].last_point = pt;
            add_point(b2, active_bounds, pt, bnds, rings);
            swap_sides(b1, b2, bnds);
            swap_rings(b1, b2, bnds);
        }
    } else if (b1_wc == 0 || b1_wc == 1) && (b2_wc == 0 || b2_wc == 1) {
        // neither bound is currently contributing ...

        let b1_wc2 = match b1_fill_type2 {
            FillType::Positive => bnds[b1].winding_count2,
            FillType::Negative => -bnds[b1].winding_count2,
            FillType::EvenOdd | FillType::NonZero => bnds[b1].winding_count2.abs(),
        };
        let b2_wc2 = match b2_fill_type2 {
            FillType::Positive => bnds[b2].winding_count2,
            FillType::Negative => -bnds[b2].winding_count2,
            FillType::EvenOdd | FillType::NonZero => bnds[b2].winding_count2.abs(),
        };

        if bnds[b1].poly_type != bnds[b2].poly_type {
            add_local_minimum_point(b1, b2, active_bounds, pt, bnds, rings);
        } else if b1_wc == 1 && b2_wc == 1 {
            match cliptype {
                ClipType::Intersection => {
                    if b1_wc2 > 0 && b2_wc2 > 0 {
                        add_local_minimum_point(b1, b2, active_bounds, pt, bnds, rings);
                    }
                }
                ClipType::Union => {
                    if b1_wc2 <= 0 && b2_wc2 <= 0 {
                        add_local_minimum_point(b1, b2, active_bounds, pt, bnds, rings);
                    }
                }
                ClipType::Difference => {
                    if ((bnds[b1].poly_type == PolygonType::Clip) && (b1_wc2 > 0) && (b2_wc2 > 0))
                        || ((bnds[b1].poly_type == PolygonType::Subject) && (b1_wc2 <= 0) && (b2_wc2 <= 0))
                    {
                        add_local_minimum_point(b1, b2, active_bounds, pt, bnds, rings);
                    }
                }
                ClipType::XOr => {
                    add_local_minimum_point(b1, b2, active_bounds, pt, bnds, rings);
                }
            }
        } else {
            swap_sides(b1, b2, bnds);
        }
    }
}

fn bounds_adjacent(inode: &IntersectNode, next: BoundPtr) -> bool {
    (next == inode.bound2) || (next == inode.bound1)
}

/// `std::find_if(active_bounds, find_first_bound(inode))`
fn find_first_bound(inode: &IntersectNode, active_bounds: &ActiveBoundList) -> usize {
    active_bounds
        .iter()
        .position(|&b| b == inode.bound1 || b == inode.bound2)
        .unwrap_or(active_bounds.len())
}

fn process_intersect_list(
    intersects: &mut IntersectList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
    rings: &mut RingManager,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
) -> Result<()> {
    for node_itr in 0..intersects.len() {
        let mut b1 = find_first_bound(&intersects[node_itr], active_bounds);
        let mut b2 = b1 + 1;
        if !bounds_adjacent(&intersects[node_itr], active_bounds[b2]) {
            let mut next_itr = node_itr + 1;
            while next_itr != intersects.len() {
                let n1 = find_first_bound(&intersects[next_itr], active_bounds);
                let n2 = n1 + 1;
                if bounds_adjacent(&intersects[next_itr], active_bounds[n2]) {
                    b1 = n1;
                    b2 = n2;
                    break;
                }
                next_itr += 1;
            }
            if next_itr == intersects.len() {
                return Err(Error::new("Could not properly correct intersection order."));
            }
            intersects.swap(node_itr, next_itr);
        }
        let node = intersects[node_itr];
        let pt = round_point(node.pt);
        intersect_bounds(
            node.bound1,
            node.bound2,
            pt,
            cliptype,
            subject_fill_type,
            clip_fill_type,
            rings,
            active_bounds,
            bnds,
        );
        active_bounds.swap(b1, b2);
    }
    Ok(())
}

pub(crate) fn update_current_x(active_bounds: &ActiveBoundList, bnds: &mut [Bound], top_y: i64) {
    for (pos, &bnd) in active_bounds.iter().enumerate() {
        bnds[bnd].pos = pos;
        bnds[bnd].current_x = get_current_x(bnds[bnd].ce(), top_y);
    }
}

pub(crate) fn process_intersections(
    top_y: i64,
    active_bounds: &mut ActiveBoundList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
    rings: &mut RingManager,
    bnds: &mut [Bound],
) -> Result<()> {
    if active_bounds.is_empty() {
        return Ok(());
    }
    update_current_x(active_bounds, bnds, top_y);
    let mut intersects: IntersectList = Vec::new();
    build_intersect_list(active_bounds, bnds, &mut intersects)?;

    if intersects.is_empty() {
        return Ok(());
    }

    // Restore order of active bounds list
    stable_sort(active_bounds, true, |&b1, &b2| bnds[b1].pos < bnds[b2].pos);

    // Sort the intersection list
    // intersect_node declares its own move operations, so it is not trivially copy assignable.
    stable_sort(&mut intersects, false, |n1, n2| intersect_list_sorter(n1, n2, bnds));

    process_intersect_list(
        &mut intersects,
        cliptype,
        subject_fill_type,
        clip_fill_type,
        rings,
        active_bounds,
        bnds,
    )
}
