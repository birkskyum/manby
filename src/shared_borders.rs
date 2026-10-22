//! Port of shared_borders.cpp.

use crate::serial::SerialFeaturePtr;

pub fn find_common_edges(
    features: &mut Vec<SerialFeaturePtr>,
    z: i32,
    line_detail: i32,
    simplification: f64,
    maxzoom: i32,
    merge_fraction: f64,
) -> bool {
    todo!("shared_borders::find_common_edges")
}
