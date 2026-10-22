//! Sorting the store into tippecanoe's order and assigning each feature its
//! minimum zoom. See DESIGN.md.

use crate::options::Options;
use crate::store::FeatureStore;

/// Zoom levels and other choices that depend on the whole input, such as the
/// results of `-zg` and `-Bg`.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub minzoom: i32,
    pub maxzoom: i32,
    pub basezoom: i32,
    pub droprate: f64,
}

/// Sorts `store.features` and fills in `feature_minzoom`. Returns the zoom plan.
pub fn order(store: &mut FeatureStore, options: &Options) -> crate::Result<Plan> {
    todo!("order::order")
}
