//! Port of `active_bound_list.hpp`.
//!
//! The active bound list is a `Vec<BoundPtr>` where `NIL` stands for the `nullptr` entries
//! the C++ leaves behind. Iterators into it are indices.

use crate::bound::{Bound, BoundPtr};
use crate::config::{ClipType, FillType, PolygonType};
use crate::edge::{get_current_x, is_horizontal};
use crate::local_minimum::{LocalMinimum, LocalMinimumPtr};
use crate::local_minimum_util::initialize_lm;
use crate::ring::RingManager;
use crate::ring_util::add_local_minimum_point;
use crate::scanbeam::{ScanbeamList, insert_sorted_scanbeam};
use crate::util::{greater_than, less_than, values_are_equal};

pub(crate) type ActiveBoundList = Vec<BoundPtr>;

pub(crate) fn is_even_odd_fill_type(
    bound: &Bound,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> bool {
    if bound.poly_type == PolygonType::Subject {
        subject_fill_type == FillType::EvenOdd
    } else {
        clip_fill_type == FillType::EvenOdd
    }
}

pub(crate) fn is_even_odd_alt_fill_type(
    bound: &Bound,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> bool {
    if bound.poly_type == PolygonType::Subject {
        clip_fill_type == FillType::EvenOdd
    } else {
        subject_fill_type == FillType::EvenOdd
    }
}

fn bound_insert_location(bound2: &Bound, bound1: &Bound) -> bool {
    if values_are_equal(bound2.current_x, bound1.current_x) {
        if bound2.ce().top.y > bound1.ce().top.y {
            less_than(
                bound2.ce().top.x as f64,
                get_current_x(bound1.ce(), bound2.ce().top.y),
            )
        } else {
            greater_than(
                bound1.ce().top.x as f64,
                get_current_x(bound2.ce(), bound1.ce().top.y),
            )
        }
    } else {
        bound2.current_x < bound1.current_x
    }
}

pub(crate) fn insert_bound_into_abl(
    left: BoundPtr,
    right: BoundPtr,
    active_bounds: &mut ActiveBoundList,
    bnds: &[Bound],
) -> usize {
    let itr = active_bounds
        .iter()
        .position(|&b| bound_insert_location(&bnds[left], &bnds[b]))
        .unwrap_or(active_bounds.len());
    active_bounds.splice(itr..itr, [left, right]);
    itr
}

#[inline]
pub(crate) fn is_maxima(bnd: &Bound, y: i64) -> bool {
    bnd.next_edge == bnd.edges.len() && bnd.ce().top.y == y
}

#[inline]
pub(crate) fn is_intermediate(bnd: &Bound, y: i64) -> bool {
    bnd.next_edge != bnd.edges.len() && bnd.ce().top.y == y
}

#[inline]
pub(crate) fn current_edge_is_horizontal(bnd: &Bound) -> bool {
    is_horizontal(bnd.ce())
}

#[inline]
pub(crate) fn next_edge_is_horizontal(bnd: &Bound) -> bool {
    is_horizontal(&bnd.edges[bnd.next_edge])
}

pub(crate) fn next_edge_in_bound(bnd: &mut Bound, scanbeam: &mut ScanbeamList) {
    bnd.current_edge += 1;
    if bnd.current_edge != bnd.edges.len() {
        bnd.next_edge += 1;
        bnd.current_x = bnd.ce().bot.x as f64;
        if !is_horizontal(bnd.ce()) {
            insert_sorted_scanbeam(scanbeam, bnd.ce().top.y);
        }
    }
}

pub(crate) fn get_maxima_pair(
    bnd: usize,
    active_bounds: &ActiveBoundList,
    bnds: &[Bound],
) -> usize {
    let maximum = bnds[active_bounds[bnd]].maximum_bound;
    active_bounds
        .iter()
        .position(|&b| b == maximum)
        .unwrap_or(active_bounds.len())
}

fn set_winding_count(
    bnd_itr: usize,
    active_bounds: &ActiveBoundList,
    bnds: &mut [Bound],
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) {
    let b = active_bounds[bnd_itr];
    // `rev_bnd_itr` is the index of the element the reverse iterator points at; -1 is rend().
    let mut rev_bnd_itr: isize = bnd_itr as isize - 1;
    if rev_bnd_itr < 0 {
        bnds[b].winding_count = bnds[b].winding_delta as i32;
        bnds[b].winding_count2 = 0;
        return;
    }

    // find the edge of the same polytype that immediately preceeds 'edge' in
    // AEL
    while rev_bnd_itr >= 0
        && bnds[active_bounds[rev_bnd_itr as usize]].poly_type != bnds[b].poly_type
    {
        rev_bnd_itr -= 1;
    }
    if rev_bnd_itr < 0 {
        bnds[b].winding_count = bnds[b].winding_delta as i32;
        bnds[b].winding_count2 = 0;
    } else if is_even_odd_fill_type(&bnds[b], subject_fill_type, clip_fill_type) {
        // EvenOdd filling ...
        let rev = active_bounds[rev_bnd_itr as usize];
        bnds[b].winding_count = bnds[b].winding_delta as i32;
        bnds[b].winding_count2 = bnds[rev].winding_count2;
    } else {
        let rev = active_bounds[rev_bnd_itr as usize];
        let rev_wc = bnds[rev].winding_count;
        let rev_wd = bnds[rev].winding_delta as i32;
        let wd = bnds[b].winding_delta as i32;
        // nonZero, Positive or Negative filling ...
        if rev_wc * rev_wd < 0 {
            // prev edge is 'decreasing' WindCount (WC) toward zero
            // so we're outside the previous polygon ...
            if rev_wc.abs() > 1 {
                // outside prev poly but still inside another.
                // when reversing direction of prev poly use the same WC
                if rev_wd * wd < 0 {
                    bnds[b].winding_count = rev_wc;
                } else {
                    // otherwise continue to 'decrease' WC ...
                    bnds[b].winding_count = rev_wc + wd;
                }
            } else {
                // now outside all polys of same polytype so set own WC ...
                bnds[b].winding_count = wd;
            }
        } else {
            // prev edge is 'increasing' WindCount (WC) away from zero
            // so we're inside the previous polygon ...
            if rev_wd * wd < 0 {
                // if wind direction is reversing prev then use same WC
                bnds[b].winding_count = rev_wc;
            } else {
                // otherwise add to WC ...
                bnds[b].winding_count = rev_wc + wd;
            }
        }
        bnds[b].winding_count2 = bnds[rev].winding_count2;
    }

    // update winding_count2 ...
    let mut bnd_itr_forward = (rev_bnd_itr + 1) as usize;
    if is_even_odd_alt_fill_type(&bnds[b], subject_fill_type, clip_fill_type) {
        // EvenOdd filling ...
        while bnd_itr_forward != bnd_itr {
            bnds[b].winding_count2 = if bnds[b].winding_count2 == 0 { 1 } else { 0 };
            bnd_itr_forward += 1;
        }
    } else {
        // nonZero, Positive or Negative filling ...
        while bnd_itr_forward != bnd_itr {
            bnds[b].winding_count2 += bnds[active_bounds[bnd_itr_forward]].winding_delta as i32;
            bnd_itr_forward += 1;
        }
    }
}

pub(crate) fn is_contributing(
    bnd: &Bound,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> bool {
    let mut pft = subject_fill_type;
    let mut pft2 = clip_fill_type;
    if bnd.poly_type != PolygonType::Subject {
        pft = clip_fill_type;
        pft2 = subject_fill_type;
    }

    match pft {
        FillType::EvenOdd => {}
        FillType::NonZero => {
            if bnd.winding_count.abs() != 1 {
                return false;
            }
        }
        FillType::Positive => {
            if bnd.winding_count != 1 {
                return false;
            }
        }
        FillType::Negative => {
            if bnd.winding_count != -1 {
                return false;
            }
        }
    }

    match cliptype {
        ClipType::Intersection => match pft2 {
            FillType::EvenOdd | FillType::NonZero => bnd.winding_count2 != 0,
            FillType::Positive => bnd.winding_count2 > 0,
            FillType::Negative => bnd.winding_count2 < 0,
        },
        ClipType::Union => match pft2 {
            FillType::EvenOdd | FillType::NonZero => bnd.winding_count2 == 0,
            FillType::Positive => bnd.winding_count2 <= 0,
            FillType::Negative => bnd.winding_count2 >= 0,
        },
        ClipType::Difference => {
            if bnd.poly_type == PolygonType::Subject {
                match pft2 {
                    FillType::EvenOdd | FillType::NonZero => bnd.winding_count2 == 0,
                    FillType::Positive => bnd.winding_count2 <= 0,
                    FillType::Negative => bnd.winding_count2 >= 0,
                }
            } else {
                match pft2 {
                    FillType::EvenOdd | FillType::NonZero => bnd.winding_count2 != 0,
                    FillType::Positive => bnd.winding_count2 > 0,
                    FillType::Negative => bnd.winding_count2 < 0,
                }
            }
        }
        ClipType::XOr => true,
    }
}

fn insert_lm_left_and_right_bound(
    left_bound: BoundPtr,
    right_bound: BoundPtr,
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) {
    // Both left and right bound
    let lb_abl_itr = insert_bound_into_abl(left_bound, right_bound, active_bounds, bnds);
    let rb_abl_itr = lb_abl_itr + 1;
    set_winding_count(
        lb_abl_itr,
        active_bounds,
        bnds,
        subject_fill_type,
        clip_fill_type,
    );
    let lb = active_bounds[lb_abl_itr];
    let rb = active_bounds[rb_abl_itr];
    bnds[rb].winding_count = bnds[lb].winding_count;
    bnds[rb].winding_count2 = bnds[lb].winding_count2;
    if is_contributing(
        &bnds[left_bound],
        cliptype,
        subject_fill_type,
        clip_fill_type,
    ) {
        let pt = bnds[lb].ce().bot;
        add_local_minimum_point(lb, rb, active_bounds, pt, bnds, rings);
    }

    // Add top of edges to scanbeam
    insert_sorted_scanbeam(scanbeam, bnds[lb].ce().top.y);

    if !current_edge_is_horizontal(&bnds[rb]) {
        insert_sorted_scanbeam(scanbeam, bnds[rb].ce().top.y);
    }
}

pub(crate) fn insert_local_minima_into_abl(
    bot_y: i64,
    minima_sorted: &[LocalMinimumPtr],
    current_lm: &mut usize,
    minima_list: &[LocalMinimum],
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) {
    while *current_lm != minima_sorted.len() && bot_y == minima_list[minima_sorted[*current_lm]].y {
        let lm = &minima_list[minima_sorted[*current_lm]];
        initialize_lm(lm, bnds);
        insert_lm_left_and_right_bound(
            lm.left_bound,
            lm.right_bound,
            active_bounds,
            bnds,
            rings,
            scanbeam,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        );
        *current_lm += 1;
    }
}

pub(crate) fn insert_horizontal_local_minima_into_abl(
    top_y: i64,
    minima_sorted: &[LocalMinimumPtr],
    current_lm: &mut usize,
    minima_list: &[LocalMinimum],
    active_bounds: &mut ActiveBoundList,
    bnds: &mut [Bound],
    rings: &mut RingManager,
    scanbeam: &mut ScanbeamList,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) {
    while *current_lm != minima_sorted.len()
        && top_y == minima_list[minima_sorted[*current_lm]].y
        && minima_list[minima_sorted[*current_lm]].minimum_has_horizontal
    {
        let lm = &minima_list[minima_sorted[*current_lm]];
        initialize_lm(lm, bnds);
        insert_lm_left_and_right_bound(
            lm.left_bound,
            lm.right_bound,
            active_bounds,
            bnds,
            rings,
            scanbeam,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        );
        *current_lm += 1;
    }
}
