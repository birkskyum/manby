//! Port of `topology_correction.hpp`, including tippecanoe's change to `correct_tree` that
//! drops a hole no ring can parent instead of throwing.

use std::collections::{HashSet, VecDeque};

use crate::config::{Error, Result};
use crate::geometry::BoundingBox;
use crate::point::{NIL, PointNode, PointPtr, RingPtr, same_pos};
use crate::ring::{
    RingManager, area_from_point, assign_as_child, assign_as_sibling, create_new_ring,
    reassign_as_child, remove_ring, remove_ring_and_points, reverse_ring, ring_depth, ring_is_hole,
    ring1_replaces_ring2,
};
use crate::ring_util::{
    RingIndexes, poly2_contains_poly1, poly2_contains_poly1_indexed, update_points_ring,
};
use crate::stable_sort::stable_sort;
use crate::util::value_is_zero;

#[derive(Clone, Copy, Debug)]
struct PointPtrPair {
    op1: PointPtr,
    op2: PointPtr,
}

/// `std::unordered_multimap<ring_ptr<T>, point_ptr_pair<T>>`
///
/// PORT: wagyu only ever reaches this map through `equal_range`, `emplace`, `insert` and
/// `erase`, so the only order that matters is the order of the entries under one key. libc++
/// appends a new entry to the end of its key's group and keeps that order through rehashing
/// and erasing, so each key holds a vector in insertion order. (libstdc++ inserts at the
/// front of the group instead, so a Linux build of the C++ can differ when a ring has more
/// than one connection.)
struct ConnectionMap {
    groups: Vec<Vec<PointPtrPair>>,
}

impl ConnectionMap {
    fn new() -> ConnectionMap {
        ConnectionMap { groups: Vec::new() }
    }

    fn group(&mut self, r: RingPtr) -> &mut Vec<PointPtrPair> {
        assert!(r != NIL);
        if r >= self.groups.len() {
            self.groups.resize_with(r + 1, Vec::new);
        }
        &mut self.groups[r]
    }

    fn emplace(&mut self, r: RingPtr, pair: PointPtrPair) {
        self.group(r).push(pair);
    }

    fn erase_key(&mut self, r: RingPtr) {
        self.group(r).clear();
    }
}

type IList = VecDeque<(RingPtr, PointPtrPair)>;

fn find_intersect_loop(
    dupe_ring: &mut ConnectionMap,
    i_list: &mut IList,
    ring_parent: RingPtr,
    ring_origin: RingPtr,
    ring_search: RingPtr,
    visited: &mut HashSet<RingPtr>,
    orig_pt: PointPtr,
    prev_pt: PointPtr,
    rings: &mut RingManager,
) -> bool {
    {
        // Check for direct connection
        let mut it = 0;
        while it < dupe_ring.group(ring_search).len() {
            let pair = dupe_ring.group(ring_search)[it];
            let it_ring1 = rings.points[pair.op1].ring;
            let it_ring2 = rings.points[pair.op2].ring;
            if it_ring1 == NIL
                || it_ring2 == NIL
                || it_ring1 != ring_search
                || (!rings.is_hole(it_ring1) && !rings.is_hole(it_ring2))
            {
                dupe_ring.group(ring_search).remove(it);
                continue;
            }
            if it_ring2 == ring_origin
                && (ring_parent == it_ring2 || ring_parent == rings.rings[it_ring2].parent)
                && !same_pos(&rings.points, prev_pt, pair.op2)
                && !same_pos(&rings.points, orig_pt, pair.op2)
            {
                i_list.push_front((ring_search, pair));
                return true;
            }
            it += 1;
        }
    }
    visited.insert(ring_search);
    // Check for connection through chain of other intersections
    // (the recursion below never changes the entries for `ring_search`)
    let n = dupe_ring.group(ring_search).len();
    for it in 0..n {
        let pair = dupe_ring.group(ring_search)[it];
        let it_ring = rings.points[pair.op2].ring;
        if visited.contains(&it_ring)
            || it_ring == NIL
            || (ring_parent != it_ring && ring_parent != rings.rings[it_ring].parent)
            || value_is_zero(rings.area(it_ring))
            || same_pos(&rings.points, prev_pt, pair.op2)
        {
            continue;
        }
        if find_intersect_loop(
            dupe_ring,
            i_list,
            ring_parent,
            ring_origin,
            it_ring,
            visited,
            orig_pt,
            pair.op2,
            rings,
        ) {
            i_list.push_front((ring_search, pair));
            return true;
        }
    }
    false
}

/// `point_ptr_cmp`
fn point_ptr_cmp(op1: PointPtr, op2: PointPtr, manager: &RingManager) -> bool {
    let p1 = &manager.points[op1];
    let p2 = &manager.points[op2];
    if p1.y != p2.y {
        p1.y > p2.y
    } else if p1.x != p2.x {
        p1.x < p2.x
    } else {
        let depth_1 = ring_depth(&manager.rings, p1.ring);
        let depth_2 = ring_depth(&manager.rings, p2.ring);
        depth_1 > depth_2
    }
}

fn correct_orientations(manager: &mut RingManager) {
    for r in 0..manager.rings.len() {
        if manager.rings[r].points == NIL {
            continue;
        }
        manager.recalculate_stats(r);
        if manager.size(r) < 3 {
            remove_ring_and_points(r, manager, false, true);
            continue;
        }
        if ring_is_hole(&manager.rings, r) != manager.is_hole(r) {
            reverse_ring(&mut manager.points, manager.rings[r].points);
            manager.recalculate_stats(r);
        }
    }
}

fn sort_ring_points(r: RingPtr, manager: &RingManager) -> Vec<PointPtr> {
    let pts = &manager.points;
    let mut sorted_points: Vec<PointPtr> = Vec::new();
    let mut point_itr = manager.rings[r].points;
    let last_point = pts[point_itr].prev;
    while point_itr != last_point {
        sorted_points.push(point_itr);
        point_itr = pts[point_itr].next;
    }
    sorted_points.push(last_point);
    stable_sort(&mut sorted_points, true, |&pt1, &pt2| {
        if pts[pt1].y != pts[pt2].y {
            return pts[pt1].y > pts[pt2].y;
        }
        pts[pt1].x < pts[pt2].x
    });
    sorted_points
}

fn correct_self_intersection(pt1: PointPtr, pt2: PointPtr, manager: &mut RingManager) -> RingPtr {
    if manager.points[pt1].ring != manager.points[pt2].ring {
        return NIL;
    }

    let ring = manager.points[pt1].ring;

    // split the polygon into two ...
    let pts = &mut manager.points;
    let pt3 = pts[pt1].prev;
    let pt4 = pts[pt2].prev;
    pts[pt1].prev = pt4;
    pts[pt4].next = pt1;
    pts[pt2].prev = pt3;
    pts[pt3].next = pt2;

    let new_ring = create_new_ring(manager);
    let mut size_1 = 0;
    let mut size_2 = 0;
    let mut box1 = BoundingBox::default();
    let mut box2 = BoundingBox::default();
    let area_1 = area_from_point(&manager.points, pt1, &mut size_1, &mut box1);
    let area_2 = area_from_point(&manager.points, pt2, &mut size_2, &mut box2);

    if area_1.abs() > area_2.abs() {
        manager.rings[ring].points = pt1;
        manager.rings[ring].set_stats(area_1, size_1, box1);
        manager.rings[new_ring].points = pt2;
        manager.rings[new_ring].set_stats(area_2, size_2, box2);
    } else {
        manager.rings[ring].points = pt2;
        manager.rings[ring].set_stats(area_2, size_2, box2);
        manager.rings[new_ring].points = pt1;
        manager.rings[new_ring].set_stats(area_1, size_1, box1);
    }
    update_points_ring(new_ring, manager);
    new_ring
}

fn correct_repeated_points(
    manager: &mut RingManager,
    new_rings: &mut Vec<RingPtr>,
    points: &[PointPtr],
) {
    for itr1 in 0..points.len() {
        if manager.points[points[itr1]].ring == NIL {
            continue;
        }
        for itr2 in itr1 + 1..points.len() {
            if manager.points[points[itr2]].ring == NIL {
                continue;
            }
            let new_ring = correct_self_intersection(points[itr1], points[itr2], manager);
            if new_ring != NIL {
                new_rings.push(new_ring);
            }
        }
    }
}

/// Calls `f` on every run of points at the same position (of two or more) in `sorted`.
/// This is the grouping loop that `find_and_correct_repeated_points`, `correct_chained_rings`
/// and `correct_collinear_edges` share.
fn for_each_repeated_run<F: FnMut(&mut RingManager, &[PointPtr]) -> Result<()>>(
    sorted: &[PointPtr],
    manager: &mut RingManager,
    mut f: F,
) -> Result<()> {
    let mut count: usize = 0;
    let mut prev_itr = 0;
    let mut itr = prev_itr + 1;
    while itr != sorted.len() {
        if same_pos(&manager.points, sorted[prev_itr], sorted[itr]) {
            count += 1;
            prev_itr += 1;
            itr += 1;
            if itr != sorted.len() {
                continue;
            } else {
                prev_itr += 1;
            }
        } else {
            prev_itr += 1;
            itr += 1;
        }
        if count == 0 {
            continue;
        }
        let first = prev_itr - (count + 1);
        f(manager, &sorted[first..prev_itr])?;
        count = 0;
    }
    Ok(())
}

fn find_and_correct_repeated_points(
    r: RingPtr,
    manager: &mut RingManager,
    new_rings: &mut Vec<RingPtr>,
) {
    let sorted_points = sort_ring_points(r, manager);
    // Find sets of repeated points
    let _ = for_each_repeated_run(&sorted_points, manager, |manager, run| {
        correct_repeated_points(manager, new_rings, run);
        Ok(())
    });
}

fn reassign_children_if_necessary(
    new_ring: RingPtr,
    sibling_ring: RingPtr,
    manager: &mut RingManager,
    new_rings: &[RingPtr],
) -> Result<()> {
    let n = manager.children_of(sibling_ring).len();
    for i in 0..n {
        let c = manager.children_of(sibling_ring)[i];
        if c == NIL {
            continue;
        }
        if new_rings.contains(&c) {
            continue;
        }
        if poly2_contains_poly1(c, new_ring, manager)? {
            reassign_as_child(c, new_ring, manager)?;
        }
    }
    Ok(())
}

fn find_parent_in_tree(
    r: RingPtr,
    possible_parent: RingPtr,
    manager: &mut RingManager,
) -> Result<bool> {
    // Before starting this we are assuming that possible_parent
    // and r have opposite signs of their areas

    // First we must search all grandchildren
    let n = manager.rings[possible_parent].children.len();
    for i in 0..n {
        let c = manager.rings[possible_parent].children[i];
        if c == NIL {
            continue;
        }
        let m = manager.rings[c].children.len();
        for j in 0..m {
            let gc = manager.rings[c].children[j];
            if gc == NIL {
                continue;
            }
            if find_parent_in_tree(r, gc, manager)? {
                return Ok(true);
            }
        }
    }

    if poly2_contains_poly1(r, possible_parent, manager)? {
        reassign_as_child(r, possible_parent, manager)?;
        return Ok(true);
    }
    Ok(false)
}

fn assign_new_ring_parents(
    manager: &mut RingManager,
    original_ring: RingPtr,
    new_rings: &mut Vec<RingPtr>,
) -> Result<()> {
    // First lets remove any rings that have zero area
    // or have no points
    let mut kept = Vec::with_capacity(new_rings.len());
    for &r in new_rings.iter() {
        let remove = if manager.rings[r].points == NIL {
            true
        } else {
            value_is_zero(manager.area(r))
        };
        if !remove {
            kept.push(r);
        }
    }
    *new_rings = kept;

    if new_rings.is_empty() {
        // No new rings created simply return;
        return Ok(());
    }

    // We should not have to re-assign the parent of the original ring
    // because we always maintained the largest ring during splitting
    // on repeated points.

    let original_ring_area = manager.area(original_ring);
    let original_positive = original_ring_area > 0.0;

    // If there is only one new ring the logic is very simple and we
    // do not have to check which ring contains, we only need to compare
    // the areas of the original ring and that of the new ring.
    if new_rings.len() == 1 {
        let new_ring_area = manager.area(new_rings[0]);
        let new_positive = new_ring_area > 0.0;
        if original_positive == new_positive {
            // The rings should be siblings
            let parent = manager.rings[original_ring].parent;
            assign_as_child(new_rings[0], parent, manager)?;
            reassign_children_if_necessary(new_rings[0], original_ring, manager, new_rings)?;
        } else {
            // The new ring is a child of original ring
            // Check the
            assign_as_child(new_rings[0], original_ring, manager)?;
            let parent = manager.rings[original_ring].parent;
            reassign_children_if_necessary(new_rings[0], parent, manager, new_rings)?;
        }
        return Ok(());
    }

    // Now we want to sort rings from the largest in absolute area to the smallest
    // as we will assign the rings with the largest areas first
    stable_sort(new_rings, true, |&r1, &r2| {
        manager.area(r1).abs() > manager.area(r2).abs()
    });

    for r_itr in 0..new_rings.len() {
        let r = new_rings[r_itr];
        let new_ring_area = manager.area(r);
        let new_positive = new_ring_area > 0.0;
        let same_orientation = new_positive == original_positive;
        let mut found = false;
        // First lets check the trees of any new_rings that might have
        // been assigned as siblings to the original ring.
        for s_itr in 0..r_itr {
            let s = new_rings[s_itr];
            if manager.rings[s].parent != manager.rings[original_ring].parent {
                continue;
            }
            if same_orientation {
                let n = manager.rings[s].children.len();
                for i in 0..n {
                    let s_child = manager.rings[s].children[i];
                    if s_child == NIL {
                        continue;
                    }
                    if find_parent_in_tree(r, s_child, manager)? {
                        reassign_children_if_necessary(r, original_ring, manager, new_rings)?;
                        found = true;
                        break;
                    }
                }
            } else if find_parent_in_tree(r, s, manager)? {
                let parent = manager.rings[original_ring].parent;
                reassign_children_if_necessary(r, parent, manager, new_rings)?;
                found = true;
            }
            if found {
                break;
            }
        }

        if found {
            continue;
        }

        // Next lets check the tree of the original_ring
        if same_orientation {
            let n = manager.rings[original_ring].children.len();
            for i in 0..n {
                let o_child = manager.rings[original_ring].children[i];
                if o_child == NIL {
                    continue;
                }
                if find_parent_in_tree(r, o_child, manager)? {
                    reassign_children_if_necessary(r, original_ring, manager, new_rings)?;
                    found = true;
                    break;
                }
            }
            if !found {
                // If we didn't find any parent and the same orientation
                // then it must be a sibling of the original ring
                let parent = manager.rings[original_ring].parent;
                assign_as_child(r, parent, manager)?;
                reassign_children_if_necessary(r, original_ring, manager, new_rings)?;
            }
        } else if find_parent_in_tree(r, original_ring, manager)? {
            let parent = manager.rings[original_ring].parent;
            reassign_children_if_necessary(r, parent, manager, new_rings)?;
        } else {
            return Err(Error::new("Unable to find a proper parent ring"));
        }
    }
    Ok(())
}

fn correct_ring_self_intersections(
    manager: &mut RingManager,
    r: RingPtr,
    correct_tree: bool,
) -> Result<bool> {
    if manager.rings[r].corrected || manager.rings[r].points == NIL {
        return Ok(false);
    }

    let mut new_rings: Vec<RingPtr> = Vec::new();

    find_and_correct_repeated_points(r, manager, &mut new_rings);

    if correct_tree {
        assign_new_ring_parents(manager, r, &mut new_rings)?;
    }

    manager.rings[r].corrected = true;
    Ok(true)
}

fn process_single_intersection(
    connection_map: &mut ConnectionMap,
    op_j: PointPtr,
    op_k: PointPtr,
    manager: &mut RingManager,
) -> Result<()> {
    let ring_j = manager.points[op_j].ring;
    let ring_k = manager.points[op_k].ring;
    if ring_j == ring_k {
        return Ok(());
    }

    if !manager.is_hole(ring_j) && !manager.is_hole(ring_k) {
        // Both are not holes, return nothing to do.
        return Ok(());
    }

    let mut ring_origin;
    let ring_search;
    let mut ring_parent;
    let mut op_origin_1;
    let mut op_origin_2;
    if !manager.is_hole(ring_j) {
        ring_origin = ring_j;
        ring_parent = ring_origin;
        ring_search = ring_k;
        op_origin_1 = op_j;
        op_origin_2 = op_k;
    } else if !manager.is_hole(ring_k) {
        ring_origin = ring_k;
        ring_parent = ring_origin;
        ring_search = ring_j;
        op_origin_1 = op_k;
        op_origin_2 = op_j;
    } else {
        // both are holes
        // Order doesn't matter
        ring_origin = ring_j;
        ring_parent = manager.rings[ring_origin].parent;
        ring_search = ring_k;
        op_origin_1 = op_j;
        op_origin_2 = op_k;
    }
    if ring_parent != manager.rings[ring_search].parent {
        // The two holes do not have the same parent, do not add them
        // simply return!
        return Ok(());
    }
    let mut found = false;
    let mut i_list: IList = VecDeque::new();
    {
        // Check for direct connection
        let mut it = 0;
        while it < connection_map.group(ring_search).len() {
            let pair = connection_map.group(ring_search)[it];
            if manager.points[pair.op1].ring == NIL {
                connection_map.group(ring_search).remove(it);
                continue;
            }
            if manager.points[pair.op2].ring == NIL {
                connection_map.group(ring_search).remove(it);
                continue;
            }
            let it_ring2 = manager.points[pair.op2].ring;
            if it_ring2 == ring_origin {
                found = true;
                if !same_pos(&manager.points, op_origin_1, pair.op2) {
                    i_list.push_back((ring_search, pair));
                    break;
                }
            }
            it += 1;
        }
    }
    if i_list.is_empty() {
        let mut visited: HashSet<RingPtr> = HashSet::new();
        visited.insert(ring_search);
        // Check for connection through chain of other intersections
        // (the recursion below never changes the entries for `ring_search`)
        let n = connection_map.group(ring_search).len();
        for it in 0..n {
            let pair = connection_map.group(ring_search)[it];
            let it_ring = manager.points[pair.op2].ring;
            if it_ring != ring_search
                && !same_pos(&manager.points, op_origin_2, pair.op2)
                && it_ring != NIL
                && (ring_parent == it_ring || ring_parent == manager.rings[it_ring].parent)
                && !value_is_zero(manager.area(it_ring))
                && find_intersect_loop(
                    connection_map,
                    &mut i_list,
                    ring_parent,
                    ring_origin,
                    it_ring,
                    &mut visited,
                    op_origin_2,
                    pair.op2,
                    manager,
                )
            {
                found = true;
                i_list.push_front((ring_search, pair));
                break;
            }
        }
    }
    if !found {
        let int_pt_origin = PointPtrPair {
            op1: op_origin_1,
            op2: op_origin_2,
        };
        let int_pt_search = PointPtrPair {
            op1: op_origin_2,
            op2: op_origin_1,
        };
        connection_map.emplace(ring_origin, int_pt_origin);
        connection_map.emplace(ring_search, int_pt_search);
        return Ok(());
    }

    if i_list.is_empty() {
        // The situation where both origin and search are holes might have a missing
        // search condition, we must check if a new pair must be added.
        let mut missing = true;
        // Check for direct connection
        for pair in connection_map.group(ring_origin).iter() {
            let it_ring2 = manager.points[pair.op2].ring;
            if it_ring2 == ring_search {
                missing = false;
            }
        }
        if missing {
            let int_pt_origin = PointPtrPair {
                op1: op_origin_1,
                op2: op_origin_2,
            };
            connection_map.emplace(ring_origin, int_pt_origin);
        }
        return Ok(());
    }
    if manager.is_hole(ring_origin) {
        for i_ring in i_list.iter_mut() {
            let ring_itr = i_ring.0;
            if !manager.is_hole(ring_itr) {
                // Make the hole the origin!
                let op1 = op_origin_1;
                op_origin_1 = i_ring.1.op1;
                i_ring.1.op1 = op1;
                let op2 = op_origin_2;
                op_origin_2 = i_ring.1.op2;
                i_ring.1.op2 = op2;
                i_ring.0 = ring_origin;
                ring_origin = ring_itr;
                ring_parent = ring_origin;
                break;
            }
        }
    }
    let origin_is_hole = manager.is_hole(ring_origin);

    // Switch
    {
        let pts = &mut manager.points;
        let op_origin_1_next = pts[op_origin_1].next;
        let op_origin_2_next = pts[op_origin_2].next;
        pts[op_origin_1].next = op_origin_2_next;
        pts[op_origin_2].next = op_origin_1_next;
        pts[op_origin_1_next].prev = op_origin_2;
        pts[op_origin_2_next].prev = op_origin_1;

        for i_ring in i_list.iter() {
            let op_search_1 = i_ring.1.op1;
            let op_search_2 = i_ring.1.op2;
            let op_search_1_next = pts[op_search_1].next;
            let op_search_2_next = pts[op_search_2].next;
            pts[op_search_1].next = op_search_2_next;
            pts[op_search_2].next = op_search_1_next;
            pts[op_search_1_next].prev = op_search_2;
            pts[op_search_2_next].prev = op_search_1;
        }
    }

    let ring_new = create_new_ring(manager);
    manager.rings[ring_origin].corrected = false;
    let mut size_1 = 0;
    let mut size_2 = 0;
    let mut box1 = BoundingBox::default();
    let mut box2 = BoundingBox::default();
    let area_1 = area_from_point(&manager.points, op_origin_1, &mut size_1, &mut box1);
    let area_2 = area_from_point(&manager.points, op_origin_2, &mut size_2, &mut box2);
    if origin_is_hole && (area_1 < 0.0) {
        manager.rings[ring_origin].points = op_origin_1;
        manager.rings[ring_origin].set_stats(area_1, size_1, box1);
        manager.rings[ring_new].points = op_origin_2;
        manager.rings[ring_new].set_stats(area_2, size_2, box2);
    } else {
        manager.rings[ring_origin].points = op_origin_2;
        manager.rings[ring_origin].set_stats(area_2, size_2, box2);
        manager.rings[ring_new].points = op_origin_1;
        manager.rings[ring_new].set_stats(area_1, size_1, box1);
    }

    update_points_ring(ring_origin, manager);
    update_points_ring(ring_new, manager);

    manager.rings[ring_origin].bottom_point = NIL;

    for i_ring in i_list.iter() {
        let ring_itr = i_ring.0;
        manager.rings[ring_itr].bottom_point = NIL;
        if origin_is_hole {
            ring1_replaces_ring2(ring_origin, ring_itr, manager);
        } else {
            let parent = manager.rings[ring_origin].parent;
            ring1_replaces_ring2(parent, ring_itr, manager);
        }
    }
    if origin_is_hole {
        assign_as_child(ring_new, ring_origin, manager)?;
        // The parent ring in this situation might need to give up children
        // to the new ring.
        let n = manager.rings[ring_parent].children.len();
        for i in 0..n {
            let c = manager.rings[ring_parent].children[i];
            if c == NIL {
                continue;
            }
            if poly2_contains_poly1(c, ring_new, manager)? {
                reassign_as_child(c, ring_new, manager)?;
            }
        }
    } else {
        // The new ring and the origin ring need to be siblings
        // however some children ring from the ring origin might
        // need to be re-assigned to the new ring
        assign_as_sibling(ring_new, ring_origin, manager)?;
        let n = manager.rings[ring_origin].children.len();
        for i in 0..n {
            let c = manager.rings[ring_origin].children[i];
            if c == NIL {
                continue;
            }
            if poly2_contains_poly1(c, ring_new, manager)? {
                reassign_as_child(c, ring_new, manager)?;
            }
        }
    }

    let mut move_list: Vec<(RingPtr, PointPtrPair)> = Vec::new();

    for i_ring in i_list.iter() {
        let n = connection_map.group(i_ring.0).len();
        if n != 0 {
            for it in 0..n {
                let pair = connection_map.group(i_ring.0)[it];
                let it_ring = manager.points[pair.op1].ring;
                let it_ring2 = manager.points[pair.op2].ring;
                if it_ring == NIL || it_ring2 == NIL || it_ring == it_ring2 {
                    continue;
                }
                if manager.is_hole(it_ring) || manager.is_hole(it_ring2) {
                    move_list.push((it_ring, pair));
                }
            }
            connection_map.erase_key(i_ring.0);
        }
    }

    let mut it = 0;
    while it < connection_map.group(ring_origin).len() {
        let pair = connection_map.group(ring_origin)[it];
        let it_ring = manager.points[pair.op1].ring;
        let it_ring2 = manager.points[pair.op2].ring;
        if it_ring == NIL || it_ring2 == NIL || it_ring == it_ring2 {
            connection_map.group(ring_origin).remove(it);
            continue;
        }
        if it_ring != ring_origin {
            if manager.is_hole(it_ring) || manager.is_hole(it_ring2) {
                move_list.push((it_ring, pair));
            }
            connection_map.group(ring_origin).remove(it);
        } else if manager.is_hole(it_ring) || manager.is_hole(it_ring2) {
            it += 1;
        } else {
            connection_map.group(ring_origin).remove(it);
        }
    }

    for (k, v) in move_list {
        connection_map.emplace(k, v);
    }

    Ok(())
}

fn correct_chained_repeats(
    manager: &mut RingManager,
    connection_map: &mut ConnectionMap,
    points: &[PointPtr],
) -> Result<()> {
    for itr1 in 0..points.len() {
        if manager.points[points[itr1]].ring == NIL {
            continue;
        }
        for itr2 in itr1 + 1..points.len() {
            if manager.points[points[itr2]].ring == NIL {
                continue;
            }
            process_single_intersection(connection_map, points[itr1], points[itr2], manager)?;
        }
    }
    Ok(())
}

fn correct_chained_rings(manager: &mut RingManager) -> Result<()> {
    if manager.all_points.len() < 2 {
        return Ok(());
    }
    // Setup connection map which is a map of rings and their
    // connection point pairs with other rings.
    let mut connection_map = ConnectionMap::new();

    // Now lets find and process any points
    // that overlap -- we should have solved
    // all situations where these points
    // would be self intersections of a ring with
    // earlier processing so this should just be
    // points where different rings are touching.
    let all_points = std::mem::take(&mut manager.all_points);
    let result = for_each_repeated_run(&all_points, manager, |manager, run| {
        correct_chained_repeats(manager, &mut connection_map, run)
    });
    manager.all_points = all_points;
    result
}

fn sort_rings_largest_to_smallest(manager: &mut RingManager) -> Vec<RingPtr> {
    let mut sorted_rings: Vec<RingPtr> = (0..manager.rings.len()).collect();
    stable_sort(&mut sorted_rings, true, |&r1, &r2| {
        if manager.rings[r1].points == NIL || manager.rings[r2].points == NIL {
            return manager.rings[r1].points != NIL;
        }
        manager.area(r1).abs() > manager.area(r2).abs()
    });
    sorted_rings
}

fn sort_rings_smallest_to_largest(manager: &mut RingManager) -> Vec<RingPtr> {
    let mut sorted_rings: Vec<RingPtr> = (0..manager.rings.len()).collect();
    stable_sort(&mut sorted_rings, true, |&r1, &r2| {
        if manager.rings[r1].points == NIL || manager.rings[r2].points == NIL {
            return manager.rings[r1].points != NIL;
        }
        manager.area(r1).abs() < manager.area(r2).abs()
    });
    sorted_rings
}

#[derive(Clone, Copy, Debug)]
struct CollinearPath {
    // Collinear edges are in opposite directions
    // such that start_1 is at the same position
    // of end_2 and vise versa. Start to end is
    // always forward (next) on path.
    start_1: PointPtr,
    end_1: PointPtr,
    start_2: PointPtr,
    end_2: PointPtr,
}

#[derive(Clone, Copy, Debug)]
struct CollinearResult {
    pt1: PointPtr,
    pt2: PointPtr,
}

/// `itr->prev->next = nullptr; itr->prev = nullptr; itr->ring = nullptr;`
#[inline]
fn detach_point(pts: &mut [PointNode], itr: PointPtr) {
    let prev = pts[itr].prev;
    pts[prev].next = NIL;
    pts[itr].prev = NIL;
    pts[itr].ring = NIL;
}

fn fix_collinear_path(path: &CollinearPath, pts: &mut [PointNode]) -> CollinearResult {
    // Left and right are just the opposite ends of the
    // collinear path, they may not be actually left
    // and right of each other.
    // The left end is start_1 and end_2
    // The right end is start_2 and end_1

    // NOTE spike detection is checking that the
    // pointers are the same values, not position!
    // additionally duplicate points we will treat
    // if they are a spike left.
    let spike_left = path.start_1 == path.end_2;
    let spike_right = path.start_2 == path.end_1;

    if spike_left && spike_right {
        // If both ends are spikes we should simply
        // delete all the points. (they should be in a loop)
        let mut itr = path.start_1;
        while itr != NIL {
            detach_point(pts, itr);
            itr = pts[itr].next;
        }
        CollinearResult { pt1: NIL, pt2: NIL }
    } else if spike_left {
        let prev = pts[path.start_2].prev;
        let mut itr = path.start_2;
        while itr != path.end_1 {
            detach_point(pts, itr);
            itr = pts[itr].next;
        }
        pts[prev].next = path.end_1;
        pts[path.end_1].prev = prev;
        CollinearResult {
            pt1: path.end_1,
            pt2: NIL,
        }
    } else if spike_right {
        let prev = pts[path.start_1].prev;
        let mut itr = path.start_1;
        while itr != path.end_2 {
            detach_point(pts, itr);
            itr = pts[itr].next;
        }
        pts[prev].next = path.end_2;
        pts[path.end_2].prev = prev;
        CollinearResult {
            pt1: path.end_2,
            pt2: NIL,
        }
    } else {
        let prev_1 = pts[path.start_1].prev;
        let prev_2 = pts[path.start_2].prev;
        let mut itr = path.start_1;
        loop {
            detach_point(pts, itr);
            itr = pts[itr].next;
            if !(itr != path.end_1 && itr != NIL) {
                break;
            }
        }
        itr = path.start_2;
        loop {
            detach_point(pts, itr);
            itr = pts[itr].next;
            if !(itr != path.end_2 && itr != NIL) {
                break;
            }
        }
        if path.start_1 == path.end_1 && path.start_2 == path.end_2 {
            CollinearResult { pt1: NIL, pt2: NIL }
        } else if path.start_1 == path.end_1 {
            pts[prev_2].next = path.end_2;
            pts[path.end_2].prev = prev_2;
            CollinearResult {
                pt1: path.end_2,
                pt2: NIL,
            }
        } else if path.start_2 == path.end_2 {
            pts[prev_1].next = path.end_1;
            pts[path.end_1].prev = prev_1;
            CollinearResult {
                pt1: path.end_1,
                pt2: NIL,
            }
        } else {
            pts[prev_1].next = path.end_2;
            pts[path.end_2].prev = prev_1;
            pts[prev_2].next = path.end_1;
            pts[path.end_1].prev = prev_2;
            CollinearResult {
                pt1: path.end_1,
                pt2: path.end_2,
            }
        }
    }
}

fn find_start_and_end_of_collinear_edges(
    pt_a: PointPtr,
    pt_b: PointPtr,
    pts: &[PointNode],
) -> CollinearPath {
    let next = |p: PointPtr| pts[p].next;
    let prev = |p: PointPtr| pts[p].prev;
    let eq = |a: PointPtr, b: PointPtr| same_pos(pts, a, b);

    // Search backward on A, forwards on B first
    let same_ring = pts[pt_a].ring == pts[pt_b].ring;
    let mut back = pt_a;
    let mut forward = pt_b;
    let mut first = true;
    loop {
        while eq(prev(back), back) && back != forward {
            back = prev(back);
            if back == pt_a {
                break;
            }
        }
        if back == forward {
            back = prev(back);
            forward = next(forward);
            break;
        }
        while eq(next(forward), forward) && back != forward {
            forward = next(forward);
            if forward == pt_b {
                break;
            }
        }
        if !first && (back == pt_a || forward == pt_b) {
            break;
        }
        if back == forward {
            back = prev(back);
            forward = next(forward);
            break;
        }
        back = prev(back);
        forward = next(forward);
        first = false;
        if !eq(back, forward) {
            break;
        }
    }
    let mut start_a = next(back);
    // If there are repeated points at the diverge we want to select
    // only the first of those repeated points.
    while !same_ring && eq(start_a, next(start_a)) && start_a != pt_a {
        start_a = next(start_a);
    }
    let mut end_b = prev(forward);
    while !same_ring && eq(end_b, prev(end_b)) && end_b != pt_b {
        end_b = prev(end_b);
    }
    // Search backward on B, forwards on A next
    back = pt_b;
    forward = pt_a;
    first = true;
    loop {
        while eq(prev(back), back) && back != forward {
            back = prev(back);
            if back == pt_b {
                break;
            }
        }
        if back == forward {
            back = prev(back);
            forward = next(forward);
            break;
        }
        while eq(next(forward), forward) && back != forward {
            forward = next(forward);
            if forward == pt_a {
                break;
            }
        }
        if !first && (back == pt_b || forward == pt_a) {
            break;
        }
        if back == forward || (!first && (back == end_b || forward == start_a)) {
            back = prev(back);
            forward = next(forward);
            break;
        }
        back = prev(back);
        forward = next(forward);
        first = false;
        if !eq(back, forward) {
            break;
        }
    }
    let mut start_b = next(back);
    while !same_ring && eq(start_b, next(start_b)) && start_b != pt_b {
        start_b = next(start_b);
    }
    let mut end_a = prev(forward);
    while !same_ring && eq(end_a, prev(end_a)) && end_a != pt_a {
        end_a = prev(end_a);
    }
    CollinearPath {
        start_1: start_a,
        end_1: end_a,
        start_2: start_b,
        end_2: end_b,
    }
}

fn has_collinear_edge(pt_a: PointPtr, pt_b: PointPtr, pts: &[PointNode]) -> bool {
    // It is assumed pt_a and pt_b are at the same location.
    same_pos(pts, pts[pt_a].next, pts[pt_b].prev) || same_pos(pts, pts[pt_b].next, pts[pt_a].prev)
}

fn process_collinear_edges_same_ring(pt_a: PointPtr, pt_b: PointPtr, manager: &mut RingManager) {
    let original_ring = manager.points[pt_a].ring;
    // As they are the same ring that are forming a collinear edge
    // we should expect the creation of two different rings.
    let path = find_start_and_end_of_collinear_edges(pt_a, pt_b, &manager.points);
    let results = fix_collinear_path(&path, &mut manager.points);
    if results.pt1 == NIL {
        // If pt1 is a nullptr, both values
        // are nullptrs. This mean the ring was
        // removed during this processing.
        remove_ring(original_ring, manager, false, true);
    } else if results.pt2 == NIL {
        // If a single point is only returned, we simply removed a spike.
        // In this case, we don't need to worry about parent or children
        // and we simply need to set the points and clear the area
        manager.rings[original_ring].points = results.pt1;
        manager.recalculate_stats(original_ring);
    } else {
        // If we have two seperate points, the ring has split into
        // two different rings.
        let ring_new = create_new_ring(manager);
        manager.rings[ring_new].points = results.pt2;
        manager.recalculate_stats(ring_new);
        update_points_ring(ring_new, manager);
        manager.rings[original_ring].points = results.pt1;
        manager.recalculate_stats(original_ring);
    }
}

fn process_collinear_edges_different_rings(
    pt_a: PointPtr,
    pt_b: PointPtr,
    manager: &mut RingManager,
) {
    let ring_a = manager.points[pt_a].ring;
    let ring_b = manager.points[pt_b].ring;
    let ring_a_larger = manager.area(ring_a).abs() > manager.area(ring_b).abs();
    let path = find_start_and_end_of_collinear_edges(pt_a, pt_b, &manager.points);
    // This should result in two rings becoming one.
    let results = fix_collinear_path(&path, &mut manager.points);
    if results.pt1 == NIL {
        remove_ring(ring_a, manager, false, true);
        remove_ring(ring_b, manager, false, true);
        return;
    }
    // Rings should merge into a single ring of the same orientation.
    // Therefore, we we will need to replace one ring with the other
    let merged_ring = if ring_a_larger { ring_a } else { ring_b };
    let deleted_ring = if ring_a_larger { ring_b } else { ring_a };

    manager.rings[merged_ring].points = results.pt1;
    update_points_ring(merged_ring, manager);
    manager.recalculate_stats(merged_ring);
    if manager.size(merged_ring) < 3 {
        remove_ring_and_points(merged_ring, manager, false, true);
    }
    remove_ring(deleted_ring, manager, false, true);
}

/// Unlinks `remove`, a duplicate neighbour of `keep`, the way the loops in
/// `remove_duplicate_points` do.
fn unlink_duplicate(keep: PointPtr, remove: PointPtr, forward: bool, manager: &mut RingManager) {
    let pts = &mut manager.points;
    if forward {
        pts[keep].next = pts[remove].next;
        let n = pts[keep].next;
        pts[n].prev = keep;
    } else {
        pts[keep].prev = pts[remove].prev;
        let p = pts[keep].prev;
        pts[p].next = keep;
    }
    pts[remove].next = NIL;
    pts[remove].prev = NIL;
    pts[remove].ring = NIL;
    let r = pts[keep].ring;
    if manager.rings[r].points == remove {
        manager.rings[r].points = keep;
    }
}

fn remove_duplicate_points(pt_a: PointPtr, pt_b: PointPtr, manager: &mut RingManager) -> bool {
    if manager.points[pt_a].ring == manager.points[pt_b].ring {
        if manager.points[pt_a].next == pt_b {
            unlink_duplicate(pt_a, pt_b, true, manager);
            return true;
        } else if manager.points[pt_b].next == pt_a {
            unlink_duplicate(pt_a, pt_b, false, manager);
            return true;
        }
    }
    while same_pos(&manager.points, manager.points[pt_a].next, pt_a)
        && manager.points[pt_a].next != pt_a
    {
        let remove = manager.points[pt_a].next;
        unlink_duplicate(pt_a, remove, true, manager);
    }
    while same_pos(&manager.points, manager.points[pt_a].prev, pt_a)
        && manager.points[pt_a].prev != pt_a
    {
        let remove = manager.points[pt_a].prev;
        unlink_duplicate(pt_a, remove, false, manager);
    }
    if manager.points[pt_a].next == pt_a {
        remove_ring_and_points(manager.points[pt_a].ring, manager, false, true);
        return true;
    }
    if manager.points[pt_b].ring == NIL {
        return true;
    }
    while same_pos(&manager.points, manager.points[pt_b].next, pt_b)
        && manager.points[pt_b].next != pt_b
    {
        let remove = manager.points[pt_b].next;
        unlink_duplicate(pt_b, remove, true, manager);
    }
    while same_pos(&manager.points, manager.points[pt_b].prev, pt_b)
        && manager.points[pt_b].prev != pt_b
    {
        let remove = manager.points[pt_b].prev;
        unlink_duplicate(pt_b, remove, false, manager);
    }
    if manager.points[pt_b].next == pt_b {
        remove_ring_and_points(manager.points[pt_b].ring, manager, false, true);
        return true;
    }
    if manager.points[pt_a].ring == NIL {
        return true;
    }
    false
}

fn process_collinear_edges(pt_a: PointPtr, pt_b: PointPtr, manager: &mut RingManager) -> bool {
    // Neither point assigned to a ring (deleted points)
    if manager.points[pt_a].ring == NIL || manager.points[pt_b].ring == NIL {
        return false;
    }

    if remove_duplicate_points(pt_a, pt_b, manager) {
        return true;
    }

    if !has_collinear_edge(pt_a, pt_b, &manager.points) {
        if manager.points[pt_a].ring == manager.points[pt_b].ring {
            correct_self_intersection(pt_a, pt_b, manager);
            return true;
        }
        return false;
    }

    if manager.points[pt_a].ring == manager.points[pt_b].ring {
        process_collinear_edges_same_ring(pt_a, pt_b, manager);
    } else {
        process_collinear_edges_different_rings(pt_a, pt_b, manager);
    }
    true
}

fn correct_collinear_repeats(manager: &mut RingManager, points: &[PointPtr]) {
    for itr1 in 0..points.len() {
        if manager.points[points[itr1]].ring == NIL {
            continue;
        }
        let mut itr2 = 0;
        while itr2 != points.len() {
            if manager.points[points[itr1]].ring == NIL {
                break;
            }
            if manager.points[points[itr2]].ring == NIL || points[itr2] == points[itr1] {
                itr2 += 1;
                continue;
            }
            if process_collinear_edges(points[itr1], points[itr2], manager) {
                itr2 = 0;
            } else {
                itr2 += 1;
            }
        }
    }
}

fn correct_collinear_edges(manager: &mut RingManager) {
    if manager.all_points.len() < 2 {
        return;
    }
    let all_points = std::mem::take(&mut manager.all_points);
    let _ = for_each_repeated_run(&all_points, manager, |manager, run| {
        correct_collinear_repeats(manager, run);
        Ok(())
    });
    manager.all_points = all_points;
}

fn correct_tree(manager: &mut RingManager) -> Result<()> {
    // It is possible that vatti resulted in some parent child
    // relationships that are not quite right, therefore, we
    // need to just rebuild the entire tree of rings

    // First lets process the rings in order of size from largest
    // area to smallest, we know right away that no smaller ring could ever
    // contain a larger ring so we can use this to our advantage
    // as we iterate over the rings.
    let sorted_rings = sort_rings_largest_to_smallest(manager);
    // Only the ring being placed loses its points below, and it has not been
    // tested against yet, so the rings that are indexed do not change.
    let mut indexes = RingIndexes::default();
    for itr in 0..sorted_rings.len() {
        let r = sorted_rings[itr];
        if manager.rings[r].points == NIL {
            continue;
        }
        if manager.size(r) < 3 || value_is_zero(manager.area(r)) {
            remove_ring_and_points(r, manager, false, true);
            continue;
        }
        manager.rings[r].corrected = true;
        let mut found = false;
        // Search in reverse from the current iterator back to the begining
        // to see if any of those rings might be its parent.
        for r_itr in (0..itr).rev() {
            let rr = sorted_rings[r_itr];
            // If orientations are not different, this can't be its parent.
            if manager.is_hole(rr) == manager.is_hole(r) {
                continue;
            }
            if poly2_contains_poly1_indexed(r, rr, manager, &mut indexes)? {
                reassign_as_child(r, rr, manager)?;
                found = true;
                break;
            }
        }
        if !found {
            if manager.is_hole(r) {
                // No remaining ring contains this hole (degenerate input, e.g. stacked
                // duplicate rings from coalesced tiny-polygon placeholders); drop it
                // rather than fail the entire tiling run.
                remove_ring_and_points(r, manager, false, true);
            } else {
                // Assign to base of tree by passing nullptr
                reassign_as_child(r, NIL, manager)?;
            }
        }
    }
    Ok(())
}

fn correct_self_intersections(manager: &mut RingManager, correct_tree: bool) -> Result<bool> {
    let mut fixed_intersections = false;
    let sorted_rings = sort_rings_smallest_to_largest(manager);
    for r in sorted_rings {
        if correct_ring_self_intersections(manager, r, correct_tree)? {
            fixed_intersections = true;
        }
    }
    Ok(fixed_intersections)
}

pub(crate) fn correct_topology(manager: &mut RingManager) -> Result<()> {
    // Sort all the points, this will be used for the locating of chained rings
    // and the collinear edges and only needs to be done once.
    let mut all_points = std::mem::take(&mut manager.all_points);
    stable_sort(&mut all_points, true, |&op1, &op2| {
        point_ptr_cmp(op1, op2, manager)
    });
    manager.all_points = all_points;

    // Initially the orientations of the rings
    // could be incorrect, we need to adjust them
    correct_orientations(manager);

    // We should only have to fix collinear edges once.
    // During this we also correct self intersections
    correct_collinear_edges(manager);

    correct_self_intersections(manager, false)?;

    correct_tree(manager)?;

    let mut fixed_intersections = true;
    while fixed_intersections {
        correct_chained_rings(manager)?;
        fixed_intersections = correct_self_intersections(manager, true)?;
    }
    Ok(())
}
