//! Port of `point.hpp`.
//!
//! The C++ links points and rings with raw pointers. Here points and rings live in arenas
//! in the ring manager and refer to each other by index, with `NIL` standing in for `nullptr`.

use crate::geometry::Point;

/// Index of a `PointNode` in `RingManager::points`, or `NIL`.
pub(crate) type PointPtr = usize;

/// Index of a `Ring` in `RingManager::rings`, or `NIL`.
pub(crate) type RingPtr = usize;

/// The null pointer for `PointPtr`, `RingPtr` and `BoundPtr`.
pub(crate) const NIL: usize = usize::MAX;

/// `mapbox::geometry::wagyu::point<long long>`
#[derive(Clone, Copy, Debug)]
pub(crate) struct PointNode {
    pub ring: RingPtr,
    pub x: i64,
    pub y: i64,
    pub next: PointPtr,
    pub prev: PointPtr,
}

impl PointNode {
    pub fn pt(&self) -> Point {
        Point::new(self.x, self.y)
    }
}

/// `*a == *b` for two point pointers: compares positions.
#[inline]
pub(crate) fn same_pos(points: &[PointNode], a: PointPtr, b: PointPtr) -> bool {
    points[a].x == points[b].x && points[a].y == points[b].y
}

/// `geometry_point == *op`
#[inline]
pub(crate) fn pos_is(points: &[PointNode], p: Point, op: PointPtr) -> bool {
    p.x == points[op].x && p.y == points[op].y
}
