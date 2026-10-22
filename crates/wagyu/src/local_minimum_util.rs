//! Port of `local_minimum_util.hpp`.

use crate::bound::{Bound, BoundPtr};
use crate::config::{EdgeSide, Error, PolygonType, Result};
use crate::edge::{Edge, is_horizontal};
use crate::local_minimum::LocalMinimum;
use crate::point::NIL;

#[inline]
fn reverse_horizontal(e: &mut Edge) {
    // swap horizontal edges' top and bottom x's so they follow the natural
    // progression of the bounds - ie so their xbots will align with the
    // adjoining lower edge. [Helpful in the process_horizontal() method.]
    std::mem::swap(&mut e.top.x, &mut e.bot.x);
}

// Make a list start on a local maximum by
// shifting all the points not on a local maximum to the
fn start_list_on_local_maximum(edges: &mut [Edge]) {
    if edges.len() <= 2 {
        return;
    }
    // Find the first local maximum going forward in the list
    let mut prev_edge = edges.len() - 1;
    let mut prev_edge_is_horizontal = is_horizontal(&edges[prev_edge]);
    let mut edge = 0;
    let mut y_decreasing_before_last_horizontal = false; // assume false at start

    while edge != edges.len() {
        let edge_is_horizontal = is_horizontal(&edges[edge]);
        if !prev_edge_is_horizontal && !edge_is_horizontal && edges[edge].top == edges[prev_edge].top {
            break;
        }
        if !edge_is_horizontal && prev_edge_is_horizontal {
            if y_decreasing_before_last_horizontal
                && (edges[edge].top == edges[prev_edge].bot || edges[edge].top == edges[prev_edge].top)
            {
                break;
            }
        } else if !y_decreasing_before_last_horizontal
            && !prev_edge_is_horizontal
            && edge_is_horizontal
            && (edges[prev_edge].top == edges[edge].top || edges[prev_edge].top == edges[edge].bot)
        {
            y_decreasing_before_last_horizontal = true;
        }
        prev_edge_is_horizontal = edge_is_horizontal;
        prev_edge = edge;
        edge += 1;
    }
    edges.rotate_left(edge);
}

fn create_bound_towards_minimum(edges: &mut Vec<Edge>) -> Bound {
    if edges.len() == 1 {
        if is_horizontal(&edges[0]) {
            reverse_horizontal(&mut edges[0]);
        }
        let mut bnd = Bound::new();
        std::mem::swap(&mut bnd.edges, edges);
        return bnd;
    }
    let mut next_edge = 0;
    let mut edge = next_edge;
    next_edge += 1;
    let mut edge_is_horizontal = is_horizontal(&edges[edge]);
    if edge_is_horizontal {
        reverse_horizontal(&mut edges[edge]);
    }
    let mut y_increasing_before_last_horizontal = false; // assume false at start

    while next_edge != edges.len() {
        let next_edge_is_horizontal = is_horizontal(&edges[next_edge]);
        if !next_edge_is_horizontal && !edge_is_horizontal && edges[edge].bot == edges[next_edge].bot {
            break;
        }
        if !next_edge_is_horizontal && edge_is_horizontal {
            if y_increasing_before_last_horizontal
                && (edges[next_edge].bot == edges[edge].bot || edges[next_edge].bot == edges[edge].top)
            {
                break;
            }
        } else if !y_increasing_before_last_horizontal
            && !edge_is_horizontal
            && next_edge_is_horizontal
            && (edges[edge].bot == edges[next_edge].top || edges[edge].bot == edges[next_edge].bot)
        {
            y_increasing_before_last_horizontal = true;
        }
        edge_is_horizontal = next_edge_is_horizontal;
        edge = next_edge;
        if edge_is_horizontal {
            reverse_horizontal(&mut edges[edge]);
        }
        next_edge += 1;
    }
    let mut bnd = Bound::new();
    if next_edge == edges.len() {
        std::mem::swap(edges, &mut bnd.edges);
    } else {
        bnd.edges = edges.drain(0..next_edge).collect();
    }
    bnd.edges.reverse();
    bnd
}

fn create_bound_towards_maximum(edges: &mut Vec<Edge>) -> Bound {
    if edges.len() == 1 {
        let mut bnd = Bound::new();
        std::mem::swap(&mut bnd.edges, edges);
        return bnd;
    }
    let mut next_edge = 0;
    let mut edge = next_edge;
    next_edge += 1;
    let mut edge_is_horizontal = is_horizontal(&edges[edge]);
    let mut y_decreasing_before_last_horizontal = false; // assume false at start

    while next_edge != edges.len() {
        let next_edge_is_horizontal = is_horizontal(&edges[next_edge]);
        if !next_edge_is_horizontal && !edge_is_horizontal && edges[edge].top == edges[next_edge].top {
            break;
        }
        if !next_edge_is_horizontal && edge_is_horizontal {
            if y_decreasing_before_last_horizontal
                && (edges[next_edge].top == edges[edge].bot || edges[next_edge].top == edges[edge].top)
            {
                break;
            }
        } else if !y_decreasing_before_last_horizontal
            && !edge_is_horizontal
            && next_edge_is_horizontal
            && (edges[edge].top == edges[next_edge].top || edges[edge].top == edges[next_edge].bot)
        {
            y_decreasing_before_last_horizontal = true;
        }
        edge_is_horizontal = next_edge_is_horizontal;
        edge = next_edge;
        next_edge += 1;
    }
    let mut bnd = Bound::new();
    if next_edge == edges.len() {
        std::mem::swap(&mut bnd.edges, edges);
    } else {
        bnd.edges = edges.drain(0..next_edge).collect();
    }
    bnd
}

fn fix_horizontals(bnd: &mut Bound) {
    let edges = &mut bnd.edges;
    let mut edge_itr = 0;
    let next_itr = edge_itr + 1;
    if next_itr == edges.len() {
        return;
    }
    if is_horizontal(&edges[edge_itr]) && edges[next_itr].bot != edges[edge_itr].top {
        reverse_horizontal(&mut edges[edge_itr]);
    }
    let mut prev_itr = edge_itr;
    edge_itr += 1;
    while edge_itr != edges.len() {
        if is_horizontal(&edges[edge_itr]) && edges[prev_itr].top != edges[edge_itr].bot {
            reverse_horizontal(&mut edges[edge_itr]);
        }
        prev_itr = edge_itr;
        edge_itr += 1;
    }
}

fn move_horizontals_on_left_to_right(left_bound: &mut Bound, right_bound: &mut Bound) {
    // We want all the horizontal segments that are at the same Y as the minimum to be on the right
    // bound
    let mut edge_itr = 0;
    while edge_itr != left_bound.edges.len() {
        if !is_horizontal(&left_bound.edges[edge_itr]) {
            break;
        }
        reverse_horizontal(&mut left_bound.edges[edge_itr]);
        edge_itr += 1;
    }
    if edge_itr == 0 {
        return;
    }
    left_bound.edges[..edge_itr].reverse();
    let dist = edge_itr;
    right_bound.edges.extend(left_bound.edges.drain(0..edge_itr));
    let len = right_bound.edges.len();
    right_bound.edges[..len].rotate_right(dist);
}

pub(crate) fn add_ring_to_local_minima_list(
    edges: &mut Vec<Edge>,
    minima_list: &mut Vec<LocalMinimum>,
    bounds: &mut Vec<Bound>,
    poly_type: PolygonType,
) -> Result<()> {
    if edges.is_empty() {
        return Ok(());
    }
    // Adjust the order of the ring so we start on a local maximum
    // therefore we start right away on a bound.
    start_list_on_local_maximum(edges);

    let mut first_minimum: BoundPtr = NIL;
    let mut last_maximum: BoundPtr = NIL;
    while !edges.is_empty() {
        let mut lm_minimum_has_horizontal = false;
        let mut to_minimum = create_bound_towards_minimum(edges);
        if edges.is_empty() {
            return Err(Error::new("Edges is empty after only creating a single bound."));
        }
        let mut to_maximum = create_bound_towards_maximum(edges);
        fix_horizontals(&mut to_minimum);
        fix_horizontals(&mut to_maximum);
        let mut to_max_first_non_horizontal = 0;
        let mut to_min_first_non_horizontal = 0;
        let minimum_is_left;
        while to_max_first_non_horizontal != to_maximum.edges.len()
            && is_horizontal(&to_maximum.edges[to_max_first_non_horizontal])
        {
            lm_minimum_has_horizontal = true;
            to_max_first_non_horizontal += 1;
        }
        while to_min_first_non_horizontal != to_minimum.edges.len()
            && is_horizontal(&to_minimum.edges[to_min_first_non_horizontal])
        {
            lm_minimum_has_horizontal = true;
            to_min_first_non_horizontal += 1;
        }

        if to_max_first_non_horizontal == to_maximum.edges.len()
            || to_min_first_non_horizontal == to_minimum.edges.len()
        {
            return Err(Error::new("should not have a horizontal only bound for a ring"));
        }

        if lm_minimum_has_horizontal {
            if to_maximum.edges[to_max_first_non_horizontal].bot.x > to_minimum.edges[to_min_first_non_horizontal].bot.x {
                minimum_is_left = true;
                move_horizontals_on_left_to_right(&mut to_minimum, &mut to_maximum);
            } else {
                minimum_is_left = false;
                move_horizontals_on_left_to_right(&mut to_maximum, &mut to_minimum);
            }
        } else if to_maximum.edges[to_max_first_non_horizontal].dx > to_minimum.edges[to_min_first_non_horizontal].dx {
            minimum_is_left = false;
        } else {
            minimum_is_left = true;
        }
        let min_front_bot_y = to_minimum.edges[0].bot.y;
        if last_maximum != NIL {
            to_minimum.maximum_bound = last_maximum;
        }
        to_minimum.poly_type = poly_type;
        to_maximum.poly_type = poly_type;
        let left_ptr: BoundPtr = bounds.len();
        let right_ptr: BoundPtr = left_ptr + 1;
        if !minimum_is_left {
            to_minimum.side = EdgeSide::Right;
            to_maximum.side = EdgeSide::Left;
            to_minimum.winding_delta = -1;
            to_maximum.winding_delta = 1;
            bounds.push(to_maximum);
            bounds.push(to_minimum);
            minima_list.push(LocalMinimum {
                left_bound: left_ptr,
                right_bound: right_ptr,
                y: min_front_bot_y,
                minimum_has_horizontal: lm_minimum_has_horizontal,
            });
            if last_maximum == NIL {
                first_minimum = right_ptr;
            } else {
                bounds[last_maximum].maximum_bound = right_ptr;
            }
            last_maximum = left_ptr;
        } else {
            to_minimum.side = EdgeSide::Left;
            to_maximum.side = EdgeSide::Right;
            to_minimum.winding_delta = -1;
            to_maximum.winding_delta = 1;
            bounds.push(to_minimum);
            bounds.push(to_maximum);
            minima_list.push(LocalMinimum {
                left_bound: left_ptr,
                right_bound: right_ptr,
                y: min_front_bot_y,
                minimum_has_horizontal: lm_minimum_has_horizontal,
            });
            if last_maximum == NIL {
                first_minimum = left_ptr;
            } else {
                bounds[last_maximum].maximum_bound = left_ptr;
            }
            last_maximum = right_ptr;
        }
    }
    bounds[last_maximum].maximum_bound = first_minimum;
    bounds[first_minimum].maximum_bound = last_maximum;
    Ok(())
}

pub(crate) fn initialize_lm(lm: &LocalMinimum, bounds: &mut [Bound]) {
    let left_bound = &mut bounds[lm.left_bound];
    if !left_bound.edges.is_empty() {
        left_bound.current_edge = 0;
        left_bound.next_edge = left_bound.current_edge + 1;
        left_bound.current_x = left_bound.edges[0].bot.x as f64;
        left_bound.winding_count = 0;
        left_bound.winding_count2 = 0;
        left_bound.side = EdgeSide::Left;
        left_bound.ring = NIL;
    }
    let right_bound = &mut bounds[lm.right_bound];
    if !right_bound.edges.is_empty() {
        right_bound.current_edge = 0;
        right_bound.next_edge = right_bound.current_edge + 1;
        right_bound.current_x = right_bound.edges[0].bot.x as f64;
        right_bound.winding_count = 0;
        right_bound.winding_count2 = 0;
        right_bound.side = EdgeSide::Right;
        right_bound.ring = NIL;
    }
}
