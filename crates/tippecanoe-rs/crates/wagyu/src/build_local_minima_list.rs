//! Port of `build_local_minima_list.hpp`.

use crate::bound::Bound;
use crate::build_edges::build_edge_list;
use crate::config::{PolygonType, Result};
use crate::edge::Edge;
use crate::geometry::Point;
use crate::local_minimum::LocalMinimum;
use crate::local_minimum_util::add_ring_to_local_minima_list;

pub(crate) fn add_linear_ring(
    path_geometry: &[Point],
    minima_list: &mut Vec<LocalMinimum>,
    bounds: &mut Vec<Bound>,
    p_type: PolygonType,
) -> Result<bool> {
    let mut new_edges: Vec<Edge> = Vec::with_capacity(path_geometry.len());
    if !build_edge_list(path_geometry, &mut new_edges) || new_edges.is_empty() {
        return Ok(false);
    }
    add_ring_to_local_minima_list(&mut new_edges, minima_list, bounds, p_type)?;
    Ok(true)
}
