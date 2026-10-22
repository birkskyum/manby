/*******************************************************************************
* Author    :  Angus Johnson                                                   *
* Date      :  12 May 2024                                                     *
* Website   :  http://www.angusj.com                                           *
* Copyright :  Angus Johnson 2010-2024                                         *
* Purpose   :  Core Clipper Library structures and functions                   *
* License   :  http://www.boost.org/LICENSE_1_0.txt                            *
*******************************************************************************/

//! The parts of `clipper.core.h` that the `Clipper64` engine uses, for `int64_t` points only.

//By far the most widely used filling rules for polygons are EvenOdd
//and NonZero, sometimes called Alternate and Winding respectively.
//https://en.wikipedia.org/wiki/Nonzero-rule
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    EvenOdd,
    NonZero,
    Positive,
    Negative,
}

// Point ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point64 {
    pub x: i64,
    pub y: i64,
}

impl Point64 {
    pub const fn new(x: i64, y: i64) -> Self {
        Point64 { x, y }
    }
}

pub type Path64 = Vec<Point64>;
pub type Paths64 = Vec<Path64>;

#[inline]
pub(crate) fn sqr(val: f64) -> f64 {
    val * val
}

// returns true if (and only if) a * b == c * d
#[inline]
pub(crate) fn products_are_equal(a: i64, b: i64, c: i64, d: i64) -> bool {
    let ab = a as i128 * b as i128;
    let cd = c as i128 * d as i128;
    ab == cd
}

#[inline]
pub(crate) fn is_collinear(pt1: Point64, shared_pt: Point64, pt2: Point64) -> bool {
    // #777
    let a = shared_pt.x - pt1.x;
    let b = pt2.y - shared_pt.y;
    let c = shared_pt.y - pt1.y;
    let d = pt2.x - shared_pt.x;
    // When checking for collinearity with very large coordinate values
    // then ProductsAreEqual is more accurate than using CrossProduct.
    products_are_equal(a, b, c, d)
}

#[inline]
pub(crate) fn cross_product(pt1: Point64, pt2: Point64, pt3: Point64) -> f64 {
    (pt2.x - pt1.x) as f64 * (pt3.y - pt2.y) as f64
        - (pt2.y - pt1.y) as f64 * (pt3.x - pt2.x) as f64
}

#[inline]
pub(crate) fn dot_product(pt1: Point64, pt2: Point64, pt3: Point64) -> f64 {
    (pt2.x - pt1.x) as f64 * (pt3.x - pt2.x) as f64
        + (pt2.y - pt1.y) as f64 * (pt3.y - pt2.y) as f64
}

#[inline]
pub(crate) fn perpendic_dist_from_line_sqrd(pt: Point64, line1: Point64, line2: Point64) -> f64 {
    //perpendicular distance of point (x³,y³) = (Ax³ + By³ + C)/Sqrt(A² + B²)
    //see http://en.wikipedia.org/wiki/Perpendicular_distance
    let a = (pt.x - line1.x) as f64;
    let b = (pt.y - line1.y) as f64;
    let c = (line2.x - line1.x) as f64;
    let d = (line2.y - line1.y) as f64;
    if c == 0.0 && d == 0.0 {
        return 0.0;
    }
    sqr(a * d - c * b) / (c * c + d * d)
}

// PORT: CLIPPER2_HI_PRECISION is not defined, so this is the plain version.
#[inline]
pub(crate) fn get_segment_intersect_pt(
    ln1a: Point64,
    ln1b: Point64,
    ln2a: Point64,
    ln2b: Point64,
    ip: &mut Point64,
) -> bool {
    // https://en.wikipedia.org/wiki/Line%E2%80%93line_intersection
    let dx1 = (ln1b.x - ln1a.x) as f64;
    let dy1 = (ln1b.y - ln1a.y) as f64;
    let dx2 = (ln2b.x - ln2a.x) as f64;
    let dy2 = (ln2b.y - ln2a.y) as f64;

    let det = dy1 * dx2 - dy2 * dx1;
    if det == 0.0 {
        return false;
    }
    let t = ((ln1a.x - ln2a.x) as f64 * dy2 - (ln1a.y - ln2a.y) as f64 * dx2) / det;
    if t <= 0.0 {
        *ip = ln1a;
    } else if t >= 1.0 {
        *ip = ln1b;
    } else {
        ip.x = (ln1a.x as f64 + t * dx1) as i64;
        ip.y = (ln1a.y as f64 + t * dy1) as i64;
    }
    true
}

#[inline]
pub(crate) fn get_sign(val: f64) -> i32 {
    if val == 0.0 {
        return 0;
    }
    if val > 0.0 { 1 } else { -1 }
}

#[inline]
pub(crate) fn segments_intersect(
    seg1a: Point64,
    seg1b: Point64,
    seg2a: Point64,
    seg2b: Point64,
    inclusive: bool,
) -> bool {
    if inclusive {
        let res1 = cross_product(seg1a, seg2a, seg2b);
        let res2 = cross_product(seg1b, seg2a, seg2b);
        if res1 * res2 > 0.0 {
            return false;
        }
        let res3 = cross_product(seg2a, seg1a, seg1b);
        let res4 = cross_product(seg2b, seg1a, seg1b);
        if res3 * res4 > 0.0 {
            return false;
        }
        res1 != 0.0 || res2 != 0.0 || res3 != 0.0 || res4 != 0.0 // ensures not collinear
    } else {
        (get_sign(cross_product(seg1a, seg2a, seg2b))
            * get_sign(cross_product(seg1b, seg2a, seg2b))
            < 0)
            && (get_sign(cross_product(seg2a, seg1a, seg1b))
                * get_sign(cross_product(seg2b, seg1a, seg1b))
                < 0)
    }
}

// PORT: nearbyint rounds half to even in the default rounding mode, hence round_ties_even.
#[inline]
pub(crate) fn get_closest_point_on_segment(
    off_pt: Point64,
    seg1: Point64,
    seg2: Point64,
) -> Point64 {
    if seg1.x == seg2.x && seg1.y == seg2.y {
        return seg1;
    }
    let dx = (seg2.x - seg1.x) as f64;
    let dy = (seg2.y - seg1.y) as f64;
    let mut q =
        ((off_pt.x - seg1.x) as f64 * dx + (off_pt.y - seg1.y) as f64 * dy) / (sqr(dx) + sqr(dy));
    #[allow(clippy::manual_clamp)]
    if q < 0.0 {
        q = 0.0;
    } else if q > 1.0 {
        q = 1.0;
    }
    Point64::new(
        seg1.x + (q * dx).round_ties_even() as i64,
        seg1.y + (q * dy).round_ties_even() as i64,
    )
}
