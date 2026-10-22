//! Port of `build_result.hpp`.

use crate::geometry::{LinearRing, MultiPolygon, Point, Polygon};
use crate::point::{NIL, RingPtr};
use crate::ring::RingManager;

fn push_ring_to_polygon(
    poly: &mut Polygon,
    r: RingPtr,
    manager: &RingManager,
    reverse_output: bool,
) {
    let pts = &manager.points;
    let ring = &manager.rings[r];
    let mut lr: LinearRing = Vec::with_capacity(ring.size_ + 1);
    let first_pt = ring.points;
    let mut pt_it = ring.points;
    if reverse_output {
        loop {
            lr.push(Point::new(pts[pt_it].x, pts[pt_it].y));
            pt_it = pts[pt_it].next;
            if pt_it == first_pt {
                break;
            }
        }
    } else {
        loop {
            lr.push(Point::new(pts[pt_it].x, pts[pt_it].y));
            pt_it = pts[pt_it].prev;
            if pt_it == first_pt {
                break;
            }
        }
    }
    lr.push(Point::new(pts[first_pt].x, pts[first_pt].y)); // close the ring
    poly.push(lr);
}

fn build_result_polygons(
    solution: &mut MultiPolygon,
    rings: &[RingPtr],
    manager: &RingManager,
    reverse_output: bool,
) {
    for &r in rings {
        if r == NIL {
            continue;
        }
        debug_assert!(manager.rings[r].points != NIL);
        solution.push(Polygon::new());
        let poly = solution.last_mut().unwrap();
        push_ring_to_polygon(poly, r, manager, reverse_output);
        for &c in &manager.rings[r].children {
            if c == NIL {
                continue;
            }
            debug_assert!(manager.rings[c].points != NIL);
            push_ring_to_polygon(poly, c, manager, reverse_output);
        }
        for &c in &manager.rings[r].children {
            if c == NIL {
                continue;
            }
            if !manager.rings[c].children.is_empty() {
                build_result_polygons(
                    solution,
                    &manager.rings[c].children,
                    manager,
                    reverse_output,
                );
            }
        }
    }
}

pub(crate) fn build_result(solution: &mut MultiPolygon, rings: &RingManager, reverse_output: bool) {
    build_result_polygons(solution, &rings.children, rings, reverse_output);
}
