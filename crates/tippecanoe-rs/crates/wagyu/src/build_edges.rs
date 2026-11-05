//! Port of `build_edges.hpp`.

use crate::edge::{Edge, slopes_equal_edges};
use crate::geometry::Point;
use crate::util::slopes_equal;

fn point_2_is_between_point_1_and_point_3(pt1: Point, pt2: Point, pt3: Point) -> bool {
    if (pt1 == pt3) || (pt1 == pt2) || (pt3 == pt2) {
        false
    } else if pt1.x != pt3.x {
        (pt2.x > pt1.x) == (pt2.x < pt3.x)
    } else {
        (pt2.y > pt1.y) == (pt2.y < pt3.y)
    }
}

pub(crate) fn build_edge_list(path_geometry: &[Point], edges: &mut Vec<Edge>) -> bool {
    if path_geometry.len() < 3 {
        return false;
    }

    // As this is a loop, we need to first go backwards from end to try and find
    // the proper starting point for the iterators before the beginning

    // `itr_rev` is the index of the element the reverse iterator points at; -1 is rend().
    let mut itr_rev: isize = path_geometry.len() as isize - 1;
    let mut itr: usize = 0;
    let mut pt1 = path_geometry[itr_rev as usize];
    let mut pt2 = path_geometry[itr];

    // Find next non repeated point going backwards from
    // end for pt1
    while pt1 == pt2 {
        itr_rev -= 1;
        if itr_rev < 0 {
            return false;
        }
        pt1 = path_geometry[itr_rev as usize];
    }
    itr += 1;
    let mut pt3 = path_geometry[itr];
    let mut itr_last: usize = (itr_rev + 1) as usize;
    let mut front_pt = Point::default();
    let mut back_pt = Point::default();
    loop {
        if pt3 == pt2 {
            // Duplicate point advance itr, but do not
            // advance other points
            if itr == itr_last {
                break;
            }
            itr += 1;
            if itr == itr_last {
                if edges.is_empty() {
                    break;
                }
                pt3 = front_pt;
            } else {
                pt3 = path_geometry[itr];
            }
            continue;
        }

        // Now check if slopes are equal between two segments - either
        // a spike or a collinear point - if so drop point number 2.
        if slopes_equal(pt1, pt2, pt3) {
            // We need to reconsider previously added points
            // because the point it was using was found to be collinear
            // or a spike
            pt2 = pt1;
            if !edges.is_empty() {
                edges.pop(); // remove previous edge (pt1)
            }
            if let Some(back) = edges.last() {
                let back_top = back.top;
                if back_pt.x == back_top.x && back_pt.y == back_top.y {
                    pt1 = back.bot;
                } else {
                    pt1 = back_top;
                }
                back_pt = pt1;
            } else {
                // If this occurs we must look to the back of the
                // ring for new points.
                while path_geometry[itr_rev as usize] == pt2 {
                    itr_rev -= 1;
                    if (itr + 1) as isize == itr_rev + 1 {
                        return false;
                    }
                    // PORT: the C++ would read before the start of the ring here.
                    if itr_rev < 0 {
                        return false;
                    }
                }
                pt1 = path_geometry[itr_rev as usize];
                itr_last = (itr_rev + 1) as usize;
            }
            continue;
        }

        if edges.is_empty() {
            front_pt = pt2;
        }
        edges.push(Edge::new(pt2, pt3));
        back_pt = pt2;
        if itr == itr_last {
            break;
        }
        pt1 = pt2;
        pt2 = pt3;
        itr += 1;
        if itr == itr_last {
            if edges.is_empty() {
                break;
            }
            pt3 = front_pt;
        } else {
            pt3 = path_geometry[itr];
        }
    }

    loop {
        let mut modified = false;
        if edges.len() < 3 {
            return false;
        }
        let last = edges.len() - 1;
        let f = edges[0];
        let b = edges[last];
        if slopes_equal_edges(&f, &b) {
            if f.bot == b.top {
                if f.top == b.bot {
                    edges.pop();
                    edges.remove(0);
                } else {
                    edges[0].bot = b.bot;
                    edges.pop();
                }
                modified = true;
            } else if f.top == b.bot {
                edges[0].top = b.top;
                edges.pop();
                modified = true;
            } else if f.top == b.top && f.bot == b.bot {
                edges.pop();
                edges.remove(0);
                modified = true;
            } else if f.top == b.top {
                if point_2_is_between_point_1_and_point_3(f.top, f.bot, b.bot) {
                    edges[last].top = f.bot;
                    edges.remove(0);
                } else {
                    edges[0].top = b.bot;
                    edges.pop();
                }
                modified = true;
            } else if f.bot == b.bot {
                if point_2_is_between_point_1_and_point_3(f.bot, f.top, b.top) {
                    edges[last].bot = f.top;
                    edges.remove(0);
                } else {
                    edges[0].bot = b.top;
                    edges.pop();
                }
                modified = true;
            }
        }
        if !modified {
            break;
        }
    }

    true
}
