//! Port of `snap_rounding.hpp`.

use crate::active_bound_list::{
    ActiveBoundList, current_edge_is_horizontal, insert_bound_into_abl, next_edge_in_bound,
};
use crate::bound::Bound;
use crate::bubble_sort::bubble_sort;
use crate::config::{Error, Result};
use crate::edge::is_horizontal;
use crate::geometry::{Point, PointF64};
use crate::intersect_util::{
    get_edge_intersection, intersection_compare, round_point, update_current_x,
};
use crate::local_minimum::{LocalMinimum, LocalMinimumPtr, local_minimum_sorter};
use crate::point::NIL;
use crate::ring::RingManager;
use crate::ring_util::{add_to_hot_pixels, sort_hot_pixels};
use crate::scanbeam::{ScanbeamList, insert_sorted_scanbeam, pop_from_scanbeam, setup_scanbeam};
use crate::stable_sort::stable_sort;
use crate::util::wround;

fn process_hot_pixel_intersections(
    top_y: i64,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    manager: &mut RingManager,
) -> Result<()> {
    if active_bounds.is_empty() {
        return Ok(());
    }
    update_current_x(active_bounds, bnds, top_y);
    let bnds: &[Bound] = bnds;
    bubble_sort(
        active_bounds,
        |b1, b2| intersection_compare(&bnds[b1], &bnds[b2]),
        |b1, b2| {
            // hp_intersection_swap
            let mut pt = PointF64::default();
            if !get_edge_intersection(bnds[b1].ce(), bnds[b2].ce(), &mut pt) {
                return Err(Error::new(
                    "Trying to find intersection of lines that do not intersect",
                ));
            }
            add_to_hot_pixels(round_point(pt), manager);
            Ok(())
        },
    )
}

fn horizontals_at_top_scanbeam(
    top_y: i64,
    bnd_curr: &mut usize,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    manager: &mut RingManager,
) -> bool {
    let mut shifted = false;
    let cb = active_bounds[*bnd_curr];
    let current_edge = *bnds[cb].ce();
    bnds[cb].current_x = current_edge.top.x as f64;
    if current_edge.bot.x < current_edge.top.x {
        // left to right
        let mut bnd_next = *bnd_curr + 1;
        while bnd_next != active_bounds.len()
            && (active_bounds[bnd_next] == NIL
                || bnds[active_bounds[bnd_next]].current_x
                    < bnds[active_bounds[*bnd_curr]].current_x)
        {
            let n = active_bounds[bnd_next];
            if n != NIL && bnds[n].ce().top.y != top_y && bnds[n].ce().bot.y != top_y {
                let pt = Point::new(wround(bnds[n].current_x), top_y);
                add_to_hot_pixels(pt, manager);
            }
            active_bounds.swap(*bnd_curr, bnd_next);
            *bnd_curr += 1;
            bnd_next += 1;
            shifted = true;
        }
    } else {
        // right to left
        if *bnd_curr != 0 {
            let mut bnd_prev = *bnd_curr - 1;
            while *bnd_curr != 0
                && (active_bounds[bnd_prev] == NIL
                    || bnds[active_bounds[bnd_prev]].current_x
                        > bnds[active_bounds[*bnd_curr]].current_x)
            {
                let p = active_bounds[bnd_prev];
                if p != NIL && bnds[p].ce().top.y != top_y && bnds[p].ce().bot.y != top_y {
                    let pt = Point::new(wround(bnds[p].current_x), top_y);
                    add_to_hot_pixels(pt, manager);
                }
                active_bounds.swap(*bnd_curr, bnd_prev);
                *bnd_curr -= 1;
                if *bnd_curr != 0 {
                    bnd_prev -= 1;
                }
            }
        }
    }
    shifted
}

fn process_hot_pixel_edges_at_top_of_scanbeam(
    top_y: i64,
    scanbeam: &mut ScanbeamList,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    manager: &mut RingManager,
) {
    let mut bnd = 0;
    while bnd != active_bounds.len() {
        if active_bounds[bnd] == NIL {
            bnd += 1;
            continue;
        }
        let current_bound = active_bounds[bnd];
        let mut bnd_curr = bnd;
        let mut shifted = false;
        while bnds[current_bound].current_edge != bnds[current_bound].edges.len()
            && bnds[current_bound].ce().top.y == top_y
        {
            add_to_hot_pixels(bnds[current_bound].ce().top, manager);
            if is_horizontal(bnds[current_bound].ce())
                && horizontals_at_top_scanbeam(top_y, &mut bnd_curr, active_bounds, bnds, manager)
            {
                shifted = true;
            }
            next_edge_in_bound(&mut bnds[current_bound], scanbeam);
        }
        if bnds[current_bound].current_edge == bnds[current_bound].edges.len() {
            active_bounds[bnd_curr] = NIL;
        }
        if !shifted {
            bnd += 1;
        }
    }
    active_bounds.retain(|&b| b != NIL);
}

fn insert_local_minima_into_abl_hot_pixel(
    top_y: i64,
    minima_sorted: &[LocalMinimumPtr],
    lm: &mut usize,
    minima_list: &[LocalMinimum],
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    manager: &mut RingManager,
    scanbeam: &mut ScanbeamList,
) {
    while *lm != minima_sorted.len() && minima_list[minima_sorted[*lm]].y == top_y {
        let m = &minima_list[minima_sorted[*lm]];
        add_to_hot_pixels(bnds[m.left_bound].edges[0].bot, manager);
        let left_bound = &mut bnds[m.left_bound];
        left_bound.current_edge = 0;
        left_bound.next_edge = left_bound.current_edge + 1;
        left_bound.current_x = left_bound.ce().bot.x as f64;
        let right_bound = &mut bnds[m.right_bound];
        right_bound.current_edge = 0;
        right_bound.next_edge = right_bound.current_edge + 1;
        right_bound.current_x = right_bound.ce().bot.x as f64;
        let lb_abl_itr = insert_bound_into_abl(m.left_bound, m.right_bound, active_bounds, bnds);
        if !current_edge_is_horizontal(&bnds[active_bounds[lb_abl_itr]]) {
            insert_sorted_scanbeam(scanbeam, bnds[active_bounds[lb_abl_itr]].ce().top.y);
        }
        let rb_abl_itr = lb_abl_itr + 1;
        if !current_edge_is_horizontal(&bnds[active_bounds[rb_abl_itr]]) {
            insert_sorted_scanbeam(scanbeam, bnds[active_bounds[rb_abl_itr]].ce().top.y);
        }
        *lm += 1;
    }
}

pub(crate) fn build_hot_pixels(
    minima_list: &[LocalMinimum],
    bnds: &mut [Bound],
    manager: &mut RingManager,
) -> Result<()> {
    let mut active_bounds: ActiveBoundList = Vec::new();
    let mut scanbeam: ScanbeamList = Vec::new();
    let mut scanline_y = i64::MAX;

    let mut minima_sorted: Vec<LocalMinimumPtr> = (0..minima_list.len()).collect();
    stable_sort(&mut minima_sorted, true, |&a, &b| {
        local_minimum_sorter(&minima_list[a], &minima_list[b])
    });
    let mut current_lm = 0;

    setup_scanbeam(minima_list, &mut scanbeam);

    // Estimate size for reserving hot pixels
    let mut reserve = 0;
    for lm in minima_list {
        reserve += bnds[lm.left_bound].edges.len() + 2;
        reserve += bnds[lm.right_bound].edges.len() + 2;
    }
    manager.hot_pixels.reserve(reserve);

    while pop_from_scanbeam(&mut scanline_y, &mut scanbeam) || current_lm != minima_sorted.len() {
        process_hot_pixel_intersections(scanline_y, &mut active_bounds, bnds, manager)?;

        insert_local_minima_into_abl_hot_pixel(
            scanline_y,
            &minima_sorted,
            &mut current_lm,
            minima_list,
            &mut active_bounds,
            bnds,
            manager,
            &mut scanbeam,
        );

        process_hot_pixel_edges_at_top_of_scanbeam(
            scanline_y,
            &mut scanbeam,
            &mut active_bounds,
            bnds,
            manager,
        );
    }
    sort_hot_pixels(manager);
    Ok(())
}
