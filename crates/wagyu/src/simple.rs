//! A shortcut for the polygon tippecanoe cleans most often: a single ring
//! that is already simple. A union of such a ring, with positive fill, gives
//! back the same ring in the same direction, starting at its point with the
//! smallest y, or of two such points the one with the larger x. The sweep then
//! has nothing to change, and this skips it.
//!
//! The test is conservative. A ring takes the shortcut only when no two of
//! its edges cross or touch, no point repeats, no three consecutive points
//! are collinear (which the sweep would drop), and no edge comes within a
//! pixel of a point that is not its own (which snap rounding would add to
//! it). Anything else goes through the sweep. Debug builds run the sweep as
//! well and check that the two agree.

use crate::geometry::{MultiPolygon, Point};

/// Longer rings go through the sweep: the test takes time quadratic in the
/// number of points, and past this costs more than the sweep it saves.
const MAX_POINTS: usize = 64;

fn cross(o: Point, a: Point, b: Point) -> i128 {
    (a.x - o.x) as i128 * (b.y - o.y) as i128 - (a.y - o.y) as i128 * (b.x - o.x) as i128
}

fn sign(v: i128) -> i32 {
    (v > 0) as i32 - (v < 0) as i32
}

/// Whether `p`, collinear with `a` and `b`, lies between them.
fn within(a: Point, b: Point, p: Point) -> bool {
    p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
}

/// Whether segments a-b and c-d have any point in common.
fn touch(a: Point, b: Point, c: Point, d: Point) -> bool {
    let d1 = sign(cross(c, d, a));
    let d2 = sign(cross(c, d, b));
    let d3 = sign(cross(a, b, c));
    let d4 = sign(cross(a, b, d));
    if d1 * d2 < 0 && d3 * d4 < 0 {
        return true;
    }
    (d1 == 0 && within(c, d, a))
        || (d2 == 0 && within(c, d, b))
        || (d3 == 0 && within(a, b, c))
        || (d4 == 0 && within(a, b, d))
}

/// Whether segment a-b comes within one unit of `p` in x and y, which takes
/// in the pixel around `p` that snap rounding would round the edge to.
fn near(a: Point, b: Point, p: Point) -> bool {
    let (x0, x1, y0, y1) = (p.x - 1, p.x + 1, p.y - 1, p.y + 1);
    let inside = |q: Point| q.x >= x0 && q.x <= x1 && q.y >= y0 && q.y <= y1;
    if inside(a) || inside(b) {
        return true;
    }
    let c = [
        Point::new(x0, y0),
        Point::new(x1, y0),
        Point::new(x1, y1),
        Point::new(x0, y1),
    ];
    (0..4).any(|i| touch(a, b, c[i], c[(i + 1) % 4]))
}

/// The union of a single closed ring with positive fill, as the sweep makes
/// it, if the ring is simple enough to tell without the sweep.
pub fn union_of_simple_ring(ring: &[Point]) -> Option<MultiPolygon> {
    if ring.len() < 4 || ring.len() > MAX_POINTS + 1 || ring[0] != ring[ring.len() - 1] {
        return None;
    }
    let p = &ring[..ring.len() - 1];
    let n = p.len();

    let mut area: i128 = 0;
    for i in 0..n {
        let (a, b, c) = (p[(i + n - 1) % n], p[i], p[(i + 1) % n]);
        if a == b || cross(a, b, c) == 0 {
            return None;
        }
        area += b.x as i128 * c.y as i128 - c.x as i128 * b.y as i128;
    }
    if area <= 0 {
        return None;
    }

    let bbox = |i: usize| {
        let (a, b) = (p[i], p[(i + 1) % n]);
        (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y))
    };
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        let (x0, x1, y0, y1) = bbox(i);
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let (u0, u1, v0, v1) = bbox(j);
            if u0 > x1 || u1 < x0 || v0 > y1 || v1 < y0 {
                continue;
            }
            if touch(a, b, p[j], p[(j + 1) % n]) {
                return None;
            }
        }
        for (k, &q) in p.iter().enumerate() {
            if k == i || k == (i + 1) % n {
                continue;
            }
            if q.x < x0 - 1 || q.x > x1 + 1 || q.y < y0 - 1 || q.y > y1 + 1 {
                continue;
            }
            if near(a, b, q) {
                return None;
            }
        }
    }

    let min_y = p.iter().map(|q| q.y).min()?;
    let mut lowest = (0..n).filter(|&k| p[k].y == min_y);
    let first = lowest.next()?;
    let start = match (lowest.next(), lowest.next()) {
        (None, _) => first,
        (Some(second), None) => {
            if p[second].x > p[first].x {
                second
            } else {
                first
            }
        }
        _ => return None,
    };

    let mut out = Vec::with_capacity(n + 1);
    out.extend_from_slice(&p[start..]);
    out.extend_from_slice(&p[..start]);
    out.push(p[start]);
    let result = vec![vec![out]];

    #[cfg(debug_assertions)]
    {
        let mut w = crate::Wagyu::new();
        w.add_ring(ring, crate::PolygonType::Subject);
        let swept = w.execute(
            crate::ClipType::Union,
            crate::FillType::Positive,
            crate::FillType::Positive,
        );
        debug_assert_eq!(
            swept.as_ref().ok(),
            Some(&result),
            "the shortcut for {ring:?}"
        );
    }

    Some(result)
}
