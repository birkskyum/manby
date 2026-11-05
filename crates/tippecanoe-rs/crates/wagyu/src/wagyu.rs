//! Port of `wagyu.hpp`.

use crate::bound::Bound;
use crate::build_local_minima_list::add_linear_ring;
use crate::build_result::build_result;
use crate::config::{ClipType, Error, FillType, PolygonType};
use crate::geometry::{BoundingBox, MultiPolygon, Point, Polygon};
use crate::local_minimum::LocalMinimum;
use crate::ring::RingManager;
use crate::snap_rounding::build_hot_pixels;
use crate::topology_correction::correct_topology;
use crate::vatti::execute_vatti;

/// `mapbox::geometry::wagyu::wagyu<long long>`
pub struct Wagyu {
    minima_list: Vec<LocalMinimum>,
    /// The bounds that the C++ stores inside each local minimum.
    bounds: Vec<Bound>,
    reverse_output: bool,
}

impl Default for Wagyu {
    fn default() -> Self {
        Self::new()
    }
}

impl Wagyu {
    pub fn new() -> Self {
        Wagyu {
            minima_list: Vec::new(),
            bounds: Vec::new(),
            reverse_output: false,
        }
    }

    /// Adds a ring. Returns false if the ring has too few distinct, non-collinear points.
    ///
    /// # Panics
    ///
    /// Panics with the C++ `std::runtime_error` message if the ring cannot be split into
    /// bounds. tippecanoe calls `add_ring` outside its `try` block, so the C++ terminates there.
    pub fn add_ring(&mut self, pg: &[Point], p_type: PolygonType) -> bool {
        match self.try_add_ring(pg, p_type) {
            Ok(added) => added,
            Err(e) => panic!("{}", e),
        }
    }

    /// `add_ring`, returning the C++ exception as an error instead of panicking.
    pub fn try_add_ring(&mut self, pg: &[Point], p_type: PolygonType) -> Result<bool, Error> {
        add_linear_ring(pg, &mut self.minima_list, &mut self.bounds, p_type)
    }

    pub fn add_polygon(&mut self, ppg: &Polygon, p_type: PolygonType) -> bool {
        let mut result = false;
        for r in ppg {
            if self.add_ring(r, p_type) {
                result = true;
            }
        }
        result
    }

    pub fn reverse_rings(&mut self, value: bool) {
        self.reverse_output = value;
    }

    pub fn clear(&mut self) {
        self.minima_list.clear();
        self.bounds.clear();
    }

    pub fn get_bounds(&self) -> BoundingBox {
        let mut min = Point::new(0, 0);
        let mut max = Point::new(0, 0);
        if self.minima_list.is_empty() {
            return BoundingBox::new(min, max);
        }
        let mut first_set = false;
        for lm in &self.minima_list {
            for b in [lm.left_bound, lm.right_bound] {
                let edges = &self.bounds[b].edges;
                if !edges.is_empty() {
                    let front = edges[0];
                    let back = edges[edges.len() - 1];
                    if !first_set {
                        min = front.top;
                        max = back.bot;
                        first_set = true;
                    } else {
                        min.y = min.y.min(front.top.y);
                        max.y = max.y.max(back.bot.y);
                        max.x = max.x.max(back.top.x);
                        min.x = min.x.min(back.top.x);
                    }
                    for e in edges {
                        max.x = max.x.max(e.bot.x);
                        min.x = min.x.min(e.bot.x);
                    }
                }
            }
        }
        BoundingBox::new(min, max)
    }

    /// Runs the clip. Returns an empty multipolygon when no ring was added, where the C++
    /// returns false and leaves the solution alone.
    pub fn execute(
        &mut self,
        cliptype: ClipType,
        subject_fill_type: FillType,
        clip_fill_type: FillType,
    ) -> Result<MultiPolygon, Error> {
        let mut solution = MultiPolygon::new();
        self.execute_into(cliptype, &mut solution, subject_fill_type, clip_fill_type)?;
        Ok(solution)
    }

    /// The C++ signature: appends the result to `solution` and returns false if no ring was added.
    pub fn execute_into(
        &mut self,
        cliptype: ClipType,
        solution: &mut MultiPolygon,
        subject_fill_type: FillType,
        clip_fill_type: FillType,
    ) -> Result<bool, Error> {
        if self.minima_list.is_empty() {
            return Ok(false);
        }

        let mut manager = RingManager::new();

        build_hot_pixels(&self.minima_list, &mut self.bounds, &mut manager)?;

        execute_vatti(
            &self.minima_list,
            &mut self.bounds,
            &mut manager,
            cliptype,
            subject_fill_type,
            clip_fill_type,
        )?;

        correct_topology(&mut manager)?;

        build_result(solution, &manager, self.reverse_output);

        Ok(true)
    }
}
