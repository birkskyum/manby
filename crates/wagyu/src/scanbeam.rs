//! Port of `scanbeam.hpp`.

use crate::local_minimum::LocalMinimum;

pub(crate) type ScanbeamList = Vec<i64>;

pub(crate) fn insert_sorted_scanbeam(scanbeam: &mut ScanbeamList, t: i64) {
    let i = scanbeam.partition_point(|&v| v < t);
    if i == scanbeam.len() || t < scanbeam[i] {
        scanbeam.insert(i, t);
    }
}

pub(crate) fn pop_from_scanbeam(y: &mut i64, scanbeam: &mut ScanbeamList) -> bool {
    match scanbeam.pop() {
        Some(v) => {
            *y = v;
            true
        }
        None => false,
    }
}

pub(crate) fn setup_scanbeam(minima_list: &[LocalMinimum], scanbeam: &mut ScanbeamList) {
    scanbeam.reserve(minima_list.len());
    for lm in minima_list {
        scanbeam.push(lm.y);
    }
    // std::sort on integers: any sort gives the same result.
    scanbeam.sort_unstable();
}
