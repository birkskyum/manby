//! Port of `ring.hpp`.
//!
//! `reassign_as_sibling`, `remove_points` and the `link_before`/`link_after`/`transfer_point`
//! helpers are not ported because nothing reachable from `add_ring` or `execute` uses them.

use crate::config::{Error, Result};
use crate::geometry::{BoundingBox, Point};
use crate::point::{NIL, PointNode, PointPtr, RingPtr};

pub(crate) fn area_from_point(points: &[PointNode], mut op: PointPtr, size: &mut usize, bbox: &mut BoundingBox) -> f64 {
    let start_op = op;
    *size = 0;
    let mut a = 0.0;
    let mut min_x = points[op].x;
    let mut max_x = points[op].x;
    let mut min_y = points[op].y;
    let mut max_y = points[op].y;
    loop {
        *size += 1;
        let p = &points[op];
        if p.x > max_x {
            max_x = p.x;
        } else if p.x < min_x {
            min_x = p.x;
        }
        if p.y > max_y {
            max_y = p.y;
        } else if p.y < min_y {
            min_y = p.y;
        }
        let prev = &points[p.prev];
        a += (prev.x + p.x) as f64 * (prev.y - p.y) as f64;
        op = p.next;
        if op == start_op {
            break;
        }
    }
    bbox.min.x = min_x;
    bbox.max.x = max_x;
    bbox.min.y = min_y;
    bbox.max.y = max_y;
    a * 0.5
}

#[derive(Clone, Debug)]
pub(crate) struct Ring {
    pub ring_index: usize, // To support unset 0 is undefined and indexes offset by 1

    pub size_: usize,       // number of points in the ring
    pub area_: f64,         // area of the ring
    pub bbox: BoundingBox, // bounding box of the ring

    pub parent: RingPtr,
    pub children: Vec<RingPtr>,

    pub points: PointPtr,
    pub bottom_point: PointPtr,
    pub is_hole_: bool,
    pub corrected: bool,
}

impl Ring {
    fn new() -> Ring {
        Ring {
            ring_index: 0,
            size_: 0,
            area_: f64::NAN,
            bbox: BoundingBox::new(Point::new(0, 0), Point::new(0, 0)),
            parent: NIL,
            children: Vec::new(),
            points: NIL,
            bottom_point: NIL,
            is_hole_: false,
            corrected: false,
        }
    }

    pub fn reset_stats(&mut self) {
        self.area_ = f64::NAN;
        self.is_hole_ = false;
        self.bbox.min.x = 0;
        self.bbox.min.y = 0;
        self.bbox.max.x = 0;
        self.bbox.max.y = 0;
        self.size_ = 0;
    }

    pub fn set_stats(&mut self, a: f64, s: usize, b: BoundingBox) {
        self.bbox = b;
        self.area_ = a;
        self.size_ = s;
        self.is_hole_ = !(self.area_ > 0.0);
    }
}

pub(crate) struct RingManager {
    pub children: Vec<RingPtr>,
    pub all_points: Vec<PointPtr>,
    pub hot_pixels: Vec<Point>,
    pub current_hp_itr: usize,
    /// Arena for both `points` and `storage` of the C++ ring manager.
    pub points: Vec<PointNode>,
    pub rings: Vec<Ring>,
    pub index: usize,
}

impl RingManager {
    pub fn new() -> RingManager {
        RingManager {
            children: Vec::new(),
            all_points: Vec::new(),
            hot_pixels: Vec::new(),
            current_hp_itr: 0,
            points: Vec::new(),
            rings: Vec::new(),
            index: 0,
        }
    }

    pub fn recalculate_stats(&mut self, r: RingPtr) {
        let ring = &mut self.rings[r];
        if ring.points != NIL {
            ring.area_ = area_from_point(&self.points, ring.points, &mut ring.size_, &mut ring.bbox);
            ring.is_hole_ = !(ring.area_ > 0.0);
        }
    }

    pub fn area(&mut self, r: RingPtr) -> f64 {
        if self.rings[r].area_.is_nan() {
            self.recalculate_stats(r);
        }
        self.rings[r].area_
    }

    pub fn is_hole(&mut self, r: RingPtr) -> bool {
        if self.rings[r].area_.is_nan() {
            self.recalculate_stats(r);
        }
        self.rings[r].is_hole_
    }

    pub fn size(&mut self, r: RingPtr) -> usize {
        if self.rings[r].area_.is_nan() {
            self.recalculate_stats(r);
        }
        self.rings[r].size_
    }

    /// `parent == nullptr ? manager.children : parent->children`
    pub fn children_of(&mut self, parent: RingPtr) -> &mut Vec<RingPtr> {
        if parent == NIL { &mut self.children } else { &mut self.rings[parent].children }
    }
}

pub(crate) fn create_new_ring(manager: &mut RingManager) -> RingPtr {
    let mut ring = Ring::new();
    ring.ring_index = manager.index;
    manager.index += 1;
    manager.rings.push(ring);
    manager.rings.len() - 1
}

pub(crate) fn create_new_point(r: RingPtr, pt: Point, rings: &mut RingManager) -> PointPtr {
    let point = rings.points.len();
    rings.points.push(PointNode { ring: r, x: pt.x, y: pt.y, next: point, prev: point });
    rings.all_points.push(point);
    point
}

pub(crate) fn create_new_point_before(
    r: RingPtr,
    pt: Point,
    before_this_point: PointPtr,
    rings: &mut RingManager,
) -> PointPtr {
    let point = rings.points.len();
    let prev = rings.points[before_this_point].prev;
    rings.points.push(PointNode { ring: r, x: pt.x, y: pt.y, next: before_this_point, prev });
    rings.points[before_this_point].prev = point;
    rings.points[prev].next = point;
    rings.all_points.push(point);
    point
}

pub(crate) fn set_to_children(r: RingPtr, children: &mut Vec<RingPtr>) {
    for c in children.iter_mut() {
        if *c == NIL {
            *c = r;
            return;
        }
    }
    children.push(r);
}

pub(crate) fn remove_from_children(r: RingPtr, children: &mut [RingPtr]) {
    for c in children.iter_mut() {
        if *c == r {
            *c = NIL;
            return;
        }
    }
}

pub(crate) fn assign_as_child(new_ring: RingPtr, parent: RingPtr, manager: &mut RingManager) -> Result<()> {
    // Assigning as a child assumes that this is
    // a brand new ring. Therefore it does
    // not have any existing relationships
    if (parent == NIL && manager.is_hole(new_ring))
        || (parent != NIL && manager.is_hole(new_ring) == manager.is_hole(parent))
    {
        return Err(Error::new("Trying to assign a child that is the same orientation as the parent"));
    }
    set_to_children(new_ring, manager.children_of(parent));
    manager.rings[new_ring].parent = parent;
    Ok(())
}

pub(crate) fn reassign_as_child(ring: RingPtr, parent: RingPtr, manager: &mut RingManager) -> Result<()> {
    // Reassigning a ring assumes it already
    // has an existing parent
    if (parent == NIL && manager.is_hole(ring)) || (parent != NIL && manager.is_hole(ring) == manager.is_hole(parent)) {
        return Err(Error::new("Trying to re-assign a child that is the same orientation as the parent"));
    }

    // Remove the old child relationship
    let old_parent = manager.rings[ring].parent;
    remove_from_children(ring, manager.children_of(old_parent));

    // Add new child relationship
    set_to_children(ring, manager.children_of(parent));
    manager.rings[ring].parent = parent;
    Ok(())
}

pub(crate) fn assign_as_sibling(new_ring: RingPtr, sibling: RingPtr, manager: &mut RingManager) -> Result<()> {
    // Assigning as a sibling assumes that this is
    // a brand new ring. Therefore it does
    // not have any existing relationships
    if manager.is_hole(new_ring) != manager.is_hole(sibling) {
        return Err(Error::new(
            "Trying to assign to be a sibling that is not the same orientation as the sibling",
        ));
    }
    let sibling_parent = manager.rings[sibling].parent;
    set_to_children(new_ring, manager.children_of(sibling_parent));
    manager.rings[new_ring].parent = sibling_parent;
    Ok(())
}

pub(crate) fn ring1_replaces_ring2(ring1: RingPtr, ring2: RingPtr, manager: &mut RingManager) {
    debug_assert!(ring1 != ring2);
    let n = manager.rings[ring2].children.len();
    for i in 0..n {
        let c = manager.rings[ring2].children[i];
        if c == NIL {
            continue;
        }
        manager.rings[c].parent = ring1;
        set_to_children(c, manager.children_of(ring1));
        manager.rings[ring2].children[i] = NIL;
    }
    // Remove the old child relationship
    let old_parent = manager.rings[ring2].parent;
    remove_from_children(ring2, manager.children_of(old_parent));
    manager.rings[ring2].points = NIL;
    manager.rings[ring2].reset_stats();
}

pub(crate) fn remove_ring_and_points(
    r: RingPtr,
    manager: &mut RingManager,
    remove_children: bool,
    remove_from_parent: bool,
) {
    // Removes a ring and any children that might be
    // under that ring.
    let n = manager.rings[r].children.len();
    for i in 0..n {
        let c = manager.rings[r].children[i];
        if c == NIL {
            continue;
        }
        if remove_children {
            remove_ring_and_points(c, manager, true, false);
        }
        manager.rings[r].children[i] = NIL;
    }
    if remove_from_parent {
        // Remove the old child relationship
        let old_parent = manager.rings[r].parent;
        remove_from_children(r, manager.children_of(old_parent));
    }
    let mut pt = manager.rings[r].points;
    if pt != NIL {
        let pts = &mut manager.points;
        let prev = pts[pt].prev;
        pts[prev].next = NIL;
        while pt != NIL {
            let tmp = pt;
            pt = pts[pt].next;
            pts[tmp].next = NIL;
            pts[tmp].prev = NIL;
            pts[tmp].ring = NIL;
        }
    }
    manager.rings[r].points = NIL;
    manager.rings[r].reset_stats();
}

pub(crate) fn remove_ring(r: RingPtr, manager: &mut RingManager, remove_children: bool, remove_from_parent: bool) {
    // Removes a ring and any children that might be
    // under that ring.
    let n = manager.rings[r].children.len();
    for i in 0..n {
        let c = manager.rings[r].children[i];
        if c == NIL {
            continue;
        }
        if remove_children {
            remove_ring(c, manager, true, false);
        }
        manager.rings[r].children[i] = NIL;
    }
    if remove_from_parent {
        // Remove the old child relationship
        let old_parent = manager.rings[r].parent;
        remove_from_children(r, manager.children_of(old_parent));
    }
    manager.rings[r].points = NIL;
    manager.rings[r].reset_stats();
}

#[inline]
pub(crate) fn ring_depth(rings: &[Ring], mut r: RingPtr) -> usize {
    let mut depth = 0;
    if r == NIL {
        return depth;
    }
    while rings[r].parent != NIL {
        depth += 1;
        r = rings[r].parent;
    }
    depth
}

#[inline]
pub(crate) fn ring_is_hole(rings: &[Ring], r: RingPtr) -> bool {
    // This is different then the "normal" way of determing if
    // a ring is a hole or not because it uses the depth of the
    // the ring to determine if it is a hole or not. This is only done
    // intially when rings are output from Vatti.
    ring_depth(rings, r) & 1 != 0
}

pub(crate) fn reverse_ring(points: &mut [PointNode], pp: PointPtr) {
    if pp == NIL {
        return;
    }
    let mut pp1 = pp;
    loop {
        let pp2 = points[pp1].next;
        points[pp1].next = points[pp1].prev;
        points[pp1].prev = pp2;
        pp1 = pp2;
        if pp1 == pp {
            break;
        }
    }
}
