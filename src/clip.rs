//! Port of clip.cpp: clipping, polygon cleaning with wagyu, line clipping with
//! Clipper2, simplification, and the overzoom machinery.
//!
//! C++ overloads get distinct names here: the bounding-box forms of
//! `simple_clip_poly`, `clip_point` and `clip_lines` end in `_bbox`, and the
//! `mvt_geometry` forms of `pnpoly` and `pnpoly_mp` end in `_mvt`.

use std::collections::{BTreeSet, HashMap};

use crate::attribute::AttributeOp;
use crate::geometry::{ClipBbox, DrawVec, InputTile, SourceTile};
use crate::jsonpull::JsonRef;
use crate::mvt::{MvtGeometry, MvtLayer};

/// `simple_clip_poly(geom, minx, miny, maxx, maxy, ax, ay, bx, by, edge_nodes, prevent_simplify_shared_nodes)`.
#[allow(clippy::too_many_arguments)]
pub fn simple_clip_poly_bbox_edges(
    geom: &mut DrawVec,
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    ax: i64,
    ay: i64,
    bx: i64,
    by: i64,
    edge_nodes: &mut DrawVec,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    todo!("clip::simple_clip_poly_bbox_edges")
}

/// `simple_clip_poly(geom, minx, miny, maxx, maxy, prevent_simplify_shared_nodes)`.
pub fn simple_clip_poly_bbox(
    geom: &mut DrawVec,
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    todo!("clip::simple_clip_poly_bbox")
}

/// `simple_clip_poly(geom, z, buffer, edge_nodes, prevent_simplify_shared_nodes)`.
pub fn simple_clip_poly(
    geom: &mut DrawVec,
    z: i32,
    buffer: i32,
    edge_nodes: &mut DrawVec,
    prevent_simplify_shared_nodes: bool,
) -> DrawVec {
    todo!("clip::simple_clip_poly")
}

pub fn clip_point(geom: &mut DrawVec, z: i32, buffer: i64) -> DrawVec {
    todo!("clip::clip_point")
}

pub fn clip_point_bbox(geom: &mut DrawVec, minx: i64, miny: i64, maxx: i64, maxy: i64) -> DrawVec {
    todo!("clip::clip_point_bbox")
}

pub fn clip_lines(geom: &mut DrawVec, z: i32, buffer: i64) -> DrawVec {
    todo!("clip::clip_lines")
}

pub fn clip_lines_bbox(geom: &mut DrawVec, minx: i64, miny: i64, maxx: i64, maxy: i64) -> DrawVec {
    todo!("clip::clip_lines_bbox")
}

/// Clips the segment to the box. Returns 0 if it is outside, 1 if unchanged, 2 if clipped.
#[allow(clippy::too_many_arguments)]
pub fn clip(
    x0: &mut i64,
    y0: &mut i64,
    x1: &mut i64,
    y1: &mut i64,
    xmin: i64,
    ymin: i64,
    xmax: i64,
    ymax: i64,
) -> i32 {
    todo!("clip::clip")
}

pub fn clean_or_clip_poly(
    geom: &mut DrawVec,
    z: i32,
    buffer: i32,
    clip: bool,
    try_scaling: bool,
) -> DrawVec {
    todo!("clip::clean_or_clip_poly")
}

pub fn clip_poly_poly(geom: &DrawVec, bounds: &DrawVec) -> DrawVec {
    todo!("clip::clip_poly_poly")
}

pub fn clip_point_poly(geom: &DrawVec, bounds: &DrawVec) -> DrawVec {
    todo!("clip::clip_point_poly")
}

pub fn clip_lines_poly(geom: &DrawVec, region: &DrawVec) -> DrawVec {
    todo!("clip::clip_lines_poly")
}

pub fn to_tile_scale(geom: &mut DrawVec, z: i32, detail: i32) {
    todo!("clip::to_tile_scale")
}

pub fn from_tile_scale(geom: &DrawVec, z: i32, detail: i32) -> DrawVec {
    todo!("clip::from_tile_scale")
}

pub fn remove_noop(geom: DrawVec, type_: i32, shift: i32) -> DrawVec {
    todo!("clip::remove_noop")
}

pub fn get_area_scaled(geom: &DrawVec, i: usize, j: usize) -> f64 {
    todo!("clip::get_area_scaled")
}

pub fn get_area(geom: &DrawVec, i: usize, j: usize) -> f64 {
    todo!("clip::get_area")
}

pub fn get_mp_area(geom: &DrawVec) -> f64 {
    todo!("clip::get_mp_area")
}

pub fn close_poly(geom: &mut DrawVec) -> DrawVec {
    todo!("clip::close_poly")
}

pub fn distance_from_line(
    point_x: i64,
    point_y: i64,
    seg_a_x: i64,
    seg_a_y: i64,
    seg_b_x: i64,
    seg_b_y: i64,
) -> f64 {
    todo!("clip::distance_from_line")
}

#[allow(clippy::too_many_arguments)]
pub fn douglas_peucker(
    geom: &mut DrawVec,
    start: i32,
    n: i32,
    e: f64,
    kept: usize,
    retain: usize,
    prevent_simplify_shared_nodes: bool,
) {
    todo!("clip::douglas_peucker")
}

#[allow(clippy::too_many_arguments)]
pub fn reduce_tiny_poly(
    geom: &DrawVec,
    z: i32,
    detail: i32,
    still_needs_simplification: &mut bool,
    reduced_away: &mut bool,
    accum_area: &mut f64,
    tiny_polygon_size: f64,
) -> DrawVec {
    todo!("clip::reduce_tiny_poly")
}

pub fn pnpoly(vert: &DrawVec, start: usize, nvert: usize, testx: i64, testy: i64) -> i32 {
    todo!("clip::pnpoly")
}

pub fn pnpoly_mvt(
    vert: &[MvtGeometry],
    start: usize,
    nvert: usize,
    testx: i64,
    testy: i64,
) -> bool {
    todo!("clip::pnpoly_mvt")
}

pub fn pnpoly_mp_mvt(geom: &[MvtGeometry], x: i64, y: i64) -> bool {
    todo!("clip::pnpoly_mp_mvt")
}

pub fn pnpoly_mp(geom: &DrawVec, x: i64, y: i64) -> bool {
    todo!("clip::pnpoly_mp")
}

/// `overzoom(std::vector<input_tile> const &tiles, ...)`. Returns the encoded tile.
#[allow(clippy::too_many_arguments)]
pub fn overzoom(
    tiles: &[InputTile],
    nz: i32,
    nx: i32,
    ny: i32,
    detail: i32,
    buffer: i32,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    do_compress: bool,
    next_overzoomed_tiles: Option<&mut Vec<(u32, u32)>>,
    demultiply: bool,
    filter: Option<JsonRef<'_>>,
    preserve_input_order: bool,
    attribute_accum: &HashMap<String, AttributeOp>,
    unidecode_data: &[String],
    simplification: f64,
    tiny_polygon_size: f64,
    bins: &[MvtLayer],
    bin_by_id_list: &str,
    accumulate_numeric: &str,
    feature_limit: usize,
    clipbboxes: &[ClipBbox],
    deduplicate_by_id: bool,
) -> Vec<u8> {
    todo!("clip::overzoom")
}

/// `overzoom(std::vector<source_tile> const &tiles, ...)`. Returns the encoded tile.
#[allow(clippy::too_many_arguments)]
pub fn overzoom_source_tiles(
    tiles: &[SourceTile],
    nz: i32,
    nx: i32,
    ny: i32,
    detail: i32,
    buffer: i32,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    do_compress: bool,
    next_overzoomed_tiles: Option<&mut Vec<(u32, u32)>>,
    demultiply: bool,
    filter: Option<JsonRef<'_>>,
    preserve_input_order: bool,
    attribute_accum: &HashMap<String, AttributeOp>,
    unidecode_data: &[String],
    simplification: f64,
    tiny_polygon_size: f64,
    bins: &[MvtLayer],
    bin_by_id_list: &str,
    accumulate_numeric: &str,
    feature_limit: usize,
    clipbboxes: &[ClipBbox],
    deduplicate_by_id: bool,
) -> Vec<u8> {
    todo!("clip::overzoom_source_tiles")
}

pub fn fix_polygon(geom: &DrawVec, use_winding: bool, reverse_winding: bool) -> DrawVec {
    todo!("clip::fix_polygon")
}

pub fn line_is_too_small(geometry: &DrawVec, z: i32, detail: i32) -> bool {
    todo!("clip::line_is_too_small")
}

pub fn coalesce_polygon(geom: &mut DrawVec, scale_up: bool) {
    todo!("clip::coalesce_polygon")
}
