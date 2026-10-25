//! Port of `bound.hpp`.
//!
//! Bounds live in one arena (`Wagyu::bounds`) and are referred to by index. The left bound
//! of local minimum `i` is at `2 * i` and its right bound at `2 * i + 1`.
//! `current_edge` and `next_edge` are indices into `edges`, with `edges.len()` as `end()`.

use crate::config::{EdgeSide, PolygonType};
use crate::edge::Edge;
use crate::geometry::Point;
use crate::point::{NIL, RingPtr};

/// Index of a `Bound` in the bound arena, or `NIL`.
pub(crate) type BoundPtr = usize;

#[derive(Clone, Debug)]
pub(crate) struct Bound {
    pub edges: Vec<Edge>,
    pub current_edge: usize,
    pub next_edge: usize,
    pub last_point: Point,
    pub ring: RingPtr,
    pub maximum_bound: BoundPtr, // the bound who's maximum connects with this bound
    pub current_x: f64,
    pub pos: usize,
    pub winding_count: i32,
    pub winding_count2: i32, // winding count of the opposite polytype
    pub winding_delta: i8,   // 1 or -1 depending on winding direction - 0 for linestrings
    pub poly_type: PolygonType,
    pub side: EdgeSide, // side only refers to current side of solution poly
}

impl Bound {
    pub fn new() -> Bound {
        Bound {
            edges: Vec::new(),
            current_edge: 0,
            next_edge: 0,
            last_point: Point::new(0, 0),
            ring: NIL,
            maximum_bound: NIL,
            current_x: 0.0,
            pos: 0,
            winding_count: 0,
            winding_count2: 0,
            winding_delta: 0,
            poly_type: PolygonType::Subject,
            side: EdgeSide::Left,
        }
    }

    /// `*bnd.current_edge`
    #[inline]
    pub fn ce(&self) -> &Edge {
        &self.edges[self.current_edge]
    }
}
