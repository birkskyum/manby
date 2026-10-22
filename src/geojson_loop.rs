//! Port of geojson-loop.cpp, which walks a stream of JSON and hands each GeoJSON
//! feature or bare geometry to a `JsonFeatureAction`.

use crate::jsonpull::{JsonPull, JsonRef};

/// `json_feature_action`. `line` is the parser's line number when the feature
/// was completed, which the C++ reads through the node's `parser` pointer.
pub trait JsonFeatureAction {
    fn fname(&self) -> &str;
    fn add_feature(
        &mut self,
        line: i32,
        geometry: JsonRef<'_>,
        geometrycollection: bool,
        properties: Option<JsonRef<'_>>,
        id: Option<JsonRef<'_>>,
        tippecanoe: Option<JsonRef<'_>>,
        feature: JsonRef<'_>,
    ) -> i32;
    fn check_crs(&mut self, j: JsonRef<'_>);
}

pub fn parse_json(jfa: &mut dyn JsonFeatureAction, jp: &mut JsonPull<'_>) {
    todo!("geojson_loop::parse_json")
}
