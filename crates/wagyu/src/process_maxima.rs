//! Port of `process_maxima.hpp`.

use crate::active_bound_list::{
    ActiveBoundList, current_edge_is_horizontal, get_maxima_pair, insert_horizontal_local_minima_into_abl,
    is_intermediate, is_maxima, next_edge_in_bound, next_edge_is_horizontal,
};
use crate::bound::Bound;
use crate::config::{ClipType, Error, FillType, Result};
use crate::edge::get_current_x;
use crate::intersect_util::intersect_bounds;
use crate::local_minimum::{LocalMinimum, LocalMinimumPtr};
use crate::point::NIL;
use crate::process_horizontal::process_horizontals;
use crate::ring::RingManager;
use crate::ring_util::{add_local_maximum_point, add_point_to_ring, insert_hot_pixels_in_path};
use crate::scanbeam::ScanbeamList;

fn do_maxima(
    bnd: &mut usize,
    bnd_max_pair: usize,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
    manager: &mut RingManager,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
) -> Result<usize> {
    let mut bnd_next = *bnd + 1;
    let mut return_bnd = *bnd;
    let mut skipped = false;
    while bnd_next != active_bounds.len() && bnd_next != bnd_max_pair {
        if active_bounds[bnd_next] == NIL {
            bnd_next += 1;
            continue;
        }
        skipped = true;
        let b = active_bounds[*bnd];
        let top = bnds[b].ce().top;
        intersect_bounds(
            b,
            active_bounds[bnd_next],
            top,
            cliptype,
            subject_fill_type,
            clip_fill_type,
            manager,
            active_bounds,
            bnds,
        );
        active_bounds.swap(*bnd, bnd_next);
        *bnd = bnd_next;
        bnd_next += 1;
    }

    let b = active_bounds[*bnd];
    let mp = active_bounds[bnd_max_pair];
    if bnds[b].ring != NIL && bnds[mp].ring != NIL {
        let top = bnds[b].ce().top;
        add_local_maximum_point(b, mp, top, bnds, manager, active_bounds);
    } else if bnds[b].ring != NIL || bnds[mp].ring != NIL {
        return Err(Error::new("DoMaxima error"));
    }
    active_bounds[bnd_max_pair] = NIL;
    active_bounds[*bnd] = NIL;
    if !skipped {
        return_bnd += 1;
    }
    Ok(return_bnd)
}

pub(crate) fn process_edges_at_top_of_scanbeam(
    top_y: i64,
    active_bounds: &mut ActiveBoundList,
    scanbeam: &mut ScanbeamList,
    minima_sorted: &[LocalMinimumPtr],
    current_lm: &mut usize,
    minima_list: &[LocalMinimum],
    manager: &mut RingManager,
    bnds: &mut [Bound],
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> Result<()> {
    let mut bnd = 0;
    while bnd != active_bounds.len() {
        if active_bounds[bnd] == NIL {
            bnd += 1;
            continue;
        }
        // 1. Process maxima, treating them as if they are "bent" horizontal edges,
        // but exclude maxima with horizontal edges.

        let mut is_maxima_edge = is_maxima(&bnds[active_bounds[bnd]], top_y);

        if is_maxima_edge {
            let bnd_max_pair = get_maxima_pair(bnd, active_bounds, bnds);
            // PORT: when the pair is missing (`end()`), the C++ dereferences `end()`. That is
            // undefined, so this treats the bound as not being at a maximum.
            is_maxima_edge = bnd_max_pair != active_bounds.len()
                && !current_edge_is_horizontal(&bnds[active_bounds[bnd_max_pair]])
                && is_maxima(&bnds[active_bounds[bnd_max_pair]], top_y);
            if is_maxima_edge {
                bnd = do_maxima(
                    &mut bnd,
                    bnd_max_pair,
                    cliptype,
                    subject_fill_type,
                    clip_fill_type,
                    manager,
                    active_bounds,
                    bnds,
                )?;
                continue;
            }
        }

        // 2. Promote horizontal edges.
        let b = active_bounds[bnd];
        if is_intermediate(&bnds[b], top_y) && next_edge_is_horizontal(&bnds[b]) {
            if bnds[b].ring != NIL {
                let top = bnds[b].ce().top;
                insert_hot_pixels_in_path(b, top, bnds, manager, false);
            }
            next_edge_in_bound(&mut bnds[b], scanbeam);
            if bnds[b].ring != NIL {
                let bot = bnds[b].ce().bot;
                add_point_to_ring(b, bot, bnds, manager);
            }
        } else {
            bnds[b].current_x = get_current_x(bnds[b].ce(), top_y);
        }
        bnd += 1;
    }
    active_bounds.retain(|&b| b != NIL);

    insert_horizontal_local_minima_into_abl(
        top_y,
        minima_sorted,
        current_lm,
        minima_list,
        active_bounds,
        bnds,
        manager,
        scanbeam,
        cliptype,
        subject_fill_type,
        clip_fill_type,
    );

    process_horizontals(
        top_y,
        active_bounds,
        bnds,
        manager,
        scanbeam,
        cliptype,
        subject_fill_type,
        clip_fill_type,
    );

    // 4. Promote intermediate vertices

    for bnd in 0..active_bounds.len() {
        let b = active_bounds[bnd];
        if is_intermediate(&bnds[b], top_y) {
            if bnds[b].ring != NIL {
                let top = bnds[b].ce().top;
                add_point_to_ring(b, top, bnds, manager);
            }
            next_edge_in_bound(&mut bnds[b], scanbeam);
        }
    }
    Ok(())
}
