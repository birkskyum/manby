//! Port of geometry.hpp (the geometry types) and geometry.cpp.
//! The functions geometry.hpp declares but clip.cpp defines are in clip.rs.

use std::cmp::Ordering;

use crate::mvt::MvtTile;

pub const VT_POINT: i32 = 1;
pub const VT_LINE: i32 = 2;
pub const VT_POLYGON: i32 = 3;

pub const VT_END: i8 = 0;
pub const VT_MOVETO: i8 = 1;
pub const VT_LINETO: i8 = 2;
pub const VT_CLOSEPATH: i8 = 7;

/// `draw`. The C++ packs `x` and `y` into 40-bit bitfields to keep the struct at
/// 16 bytes. Coordinates stay far inside that range, so plain `i64` is used here.
///
/// Equality and ordering look only at the coordinates, `y` first, as in the C++.
#[derive(Clone, Copy, Debug, Default)]
pub struct Draw {
    pub x: i64,
    pub op: i8,
    pub y: i64,
    pub necessary: i8,
}

impl Draw {
    pub fn new(op: i8, x: i64, y: i64) -> Draw {
        Draw {
            x,
            op,
            y,
            necessary: 0,
        }
    }
}

impl PartialEq for Draw {
    fn eq(&self, s: &Draw) -> bool {
        self.y == s.y && self.x == s.x
    }
}

impl Eq for Draw {}

impl PartialOrd for Draw {
    fn partial_cmp(&self, s: &Draw) -> Option<Ordering> {
        Some(self.cmp(s))
    }
}

impl Ord for Draw {
    fn cmp(&self, s: &Draw) -> Ordering {
        self.y.cmp(&s.y).then(self.x.cmp(&s.x))
    }
}

pub type DrawVec = Vec<Draw>;

#[derive(Clone, Debug, Default)]
pub struct InputTile {
    pub tile: Vec<u8>,
    pub z: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Default)]
pub struct SourceTile {
    pub tile: MvtTile,
    pub z: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ClipBbox {
    pub lon1: f64,
    pub lat1: f64,
    pub lon2: f64,
    pub lat2: f64,

    pub minx: i64,
    pub miny: i64,
    pub maxx: i64,
    pub maxy: i64,

    /// empty, or arbitrary clipping polygon
    pub dv: DrawVec,
}

/// Reads a serialized geometry, advancing `meta` past it.
pub fn decode_geometry(
    meta: &mut &[u8],
    z: i32,
    tx: u32,
    ty: u32,
    bbox: &mut [i64; 4],
    initial_x: u32,
    initial_y: u32,
) -> DrawVec {
    todo!("geometry::decode_geometry")
}

pub fn check_polygon(geom: &mut DrawVec) {
    todo!("geometry::check_polygon")
}

pub fn quick_check(bbox: &[i64; 4], z: i32, buffer: i64) -> i32 {
    todo!("geometry::quick_check")
}

pub fn point_within_tile(x: i64, y: i64, z: i32) -> bool {
    todo!("geometry::point_within_tile")
}

pub fn simplify_lines(
    geom: &mut DrawVec,
    z: i32,
    tx: i32,
    ty: i32,
    detail: i32,
    mark_tile_bounds: bool,
    simplification: f64,
    retain: usize,
    shared_nodes: &DrawVec,
    shared_nodes_map: &[crate::serial::Node],
    nodepos: usize,
    shared_nodes_bloom: &[u8],
) -> DrawVec {
    todo!("geometry::simplify_lines")
}

pub fn reorder_lines(geom: &DrawVec) -> DrawVec {
    todo!("geometry::reorder_lines")
}

pub fn chop_polygon(geoms: &mut Vec<DrawVec>) -> Vec<DrawVec> {
    todo!("geometry::chop_polygon")
}

pub fn stairstep(geom: &mut DrawVec, z: i32, detail: i32) -> DrawVec {
    todo!("geometry::stairstep")
}

pub fn center_of_mass_mp(dv: &DrawVec) -> Draw {
    todo!("geometry::center_of_mass_mp")
}

pub fn polygon_to_anchor(geom: &DrawVec) -> DrawVec {
    todo!("geometry::polygon_to_anchor")
}

pub fn checkerboard_anchors(geom: &DrawVec, tx: i32, ty: i32, z: i32, label_point: u64) -> DrawVec {
    todo!("geometry::checkerboard_anchors")
}
