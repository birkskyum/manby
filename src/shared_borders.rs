//! Port of shared_borders.cpp.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::process::exit;

use crate::errors::EXIT_IMPOSSIBLE;
use crate::geometry::{Draw, DrawVec, VT_LINETO, VT_MOVETO, VT_POLYGON, simplify_lines};
use crate::globals::{additional, prevent};
use crate::options::{A_GRID_LOW_ZOOMS, P_CLIPPING, P_DUPLICATION, P_SIMPLIFY, P_SIMPLIFY_LOW};
use crate::serial::SerialFeaturePtr;

// This should all be removed and replaced with --no-simplification-of-shared-nodes

// Does not fix up moveto/lineto
fn reverse_subring(dv: &DrawVec) -> DrawVec {
    let mut out = DrawVec::new();

    for i in (1..=dv.len()).rev() {
        out.push(dv[i - 1]);
    }

    out
}

#[derive(Clone, Copy, Debug, Default)]
struct Edge {
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    ring: u32,
}

impl Edge {
    fn new(x1: u32, y1: u32, x2: u32, y2: u32, ring: u32) -> Edge {
        Edge {
            x1,
            y1,
            x2,
            y2,
            ring,
        }
    }

    /// `edge::operator<`
    fn lt(&self, s: &Edge) -> bool {
        let mut cmp = self.y1 as i64 - s.y1 as i64;
        if cmp == 0 {
            cmp = self.x1 as i64 - s.x1 as i64;
        }
        if cmp == 0 {
            cmp = self.y2 as i64 - s.y2 as i64;
        }
        if cmp == 0 {
            cmp = self.x2 as i64 - s.x2 as i64;
        }
        cmp < 0
    }
}

/// `edgecmp_ring`, as an `Ordering` for `sort_by`.
fn edgecmp_ring(a: &Edge, b: &Edge) -> Ordering {
    let mut cmp = a.y1 as i64 - b.y1 as i64;
    if cmp == 0 {
        cmp = a.x1 as i64 - b.x1 as i64;
    }
    if cmp == 0 {
        cmp = a.y2 as i64 - b.y2 as i64;
    }
    if cmp == 0 {
        cmp = a.x2 as i64 - b.x2 as i64;
    }
    if cmp == 0 {
        cmp = a.ring as i64 - b.ring as i64;
    }
    cmp.cmp(&0)
}

/// `std::equal_range(edges.begin(), edges.end(), key)` with `edge::operator<`,
/// as a pair of indexes.
fn equal_range(edges: &[Edge], key: &Edge) -> (usize, usize) {
    let first = edges.partition_point(|e| e.lt(key));
    let second = edges.partition_point(|e| !key.lt(e));
    (first, second)
}

fn edges_same(edges: &[Edge], e1: (usize, usize), e2: (usize, usize)) -> bool {
    if (e2.1 - e2.0) != (e1.1 - e1.0) {
        return false;
    }

    let (mut a, mut b) = (e1.0, e2.0);
    while a != e1.1 {
        if edges[a].ring != edges[b].ring {
            return false;
        }

        a += 1;
        b += 1;
    }

    true
}

#[derive(Clone, Copy, Debug, Default)]
struct MergeOrder {
    edge: isize,
    gap: u64,
    p1: usize,
    p2: usize,
}

#[allow(clippy::ptr_arg, clippy::manual_abs_diff)]
pub fn find_common_edges(
    features: &mut Vec<SerialFeaturePtr>,
    z: i32,
    line_detail: i32,
    simplification: f64,
    maxzoom: i32,
    merge_fraction: f64,
) -> bool {
    let merge_count = ((1.0 - merge_fraction) * features.len() as f64).ceil() as usize;

    for feature in features.iter_mut() {
        if feature.t as i32 == VT_POLYGON {
            let g = &feature.geometry;
            let mut out = DrawVec::new();

            for k in 0..g.len() {
                if g[k].op == VT_LINETO && k > 0 && g[k - 1] == g[k] {
                } else {
                    out.push(g[k]);
                }
            }

            feature.geometry = out;
        }
    }

    // Construct a mapping from all polygon edges to the set of rings
    // that each edge appears in. (The ring number is across all polygons;
    // we don't need to look it back up, just to tell where it changes.)

    let mut edges: Vec<Edge> = Vec::new();
    let mut ring: usize = 0;
    for feature in features.iter() {
        if feature.t as i32 == VT_POLYGON {
            let geometry = &feature.geometry;
            let mut k = 0;
            while k + 1 < geometry.len() {
                if geometry[k].op == VT_MOVETO {
                    ring += 1;
                }

                if geometry[k + 1].op == VT_LINETO {
                    let dv: [Draw; 2] = if geometry[k] < geometry[k + 1] {
                        [geometry[k], geometry[k + 1]]
                    } else {
                        [geometry[k + 1], geometry[k]]
                    };

                    edges.push(Edge::new(
                        dv[0].x as u32,
                        dv[0].y as u32,
                        dv[1].x as u32,
                        dv[1].y as u32,
                        ring as u32,
                    ));
                }
                k += 1;
            }
        }
    }

    edges.sort_by(edgecmp_ring);
    let mut necessaries: BTreeSet<Draw> = BTreeSet::new();

    // Now mark all the points where the set of rings using the edge on one side
    // is not the same as the set of rings using the edge on the other side.

    for feature in features.iter_mut() {
        if feature.t as i32 == VT_POLYGON {
            let g = &mut feature.geometry;

            for d in g.iter_mut() {
                d.necessary = 0;
            }

            let mut a = 0;
            while a < g.len() {
                if g[a].op == VT_MOVETO {
                    let mut b = a + 1;
                    while b < g.len() {
                        if g[b].op != VT_LINETO {
                            break;
                        }
                        b += 1;
                    }

                    // -1 because of duplication at the end
                    let s = b - a - 1;

                    if s > 0 {
                        let left: [Draw; 2] = if g[a + (s - 1) % s] < g[a] {
                            [g[a + (s - 1) % s], g[a]]
                        } else {
                            [g[a], g[a + (s - 1) % s]]
                        };
                        if left[1] < left[0] {
                            eprintln!("left misordered");
                        }
                        let mut e1 = equal_range(
                            &edges,
                            &Edge::new(
                                left[0].x as u32,
                                left[0].y as u32,
                                left[1].x as u32,
                                left[1].y as u32,
                                0,
                            ),
                        );

                        for k in 0..s {
                            let right: [Draw; 2] = if g[a + k] < g[a + k + 1] {
                                [g[a + k], g[a + k + 1]]
                            } else {
                                [g[a + k + 1], g[a + k]]
                            };

                            let e2 = equal_range(
                                &edges,
                                &Edge::new(
                                    right[0].x as u32,
                                    right[0].y as u32,
                                    right[1].x as u32,
                                    right[1].y as u32,
                                    0,
                                ),
                            );

                            if right[1] < right[0] {
                                eprintln!("left misordered");
                            }

                            if e1.0 == e1.1 || e2.0 == e2.1 {
                                eprintln!(
                                    "Internal error: polygon edge lookup failed for {},{} to {},{} or {},{} to {},{}",
                                    left[0].x,
                                    left[0].y,
                                    left[1].x,
                                    left[1].y,
                                    right[0].x,
                                    right[0].y,
                                    right[1].x,
                                    right[1].y
                                );
                                exit(EXIT_IMPOSSIBLE);
                            }

                            if !edges_same(&edges, e1, e2) {
                                g[a + k].necessary = 1;
                                necessaries.insert(g[a + k]);
                            }

                            e1 = e2;
                        }
                    }

                    a = b - 1;
                }
                a += 1;
            }
        }
    }

    edges.clear();
    let mut arcs: BTreeMap<DrawVec, usize> = BTreeMap::new();
    let mut merge_candidates: BTreeMap<isize, Vec<usize>> = BTreeMap::new(); // from arc to serial_feature

    // Roll rings that include a necessary point around so they start at one

    for (i, feature) in features.iter_mut().enumerate() {
        if feature.t as i32 == VT_POLYGON {
            let g = &mut feature.geometry;

            for d in g.iter_mut() {
                if necessaries.contains(d) {
                    d.necessary = 1;
                }
            }

            let mut k = 0;
            while k < g.len() {
                if g[k].op == VT_MOVETO {
                    let mut necessary: isize = -1;
                    let mut lowest: isize = k as isize;
                    let mut l = k + 1;
                    while l < g.len() {
                        if g[l].op != VT_LINETO {
                            break;
                        }

                        if g[l].necessary != 0 {
                            necessary = l as isize;
                        }
                        if g[l] < g[lowest as usize] {
                            lowest = l as isize;
                        }
                        l += 1;
                    }

                    if necessary < 0 {
                        necessary = lowest;
                        // Add a necessary marker if there was none in the ring,
                        // so the arc code below can find it.
                        g[lowest as usize].necessary = 1;
                    }

                    {
                        let mut tmp = DrawVec::new();

                        // l - 1 because the endpoint is duplicated
                        for d in &g[necessary as usize..l - 1] {
                            tmp.push(*d);
                        }
                        for m in k as isize..necessary {
                            tmp.push(g[m as usize]);
                        }

                        // replace the endpoint
                        tmp.push(g[necessary as usize]);

                        if tmp.len() != l - k {
                            eprintln!("internal error shifting ring");
                            exit(EXIT_IMPOSSIBLE);
                        }

                        for (m, t) in tmp.iter_mut().enumerate() {
                            if m == 0 {
                                t.op = VT_MOVETO;
                            } else {
                                t.op = VT_LINETO;
                            }

                            g[k + m] = *t;
                        }
                    }

                    // Now peel off each set of segments from one necessary point to the next
                    // into an "arc" as in TopoJSON

                    let mut m = k;
                    while m < l {
                        if g[m].necessary == 0 {
                            eprintln!("internal error in arc building");
                            exit(EXIT_IMPOSSIBLE);
                        }

                        let mut arc = DrawVec::new();
                        let mut n = m;
                        while n < l {
                            arc.push(g[n]);
                            if n > m && g[n].necessary != 0 {
                                break;
                            }
                            n += 1;
                        }

                        match arcs.get(&arc) {
                            None => {
                                let arc2 = reverse_subring(&arc);

                                match arcs.get(&arc2) {
                                    None => {
                                        // Add new arc
                                        let added = arcs.len() + 1;
                                        arcs.insert(arc, added);
                                        feature.arc_polygon.push(added as isize);
                                        merge_candidates.entry(added as isize).or_default().push(i);
                                    }
                                    Some(&f2) => {
                                        feature.arc_polygon.push(-(f2 as isize));
                                        merge_candidates.entry(-(f2 as isize)).or_default().push(i);
                                    }
                                }
                            }
                            Some(&f) => {
                                feature.arc_polygon.push(f as isize);
                                merge_candidates.entry(f as isize).or_default().push(i);
                            }
                        }

                        m = n - 1;
                        m += 1;
                    }

                    feature.arc_polygon.push(0);

                    k = l - 1;
                }
                k += 1;
            }
        }
    }

    // Simplify each arc

    let mut simplified_arcs: Vec<DrawVec> = Vec::new();

    for (key, &value) in &arcs {
        if simplified_arcs.len() < value + 1 {
            simplified_arcs.resize(value + 1, DrawVec::new());
        }

        let mut dv = key.clone();
        for (i, d) in dv.iter_mut().enumerate() {
            if i == 0 {
                d.op = VT_MOVETO;
            } else {
                d.op = VT_LINETO;
            }
        }
        if !(prevent(P_SIMPLIFY) != 0
            || (z == maxzoom && prevent(P_SIMPLIFY_LOW) != 0)
            || (z < maxzoom && additional(A_GRID_LOW_ZOOMS) != 0))
        {
            // tx and ty are 0 here because we aren't trying to do anything with the shared_nodes_map
            simplified_arcs[value] = simplify_lines(
                &mut dv,
                z,
                0,
                0,
                line_detail,
                !(prevent(P_CLIPPING) != 0 || prevent(P_DUPLICATION) != 0),
                simplification,
                4,
                &DrawVec::new(),
                &[],
                0,
                &[],
            );
        } else {
            simplified_arcs[value] = dv;
        }
    }

    // If necessary, merge some adjacent polygons into some other polygons

    let mut order: Vec<MergeOrder> = Vec::new();

    let empty: Vec<usize> = Vec::new();
    for i in 0..simplified_arcs.len() as isize {
        let r1 = merge_candidates.get(&i).unwrap_or(&empty);
        for &r1i in r1 {
            let r2 = merge_candidates.get(&-i).unwrap_or(&empty);
            for &r2i in r2 {
                if r1i != r2i {
                    let gap = if features[r1i].index > features[r2i].index {
                        features[r1i].index - features[r2i].index
                    } else {
                        features[r2i].index - features[r1i].index
                    };
                    order.push(MergeOrder {
                        edge: i,
                        gap,
                        p1: r1i,
                        p2: r2i,
                    });
                }
            }
        }
    }
    order.sort_by(|a, b| a.gap.cmp(&b.gap));

    // The C++ holds `here` and `other` as references to the arc_polygon vectors of
    // features i and i2, which can be the same feature. Every access below goes
    // through the features vector by index so that case behaves the same.
    let mut merged: usize = 0;
    for o in &order {
        if merged >= merge_count {
            break;
        }

        let mut i = o.p1;
        while features[i].renamed >= 0 {
            i = features[i].renamed as usize;
        }
        let mut i2 = o.p2;
        while features[i2].renamed >= 0 {
            i2 = features[i2].renamed as usize;
        }

        let mut j = 0;
        while j < features[i].arc_polygon.len() && merged < merge_count {
            if features[i].arc_polygon[j] == o.edge {
                // XXX snap links
                if !features[o.p2].arc_polygon.is_empty() {
                    // This has to merge the ring that contains the anti-arc to this arc
                    // into the current ring, and then add whatever other rings were in
                    // that feature on to the end.
                    //
                    // This can't be good for keeping parent-child relationships among
                    // the rings in order, but Wagyu should sort that out later

                    let mut additions: Vec<isize> = Vec::new();

                    let mut k = 0;
                    while k < features[i2].arc_polygon.len() {
                        let mut l = k;
                        while l < features[i2].arc_polygon.len() {
                            if features[i2].arc_polygon[l] == 0 {
                                break;
                            }
                            l += 1;
                        }
                        if l >= features[i2].arc_polygon.len() {
                            l -= 1;
                        }

                        let mut m = k;
                        while m <= l {
                            if features[i2].arc_polygon[m] == -features[i].arc_polygon[j] {
                                break;
                            }
                            m += 1;
                        }

                        if m <= l {
                            // Found the shared arc

                            features[i].arc_polygon.remove(j);

                            let mut off = 0;
                            for n in m + 1..l {
                                let v = features[i2].arc_polygon[n];
                                features[i].arc_polygon.insert(j + off, v);
                                off += 1;
                            }
                            for n in k..m {
                                let v = features[i2].arc_polygon[n];
                                features[i].arc_polygon.insert(j + off, v);
                                off += 1;
                            }
                        } else {
                            // Looking at some other ring

                            for n in k..=l {
                                additions.push(features[i2].arc_polygon[n]);
                            }
                        }

                        k = l;
                        k += 1;
                    }

                    features[i2].arc_polygon.clear();
                    features[i2].renamed = i as isize;
                    merged += 1;

                    for a in additions {
                        features[i].arc_polygon.push(a);
                    }
                }
            }
            j += 1;
        }
    }

    // Turn the arc representations of the polygons back into standard polygon geometries

    for feature in features.iter_mut() {
        if feature.t as i32 == VT_POLYGON {
            feature.geometry.clear();
            let mut at_start = true;
            let mut first = Draw::new(-1, 0, 0);

            for j in 0..feature.arc_polygon.len() {
                let p = feature.arc_polygon[j];

                if p == 0 {
                    if first.op >= 0 {
                        feature.geometry.push(first);
                        first = Draw::new(-1, 0, 0);
                    }
                    at_start = true;
                } else if p > 0 {
                    let arc = &simplified_arcs[p as usize];
                    let mut k = 0;
                    while k + 1 < arc.len() {
                        if at_start {
                            feature
                                .geometry
                                .push(Draw::new(VT_MOVETO, arc[k].x, arc[k].y));
                            first = Draw::new(VT_LINETO, arc[k].x, arc[k].y);
                        } else {
                            feature
                                .geometry
                                .push(Draw::new(VT_LINETO, arc[k].x, arc[k].y));
                        }
                        at_start = false;
                        k += 1;
                    }
                } else {
                    /* p < 0 */
                    let arc = &simplified_arcs[(-p) as usize];
                    let mut k = arc.len() as isize - 1;
                    while k > 0 {
                        let ku = k as usize;
                        if at_start {
                            feature
                                .geometry
                                .push(Draw::new(VT_MOVETO, arc[ku].x, arc[ku].y));
                            first = Draw::new(VT_LINETO, arc[ku].x, arc[ku].y);
                        } else {
                            feature
                                .geometry
                                .push(Draw::new(VT_LINETO, arc[ku].x, arc[ku].y));
                        }
                        at_start = false;
                        k -= 1;
                    }
                }
            }
        }
    }

    merged >= merge_count
}
