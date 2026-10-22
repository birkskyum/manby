//! Port of read_json.cpp.

use std::io::Read;

use crate::geometry::DrawVec;
use crate::jsonpull::{JsonPull, JsonRef};
use crate::mvt::MvtLayer;
use crate::serial::SerialVal;

/// array of positions
pub const GEOM_POINT: i32 = 0;
/// array of arrays of positions
pub const GEOM_MULTIPOINT: i32 = 1;
/// array of arrays of positions
pub const GEOM_LINESTRING: i32 = 2;
/// array of arrays of arrays of positions
pub const GEOM_MULTILINESTRING: i32 = 3;
/// array of arrays of arrays of positions
pub const GEOM_POLYGON: i32 = 4;
/// array of arrays of arrays of arrays of positions
pub const GEOM_MULTIPOLYGON: i32 = 5;
pub const GEOM_TYPES: usize = 6;

pub static GEOMETRY_NAMES: [&str; GEOM_TYPES] = [
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Polygon",
    "MultiPolygon",
];
pub static GEOMETRY_WITHIN: [i32; GEOM_TYPES] = [
    -1,
    GEOM_POINT,
    GEOM_POINT,
    GEOM_LINESTRING,
    GEOM_LINESTRING,
    GEOM_POLYGON,
];
pub static MB_GEOMETRY: [i32; GEOM_TYPES] = [
    crate::geometry::VT_POINT,
    crate::geometry::VT_POINT,
    crate::geometry::VT_LINE,
    crate::geometry::VT_LINE,
    crate::geometry::VT_POLYGON,
    crate::geometry::VT_POLYGON,
];

/// Prints `in JSON object ...` with the object, truncated, to stderr.
pub fn json_context(j: JsonRef<'_>) {
    todo!("read_json::json_context")
}

#[allow(clippy::too_many_arguments)]
pub fn parse_coordinates(
    t: i32,
    j: JsonRef<'_>,
    out: &mut DrawVec,
    op: i8,
    fname: &str,
    line: i32,
    feature: JsonRef<'_>,
) {
    todo!("read_json::parse_coordinates")
}

#[allow(clippy::too_many_arguments)]
pub fn parse_geometry(
    geometry: JsonRef<'_>,
    jp: &JsonPull<'_>,
    j: JsonRef<'_>,
    z: i32,
    x: i32,
    y: i32,
    extent: i64,
    fix_longitudes: bool,
    mvt_style: bool,
) -> (i32, DrawVec) {
    todo!("read_json::parse_geometry")
}

pub fn parse_layers(
    fp: Box<dyn Read + '_>,
    z: i32,
    x: u32,
    y: u32,
    extent: i32,
    fix_longitudes: bool,
) -> Vec<MvtLayer> {
    todo!("read_json::parse_layers")
}

pub fn stringify_value(
    value: Option<JsonRef<'_>>,
    reading: &str,
    line: i32,
    feature: JsonRef<'_>,
) -> SerialVal {
    todo!("read_json::stringify_value")
}
