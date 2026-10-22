//! Port of `vatti.hpp`.

use crate::active_bound_list::{ActiveBoundList, insert_local_minima_into_abl};
use crate::bound::Bound;
use crate::config::{ClipType, FillType, Result};
use crate::intersect_util::process_intersections;
use crate::local_minimum::{LocalMinimum, LocalMinimumPtr, local_minimum_sorter};
use crate::process_maxima::process_edges_at_top_of_scanbeam;
use crate::ring::RingManager;
use crate::ring_util::update_current_hp_itr;
use crate::scanbeam::{ScanbeamList, pop_from_scanbeam, setup_scanbeam};
use crate::stable_sort::stable_sort;

pub(crate) fn execute_vatti(
    minima_list: &[LocalMinimum],
    bnds: &mut [Bound],
    manager: &mut RingManager,
    cliptype: ClipType,
    subject_fill_type: FillType,
    clip_fill_type: FillType,
) -> Result<()> {
    let mut active_bounds: ActiveBoundList = Vec::new();
    let mut scanbeam: ScanbeamList = Vec::new();
    let mut scanline_y = i64::MAX;

    let mut minima_sorted: Vec<LocalMinimumPtr> = (0..minima_list.len()).collect();
    stable_sort(&mut minima_sorted, true, |&a, &b| local_minimum_sorter(&minima_list[a], &minima_list[b]));
    let mut current_lm = 0;

    setup_scanbeam(minima_list, &mut scanbeam);
    manager.current_hp_itr = 0;

    while pop_from_scanbeam(&mut scanline_y, &mut scanbeam) || current_lm != minima_sorted.len() {
        process_intersections(
            scanline_y,
            &mut active_bounds,
            cliptype,
            subject_fill_type,
            clip_fill_type,
            manager,
            bnds,
        )?;

        update_current_hp_itr(scanline_y, manager);

        // First we process bounds that has already been added to the active bound list --
        // if the active bound list is empty local minima that are at this scanline_y and
        // have a horizontal edge at the local minima will be processed
        process_edges_at_top_of_scanbeam(
            scanline_y,
            &mut active_bounds,
            &mut scanbeam,
            &minima_sorted,
            &mut current_lm,
            minima_list,
            manager,
            bnds,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        )?;

        // Next we will add local minima bounds to the active bounds list that are on the local
        // minima queue at
        // this current scanline_y
        insert_local_minima_into_abl(
            scanline_y,
            &minima_sorted,
            &mut current_lm,
            minima_list,
            &mut active_bounds,
            bnds,
            manager,
            &mut scanbeam,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        );
    }
    Ok(())
}
