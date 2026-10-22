//! Port of `intersect.hpp`.

use crate::bound::BoundPtr;
use crate::geometry::PointF64;

#[derive(Clone, Copy, Debug)]
pub(crate) struct IntersectNode {
    pub bound1: BoundPtr,
    pub bound2: BoundPtr,
    pub pt: PointF64,
}

pub(crate) type IntersectList = Vec<IntersectNode>;
