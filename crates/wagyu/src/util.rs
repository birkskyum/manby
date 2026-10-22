//! Port of `util.hpp`.
//!
//! `area(linear_ring)` is not ported because nothing reachable from `add_ring` or `execute` calls it.

use crate::almost_equal::almost_equals;
use crate::geometry::Point;

pub(crate) fn values_are_equal(x: f64, y: f64) -> bool {
    almost_equals(x, y)
}

pub(crate) fn value_is_zero(val: f64) -> bool {
    values_are_equal(val, 0.0)
}

pub(crate) fn greater_than_or_equal(x: f64, y: f64) -> bool {
    x > y || values_are_equal(x, y)
}

pub(crate) fn greater_than(x: f64, y: f64) -> bool {
    !values_are_equal(x, y) && x > y
}

pub(crate) fn less_than(x: f64, y: f64) -> bool {
    !values_are_equal(x, y) && x < y
}

// PORT: the int64 products overflow for coordinates near 2^34 and beyond. C++ leaves that
// undefined, and the compiled code wraps, so the products wrap here too.
pub(crate) fn slopes_equal(pt1: Point, pt2: Point, pt3: Point) -> bool {
    (pt1.y - pt2.y).wrapping_mul(pt2.x - pt3.x) == (pt1.x - pt2.x).wrapping_mul(pt2.y - pt3.y)
}

/// `std::llround`: round half away from zero.
pub(crate) fn llround(value: f64) -> i64 {
    value.round() as i64
}

pub(crate) fn wround(value: f64) -> i64 {
    llround(value)
}

/// `std::max` on doubles: `(a < b) ? b : a`.
pub(crate) fn std_max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

/// `std::min` on doubles: `(b < a) ? b : a`.
pub(crate) fn std_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}
