/*******************************************************************************
 * Author    :  Angus Johnson                                                   *
 * Date      :  17 September 2024                                               *
 * Website   :  http://www.angusj.com                                           *
 * Copyright :  Angus Johnson 2010-2024                                         *
 * Purpose   :  This is the main polygon clipping module                        *
 * License   :  http://www.boost.org/LICENSE_1_0.txt                            *
 *******************************************************************************/

//! Port of `clipper.engine.h` and `clipper.engine.cpp`: `ClipperBase` and `Clipper64`,
//! returning closed and open paths (no PolyTree output).
//!
//! PORT: the C++ links `Vertex`, `OutPt`, `OutRec`, `Active` and `LocalMinima` objects with
//! raw pointers. Here each kind lives in a `Vec` owned by `ClipperBase` and the links are
//! indices into it; a null pointer is `None`. `delete` leaves the slot unused until `CleanUp`.
//!
//! PORT: left out because nothing reaches them from `Clipper64` with path output: PolyTree
//! and `ClipperD`, `ReuseableDataContainer64`, `AddPath`, the unused `ClipperBase::AddLocMin`
//! member, and the helpers this version never calls (`IsInvalidPath`, `PointCount`,
//! `ReverseOutPts`, `SwapSides`, `UpdateOutrecOwner`, `GetHorzExtendedHorzSeg`).

use std::collections::BinaryHeap;

use crate::clipper_core::{
    FillRule, Path64, Paths64, Point64, cross_product, dot_product, get_closest_point_on_segment,
    get_segment_intersect_pt, is_collinear, perpendic_dist_from_line_sqrd, segments_intersect,
};
use crate::libcxx_sort;

//Note: all clipping operations except for Difference are commutative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipType {
    NoClip,
    Intersection,
    Union,
    Difference,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathType {
    Subject,
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinWith {
    NoJoin,
    Left,
    Right,
}

// enum class VertexFlags
type VertexFlags = u32;
const VF_EMPTY: VertexFlags = 0;
const VF_OPEN_START: VertexFlags = 1;
const VF_OPEN_END: VertexFlags = 2;
const VF_LOCAL_MAX: VertexFlags = 4;
const VF_LOCAL_MIN: VertexFlags = 8;

// PORT: a null Vertex link, only seen while AddPaths_ builds the rings.
const NIL: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Vertex {
    pt: Point64,
    next: usize,
    prev: usize,
    flags: VertexFlags,
}

impl Default for Vertex {
    fn default() -> Self {
        Vertex {
            pt: Point64::default(),
            next: NIL,
            prev: NIL,
            flags: VF_EMPTY,
        }
    }
}

#[derive(Clone, Copy)]
struct OutPt {
    pt: Point64,
    next: usize,
    prev: usize,
    outrec: usize,
    // PORT: only ever tested for null. The C++ pointer dangles once horz_seg_list_ is cleared.
    horz: Option<usize>,
}

//OutRec: contains a path in the clipping solution. Edges in the AEL will
//have OutRec pointers assigned when they form part of the clipping solution.
// PORT: polypath, splits, recursive_split, bounds and path are only used for PolyTree output.
struct OutRec {
    idx: usize,
    owner: Option<usize>,
    front_edge: Option<usize>,
    back_edge: Option<usize>,
    pts: Option<usize>,
    is_open: bool,
}

///////////////////////////////////////////////////////////////////
//Important: UP and DOWN here are premised on Y-axis positive down
//displays, which is the orientation used in Clipper's development.
///////////////////////////////////////////////////////////////////

struct Active {
    bot: Point64,
    top: Point64,
    curr_x: i64, //current (updated at every new scanline)
    dx: f64,
    wind_dx: i32, //1 or -1 depending on winding direction
    wind_cnt: i32,
    wind_cnt2: i32, //winding count of the opposite polytype
    outrec: Option<usize>,
    //AEL: 'active edge list' (Vatti's AET - active edge table)
    //     a linked list of all edges (from left to right) that are present
    //     (or 'active') within the current scanbeam (a horizontal 'beam' that
    //     sweeps from bottom to top over the paths in the clipping operation).
    prev_in_ael: Option<usize>,
    next_in_ael: Option<usize>,
    //SEL: 'sorted edge list' (Vatti's ST - sorted table)
    //     linked list used when sorting edges into their new positions at the
    //     top of scanbeams, but also (re)used to process horizontals.
    prev_in_sel: Option<usize>,
    next_in_sel: Option<usize>,
    jump: Option<usize>,
    vertex_top: usize,
    local_min: usize, // the bottom of an edge 'bound' (also Vatti)
    is_left_bound: bool,
    join_with: JoinWith,
}

pub(crate) struct LocalMinima {
    vertex: usize,
    polytype: PathType,
    is_open: bool,
}

#[derive(Clone, Copy)]
struct IntersectNode {
    pt: Point64,
    edge1: usize,
    edge2: usize,
}

#[derive(Clone, Copy)]
struct HorzSegment {
    left_op: usize,
    right_op: Option<usize>,
    left_to_right: bool,
}

#[derive(Clone, Copy)]
struct HorzJoin {
    op1: usize,
    op2: usize,
}

// ClipperBase -------------------------------------------------------------

pub(crate) struct ClipperBase {
    cliptype: ClipType,
    fillrule: FillRule,
    fillpos: FillRule,
    bot_y: i64,
    minima_list_sorted: bool,
    actives: Option<usize>,
    sel: Option<usize>,
    minima_list: Vec<LocalMinima>,
    current_locmin_iter: usize,
    // PORT: one Vec instead of one `new Vertex[]` per AddPaths call.
    vertex_lists: Vec<Vertex>,
    scanline_list: BinaryHeap<i64>,
    intersect_nodes: Vec<IntersectNode>,
    horz_seg_list: Vec<HorzSegment>,
    horz_join_list: Vec<HorzJoin>,
    preserve_collinear: bool,
    reverse_solution: bool,
    has_open_paths: bool,
    succeeded: bool,
    outrec_list: Vec<OutRec>,
    // PORT: storage for the objects the C++ allocates with new.
    ae: Vec<Active>,
    outpts: Vec<OutPt>,
}

// PORT: libc++'s std::priority_queue<int64_t> and BinaryHeap<i64> are both max-heaps, and
// equal values are indistinguishable, so the pop order is the same.

struct HorzSegSorter;

impl HorzSegSorter {
    // PORT: the C++ predicate as an Ordering, for the stable sort.
    fn cmp(outpts: &[OutPt], hs1: &HorzSegment, hs2: &HorzSegment) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (hs1.right_op, hs2.right_op) {
            (Some(_), Some(_)) => outpts[hs1.left_op].pt.x.cmp(&outpts[hs2.left_op].pt.x),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

struct LocMinSorter;

impl LocMinSorter {
    // PORT: the C++ predicate as an Ordering, for the stable sort.
    fn cmp(
        vertices: &[Vertex],
        loc_min1: &LocalMinima,
        loc_min2: &LocalMinima,
    ) -> std::cmp::Ordering {
        let p1 = vertices[loc_min1.vertex].pt;
        let p2 = vertices[loc_min2.vertex].pt;
        if p2.y != p1.y {
            p2.y.cmp(&p1.y)
        } else {
            p1.x.cmp(&p2.x)
        }
    }
}

#[inline]
fn is_odd(val: i32) -> bool {
    (val & 1) != 0
}

/*******************************************************************************
 *  Dx:                             0(90deg)                                    *
 *                                  |                                           *
 *               +inf (180deg) <--- o ---> -inf (0deg)                          *
 *******************************************************************************/

#[inline]
fn get_dx(pt1: Point64, pt2: Point64) -> f64 {
    let dy = (pt2.y - pt1.y) as f64;
    if dy != 0.0 {
        (pt2.x - pt1.x) as f64 / dy
    } else if pt2.x > pt1.x {
        -f64::MAX
    } else {
        f64::MAX
    }
}

// PORT: nearbyint rounds half to even in the default rounding mode, hence round_ties_even.
#[inline]
fn top_x(ae: &Active, current_y: i64) -> i64 {
    if (current_y == ae.top.y) || (ae.top.x == ae.bot.x) {
        ae.top.x
    } else if current_y == ae.bot.y {
        ae.bot.x
    } else {
        ae.bot.x + (ae.dx * (current_y - ae.bot.y) as f64).round_ties_even() as i64
    }
    // nb: std::nearbyint (or std::round) substantially *improves* performance here
    // as it greatly improves the likelihood of edge adjacency in ProcessIntersectList().
}

#[inline]
fn is_horizontal(e: &Active) -> bool {
    e.top.y == e.bot.y
}

#[inline]
fn is_heading_right_horz(e: &Active) -> bool {
    e.dx == -f64::MAX
}

#[inline]
fn is_heading_left_horz(e: &Active) -> bool {
    e.dx == f64::MAX
}

#[inline]
fn set_dx(e: &mut Active) {
    e.dx = get_dx(e.bot, e.top);
}

#[inline]
fn intersect_list_sort(a: &IntersectNode, b: &IntersectNode) -> bool {
    //note different inequality tests ...
    if a.pt.y == b.pt.y {
        a.pt.x < b.pt.x
    } else {
        a.pt.y > b.pt.y
    }
}

fn area(outpts: &[OutPt], op: usize) -> f64 {
    //https://en.wikipedia.org/wiki/Shoelace_formula
    let mut result = 0.0;
    let mut op2 = op;
    loop {
        let prev = outpts[op2].prev;
        result += (outpts[prev].pt.y + outpts[op2].pt.y) as f64
            * (outpts[prev].pt.x - outpts[op2].pt.x) as f64;
        op2 = outpts[op2].next;
        if op2 == op {
            break;
        }
    }
    result * 0.5
}

#[inline]
fn area_triangle(pt1: Point64, pt2: Point64, pt3: Point64) -> f64 {
    (pt3.y + pt1.y) as f64 * (pt3.x - pt1.x) as f64
        + (pt1.y + pt2.y) as f64 * (pt1.x - pt2.x) as f64
        + (pt2.y + pt3.y) as f64 * (pt2.x - pt3.x) as f64
}

#[inline]
fn pts_really_close(pt1: Point64, pt2: Point64) -> bool {
    ((pt1.x - pt2.x).abs() < 2) && ((pt1.y - pt2.y).abs() < 2)
}

#[inline]
fn is_very_small_triangle(outpts: &[OutPt], op: usize) -> bool {
    let next = outpts[op].next;
    let prev = outpts[op].prev;
    outpts[next].next == prev
        && (pts_really_close(outpts[prev].pt, outpts[next].pt)
            || pts_really_close(outpts[op].pt, outpts[next].pt)
            || pts_really_close(outpts[op].pt, outpts[prev].pt))
}

#[inline]
fn is_valid_closed_path(outpts: &[OutPt], op: Option<usize>) -> bool {
    match op {
        None => false,
        Some(op) => {
            (outpts[op].next != op)
                && (outpts[op].next != outpts[op].prev)
                && !is_very_small_triangle(outpts, op)
        }
    }
}

#[inline]
fn is_joined(e: &Active) -> bool {
    e.join_with != JoinWith::NoJoin
}

//------------------------------------------------------------------------------
//------------------------------------------------------------------------------

fn add_loc_min(
    list: &mut Vec<LocalMinima>,
    vertices: &mut [Vertex],
    vert: usize,
    polytype: PathType,
    is_open: bool,
) {
    //make sure the vertex is added only once ...
    if (VF_LOCAL_MIN & vertices[vert].flags) != VF_EMPTY {
        return;
    }

    vertices[vert].flags |= VF_LOCAL_MIN;
    list.push(LocalMinima {
        vertex: vert,
        polytype,
        is_open,
    });
}

fn add_paths_(
    paths: &Paths64,
    polytype: PathType,
    is_open: bool,
    vertex_lists: &mut Vec<Vertex>,
    loc_min_list: &mut Vec<LocalMinima>,
) {
    let total_vertex_count: usize = paths.iter().map(|path| path.len()).sum();
    if total_vertex_count == 0 {
        return;
    }

    let vertices = vertex_lists.len();
    vertex_lists.resize(vertices + total_vertex_count, Vertex::default());
    let vl = vertex_lists;
    let mut v = vertices;
    for path in paths {
        //for each path create a circular double linked list of vertices
        let v0 = v;
        let mut curr_v = v;
        let mut prev_v: Option<usize> = None;

        if path.is_empty() {
            continue;
        }

        vl[v].prev = NIL;
        let mut cnt = 0;
        for &pt in path {
            if let Some(pv) = prev_v {
                if vl[pv].pt == pt {
                    continue; // ie skips duplicates
                }
                vl[pv].next = curr_v;
            }
            vl[curr_v].prev = prev_v.unwrap_or(NIL);
            vl[curr_v].pt = pt;
            vl[curr_v].flags = VF_EMPTY;
            prev_v = Some(curr_v);
            curr_v += 1;
            cnt += 1;
        }
        let Some(mut prev_v) = prev_v else { continue };
        if vl[prev_v].prev == NIL {
            continue;
        }
        if !is_open && vl[prev_v].pt == vl[v0].pt {
            prev_v = vl[prev_v].prev;
        }
        vl[prev_v].next = v0;
        vl[v0].prev = prev_v;
        v = curr_v; // ie get ready for next path
        if cnt < 2 || (cnt == 2 && !is_open) {
            continue;
        }

        //now find and assign local minima
        let mut going_up;
        if is_open {
            curr_v = vl[v0].next;
            while curr_v != v0 && vl[curr_v].pt.y == vl[v0].pt.y {
                curr_v = vl[curr_v].next;
            }
            going_up = vl[curr_v].pt.y <= vl[v0].pt.y;
            if going_up {
                vl[v0].flags = VF_OPEN_START;
                add_loc_min(loc_min_list, vl, v0, polytype, true);
            } else {
                vl[v0].flags = VF_OPEN_START | VF_LOCAL_MAX;
            }
        } else {
            // closed path
            prev_v = vl[v0].prev;
            while prev_v != v0 && vl[prev_v].pt.y == vl[v0].pt.y {
                prev_v = vl[prev_v].prev;
            }
            if prev_v == v0 {
                continue; // only open paths can be completely flat
            }
            going_up = vl[prev_v].pt.y > vl[v0].pt.y;
        }

        let going_up0 = going_up;
        prev_v = v0;
        curr_v = vl[v0].next;
        while curr_v != v0 {
            if vl[curr_v].pt.y > vl[prev_v].pt.y && going_up {
                vl[prev_v].flags |= VF_LOCAL_MAX;
                going_up = false;
            } else if vl[curr_v].pt.y < vl[prev_v].pt.y && !going_up {
                going_up = true;
                add_loc_min(loc_min_list, vl, prev_v, polytype, is_open);
            }
            prev_v = curr_v;
            curr_v = vl[curr_v].next;
        }

        if is_open {
            vl[prev_v].flags |= VF_OPEN_END;
            if going_up {
                vl[prev_v].flags |= VF_LOCAL_MAX;
            } else {
                add_loc_min(loc_min_list, vl, prev_v, polytype, is_open);
            }
        } else if going_up != going_up0 {
            if going_up0 {
                add_loc_min(loc_min_list, vl, prev_v, polytype, false);
            } else {
                vl[prev_v].flags |= VF_LOCAL_MAX;
            }
        }
    } // end processing current path
}

//------------------------------------------------------------------------------
// ClipperBase methods ...
//------------------------------------------------------------------------------

impl ClipperBase {
    pub(crate) fn new() -> Self {
        ClipperBase {
            cliptype: ClipType::NoClip,
            fillrule: FillRule::EvenOdd,
            fillpos: FillRule::Positive,
            bot_y: 0,
            minima_list_sorted: false,
            actives: None,
            sel: None,
            minima_list: Vec::new(),
            current_locmin_iter: 0,
            vertex_lists: Vec::new(),
            scanline_list: BinaryHeap::new(),
            intersect_nodes: Vec::new(),
            horz_seg_list: Vec::new(),
            horz_join_list: Vec::new(),
            preserve_collinear: true,
            reverse_solution: false,
            has_open_paths: false,
            succeeded: true,
            outrec_list: Vec::new(),
            ae: Vec::new(),
            outpts: Vec::new(),
        }
    }

    pub(crate) fn preserve_collinear(&self) -> bool {
        self.preserve_collinear
    }

    pub(crate) fn set_preserve_collinear(&mut self, val: bool) {
        self.preserve_collinear = val;
    }

    pub(crate) fn reverse_solution(&self) -> bool {
        self.reverse_solution
    }

    pub(crate) fn set_reverse_solution(&mut self, val: bool) {
        self.reverse_solution = val;
    }

    #[inline]
    fn new_out_pt(&mut self, pt: Point64, outrec: usize) -> usize {
        let idx = self.outpts.len();
        self.outpts.push(OutPt {
            pt,
            next: idx,
            prev: idx,
            outrec,
            horz: None,
        });
        idx
    }

    #[inline]
    fn new_active(
        &mut self,
        bot: Point64,
        wind_dx: i32,
        vertex_top: usize,
        local_min: usize,
    ) -> usize {
        let idx = self.ae.len();
        self.ae.push(Active {
            bot,
            top: self.vertex_lists[vertex_top].pt,
            curr_x: bot.x,
            dx: 0.0,
            wind_dx,
            wind_cnt: 0,
            wind_cnt2: 0,
            outrec: None,
            prev_in_ael: None,
            next_in_ael: None,
            prev_in_sel: None,
            next_in_sel: None,
            jump: None,
            vertex_top,
            local_min,
            is_left_bound: false,
            join_with: JoinWith::NoJoin,
        });
        idx
    }

    #[inline]
    fn is_hot_edge(&self, e: usize) -> bool {
        self.ae[e].outrec.is_some()
    }

    #[inline]
    fn is_open(&self, e: usize) -> bool {
        self.minima_list[self.ae[e].local_min].is_open
    }

    #[inline]
    fn is_open_end_v(&self, v: usize) -> bool {
        (self.vertex_lists[v].flags & (VF_OPEN_START | VF_OPEN_END)) != VF_EMPTY
    }

    #[inline]
    fn is_open_end(&self, ae: usize) -> bool {
        self.is_open_end_v(self.ae[ae].vertex_top)
    }

    #[inline]
    fn get_prev_hot_edge(&self, e: usize) -> Option<usize> {
        let mut prev = self.ae[e].prev_in_ael;
        while let Some(p) = prev
            && (self.is_open(p) || !self.is_hot_edge(p))
        {
            prev = self.ae[p].prev_in_ael;
        }
        prev
    }

    #[inline]
    fn is_front(&self, e: usize) -> bool {
        Some(e) == self.outrec_list[self.ae[e].outrec.unwrap()].front_edge
    }

    #[inline]
    fn get_poly_type(&self, e: usize) -> PathType {
        self.minima_list[self.ae[e].local_min].polytype
    }

    #[inline]
    fn is_same_poly_type(&self, e1: usize, e2: usize) -> bool {
        self.minima_list[self.ae[e1].local_min].polytype
            == self.minima_list[self.ae[e2].local_min].polytype
    }

    #[inline]
    fn next_vertex(&self, e: usize) -> usize {
        if self.ae[e].wind_dx > 0 {
            self.vertex_lists[self.ae[e].vertex_top].next
        } else {
            self.vertex_lists[self.ae[e].vertex_top].prev
        }
    }

    //PrevPrevVertex: useful to get the (inverted Y-axis) top of the
    //alternate edge (ie left or right bound) during edge insertion.
    #[inline]
    fn prev_prev_vertex(&self, ae: usize) -> usize {
        let vl = &self.vertex_lists;
        if self.ae[ae].wind_dx > 0 {
            vl[vl[self.ae[ae].vertex_top].prev].prev
        } else {
            vl[vl[self.ae[ae].vertex_top].next].next
        }
    }

    #[inline]
    fn extract_from_sel(&mut self, ae: usize) -> Option<usize> {
        let res = self.ae[ae].next_in_sel;
        if let Some(r) = res {
            self.ae[r].prev_in_sel = self.ae[ae].prev_in_sel;
        }
        let p = self.ae[ae].prev_in_sel.unwrap();
        self.ae[p].next_in_sel = res;
        res
    }

    #[inline]
    fn insert1_before2_in_sel(&mut self, ae1: usize, ae2: usize) {
        self.ae[ae1].prev_in_sel = self.ae[ae2].prev_in_sel;
        if let Some(p) = self.ae[ae1].prev_in_sel {
            self.ae[p].next_in_sel = Some(ae1);
        }
        self.ae[ae1].next_in_sel = Some(ae2);
        self.ae[ae2].prev_in_sel = Some(ae1);
    }

    #[inline]
    fn is_maxima_v(&self, v: usize) -> bool {
        (self.vertex_lists[v].flags & VF_LOCAL_MAX) != VF_EMPTY
    }

    #[inline]
    fn is_maxima(&self, e: usize) -> bool {
        self.is_maxima_v(self.ae[e].vertex_top)
    }

    #[inline]
    fn get_curr_y_maxima_vertex_open(&self, e: usize) -> Option<usize> {
        let vl = &self.vertex_lists;
        let mut result = self.ae[e].vertex_top;
        if self.ae[e].wind_dx > 0 {
            while (vl[vl[result].next].pt.y == vl[result].pt.y)
                && ((vl[result].flags & (VF_OPEN_END | VF_LOCAL_MAX)) == VF_EMPTY)
            {
                result = vl[result].next;
            }
        } else {
            while vl[vl[result].prev].pt.y == vl[result].pt.y
                && ((vl[result].flags & (VF_OPEN_END | VF_LOCAL_MAX)) == VF_EMPTY)
            {
                result = vl[result].prev;
            }
        }
        if !self.is_maxima_v(result) {
            return None; // not a maxima
        }
        Some(result)
    }

    #[inline]
    fn get_curr_y_maxima_vertex(&self, e: usize) -> Option<usize> {
        let vl = &self.vertex_lists;
        let mut result = self.ae[e].vertex_top;
        if self.ae[e].wind_dx > 0 {
            while vl[vl[result].next].pt.y == vl[result].pt.y {
                result = vl[result].next;
            }
        } else {
            while vl[vl[result].prev].pt.y == vl[result].pt.y {
                result = vl[result].prev;
            }
        }
        if !self.is_maxima_v(result) {
            return None; // not a maxima
        }
        Some(result)
    }

    fn get_maxima_pair(&self, e: usize) -> Option<usize> {
        let mut e2 = self.ae[e].next_in_ael;
        while let Some(x) = e2 {
            if self.ae[x].vertex_top == self.ae[e].vertex_top {
                return Some(x); // Found!
            }
            e2 = self.ae[x].next_in_ael;
        }
        None
    }

    #[inline]
    fn duplicate_op(&mut self, op: usize, insert_after: bool) -> usize {
        let result = self.new_out_pt(self.outpts[op].pt, self.outpts[op].outrec);
        if insert_after {
            self.outpts[result].next = self.outpts[op].next;
            let rn = self.outpts[result].next;
            self.outpts[rn].prev = result;
            self.outpts[result].prev = op;
            self.outpts[op].next = result;
        } else {
            self.outpts[result].prev = self.outpts[op].prev;
            let rp = self.outpts[result].prev;
            self.outpts[rp].next = result;
            self.outpts[result].next = op;
            self.outpts[op].prev = result;
        }
        result
    }

    #[inline]
    fn dispose_out_pt(&mut self, op: usize) -> usize {
        let result = self.outpts[op].next;
        let (prev, next) = (self.outpts[op].prev, self.outpts[op].next);
        self.outpts[prev].next = next;
        self.outpts[next].prev = prev;
        result
    }

    // PORT: the OutPts are not freed, they just stop being reachable.
    #[inline]
    fn dispose_out_pts(&mut self, outrec: usize) {
        self.outrec_list[outrec].pts = None;
    }

    #[inline]
    fn set_sides(&mut self, outrec: usize, start_edge: usize, end_edge: usize) {
        self.outrec_list[outrec].front_edge = Some(start_edge);
        self.outrec_list[outrec].back_edge = Some(end_edge);
    }

    fn swap_outrecs(&mut self, e1: usize, e2: usize) {
        let or1 = self.ae[e1].outrec;
        let or2 = self.ae[e2].outrec;
        if or1 == or2 {
            let or1 = &mut self.outrec_list[or1.unwrap()];
            std::mem::swap(&mut or1.front_edge, &mut or1.back_edge);
            return;
        }
        if let Some(o1) = or1 {
            if Some(e1) == self.outrec_list[o1].front_edge {
                self.outrec_list[o1].front_edge = Some(e2);
            } else {
                self.outrec_list[o1].back_edge = Some(e2);
            }
        }
        if let Some(o2) = or2 {
            if Some(e2) == self.outrec_list[o2].front_edge {
                self.outrec_list[o2].front_edge = Some(e1);
            } else {
                self.outrec_list[o2].back_edge = Some(e1);
            }
        }
        self.ae[e1].outrec = or2;
        self.ae[e2].outrec = or1;
    }

    #[inline]
    fn get_real_outrec(&self, mut outrec: Option<usize>) -> Option<usize> {
        while let Some(o) = outrec
            && self.outrec_list[o].pts.is_none()
        {
            outrec = self.outrec_list[o].owner;
        }
        outrec
    }

    #[inline]
    fn uncouple_outrec(&mut self, ae: usize) {
        let Some(outrec) = self.ae[ae].outrec else {
            return;
        };
        let fe = self.outrec_list[outrec].front_edge.unwrap();
        self.ae[fe].outrec = None;
        let be = self.outrec_list[outrec].back_edge.unwrap();
        self.ae[be].outrec = None;
        self.outrec_list[outrec].front_edge = None;
        self.outrec_list[outrec].back_edge = None;
    }

    #[inline]
    fn outrec_is_ascending(&self, hot_edge: usize) -> bool {
        Some(hot_edge) == self.outrec_list[self.ae[hot_edge].outrec.unwrap()].front_edge
    }

    #[inline]
    fn swap_front_back_sides(&mut self, outrec: usize) {
        let or = &mut self.outrec_list[outrec];
        std::mem::swap(&mut or.front_edge, &mut or.back_edge);
        self.outrec_list[outrec].pts =
            Some(self.outpts[self.outrec_list[outrec].pts.unwrap()].next);
    }

    #[inline]
    fn edges_adjacent_in_ael(&self, inode: &IntersectNode) -> bool {
        (self.ae[inode.edge1].next_in_ael == Some(inode.edge2))
            || (self.ae[inode.edge1].prev_in_ael == Some(inode.edge2))
    }

    #[inline]
    fn set_owner(&mut self, outrec: usize, new_owner: usize) {
        //precondition1: new_owner is never null
        while let Some(o) = self.outrec_list[new_owner].owner
            && self.outrec_list[o].pts.is_none()
        {
            self.outrec_list[new_owner].owner = self.outrec_list[o].owner;
        }
        let mut tmp = Some(new_owner);
        while let Some(t) = tmp
            && t != outrec
        {
            tmp = self.outrec_list[t].owner;
        }
        if tmp.is_some() {
            self.outrec_list[new_owner].owner = self.outrec_list[outrec].owner;
        }
        self.outrec_list[outrec].owner = Some(new_owner);
    }

    // PORT: PointInOpPolygon, GetCleanPath and Path1InsidePath2 are only used for PolyTree output.

    pub(crate) fn clean_up(&mut self) {
        // DeleteEdges(actives_)
        self.actives = None;
        self.ae.clear();
        self.scanline_list.clear();
        self.intersect_nodes.clear();
        self.dispose_all_outrecs();
        self.horz_seg_list.clear();
        self.horz_join_list.clear();
    }

    pub(crate) fn clear(&mut self) {
        self.clean_up();
        self.dispose_vertices_and_local_minima();
        self.current_locmin_iter = 0;
        self.minima_list_sorted = false;
        self.has_open_paths = false;
    }

    fn reset(&mut self) {
        if !self.minima_list_sorted {
            let vertices = &self.vertex_lists;
            self.minima_list
                .sort_by(|a, b| LocMinSorter::cmp(vertices, a, b)); //#594
            self.minima_list_sorted = true;
        }
        for i in (0..self.minima_list.len()).rev() {
            let y = self.vertex_lists[self.minima_list[i].vertex].pt.y;
            self.insert_scanline(y);
        }

        self.current_locmin_iter = 0;
        self.actives = None;
        self.sel = None;
        self.succeeded = true;
    }

    pub(crate) fn add_paths(&mut self, paths: &Paths64, polytype: PathType, is_open: bool) {
        if is_open {
            self.has_open_paths = true;
        }
        self.minima_list_sorted = false;
        add_paths_(
            paths,
            polytype,
            is_open,
            &mut self.vertex_lists,
            &mut self.minima_list,
        );
    }

    #[inline]
    fn insert_scanline(&mut self, y: i64) {
        self.scanline_list.push(y);
    }

    #[inline]
    fn pop_scanline(&mut self) -> Option<i64> {
        let y = self.scanline_list.pop()?;
        while self.scanline_list.peek() == Some(&y) {
            self.scanline_list.pop(); // Pop duplicates.
        }
        Some(y)
    }

    #[inline]
    fn pop_local_minima(&mut self, y: i64) -> Option<usize> {
        if self.current_locmin_iter == self.minima_list.len()
            || self.vertex_lists[self.minima_list[self.current_locmin_iter].vertex]
                .pt
                .y
                != y
        {
            return None;
        }
        let local_minima = self.current_locmin_iter;
        self.current_locmin_iter += 1;
        Some(local_minima)
    }

    fn dispose_all_outrecs(&mut self) {
        self.outrec_list.clear();
        self.outpts.clear();
    }

    fn dispose_vertices_and_local_minima(&mut self) {
        self.minima_list.clear();
        self.vertex_lists.clear();
    }

    fn is_contributing_closed(&self, e: usize) -> bool {
        let ae = &self.ae[e];
        match self.fillrule {
            FillRule::EvenOdd => {}
            FillRule::NonZero => {
                if ae.wind_cnt.abs() != 1 {
                    return false;
                }
            }
            FillRule::Positive => {
                if ae.wind_cnt != 1 {
                    return false;
                }
            }
            FillRule::Negative => {
                if ae.wind_cnt != -1 {
                    return false;
                }
            }
        }

        match self.cliptype {
            ClipType::NoClip => false,
            ClipType::Intersection => match self.fillrule {
                FillRule::Positive => ae.wind_cnt2 > 0,
                FillRule::Negative => ae.wind_cnt2 < 0,
                _ => ae.wind_cnt2 != 0,
            },
            ClipType::Union => match self.fillrule {
                FillRule::Positive => ae.wind_cnt2 <= 0,
                FillRule::Negative => ae.wind_cnt2 >= 0,
                _ => ae.wind_cnt2 == 0,
            },
            ClipType::Difference => {
                let result = match self.fillrule {
                    FillRule::Positive => ae.wind_cnt2 <= 0,
                    FillRule::Negative => ae.wind_cnt2 >= 0,
                    _ => ae.wind_cnt2 == 0,
                };
                if self.get_poly_type(e) == PathType::Subject {
                    result
                } else {
                    !result
                }
            }
            ClipType::Xor => true,
        }
    }

    #[inline]
    fn is_contributing_open(&self, e: usize) -> bool {
        let ae = &self.ae[e];
        let (is_in_clip, is_in_subj) = match self.fillrule {
            FillRule::Positive => (ae.wind_cnt2 > 0, ae.wind_cnt > 0),
            FillRule::Negative => (ae.wind_cnt2 < 0, ae.wind_cnt < 0),
            _ => (ae.wind_cnt2 != 0, ae.wind_cnt != 0),
        };

        match self.cliptype {
            ClipType::Intersection => is_in_clip,
            ClipType::Union => !is_in_subj && !is_in_clip,
            _ => !is_in_clip,
        }
    }

    fn set_wind_count_for_closed_path_edge(&mut self, e: usize) {
        //Wind counts refer to polygon regions not edges, so here an edge's WindCnt
        //indicates the higher of the wind counts for the two regions touching the
        //edge. (NB Adjacent regions can only ever have their wind counts differ by
        //one. Also, open paths have no meaningful wind directions or counts.)

        let mut e2 = self.ae[e].prev_in_ael;
        //find the nearest closed path edge of the same PolyType in AEL (heading left)
        let pt = self.get_poly_type(e);
        while let Some(x) = e2 {
            if !(self.get_poly_type(x) != pt || self.is_open(x)) {
                break;
            }
            e2 = self.ae[x].prev_in_ael;
        }

        match e2 {
            None => {
                self.ae[e].wind_cnt = self.ae[e].wind_dx;
                e2 = self.actives;
            }
            Some(x) if self.fillrule == FillRule::EvenOdd => {
                self.ae[e].wind_cnt = self.ae[e].wind_dx;
                self.ae[e].wind_cnt2 = self.ae[x].wind_cnt2;
                e2 = self.ae[x].next_in_ael;
            }
            Some(x) => {
                //NonZero, positive, or negative filling here ...
                //if e's WindCnt is in the SAME direction as its WindDx, then polygon
                //filling will be on the right of 'e'.
                //NB neither e2.WindCnt nor e2.WindDx should ever be 0.
                if self.ae[x].wind_cnt * self.ae[x].wind_dx < 0 {
                    //opposite directions so 'e' is outside 'e2' ...
                    if self.ae[x].wind_cnt.abs() > 1 {
                        //outside prev poly but still inside another.
                        if self.ae[x].wind_dx * self.ae[e].wind_dx < 0 {
                            //reversing direction so use the same WC
                            self.ae[e].wind_cnt = self.ae[x].wind_cnt;
                        } else {
                            //otherwise keep 'reducing' the WC by 1 (ie towards 0) ...
                            self.ae[e].wind_cnt = self.ae[x].wind_cnt + self.ae[e].wind_dx;
                        }
                    } else {
                        //now outside all polys of same polytype so set own WC ...
                        self.ae[e].wind_cnt = if self.is_open(e) {
                            1
                        } else {
                            self.ae[e].wind_dx
                        };
                    }
                } else {
                    //'e' must be inside 'e2'
                    if self.ae[x].wind_dx * self.ae[e].wind_dx < 0 {
                        //reversing direction so use the same WC
                        self.ae[e].wind_cnt = self.ae[x].wind_cnt;
                    } else {
                        //otherwise keep 'increasing' the WC by 1 (ie away from 0) ...
                        self.ae[e].wind_cnt = self.ae[x].wind_cnt + self.ae[e].wind_dx;
                    }
                }
                self.ae[e].wind_cnt2 = self.ae[x].wind_cnt2;
                e2 = self.ae[x].next_in_ael; // ie get ready to calc WindCnt2
            }
        }

        //update wind_cnt2 ...
        if self.fillrule == FillRule::EvenOdd {
            while e2 != Some(e) {
                let x = e2.unwrap();
                if self.get_poly_type(x) != pt && !self.is_open(x) {
                    self.ae[e].wind_cnt2 = if self.ae[e].wind_cnt2 == 0 { 1 } else { 0 };
                }
                e2 = self.ae[x].next_in_ael;
            }
        } else {
            while e2 != Some(e) {
                let x = e2.unwrap();
                if self.get_poly_type(x) != pt && !self.is_open(x) {
                    self.ae[e].wind_cnt2 += self.ae[x].wind_dx;
                }
                e2 = self.ae[x].next_in_ael;
            }
        }
    }

    fn set_wind_count_for_open_path_edge(&mut self, e: usize) {
        let mut e2 = self.actives;
        if self.fillrule == FillRule::EvenOdd {
            let mut cnt1 = 0;
            let mut cnt2 = 0;
            while e2 != Some(e) {
                let x = e2.unwrap();
                if self.get_poly_type(x) == PathType::Clip {
                    cnt2 += 1;
                } else if !self.is_open(x) {
                    cnt1 += 1;
                }
                e2 = self.ae[x].next_in_ael;
            }
            self.ae[e].wind_cnt = if is_odd(cnt1) { 1 } else { 0 };
            self.ae[e].wind_cnt2 = if is_odd(cnt2) { 1 } else { 0 };
        } else {
            while e2 != Some(e) {
                let x = e2.unwrap();
                if self.get_poly_type(x) == PathType::Clip {
                    self.ae[e].wind_cnt2 += self.ae[x].wind_dx;
                } else if !self.is_open(x) {
                    self.ae[e].wind_cnt += self.ae[x].wind_dx;
                }
                e2 = self.ae[x].next_in_ael;
            }
        }
    }

    fn is_valid_ael_order(&self, resident: usize, newcomer: usize) -> bool {
        let r = &self.ae[resident];
        let n = &self.ae[newcomer];
        if n.curr_x != r.curr_x {
            return n.curr_x > r.curr_x;
        }

        //get the turning direction  a1.top, a2.bot, a2.top
        let d = cross_product(r.top, n.bot, n.top);
        if d != 0.0 {
            return d < 0.0;
        }

        //edges must be collinear to get here
        //for starting open paths, place them according to
        //the direction they're about to turn
        if !self.is_maxima(resident) && (r.top.y > n.top.y) {
            return cross_product(
                n.bot,
                r.top,
                self.vertex_lists[self.next_vertex(resident)].pt,
            ) <= 0.0;
        } else if !self.is_maxima(newcomer) && (n.top.y > r.top.y) {
            return cross_product(
                n.bot,
                n.top,
                self.vertex_lists[self.next_vertex(newcomer)].pt,
            ) >= 0.0;
        }

        let y = n.bot.y;
        let newcomer_is_left = n.is_left_bound;

        if r.bot.y != y || self.vertex_lists[self.minima_list[r.local_min].vertex].pt.y != y {
            n.is_left_bound
        }
        //resident must also have just been inserted
        else if r.is_left_bound != newcomer_is_left {
            newcomer_is_left
        } else if is_collinear(
            self.vertex_lists[self.prev_prev_vertex(resident)].pt,
            r.bot,
            r.top,
        ) {
            true
        } else {
            //compare turning direction of the alternate bound
            (cross_product(
                self.vertex_lists[self.prev_prev_vertex(resident)].pt,
                n.bot,
                self.vertex_lists[self.prev_prev_vertex(newcomer)].pt,
            ) > 0.0)
                == newcomer_is_left
        }
    }

    fn insert_left_edge(&mut self, e: usize) {
        match self.actives {
            None => {
                self.ae[e].prev_in_ael = None;
                self.ae[e].next_in_ael = None;
                self.actives = Some(e);
            }
            Some(a) if !self.is_valid_ael_order(a, e) => {
                self.ae[e].prev_in_ael = None;
                self.ae[e].next_in_ael = Some(a);
                self.ae[a].prev_in_ael = Some(e);
                self.actives = Some(e);
            }
            Some(a) => {
                let mut e2 = a;
                while let Some(n) = self.ae[e2].next_in_ael
                    && self.is_valid_ael_order(n, e)
                {
                    e2 = n;
                }
                let e2 = if self.ae[e2].join_with == JoinWith::Right {
                    self.ae[e2].next_in_ael
                } else {
                    Some(e2)
                };
                let Some(e2) = e2 else { return }; // should never happen and stops compiler warning :)
                self.ae[e].next_in_ael = self.ae[e2].next_in_ael;
                if let Some(n) = self.ae[e2].next_in_ael {
                    self.ae[n].prev_in_ael = Some(e);
                }
                self.ae[e].prev_in_ael = Some(e2);
                self.ae[e2].next_in_ael = Some(e);
            }
        }
    }

    fn insert_right_edge(&mut self, e: usize, e2: usize) {
        self.ae[e2].next_in_ael = self.ae[e].next_in_ael;
        if let Some(n) = self.ae[e].next_in_ael {
            self.ae[n].prev_in_ael = Some(e2);
        }
        self.ae[e2].prev_in_ael = Some(e);
        self.ae[e].next_in_ael = Some(e2);
    }

    fn insert_local_minima_into_ael(&mut self, bot_y: i64) {
        //Add any local minima (if any) at BotY ...
        //nb: horizontal local minima edges should contain locMin.vertex.prev

        while let Some(local_minima) = self.pop_local_minima(bot_y) {
            let lm_vertex = self.minima_list[local_minima].vertex;
            let lm_pt = self.vertex_lists[lm_vertex].pt;
            let mut left_bound = if (self.vertex_lists[lm_vertex].flags & VF_OPEN_START) != VF_EMPTY
            {
                None
            } else {
                let lb =
                    self.new_active(lm_pt, -1, self.vertex_lists[lm_vertex].prev, local_minima); // ie descending
                set_dx(&mut self.ae[lb]);
                Some(lb)
            };

            let mut right_bound = if (self.vertex_lists[lm_vertex].flags & VF_OPEN_END) != VF_EMPTY
            {
                None
            } else {
                let rb = self.new_active(lm_pt, 1, self.vertex_lists[lm_vertex].next, local_minima); // ie ascending
                set_dx(&mut self.ae[rb]);
                Some(rb)
            };

            //Currently LeftB is just the descending bound and RightB is the ascending.
            //Now if the LeftB isn't on the left of RightB then we need swap them.
            if let (Some(lb), Some(rb)) = (left_bound, right_bound) {
                if is_horizontal(&self.ae[lb]) {
                    if is_heading_right_horz(&self.ae[lb]) {
                        std::mem::swap(&mut left_bound, &mut right_bound);
                    }
                } else if is_horizontal(&self.ae[rb]) {
                    if is_heading_left_horz(&self.ae[rb]) {
                        std::mem::swap(&mut left_bound, &mut right_bound);
                    }
                } else if self.ae[lb].dx < self.ae[rb].dx {
                    std::mem::swap(&mut left_bound, &mut right_bound);
                }
            } else if left_bound.is_none() {
                left_bound = right_bound;
                right_bound = None;
            }

            let left_bound = left_bound.unwrap();
            self.ae[left_bound].is_left_bound = true;
            self.insert_left_edge(left_bound);

            let contributing = if self.is_open(left_bound) {
                self.set_wind_count_for_open_path_edge(left_bound);
                self.is_contributing_open(left_bound)
            } else {
                self.set_wind_count_for_closed_path_edge(left_bound);
                self.is_contributing_closed(left_bound)
            };

            if let Some(right_bound) = right_bound {
                self.ae[right_bound].is_left_bound = false;
                self.ae[right_bound].wind_cnt = self.ae[left_bound].wind_cnt;
                self.ae[right_bound].wind_cnt2 = self.ae[left_bound].wind_cnt2;
                self.insert_right_edge(left_bound, right_bound); ///////
                if contributing {
                    let bot = self.ae[left_bound].bot;
                    self.add_local_min_poly(left_bound, right_bound, bot, true);
                    if !is_horizontal(&self.ae[left_bound]) {
                        self.check_join_left(left_bound, bot, false);
                    }
                }

                while let Some(next) = self.ae[right_bound].next_in_ael
                    && self.is_valid_ael_order(next, right_bound)
                {
                    let bot = self.ae[right_bound].bot;
                    self.intersect_edges(right_bound, next, bot);
                    let next = self.ae[right_bound].next_in_ael.unwrap();
                    self.swap_positions_in_ael(right_bound, next);
                }

                if is_horizontal(&self.ae[right_bound]) {
                    self.push_horz(right_bound);
                } else {
                    let bot = self.ae[right_bound].bot;
                    self.check_join_right(right_bound, bot, false);
                    self.insert_scanline(self.ae[right_bound].top.y);
                }
            } else if contributing {
                let bot = self.ae[left_bound].bot;
                self.start_open_path(left_bound, bot);
            }

            if is_horizontal(&self.ae[left_bound]) {
                self.push_horz(left_bound);
            } else {
                self.insert_scanline(self.ae[left_bound].top.y);
            }
        } // while (PopLocalMinima())
    }

    #[inline]
    fn push_horz(&mut self, e: usize) {
        self.ae[e].next_in_sel = self.sel;
        self.sel = Some(e);
    }

    #[inline]
    fn pop_horz(&mut self) -> Option<usize> {
        let e = self.sel?;
        self.sel = self.ae[e].next_in_sel;
        Some(e)
    }

    fn add_local_min_poly(&mut self, e1: usize, e2: usize, pt: Point64, is_new: bool) -> usize {
        let outrec = self.new_outrec();
        self.ae[e1].outrec = Some(outrec);
        self.ae[e2].outrec = Some(outrec);

        if self.is_open(e1) {
            self.outrec_list[outrec].owner = None;
            self.outrec_list[outrec].is_open = true;
            if self.ae[e1].wind_dx > 0 {
                self.set_sides(outrec, e1, e2);
            } else {
                self.set_sides(outrec, e2, e1);
            }
        } else {
            let prev_hot_edge = self.get_prev_hot_edge(e1);
            //e.windDx is the winding direction of the **input** paths
            //and unrelated to the winding direction of output polygons.
            //Output orientation is determined by e.outrec.frontE which is
            //the ascending edge (see AddLocalMinPoly).
            if let Some(prev_hot_edge) = prev_hot_edge {
                // PORT: `if (using_polytree_) SetOwner(...)` is left out, using_polytree_ is false.
                if self.outrec_is_ascending(prev_hot_edge) == is_new {
                    self.set_sides(outrec, e2, e1);
                } else {
                    self.set_sides(outrec, e1, e2);
                }
            } else {
                self.outrec_list[outrec].owner = None;
                if is_new {
                    self.set_sides(outrec, e1, e2);
                } else {
                    self.set_sides(outrec, e2, e1);
                }
            }
        }

        let op = self.new_out_pt(pt, outrec);
        self.outrec_list[outrec].pts = Some(op);
        op
    }

    fn add_local_max_poly(&mut self, e1: usize, e2: usize, pt: Point64) -> Option<usize> {
        if is_joined(&self.ae[e1]) {
            self.split(e1, pt);
        }
        if is_joined(&self.ae[e2]) {
            self.split(e2, pt);
        }

        if self.is_front(e1) == self.is_front(e2) {
            if self.is_open_end(e1) {
                self.swap_front_back_sides(self.ae[e1].outrec.unwrap());
            } else if self.is_open_end(e2) {
                self.swap_front_back_sides(self.ae[e2].outrec.unwrap());
            } else {
                self.succeeded = false;
                return None;
            }
        }

        let mut result = self.add_out_pt(e1, pt);
        if self.ae[e1].outrec == self.ae[e2].outrec {
            let outrec = self.ae[e1].outrec.unwrap();
            self.outrec_list[outrec].pts = Some(result);

            // PORT: the `if (using_polytree_)` block is left out, using_polytree_ is false.

            self.uncouple_outrec(e1);
            result = self.outrec_list[outrec].pts.unwrap();
            if let Some(owner) = self.outrec_list[outrec].owner
                && self.outrec_list[owner].front_edge.is_none()
            {
                self.outrec_list[outrec].owner = self.get_real_outrec(Some(owner));
            }
        }
        //and to preserve the winding orientation of outrec ...
        else if self.is_open(e1) {
            if self.ae[e1].wind_dx < 0 {
                self.join_outrec_paths(e1, e2);
            } else {
                self.join_outrec_paths(e2, e1);
            }
        } else if self.outrec_list[self.ae[e1].outrec.unwrap()].idx
            < self.outrec_list[self.ae[e2].outrec.unwrap()].idx
        {
            self.join_outrec_paths(e1, e2);
        } else {
            self.join_outrec_paths(e2, e1);
        }
        Some(result)
    }

    fn join_outrec_paths(&mut self, e1: usize, e2: usize) {
        //join e2 outrec path onto e1 outrec path and then delete e2 outrec path
        //pointers. (NB Only very rarely do the joining ends share the same coords.)
        // PORT: e2.outrec is read again after front_edge/back_edge are moved,
        // since that can point e2 at e1's OutRec, as it does in the C++.
        let or1 = self.ae[e1].outrec.unwrap();
        let p1_st = self.outrec_list[or1].pts.unwrap();
        let p2_st = self.outrec_list[self.ae[e2].outrec.unwrap()].pts.unwrap();
        let p1_end = self.outpts[p1_st].next;
        let p2_end = self.outpts[p2_st].next;
        if self.is_front(e1) {
            self.outpts[p2_end].prev = p1_st;
            self.outpts[p1_st].next = p2_end;
            self.outpts[p2_st].next = p1_end;
            self.outpts[p1_end].prev = p2_st;
            self.outrec_list[or1].pts = Some(p2_st);
            self.outrec_list[or1].front_edge =
                self.outrec_list[self.ae[e2].outrec.unwrap()].front_edge;
            if let Some(fe) = self.outrec_list[or1].front_edge {
                self.ae[fe].outrec = Some(or1);
            }
        } else {
            self.outpts[p1_end].prev = p2_st;
            self.outpts[p2_st].next = p1_end;
            self.outpts[p1_st].next = p2_end;
            self.outpts[p2_end].prev = p1_st;
            self.outrec_list[or1].back_edge =
                self.outrec_list[self.ae[e2].outrec.unwrap()].back_edge;
            if let Some(be) = self.outrec_list[or1].back_edge {
                self.ae[be].outrec = Some(or1);
            }
        }

        //after joining, the e2.OutRec must contains no vertices ...
        let or2 = self.ae[e2].outrec.unwrap();
        self.outrec_list[or2].front_edge = None;
        self.outrec_list[or2].back_edge = None;
        self.outrec_list[or2].pts = None;

        if self.is_open_end(e1) {
            self.outrec_list[or2].pts = self.outrec_list[or1].pts;
            self.outrec_list[or1].pts = None;
        } else {
            self.set_owner(or2, or1);
        }

        //and e1 and e2 are maxima and are about to be dropped from the Actives list.
        self.ae[e1].outrec = None;
        self.ae[e2].outrec = None;
    }

    fn new_outrec(&mut self) -> usize {
        let idx = self.outrec_list.len();
        self.outrec_list.push(OutRec {
            idx,
            owner: None,
            front_edge: None,
            back_edge: None,
            pts: None,
            is_open: false,
        });
        idx
    }

    fn add_out_pt(&mut self, e: usize, pt: Point64) -> usize {
        //Outrec.OutPts: a circular doubly-linked-list of POutPt where ...
        //op_front[.Prev]* ~~~> op_back & op_back == op_front.Next
        let outrec = self.ae[e].outrec.unwrap();
        let to_front = self.is_front(e);
        let op_front = self.outrec_list[outrec].pts.unwrap();
        let op_back = self.outpts[op_front].next;

        if to_front {
            if pt == self.outpts[op_front].pt {
                return op_front;
            }
        } else if pt == self.outpts[op_back].pt {
            return op_back;
        }

        let new_op = self.new_out_pt(pt, outrec);
        self.outpts[op_back].prev = new_op;
        self.outpts[new_op].prev = op_front;
        self.outpts[new_op].next = op_back;
        self.outpts[op_front].next = new_op;
        if to_front {
            self.outrec_list[outrec].pts = Some(new_op);
        }
        new_op
    }

    pub(crate) fn clean_collinear(&mut self, outrec: Option<usize>) {
        let outrec = match self.get_real_outrec(outrec) {
            Some(o) if !self.outrec_list[o].is_open => o,
            _ => return,
        };
        if !is_valid_closed_path(&self.outpts, self.outrec_list[outrec].pts) {
            self.dispose_out_pts(outrec);
            return;
        }

        let mut start_op = self.outrec_list[outrec].pts.unwrap();
        let mut op2 = start_op;
        loop {
            //NB if preserveCollinear == true, then only remove 180 deg. spikes
            let prev_pt = self.outpts[self.outpts[op2].prev].pt;
            let pt = self.outpts[op2].pt;
            let next_pt = self.outpts[self.outpts[op2].next].pt;
            if is_collinear(prev_pt, pt, next_pt)
                && (pt == prev_pt
                    || pt == next_pt
                    || !self.preserve_collinear
                    || dot_product(prev_pt, pt, next_pt) < 0.0)
            {
                if Some(op2) == self.outrec_list[outrec].pts {
                    self.outrec_list[outrec].pts = Some(self.outpts[op2].prev);
                }

                op2 = self.dispose_out_pt(op2);
                if !is_valid_closed_path(&self.outpts, Some(op2)) {
                    self.dispose_out_pts(outrec);
                    return;
                }
                start_op = op2;
                continue;
            }
            op2 = self.outpts[op2].next;
            if op2 == start_op {
                break;
            }
        }
        self.fix_self_intersects(outrec);
    }

    fn do_split_op(&mut self, outrec: usize, split_op: usize) {
        // splitOp.prev -> splitOp &&
        // splitOp.next -> splitOp.next.next are intersecting
        let prev_op = self.outpts[split_op].prev;
        let next_next_op = self.outpts[self.outpts[split_op].next].next;
        self.outrec_list[outrec].pts = Some(prev_op);

        let mut ip = Point64::default();
        get_segment_intersect_pt(
            self.outpts[prev_op].pt,
            self.outpts[split_op].pt,
            self.outpts[self.outpts[split_op].next].pt,
            self.outpts[next_next_op].pt,
            &mut ip,
        );

        let area1 = area(&self.outpts, self.outrec_list[outrec].pts.unwrap());
        let abs_area1 = area1.abs();
        if abs_area1 < 2.0 {
            self.dispose_out_pts(outrec);
            return;
        }

        let area2 = area_triangle(
            ip,
            self.outpts[split_op].pt,
            self.outpts[self.outpts[split_op].next].pt,
        );
        let abs_area2 = area2.abs();

        // de-link splitOp and splitOp.next from the path
        // while inserting the intersection point
        if ip == self.outpts[prev_op].pt || ip == self.outpts[next_next_op].pt {
            self.outpts[next_next_op].prev = prev_op;
            self.outpts[prev_op].next = next_next_op;
        } else {
            let new_op2 = self.new_out_pt(ip, self.outpts[prev_op].outrec);
            self.outpts[new_op2].prev = prev_op;
            self.outpts[new_op2].next = next_next_op;
            self.outpts[next_next_op].prev = new_op2;
            self.outpts[prev_op].next = new_op2;
        }

        // area1 is the path's area *before* splitting, whereas area2 is
        // the area of the triangle containing splitOp & splitOp.next.
        // So the only way for these areas to have the same sign is if
        // the split triangle is larger than the path containing prevOp or
        // if there's more than one self-intersection.
        if abs_area2 >= 1.0 && (abs_area2 > abs_area1 || (area2 > 0.0) == (area1 > 0.0)) {
            let new_or = self.new_outrec();
            self.outrec_list[new_or].owner = self.outrec_list[outrec].owner;

            let split_next = self.outpts[split_op].next;
            self.outpts[split_op].outrec = new_or;
            self.outpts[split_next].outrec = new_or;
            let new_op = self.new_out_pt(ip, new_or);
            self.outpts[new_op].prev = split_next;
            self.outpts[new_op].next = split_op;
            self.outrec_list[new_or].pts = Some(new_op);
            self.outpts[split_op].prev = new_op;
            self.outpts[split_next].next = new_op;

            // PORT: the `if (using_polytree_)` block is left out, using_polytree_ is false.
        }
        // else: delete splitOp->next; delete splitOp;
    }

    fn fix_self_intersects(&mut self, outrec: usize) {
        let mut op2 = self.outrec_list[outrec].pts.unwrap();
        loop {
            let prev = self.outpts[op2].prev;
            let next = self.outpts[op2].next;
            let next_next = self.outpts[next].next;
            // triangles can't self-intersect
            if prev == next_next {
                break;
            }
            if segments_intersect(
                self.outpts[prev].pt,
                self.outpts[op2].pt,
                self.outpts[next].pt,
                self.outpts[next_next].pt,
                false,
            ) {
                let pts = self.outrec_list[outrec].pts.unwrap();
                if op2 == pts || next == pts {
                    self.outrec_list[outrec].pts = Some(self.outpts[pts].prev);
                }
                self.do_split_op(outrec, op2);
                match self.outrec_list[outrec].pts {
                    None => break,
                    Some(pts) => op2 = pts,
                }
                continue;
            } else {
                op2 = next;
            }

            if Some(op2) == self.outrec_list[outrec].pts {
                break;
            }
        }
    }

    #[inline]
    fn start_open_path(&mut self, e: usize, pt: Point64) -> usize {
        let outrec = self.new_outrec();
        self.outrec_list[outrec].is_open = true;

        if self.ae[e].wind_dx > 0 {
            self.outrec_list[outrec].front_edge = Some(e);
            self.outrec_list[outrec].back_edge = None;
        } else {
            self.outrec_list[outrec].front_edge = None;
            self.outrec_list[outrec].back_edge = Some(e);
        }

        self.ae[e].outrec = Some(outrec);

        let op = self.new_out_pt(pt, outrec);
        self.outrec_list[outrec].pts = Some(op);
        op
    }

    #[inline]
    fn trim_horz(&mut self, horz_edge: usize, preserve_collinear: bool) {
        let mut was_trimmed = false;
        let mut pt = self.vertex_lists[self.next_vertex(horz_edge)].pt;
        while pt.y == self.ae[horz_edge].top.y {
            //always trim 180 deg. spikes (in closed paths)
            //but otherwise break if preserveCollinear = true
            let he = &self.ae[horz_edge];
            if preserve_collinear && ((pt.x < he.top.x) != (he.bot.x < he.top.x)) {
                break;
            }

            self.ae[horz_edge].vertex_top = self.next_vertex(horz_edge);
            self.ae[horz_edge].top = pt;
            was_trimmed = true;
            if self.is_maxima(horz_edge) {
                break;
            }
            pt = self.vertex_lists[self.next_vertex(horz_edge)].pt;
        }

        if was_trimmed {
            set_dx(&mut self.ae[horz_edge]); // +/-infinity
        }
    }

    #[inline]
    fn update_edge_into_ael(&mut self, e: usize) {
        self.ae[e].bot = self.ae[e].top;
        self.ae[e].vertex_top = self.next_vertex(e);
        self.ae[e].top = self.vertex_lists[self.ae[e].vertex_top].pt;
        self.ae[e].curr_x = self.ae[e].bot.x;
        set_dx(&mut self.ae[e]);

        if is_joined(&self.ae[e]) {
            let bot = self.ae[e].bot;
            self.split(e, bot);
        }

        if is_horizontal(&self.ae[e]) {
            if !self.is_open(e) {
                self.trim_horz(e, self.preserve_collinear);
            }
            return;
        }

        self.insert_scanline(self.ae[e].top.y);
        let bot = self.ae[e].bot;
        self.check_join_left(e, bot, false);
        self.check_join_right(e, bot, true); // (#500)
    }

    fn find_edge_with_matching_loc_min(&self, e: usize) -> Option<usize> {
        let mut result = self.ae[e].next_in_ael;
        while let Some(r) = result {
            if self.ae[r].local_min == self.ae[e].local_min {
                return Some(r);
            } else if !is_horizontal(&self.ae[r]) && self.ae[e].bot != self.ae[r].bot {
                result = None;
            } else {
                result = self.ae[r].next_in_ael;
            }
        }
        result = self.ae[e].prev_in_ael;
        while let Some(r) = result {
            if self.ae[r].local_min == self.ae[e].local_min {
                return Some(r);
            } else if !is_horizontal(&self.ae[r]) && self.ae[e].bot != self.ae[r].bot {
                return None;
            } else {
                result = self.ae[r].prev_in_ael;
            }
        }
        result
    }

    fn intersect_edges(&mut self, e1: usize, e2: usize, pt: Point64) {
        //MANAGE OPEN PATH INTERSECTIONS SEPARATELY ...
        if self.has_open_paths && (self.is_open(e1) || self.is_open(e2)) {
            if self.is_open(e1) && self.is_open(e2) {
                return;
            }
            let (edge_o, edge_c) = if self.is_open(e1) { (e1, e2) } else { (e2, e1) };
            if is_joined(&self.ae[edge_c]) {
                self.split(edge_c, pt); // needed for safety
            }

            if self.ae[edge_c].wind_cnt.abs() != 1 {
                return;
            }
            match self.cliptype {
                ClipType::Union => {
                    if !self.is_hot_edge(edge_c) {
                        return;
                    }
                }
                _ => {
                    if self.minima_list[self.ae[edge_c].local_min].polytype == PathType::Subject {
                        return;
                    }
                }
            }

            match self.fillrule {
                FillRule::Positive => {
                    if self.ae[edge_c].wind_cnt != 1 {
                        return;
                    }
                }
                FillRule::Negative => {
                    if self.ae[edge_c].wind_cnt != -1 {
                        return;
                    }
                }
                _ => {
                    if self.ae[edge_c].wind_cnt.abs() != 1 {
                        return;
                    }
                }
            }

            //toggle contribution ...
            if self.is_hot_edge(edge_o) {
                self.add_out_pt(edge_o, pt);
                let outrec = self.ae[edge_o].outrec.unwrap();
                if self.is_front(edge_o) {
                    self.outrec_list[outrec].front_edge = None;
                } else {
                    self.outrec_list[outrec].back_edge = None;
                }
                self.ae[edge_o].outrec = None;
            }
            //horizontal edges can pass under open paths at a LocMins
            else if pt == self.vertex_lists[self.minima_list[self.ae[edge_o].local_min].vertex].pt
                && !self.is_open_end_v(self.minima_list[self.ae[edge_o].local_min].vertex)
            {
                //find the other side of the LocMin and
                //if it's 'hot' join up with it ...
                let e3 = self.find_edge_with_matching_loc_min(edge_o);
                match e3 {
                    Some(e3) if self.is_hot_edge(e3) => {
                        let e3_outrec = self.ae[e3].outrec.unwrap();
                        self.ae[edge_o].outrec = Some(e3_outrec);
                        if self.ae[edge_o].wind_dx > 0 {
                            self.set_sides(e3_outrec, edge_o, e3);
                        } else {
                            self.set_sides(e3_outrec, e3, edge_o);
                        }
                        return;
                    }
                    _ => {
                        self.start_open_path(edge_o, pt);
                    }
                }
            } else {
                self.start_open_path(edge_o, pt);
            }

            return;
        } // end of an open path intersection

        //MANAGING CLOSED PATHS FROM HERE ON

        if is_joined(&self.ae[e1]) {
            self.split(e1, pt);
        }
        if is_joined(&self.ae[e2]) {
            self.split(e2, pt);
        }

        //UPDATE WINDING COUNTS...

        let mut old_e1_windcnt: i32;
        let old_e2_windcnt: i32;
        if self.minima_list[self.ae[e1].local_min].polytype
            == self.minima_list[self.ae[e2].local_min].polytype
        {
            if self.fillrule == FillRule::EvenOdd {
                old_e1_windcnt = self.ae[e1].wind_cnt;
                self.ae[e1].wind_cnt = self.ae[e2].wind_cnt;
                self.ae[e2].wind_cnt = old_e1_windcnt;
            } else {
                if self.ae[e1].wind_cnt + self.ae[e2].wind_dx == 0 {
                    self.ae[e1].wind_cnt = -self.ae[e1].wind_cnt;
                } else {
                    self.ae[e1].wind_cnt += self.ae[e2].wind_dx;
                }
                if self.ae[e2].wind_cnt - self.ae[e1].wind_dx == 0 {
                    self.ae[e2].wind_cnt = -self.ae[e2].wind_cnt;
                } else {
                    self.ae[e2].wind_cnt -= self.ae[e1].wind_dx;
                }
            }
        } else if self.fillrule != FillRule::EvenOdd {
            self.ae[e1].wind_cnt2 += self.ae[e2].wind_dx;
            self.ae[e2].wind_cnt2 -= self.ae[e1].wind_dx;
        } else {
            self.ae[e1].wind_cnt2 = if self.ae[e1].wind_cnt2 == 0 { 1 } else { 0 };
            self.ae[e2].wind_cnt2 = if self.ae[e2].wind_cnt2 == 0 { 1 } else { 0 };
        }

        match self.fillrule {
            FillRule::EvenOdd | FillRule::NonZero => {
                old_e1_windcnt = self.ae[e1].wind_cnt.abs();
                old_e2_windcnt = self.ae[e2].wind_cnt.abs();
            }
            _ => {
                if self.fillrule == self.fillpos {
                    old_e1_windcnt = self.ae[e1].wind_cnt;
                    old_e2_windcnt = self.ae[e2].wind_cnt;
                } else {
                    old_e1_windcnt = -self.ae[e1].wind_cnt;
                    old_e2_windcnt = -self.ae[e2].wind_cnt;
                }
            }
        }

        let e1_windcnt_in_01 = old_e1_windcnt == 0 || old_e1_windcnt == 1;
        let e2_windcnt_in_01 = old_e2_windcnt == 0 || old_e2_windcnt == 1;

        if (!self.is_hot_edge(e1) && !e1_windcnt_in_01)
            || (!self.is_hot_edge(e2) && !e2_windcnt_in_01)
        {
            return;
        }

        //NOW PROCESS THE INTERSECTION ...
        //if both edges are 'hot' ...
        if self.is_hot_edge(e1) && self.is_hot_edge(e2) {
            if (old_e1_windcnt != 0 && old_e1_windcnt != 1)
                || (old_e2_windcnt != 0 && old_e2_windcnt != 1)
                || (self.minima_list[self.ae[e1].local_min].polytype
                    != self.minima_list[self.ae[e2].local_min].polytype
                    && self.cliptype != ClipType::Xor)
            {
                self.add_local_max_poly(e1, e2, pt);
            } else if self.is_front(e1) || (self.ae[e1].outrec == self.ae[e2].outrec) {
                //this 'else if' condition isn't strictly needed but
                //it's sensible to split polygons that ony touch at
                //a common vertex (not at common edges).

                self.add_local_max_poly(e1, e2, pt);
                self.add_local_min_poly(e1, e2, pt, false);
            } else {
                self.add_out_pt(e1, pt);
                self.add_out_pt(e2, pt);
                self.swap_outrecs(e1, e2);
            }
        } else if self.is_hot_edge(e1) {
            self.add_out_pt(e1, pt);
            self.swap_outrecs(e1, e2);
        } else if self.is_hot_edge(e2) {
            self.add_out_pt(e2, pt);
            self.swap_outrecs(e1, e2);
        } else {
            let e1_wc2: i64;
            let e2_wc2: i64;
            match self.fillrule {
                FillRule::EvenOdd | FillRule::NonZero => {
                    e1_wc2 = self.ae[e1].wind_cnt2.abs() as i64;
                    e2_wc2 = self.ae[e2].wind_cnt2.abs() as i64;
                }
                _ => {
                    if self.fillrule == self.fillpos {
                        e1_wc2 = self.ae[e1].wind_cnt2 as i64;
                        e2_wc2 = self.ae[e2].wind_cnt2 as i64;
                    } else {
                        e1_wc2 = -self.ae[e1].wind_cnt2 as i64;
                        e2_wc2 = -self.ae[e2].wind_cnt2 as i64;
                    }
                }
            }

            if !self.is_same_poly_type(e1, e2) {
                self.add_local_min_poly(e1, e2, pt, false);
            } else if old_e1_windcnt == 1 && old_e2_windcnt == 1 {
                match self.cliptype {
                    ClipType::Union => {
                        if e1_wc2 <= 0 && e2_wc2 <= 0 {
                            self.add_local_min_poly(e1, e2, pt, false);
                        }
                    }
                    ClipType::Difference => {
                        if ((self.get_poly_type(e1) == PathType::Clip)
                            && (e1_wc2 > 0)
                            && (e2_wc2 > 0))
                            || ((self.get_poly_type(e1) == PathType::Subject)
                                && (e1_wc2 <= 0)
                                && (e2_wc2 <= 0))
                        {
                            self.add_local_min_poly(e1, e2, pt, false);
                        }
                    }
                    ClipType::Xor => {
                        self.add_local_min_poly(e1, e2, pt, false);
                    }
                    _ => {
                        if e1_wc2 > 0 && e2_wc2 > 0 {
                            self.add_local_min_poly(e1, e2, pt, false);
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn delete_from_ael(&mut self, e: usize) {
        let prev = self.ae[e].prev_in_ael;
        let next = self.ae[e].next_in_ael;
        if prev.is_none() && next.is_none() && (Some(e) != self.actives) {
            return; // already deleted
        }
        if let Some(p) = prev {
            self.ae[p].next_in_ael = next;
        } else {
            self.actives = next;
        }
        if let Some(n) = next {
            self.ae[n].prev_in_ael = prev;
        }
        // delete &e;
    }

    #[inline]
    fn adjust_curr_x_and_copy_to_sel(&mut self, top_y: i64) {
        let mut e = self.actives;
        self.sel = e;
        while let Some(x) = e {
            self.ae[x].prev_in_sel = self.ae[x].prev_in_ael;
            self.ae[x].next_in_sel = self.ae[x].next_in_ael;
            self.ae[x].jump = self.ae[x].next_in_sel;
            if self.ae[x].join_with == JoinWith::Left {
                self.ae[x].curr_x = self.ae[self.ae[x].prev_in_ael.unwrap()].curr_x; // also avoids complications
            } else {
                self.ae[x].curr_x = top_x(&self.ae[x], top_y);
            }
            e = self.ae[x].next_in_ael;
        }
    }

    pub(crate) fn execute_internal(&mut self, ct: ClipType, fillrule: FillRule) -> bool {
        self.cliptype = ct;
        self.fillrule = fillrule;
        self.reset();
        if ct == ClipType::NoClip {
            return true;
        }
        let Some(mut y) = self.pop_scanline() else {
            return true;
        };

        while self.succeeded {
            self.insert_local_minima_into_ael(y);
            while let Some(e) = self.pop_horz() {
                self.do_horizontal(e);
            }
            if !self.horz_seg_list.is_empty() {
                self.convert_horz_segs_to_joins();
                self.horz_seg_list.clear();
            }
            self.bot_y = y; // bot_y_ == bottom of scanbeam
            match self.pop_scanline() {
                Some(top_y) => y = top_y, // y new top of scanbeam
                None => break,
            }
            self.do_intersections(y);
            self.do_top_of_scanbeam(y);
            while let Some(e) = self.pop_horz() {
                self.do_horizontal(e);
            }
        }
        if self.succeeded {
            self.process_horz_joins();
        }
        self.succeeded
    }

    #[inline]
    fn fix_outrec_pts(&mut self, outrec: usize) {
        let start = self.outrec_list[outrec].pts.unwrap();
        let mut op = start;
        loop {
            self.outpts[op].outrec = outrec;
            op = self.outpts[op].next;
            if op == start {
                break;
            }
        }
    }

    #[inline]
    fn set_horz_seg_heading_forward(&mut self, hs: usize, op_p: usize, op_n: usize) -> bool {
        if self.outpts[op_p].pt.x == self.outpts[op_n].pt.x {
            return false;
        }
        let hs = &mut self.horz_seg_list[hs];
        if self.outpts[op_p].pt.x < self.outpts[op_n].pt.x {
            hs.left_op = op_p;
            hs.right_op = Some(op_n);
            hs.left_to_right = true;
        } else {
            hs.left_op = op_n;
            hs.right_op = Some(op_p);
            hs.left_to_right = false;
        }
        true
    }

    #[inline]
    fn update_horz_segment(&mut self, hs: usize) -> bool {
        let op = self.horz_seg_list[hs].left_op;
        let outrec = self.get_real_outrec(Some(self.outpts[op].outrec)).unwrap();
        let outrec_has_edges = self.outrec_list[outrec].front_edge.is_some();
        let curr_y = self.outpts[op].pt.y;
        let mut op_p = op;
        let mut op_n = op;
        let o = &self.outpts;
        if outrec_has_edges {
            let op_a = self.outrec_list[outrec].pts.unwrap();
            let op_z = o[op_a].next;
            while op_p != op_z && o[o[op_p].prev].pt.y == curr_y {
                op_p = o[op_p].prev;
            }
            while op_n != op_a && o[o[op_n].next].pt.y == curr_y {
                op_n = o[op_n].next;
            }
        } else {
            while o[op_p].prev != op_n && o[o[op_p].prev].pt.y == curr_y {
                op_p = o[op_p].prev;
            }
            while o[op_n].next != op_p && o[o[op_n].next].pt.y == curr_y {
                op_n = o[op_n].next;
            }
        }
        let result = self.set_horz_seg_heading_forward(hs, op_p, op_n)
            && self.outpts[self.horz_seg_list[hs].left_op].horz.is_none();

        if result {
            let left_op = self.horz_seg_list[hs].left_op;
            self.outpts[left_op].horz = Some(hs);
        } else {
            self.horz_seg_list[hs].right_op = None; // (for sorting)
        }
        result
    }

    fn convert_horz_segs_to_joins(&mut self) {
        let mut j = 0;
        for hs in 0..self.horz_seg_list.len() {
            if self.update_horz_segment(hs) {
                j += 1;
            }
        }
        if j < 2 {
            return;
        }

        let outpts = &self.outpts;
        self.horz_seg_list
            .sort_by(|hs1, hs2| HorzSegSorter::cmp(outpts, hs1, hs2));

        let hs_end = j;
        let hs_end1 = hs_end - 1;

        for hs1 in 0..hs_end1 {
            for hs2 in (hs1 + 1)..hs_end {
                let h1 = self.horz_seg_list[hs1];
                let h2 = self.horz_seg_list[hs2];
                if (self.outpts[h2.left_op].pt.x >= self.outpts[h1.right_op.unwrap()].pt.x)
                    || (h2.left_to_right == h1.left_to_right)
                    || (self.outpts[h2.right_op.unwrap()].pt.x <= self.outpts[h1.left_op].pt.x)
                {
                    continue;
                }
                let curr_y = self.outpts[h1.left_op].pt.y;
                let o = &self.outpts;
                let mut l1 = h1.left_op;
                let mut l2 = h2.left_op;
                let join;
                if h1.left_to_right {
                    while o[o[l1].next].pt.y == curr_y && o[o[l1].next].pt.x <= o[l2].pt.x {
                        l1 = o[l1].next;
                    }
                    while o[o[l2].prev].pt.y == curr_y && o[o[l2].prev].pt.x <= o[l1].pt.x {
                        l2 = o[l2].prev;
                    }
                    self.horz_seg_list[hs1].left_op = l1;
                    self.horz_seg_list[hs2].left_op = l2;
                    join = HorzJoin {
                        op1: self.duplicate_op(l1, true),
                        op2: self.duplicate_op(l2, false),
                    };
                } else {
                    while o[o[l1].prev].pt.y == curr_y && o[o[l1].prev].pt.x <= o[l2].pt.x {
                        l1 = o[l1].prev;
                    }
                    while o[o[l2].next].pt.y == curr_y && o[o[l2].next].pt.x <= o[l1].pt.x {
                        l2 = o[l2].next;
                    }
                    self.horz_seg_list[hs1].left_op = l1;
                    self.horz_seg_list[hs2].left_op = l2;
                    join = HorzJoin {
                        op1: self.duplicate_op(l2, true),
                        op2: self.duplicate_op(l1, false),
                    };
                }
                self.horz_join_list.push(join);
            }
        }
    }

    fn process_horz_joins(&mut self) {
        for i in 0..self.horz_join_list.len() {
            let j = self.horz_join_list[i];
            let or1 = self.get_real_outrec(Some(self.outpts[j.op1].outrec));
            let or2 = self.get_real_outrec(Some(self.outpts[j.op2].outrec));

            let op1b = self.outpts[j.op1].next;
            let op2b = self.outpts[j.op2].prev;
            self.outpts[j.op1].next = j.op2;
            self.outpts[j.op2].prev = j.op1;
            self.outpts[op1b].prev = op2b;
            self.outpts[op2b].next = op1b;

            if or1 == or2 {
                // 'join' is really a split
                let or2 = self.new_outrec();
                self.outrec_list[or2].pts = Some(op1b);
                self.fix_outrec_pts(or2);

                //if or1->pts has moved to or2 then update or1->pts!!
                let or1 = or1.unwrap();
                if self.outpts[self.outrec_list[or1].pts.unwrap()].outrec == or2 {
                    self.outrec_list[or1].pts = Some(j.op1);
                    self.outpts[j.op1].outrec = or1;
                }

                // PORT: the `if (using_polytree_)` branch is left out, using_polytree_ is false.
                self.outrec_list[or2].owner = Some(or1);
            } else {
                let or2 = or2.unwrap();
                self.outrec_list[or2].pts = None;
                // PORT: the `if (using_polytree_)` branch is left out, using_polytree_ is false.
                self.outrec_list[or2].owner = or1;
            }
        }
    }

    fn do_intersections(&mut self, top_y: i64) {
        if self.build_intersect_list(top_y) {
            self.process_intersect_list();
            self.intersect_nodes.clear();
        }
    }

    fn add_new_intersect_node(&mut self, e1: usize, e2: usize, top_y: i64) {
        let a1 = &self.ae[e1];
        let a2 = &self.ae[e2];
        let mut ip = Point64::default();
        if !get_segment_intersect_pt(a1.bot, a1.top, a2.bot, a2.top, &mut ip) {
            ip = Point64::new(a1.curr_x, top_y); //parallel edges
        }

        //rounding errors can occasionally place the calculated intersection
        //point either below or above the scanbeam, so check and correct ...
        if ip.y > self.bot_y || ip.y < top_y {
            let abs_dx1 = a1.dx.abs();
            let abs_dx2 = a2.dx.abs();
            if abs_dx1 > 100.0 && abs_dx2 > 100.0 {
                if abs_dx1 > abs_dx2 {
                    ip = get_closest_point_on_segment(ip, a1.bot, a1.top);
                } else {
                    ip = get_closest_point_on_segment(ip, a2.bot, a2.top);
                }
            } else if abs_dx1 > 100.0 {
                ip = get_closest_point_on_segment(ip, a1.bot, a1.top);
            } else if abs_dx2 > 100.0 {
                ip = get_closest_point_on_segment(ip, a2.bot, a2.top);
            } else {
                if ip.y < top_y {
                    ip.y = top_y;
                } else {
                    ip.y = self.bot_y;
                }
                if abs_dx1 < abs_dx2 {
                    ip.x = top_x(a1, ip.y);
                } else {
                    ip.x = top_x(a2, ip.y);
                }
            }
        }
        self.intersect_nodes.push(IntersectNode {
            pt: ip,
            edge1: e1,
            edge2: e2,
        });
    }

    fn build_intersect_list(&mut self, top_y: i64) -> bool {
        match self.actives {
            Some(a) if self.ae[a].next_in_ael.is_some() => {}
            _ => return false,
        }

        //Calculate edge positions at the top of the current scanbeam, and from this
        //we will determine the intersections required to reach these new positions.
        self.adjust_curr_x_and_copy_to_sel(top_y);
        //Find all edge intersections in the current scanbeam using a stable merge
        //sort that ensures only adjacent edges are intersecting. Intersect info is
        //stored in FIntersectList ready to be processed in ProcessIntersectList.
        //Re merge sorts see https://stackoverflow.com/a/46319131/359538

        let mut left = self.sel;

        while left.is_some_and(|l| self.ae[l].jump.is_some()) {
            let mut prev_base: Option<usize> = None;
            while left.is_some_and(|l| self.ae[l].jump.is_some()) {
                let l = left.unwrap();
                let mut curr_base = l;
                let mut right = self.ae[l].jump;
                let mut l_end = right;
                let r_end = self.ae[right.unwrap()].jump;
                self.ae[l].jump = r_end;
                while left != l_end && right != r_end {
                    let l = left.unwrap();
                    let r = right.unwrap();
                    if self.ae[r].curr_x < self.ae[l].curr_x {
                        let mut tmp = self.ae[r].prev_in_sel.unwrap();
                        loop {
                            self.add_new_intersect_node(tmp, r, top_y);
                            if tmp == l {
                                break;
                            }
                            tmp = self.ae[tmp].prev_in_sel.unwrap();
                        }

                        let tmp = r;
                        right = self.extract_from_sel(tmp);
                        l_end = right;
                        self.insert1_before2_in_sel(tmp, l);
                        if l == curr_base {
                            curr_base = tmp;
                            self.ae[curr_base].jump = r_end;
                            match prev_base {
                                None => self.sel = Some(curr_base),
                                Some(pb) => self.ae[pb].jump = Some(curr_base),
                            }
                        }
                    } else {
                        left = self.ae[l].next_in_sel;
                    }
                }
                prev_base = Some(curr_base);
                left = r_end;
            }
            left = self.sel;
        }
        !self.intersect_nodes.is_empty()
    }

    fn process_intersect_list(&mut self) {
        //We now have a list of intersections required so that edges will be
        //correctly positioned at the top of the scanbeam. However, it's important
        //that edge intersections are processed from the bottom up, but it's also
        //crucial that intersections only occur between adjacent edges.

        //First we do a quicksort so intersections proceed in a bottom up order ...
        // PORT: libc++'s std::sort, since equal points are common and their order matters.
        libcxx_sort::sort(&mut self.intersect_nodes, intersect_list_sort);
        //Now as we process these intersections, we must sometimes adjust the order
        //to ensure that intersecting edges are always adjacent ...

        for node_iter in 0..self.intersect_nodes.len() {
            if !self.edges_adjacent_in_ael(&self.intersect_nodes[node_iter]) {
                let mut node_iter2 = node_iter + 1;
                while !self.edges_adjacent_in_ael(&self.intersect_nodes[node_iter2]) {
                    node_iter2 += 1;
                }
                self.intersect_nodes.swap(node_iter, node_iter2);
            }

            let node = self.intersect_nodes[node_iter];
            self.intersect_edges(node.edge1, node.edge2, node.pt);
            self.swap_positions_in_ael(node.edge1, node.edge2);

            self.ae[node.edge1].curr_x = node.pt.x;
            self.ae[node.edge2].curr_x = node.pt.x;
            self.check_join_left(node.edge2, node.pt, true);
            self.check_join_right(node.edge1, node.pt, true);
        }
    }

    fn swap_positions_in_ael(&mut self, e1: usize, e2: usize) {
        //preconditon: e1 must be immediately to the left of e2
        let next = self.ae[e2].next_in_ael;
        if let Some(n) = next {
            self.ae[n].prev_in_ael = Some(e1);
        }
        let prev = self.ae[e1].prev_in_ael;
        if let Some(p) = prev {
            self.ae[p].next_in_ael = Some(e2);
        }
        self.ae[e2].prev_in_ael = prev;
        self.ae[e2].next_in_ael = Some(e1);
        self.ae[e1].prev_in_ael = Some(e2);
        self.ae[e1].next_in_ael = next;
        if self.ae[e2].prev_in_ael.is_none() {
            self.actives = Some(e2);
        }
    }

    #[inline]
    fn get_last_op(&self, hot_edge: usize) -> usize {
        let outrec = self.ae[hot_edge].outrec.unwrap();
        let mut result = self.outrec_list[outrec].pts.unwrap();
        if Some(hot_edge) != self.outrec_list[outrec].front_edge {
            result = self.outpts[result].next;
        }
        result
    }

    #[inline]
    fn add_trial_horz_join(&mut self, op: usize) {
        if self.outrec_list[self.outpts[op].outrec].is_open {
            return;
        }
        self.horz_seg_list.push(HorzSegment {
            left_op: op,
            right_op: None,
            left_to_right: true,
        });
    }

    fn reset_horz_direction(
        &self,
        horz: usize,
        max_vertex: Option<usize>,
        horz_left: &mut i64,
        horz_right: &mut i64,
    ) -> bool {
        let h = &self.ae[horz];
        if h.bot.x == h.top.x {
            //the horizontal edge is going nowhere ...
            *horz_left = h.curr_x;
            *horz_right = h.curr_x;
            let mut e = h.next_in_ael;
            while let Some(x) = e
                && Some(self.ae[x].vertex_top) != max_vertex
            {
                e = self.ae[x].next_in_ael;
            }
            e.is_some()
        } else if h.curr_x < h.top.x {
            *horz_left = h.curr_x;
            *horz_right = h.top.x;
            true
        } else {
            *horz_left = h.top.x;
            *horz_right = h.curr_x;
            false // right to left
        }
    }

    fn do_horizontal(&mut self, horz: usize)
    /*******************************************************************************
     * Notes: Horizontal edges (HEs) at scanline intersections (ie at the top or    *
     * bottom of a scanbeam) are processed as if layered.The order in which HEs     *
     * are processed doesn't matter. HEs intersect with the bottom vertices of      *
     * other HEs[#] and with non-horizontal edges [*]. Once these intersections     *
     * are completed, intermediate HEs are 'promoted' to the next edge in their     *
     * bounds, and they in turn may be intersected[%] by other HEs.                 *
     *                                                                              *
     * eg: 3 horizontals at a scanline:    /   |                     /           /  *
     *              |                     /    |     (HE3)o ========%========== o   *
     *              o ======= o(HE2)     /     |         /         /                *
     *          o ============#=========*======*========#=========o (HE1)           *
     *         /              |        /       |       /                            *
     *******************************************************************************/
    {
        let mut pt: Point64;
        let horz_is_open = self.is_open(horz);
        let y = self.ae[horz].bot.y;
        let vertex_max = if horz_is_open {
            self.get_curr_y_maxima_vertex_open(horz)
        } else {
            self.get_curr_y_maxima_vertex(horz)
        };

        //// remove 180 deg.spikes and also simplify
        //// consecutive horizontals when PreserveCollinear = true
        //if (!horzIsOpen && vertex_max != horz.vertex_top)
        //  TrimHorz(horz, PreserveCollinear);

        let mut horz_left = 0;
        let mut horz_right = 0;
        let mut is_left_to_right =
            self.reset_horz_direction(horz, vertex_max, &mut horz_left, &mut horz_right);

        if self.is_hot_edge(horz) {
            let op = self.add_out_pt(horz, Point64::new(self.ae[horz].curr_x, y));
            self.add_trial_horz_join(op);
        }

        loop {
            // loop through consec. horizontal edges
            let mut e = if is_left_to_right {
                self.ae[horz].next_in_ael
            } else {
                self.ae[horz].prev_in_ael
            };

            while let Some(ex) = e {
                if Some(self.ae[ex].vertex_top) == vertex_max {
                    if self.is_hot_edge(horz) && is_joined(&self.ae[ex]) {
                        let top = self.ae[ex].top;
                        self.split(ex, top);
                    }

                    //if (IsHotEdge(horz) != IsHotEdge(*e))
                    //    DoError(undefined_error_i);

                    if self.is_hot_edge(horz) {
                        while Some(self.ae[horz].vertex_top) != vertex_max {
                            let top = self.ae[horz].top;
                            self.add_out_pt(horz, top);
                            self.update_edge_into_ael(horz);
                        }
                        let top = self.ae[horz].top;
                        if is_left_to_right {
                            self.add_local_max_poly(horz, ex, top);
                        } else {
                            self.add_local_max_poly(ex, horz, top);
                        }
                    }
                    self.delete_from_ael(ex);
                    self.delete_from_ael(horz);
                    return;
                }

                //if horzEdge is a maxima, keep going until we reach
                //its maxima pair, otherwise check for break conditions
                if vertex_max != Some(self.ae[horz].vertex_top) || self.is_open_end(horz) {
                    //otherwise stop when 'ae' is beyond the end of the horizontal line
                    if (is_left_to_right && self.ae[ex].curr_x > horz_right)
                        || (!is_left_to_right && self.ae[ex].curr_x < horz_left)
                    {
                        break;
                    }

                    if self.ae[ex].curr_x == self.ae[horz].top.x && !is_horizontal(&self.ae[ex]) {
                        pt = self.vertex_lists[self.next_vertex(horz)].pt;
                        if is_left_to_right {
                            //with open paths we'll only break once past horz's end
                            if self.is_open(ex)
                                && !self.is_same_poly_type(ex, horz)
                                && !self.is_hot_edge(ex)
                            {
                                if top_x(&self.ae[ex], pt.y) > pt.x {
                                    break;
                                }
                            }
                            //otherwise we'll only break when horz's outslope is greater than e's
                            else if top_x(&self.ae[ex], pt.y) >= pt.x {
                                break;
                            }
                        } else if self.is_open(ex)
                            && !self.is_same_poly_type(ex, horz)
                            && !self.is_hot_edge(ex)
                        {
                            if top_x(&self.ae[ex], pt.y) < pt.x {
                                break;
                            }
                        } else if top_x(&self.ae[ex], pt.y) <= pt.x {
                            break;
                        }
                    }
                }

                pt = Point64::new(self.ae[ex].curr_x, self.ae[horz].bot.y);
                if is_left_to_right {
                    self.intersect_edges(horz, ex, pt);
                    self.swap_positions_in_ael(horz, ex);
                    self.check_join_left(ex, pt, false);
                    self.ae[horz].curr_x = self.ae[ex].curr_x;
                    e = self.ae[horz].next_in_ael;
                } else {
                    self.intersect_edges(ex, horz, pt);
                    self.swap_positions_in_ael(ex, horz);
                    self.check_join_right(ex, pt, false);
                    self.ae[horz].curr_x = self.ae[ex].curr_x;
                    e = self.ae[horz].prev_in_ael;
                }

                if self.ae[horz].outrec.is_some() {
                    //nb: The outrec containining the op returned by IntersectEdges
                    //above may no longer be associated with horzEdge.
                    let op = self.get_last_op(horz);
                    self.add_trial_horz_join(op);
                }
            }

            //check if we've finished with (consecutive) horizontals ...
            if horz_is_open && self.is_open_end(horz) {
                // ie open at top
                if self.is_hot_edge(horz) {
                    let top = self.ae[horz].top;
                    self.add_out_pt(horz, top);
                    let outrec = self.ae[horz].outrec.unwrap();
                    if self.is_front(horz) {
                        self.outrec_list[outrec].front_edge = None;
                    } else {
                        self.outrec_list[outrec].back_edge = None;
                    }
                    self.ae[horz].outrec = None;
                }
                self.delete_from_ael(horz);
                return;
            } else if self.vertex_lists[self.next_vertex(horz)].pt.y != self.ae[horz].top.y {
                break;
            }

            //still more horizontals in bound to process ...
            if self.is_hot_edge(horz) {
                let top = self.ae[horz].top;
                self.add_out_pt(horz, top);
            }
            self.update_edge_into_ael(horz);

            is_left_to_right =
                self.reset_horz_direction(horz, vertex_max, &mut horz_left, &mut horz_right);
        }

        if self.is_hot_edge(horz) {
            let top = self.ae[horz].top;
            let op = self.add_out_pt(horz, top);
            self.add_trial_horz_join(op);
        }

        self.update_edge_into_ael(horz); // end of an intermediate horiz.
    }

    fn do_top_of_scanbeam(&mut self, y: i64) {
        self.sel = None; // sel_ is reused to flag horizontals (see PushHorz below)
        let mut e = self.actives;
        while let Some(x) = e {
            //nb: 'e' will never be horizontal here
            if self.ae[x].top.y == y {
                self.ae[x].curr_x = self.ae[x].top.x;
                if self.is_maxima(x) {
                    e = self.do_maxima(x); // TOP OF BOUND (MAXIMA)
                    continue;
                } else {
                    //INTERMEDIATE VERTEX ...
                    if self.is_hot_edge(x) {
                        let top = self.ae[x].top;
                        self.add_out_pt(x, top);
                    }
                    self.update_edge_into_ael(x);
                    if is_horizontal(&self.ae[x]) {
                        self.push_horz(x); // horizontals are processed later
                    }
                }
            } else {
                // i.e. not the top of the edge
                self.ae[x].curr_x = top_x(&self.ae[x], y);
            }

            e = self.ae[x].next_in_ael;
        }
    }

    fn do_maxima(&mut self, e: usize) -> Option<usize> {
        let prev_e = self.ae[e].prev_in_ael;
        let mut next_e = self.ae[e].next_in_ael;
        if self.is_open_end(e) {
            if self.is_hot_edge(e) {
                let top = self.ae[e].top;
                self.add_out_pt(e, top);
            }
            if !is_horizontal(&self.ae[e]) {
                if self.is_hot_edge(e) {
                    let outrec = self.ae[e].outrec.unwrap();
                    if self.is_front(e) {
                        self.outrec_list[outrec].front_edge = None;
                    } else {
                        self.outrec_list[outrec].back_edge = None;
                    }
                    self.ae[e].outrec = None;
                }
                self.delete_from_ael(e);
            }
            return next_e;
        }

        let Some(max_pair) = self.get_maxima_pair(e) else {
            return next_e; // eMaxPair is horizontal
        };

        if is_joined(&self.ae[e]) {
            let top = self.ae[e].top;
            self.split(e, top);
        }
        if is_joined(&self.ae[max_pair]) {
            let top = self.ae[max_pair].top;
            self.split(max_pair, top);
        }

        //only non-horizontal maxima here.
        //process any edges between maxima pair ...
        while next_e != Some(max_pair) {
            let n = next_e.unwrap();
            let top = self.ae[e].top;
            self.intersect_edges(e, n, top);
            self.swap_positions_in_ael(e, n);
            next_e = self.ae[e].next_in_ael;
        }

        if self.is_open(e) {
            if self.is_hot_edge(e) {
                let top = self.ae[e].top;
                self.add_local_max_poly(e, max_pair, top);
            }
            self.delete_from_ael(max_pair);
            self.delete_from_ael(e);
            return match prev_e {
                Some(p) => self.ae[p].next_in_ael,
                None => self.actives,
            };
        }

        // e.next_in_ael== max_pair ...
        if self.is_hot_edge(e) {
            let top = self.ae[e].top;
            self.add_local_max_poly(e, max_pair, top);
        }

        self.delete_from_ael(e);
        self.delete_from_ael(max_pair);
        match prev_e {
            Some(p) => self.ae[p].next_in_ael,
            None => self.actives,
        }
    }

    fn split(&mut self, e: usize, pt: Point64) {
        if self.ae[e].join_with == JoinWith::Right {
            self.ae[e].join_with = JoinWith::NoJoin;
            let next = self.ae[e].next_in_ael.unwrap();
            self.ae[next].join_with = JoinWith::NoJoin;
            self.add_local_min_poly(e, next, pt, true);
        } else {
            self.ae[e].join_with = JoinWith::NoJoin;
            let prev = self.ae[e].prev_in_ael.unwrap();
            self.ae[prev].join_with = JoinWith::NoJoin;
            self.add_local_min_poly(prev, e, pt, true);
        }
    }

    #[inline]
    fn check_join_left(&mut self, e: usize, pt: Point64, check_curr_x: bool) {
        let Some(prev) = self.ae[e].prev_in_ael else {
            return;
        };
        if !self.is_hot_edge(e)
            || !self.is_hot_edge(prev)
            || is_horizontal(&self.ae[e])
            || is_horizontal(&self.ae[prev])
            || self.is_open(e)
            || self.is_open(prev)
        {
            return;
        }
        let ae = &self.ae[e];
        let ap = &self.ae[prev];
        if (pt.y < ae.top.y + 2 || pt.y < ap.top.y + 2) && ((ae.bot.y > pt.y) || (ap.bot.y > pt.y))
        {
            return; // avoid trivial joins
        }

        if check_curr_x {
            if perpendic_dist_from_line_sqrd(pt, ap.bot, ap.top) > 0.25 {
                return;
            }
        } else if ae.curr_x != ap.curr_x {
            return;
        }
        if !is_collinear(ae.top, pt, ap.top) {
            return;
        }

        let e_idx = self.outrec_list[ae.outrec.unwrap()].idx;
        let prev_idx = self.outrec_list[ap.outrec.unwrap()].idx;
        if e_idx == prev_idx {
            self.add_local_max_poly(prev, e, pt);
        } else if e_idx < prev_idx {
            self.join_outrec_paths(e, prev);
        } else {
            self.join_outrec_paths(prev, e);
        }
        self.ae[prev].join_with = JoinWith::Right;
        self.ae[e].join_with = JoinWith::Left;
    }

    #[inline]
    fn check_join_right(&mut self, e: usize, pt: Point64, check_curr_x: bool) {
        let Some(next) = self.ae[e].next_in_ael else {
            return;
        };
        if !self.is_hot_edge(e)
            || !self.is_hot_edge(next)
            || is_horizontal(&self.ae[e])
            || is_horizontal(&self.ae[next])
            || self.is_open(e)
            || self.is_open(next)
        {
            return;
        }
        let ae = &self.ae[e];
        let an = &self.ae[next];
        if (pt.y < ae.top.y + 2 || pt.y < an.top.y + 2) && ((ae.bot.y > pt.y) || (an.bot.y > pt.y))
        {
            return; // avoid trivial joins
        }

        if check_curr_x {
            if perpendic_dist_from_line_sqrd(pt, an.bot, an.top) > 0.35 {
                return;
            }
        } else if ae.curr_x != an.curr_x {
            return;
        }
        if !is_collinear(ae.top, pt, an.top) {
            return;
        }

        let e_idx = self.outrec_list[ae.outrec.unwrap()].idx;
        let next_idx = self.outrec_list[an.outrec.unwrap()].idx;
        if e_idx == next_idx {
            self.add_local_max_poly(e, next, pt);
        } else if e_idx < next_idx {
            self.join_outrec_paths(e, next);
        } else {
            self.join_outrec_paths(next, e);
        }

        self.ae[e].join_with = JoinWith::Right;
        self.ae[next].join_with = JoinWith::Left;
    }
}

fn build_path64(
    outpts: &[OutPt],
    op: Option<usize>,
    reverse: bool,
    is_open: bool,
    path: &mut Path64,
) -> bool {
    let Some(mut op) = op else { return false };
    if outpts[op].next == op || (!is_open && outpts[op].next == outpts[op].prev) {
        return false;
    }

    path.clear();
    let mut last_pt;
    let mut op2;
    if reverse {
        last_pt = outpts[op].pt;
        op2 = outpts[op].prev;
    } else {
        op = outpts[op].next;
        last_pt = outpts[op].pt;
        op2 = outpts[op].next;
    }
    path.push(last_pt);

    while op2 != op {
        if outpts[op2].pt != last_pt {
            last_pt = outpts[op2].pt;
            path.push(last_pt);
        }
        if reverse {
            op2 = outpts[op2].prev;
        } else {
            op2 = outpts[op2].next;
        }
    }

    !(!is_open && path.len() == 3 && is_very_small_triangle(outpts, op2))
}

// PORT: CheckBounds, CheckSplitOwner and RecursiveCheckOwners are only used for PolyTree output.

// Clipper64 -------------------------------------------------------------------

/// The 64-bit integer clipper, returning its solution as open and closed paths.
pub struct Clipper64 {
    base: ClipperBase,
}

impl Default for Clipper64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipper64 {
    pub fn new() -> Self {
        Clipper64 {
            base: ClipperBase::new(),
        }
    }

    pub fn preserve_collinear(&self) -> bool {
        self.base.preserve_collinear()
    }

    pub fn set_preserve_collinear(&mut self, val: bool) {
        self.base.set_preserve_collinear(val);
    }

    pub fn reverse_solution(&self) -> bool {
        self.base.reverse_solution()
    }

    pub fn set_reverse_solution(&mut self, val: bool) {
        self.base.set_reverse_solution(val);
    }

    pub fn clear(&mut self) {
        self.base.clear();
    }

    pub fn add_subject(&mut self, subjects: &Paths64) {
        self.base.add_paths(subjects, PathType::Subject, false);
    }

    pub fn add_open_subject(&mut self, open_subjects: &Paths64) {
        self.base.add_paths(open_subjects, PathType::Subject, true);
    }

    pub fn add_clip(&mut self, clips: &Paths64) {
        self.base.add_paths(clips, PathType::Clip, false);
    }

    pub fn execute(
        &mut self,
        clip_type: ClipType,
        fill_rule: FillRule,
        closed_paths: &mut Paths64,
        open_paths: &mut Paths64,
    ) -> bool {
        closed_paths.clear();
        open_paths.clear();
        if self.base.execute_internal(clip_type, fill_rule) {
            self.build_paths64(closed_paths, open_paths);
        }
        self.base.clean_up();
        self.base.succeeded
    }

    fn build_paths64(&mut self, solution_closed: &mut Paths64, solution_open: &mut Paths64) {
        let base = &mut self.base;
        solution_closed.clear();
        solution_closed.reserve(base.outrec_list.len());
        solution_open.clear();
        solution_open.reserve(base.outrec_list.len());

        // nb: outrec_list_.size() may change in the following
        // while loop because polygons may be split during
        // calls to CleanCollinear which calls FixSelfIntersects
        let mut i = 0;
        while i < base.outrec_list.len() {
            let outrec = i;
            i += 1;
            if base.outrec_list[outrec].pts.is_none() {
                continue;
            }

            let mut path = Path64::new();
            if base.outrec_list[outrec].is_open {
                if build_path64(
                    &base.outpts,
                    base.outrec_list[outrec].pts,
                    base.reverse_solution,
                    true,
                    &mut path,
                ) {
                    solution_open.push(path);
                }
            } else {
                // nb: CleanCollinear can add to outrec_list_
                base.clean_collinear(Some(outrec));
                //closed paths should always return a Positive orientation
                if build_path64(
                    &base.outpts,
                    base.outrec_list[outrec].pts,
                    base.reverse_solution,
                    false,
                    &mut path,
                ) {
                    solution_closed.push(path);
                }
            }
        }
    }
}
