//! Port of `local_minimum.hpp`.

use crate::bound::BoundPtr;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalMinimum {
    pub left_bound: BoundPtr,
    pub right_bound: BoundPtr,
    pub y: i64,
    pub minimum_has_horizontal: bool,
}

/// Index of a `LocalMinimum` in the minima list.
pub(crate) type LocalMinimumPtr = usize;

pub(crate) fn local_minimum_sorter(loc_min1: &LocalMinimum, loc_min2: &LocalMinimum) -> bool {
    if loc_min2.y == loc_min1.y {
        return loc_min2.minimum_has_horizontal != loc_min1.minimum_has_horizontal
            && loc_min1.minimum_has_horizontal;
    }
    loc_min2.y < loc_min1.y
}
