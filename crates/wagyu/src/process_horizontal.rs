//! Port of `process_horizontal.hpp`.
//!
//! Iterators into the active bound list and the hot pixels are indices. A reverse iterator is
//! held as the index of the element it points at, with -1 for `rend()`.

use crate::active_bound_list::{ActiveBoundList, current_edge_is_horizontal, get_maxima_pair, is_maxima, next_edge_in_bound};
use crate::bound::Bound;
use crate::config::{ClipType, FillType};
use crate::geometry::Point;
use crate::intersect_util::intersect_bounds;
use crate::point::NIL;
use crate::ring::RingManager;
use crate::ring_util::{add_local_maximum_point, add_point_to_ring};
use crate::scanbeam::ScanbeamList;
use crate::util::{greater_than, less_than, wround};

fn process_horizontal_left_to_right(
    scanline_y: i64,
    horz_bound: &mut usize,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> usize {
    let hb = active_bounds[*horz_bound];
    let mut horizontal_itr_behind = *horz_bound;
    let mut shifted = false;
    let is_maxima_edge = is_maxima(&bnds[hb], scanline_y);
    let mut bound_max_pair = active_bounds.len();
    if is_maxima_edge {
        bound_max_pair = get_maxima_pair(*horz_bound, active_bounds, bnds);
    }

    let mut hp_itr = rings.current_hp_itr;
    while hp_itr != rings.hot_pixels.len()
        && (rings.hot_pixels[hp_itr].y > scanline_y
            || (rings.hot_pixels[hp_itr].y == scanline_y && rings.hot_pixels[hp_itr].x < bnds[hb].ce().bot.x))
    {
        hp_itr += 1;
    }

    let mut bnd = *horz_bound + 1;

    while bnd != active_bounds.len() {
        if active_bounds[bnd] == NIL {
            bnd += 1;
            continue;
        }
        let b = active_bounds[bnd];
        // this code block inserts extra coords into horizontal edges (in output
        // polygons) wherever hot pixels touch these horizontal edges. This helps
        //'simplifying' polygons (ie if the Simplify property is set).
        while hp_itr != rings.hot_pixels.len()
            && rings.hot_pixels[hp_itr].y == scanline_y
            && rings.hot_pixels[hp_itr].x < wround(bnds[b].current_x)
            && rings.hot_pixels[hp_itr].x < bnds[hb].ce().top.x
        {
            if bnds[hb].ring != NIL {
                let hp = rings.hot_pixels[hp_itr];
                add_point_to_ring(hb, hp, bnds, rings);
            }
            hp_itr += 1;
        }

        if greater_than(bnds[b].current_x, bnds[hb].ce().top.x as f64) {
            break;
        }

        // Also break if we've got to the end of an intermediate horizontal edge ...
        // nb: Smaller Dx's are to the right of larger Dx's ABOVE the horizontal.
        if wround(bnds[b].current_x) == bnds[hb].ce().top.x
            && bnds[hb].next_edge != bnds[hb].edges.len()
            && bnds[hb].ce().dx < bnds[hb].edges[bnds[hb].next_edge].dx
        {
            break;
        }

        // note: may be done multiple times
        if bnds[hb].ring != NIL {
            add_point_to_ring(hb, Point::new(wround(bnds[b].current_x), scanline_y), bnds, rings);
        }

        // OK, so far we're still in range of the horizontal Edge  but make sure
        // we're at the last of consec. horizontals when matching with eMaxPair
        if is_maxima_edge && bnd == bound_max_pair {
            let mp = active_bounds[bound_max_pair];
            if bnds[hb].ring != NIL && bnds[mp].ring != NIL {
                let top = bnds[hb].ce().top;
                add_local_maximum_point(hb, mp, top, bnds, rings, active_bounds);
            }
            active_bounds[bound_max_pair] = NIL;
            active_bounds[*horz_bound] = NIL;
            if !shifted {
                horizontal_itr_behind += 1;
            }
            return horizontal_itr_behind;
        }

        intersect_bounds(
            hb,
            b,
            Point::new(wround(bnds[b].current_x), scanline_y),
            cliptype,
            subject_fill_type,
            clip_fill_type,
            rings,
            active_bounds,
            bnds,
        );
        active_bounds.swap(*horz_bound, bnd);
        *horz_bound = bnd;
        bnd += 1;
        shifted = true;
    } // end while (bnd != active_bounds.end())

    if bnds[hb].ring != NIL {
        while hp_itr != rings.hot_pixels.len()
            && rings.hot_pixels[hp_itr].y == scanline_y
            && rings.hot_pixels[hp_itr].x < bnds[hb].ce().top.x
        {
            let hp = rings.hot_pixels[hp_itr];
            add_point_to_ring(hb, hp, bnds, rings);
            hp_itr += 1;
        }
    }

    if bnds[hb].ring != NIL {
        let top = bnds[hb].ce().top;
        add_point_to_ring(hb, top, bnds, rings);
    }

    if bnds[hb].next_edge != bnds[hb].edges.len() {
        next_edge_in_bound(&mut bnds[hb], scanbeam);
    } else {
        active_bounds[*horz_bound] = NIL;
    }
    if !shifted {
        horizontal_itr_behind += 1;
    }
    horizontal_itr_behind
}

fn process_horizontal_right_to_left(
    scanline_y: i64,
    horz_bound_fwd: &mut usize,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> usize {
    let hb = active_bounds[*horz_bound_fwd];
    let next_bnd_itr = *horz_bound_fwd + 1;
    let is_maxima_edge = is_maxima(&bnds[hb], scanline_y);
    // Reverse iterator as the index of the element it points at; -1 is rend().
    let mut bound_max_pair: isize = -1;
    if is_maxima_edge {
        // active_bound_list_rev_itr(get_maxima_pair(...)) followed by -- points at the pair itself.
        bound_max_pair = get_maxima_pair(*horz_bound_fwd, active_bounds, bnds) as isize;
    }
    let mut hp_itr_fwd = rings.current_hp_itr;
    while hp_itr_fwd != rings.hot_pixels.len()
        && (rings.hot_pixels[hp_itr_fwd].y < scanline_y
            || (rings.hot_pixels[hp_itr_fwd].y == scanline_y && rings.hot_pixels[hp_itr_fwd].x < bnds[hb].ce().top.x))
    {
        hp_itr_fwd += 1;
    }
    let mut hp_itr: isize = hp_itr_fwd as isize - 1;

    let mut bnd: isize = *horz_bound_fwd as isize - 1;
    let mut horz_bound: isize = bnd + 1;
    while bnd != -1 {
        let b = active_bounds[bnd as usize];
        if b == NIL {
            bnd -= 1;
            continue;
        }
        // this code block inserts extra coords into horizontal edges (in output
        // polygons) wherever hot pixels touch these horizontal edges.
        while hp_itr != -1
            && rings.hot_pixels[hp_itr as usize].y == scanline_y
            && rings.hot_pixels[hp_itr as usize].x > wround(bnds[b].current_x)
            && rings.hot_pixels[hp_itr as usize].x > bnds[hb].ce().top.x
        {
            if bnds[hb].ring != NIL {
                let hp = rings.hot_pixels[hp_itr as usize];
                add_point_to_ring(hb, hp, bnds, rings);
            }
            hp_itr -= 1;
        }

        if less_than(bnds[b].current_x, bnds[hb].ce().top.x as f64) {
            break;
        }

        // Also break if we've got to the end of an intermediate horizontal edge ...
        // nb: Smaller Dx's are to the right of larger Dx's ABOVE the horizontal.
        if wround(bnds[b].current_x) == bnds[hb].ce().top.x
            && bnds[hb].next_edge != bnds[hb].edges.len()
            && bnds[hb].ce().dx < bnds[hb].edges[bnds[hb].next_edge].dx
        {
            break;
        }

        // note: may be done multiple times
        if bnds[hb].ring != NIL {
            add_point_to_ring(hb, Point::new(wround(bnds[b].current_x), scanline_y), bnds, rings);
        }

        // OK, so far we're still in range of the horizontal Edge  but make sure
        // we're at the last of consec. horizontals when matching with eMaxPair
        if is_maxima_edge && bnd == bound_max_pair {
            let mp = active_bounds[bound_max_pair as usize];
            if bnds[hb].ring != NIL && bnds[mp].ring != NIL {
                let top = bnds[hb].ce().top;
                add_local_maximum_point(hb, mp, top, bnds, rings, active_bounds);
            }
            active_bounds[bound_max_pair as usize] = NIL;
            active_bounds[horz_bound as usize] = NIL;
            return next_bnd_itr;
        }

        intersect_bounds(
            b,
            hb,
            Point::new(wround(bnds[b].current_x), scanline_y),
            cliptype,
            subject_fill_type,
            clip_fill_type,
            rings,
            active_bounds,
            bnds,
        );
        active_bounds.swap(horz_bound as usize, bnd as usize);
        horz_bound = bnd;
        bnd -= 1;
    } // end while (bnd != active_bounds.rend())

    if bnds[hb].ring != NIL {
        while hp_itr != -1
            && rings.hot_pixels[hp_itr as usize].y == scanline_y
            && rings.hot_pixels[hp_itr as usize].x > bnds[hb].ce().top.x
        {
            let hp = rings.hot_pixels[hp_itr as usize];
            add_point_to_ring(hb, hp, bnds, rings);
            hp_itr -= 1;
        }
    }
    if bnds[hb].ring != NIL {
        let top = bnds[hb].ce().top;
        add_point_to_ring(hb, top, bnds, rings);
    }

    if bnds[hb].next_edge != bnds[hb].edges.len() {
        next_edge_in_bound(&mut bnds[hb], scanbeam);
    } else {
        active_bounds[horz_bound as usize] = NIL;
    }
    next_bnd_itr
}

fn process_horizontal(
    scanline_y: i64,
    horz_bound: &mut usize,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> usize {
    let hb = active_bounds[*horz_bound];
    if bnds[hb].ce().bot.x < bnds[hb].ce().top.x {
        process_horizontal_left_to_right(
            scanline_y,
            horz_bound,
            active_bounds,
            bnds,
            rings,
            scanbeam,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        )
    } else {
        process_horizontal_right_to_left(
            scanline_y,
            horz_bound,
            active_bounds,
            bnds,
            rings,
            scanbeam,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        )
    }
}

pub(crate) fn process_horizontals(
    scanline_y: i64,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) {
    let mut bnd_itr = 0;
    while bnd_itr != active_bounds.len() {
        if active_bounds[bnd_itr] != NIL && current_edge_is_horizontal(&bnds[active_bounds[bnd_itr]]) {
            let mut horz = bnd_itr;
            bnd_itr = process_horizontal(
                scanline_y,
                &mut horz,
                active_bounds,
                bnds,
                rings,
                scanbeam,
                cliptype,
                subject_fill_type,
                clip_fill_type,
            );
        } else {
            bnd_itr += 1;
        }
    }
    active_bounds.retain(|&b| b != NIL);
}
