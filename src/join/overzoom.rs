//! tile-join's overzooming: tippecanoe-rs's `overzoom_source_tiles` with the
//! arguments tile-join gives it (no attribute filters or accumulation, no
//! simplification, no feature limit), making the same bytes, without the
//! tables of 65536 slots its layers allocate, five for each tile: one for the
//! tile and one for each child it tries to find out whether it will have
//! features. Those only need to know whether a feature survives, so they stop
//! at the first.
//!
//! The geometry is tippecanoe-rs's own: its clipping, cleaning and scaling.

use tippecanoe::clip::{
    clean_or_clip_poly, clip_lines, clip_point, close_poly, remove_noop, simple_clip_poly,
    to_tile_scale,
};
use tippecanoe::errors::EXIT_MVT;
use tippecanoe::geometry::{Draw, DrawVec, VT_LINE, VT_LINETO, VT_POINT, VT_POLYGON};
use tippecanoe::mvt::{MVT_CLOSEPATH, MVT_MOVETO};

use super::tile::{Decoded, Geom, Out, OutFeature, OutLayer, Point, Scratch, for_each_point};
use crate::{Error, Result};

/// A tile's zoom, x and y.
pub type Zxy = (i32, i64, i64);

/// The detail of a layer: `log2` of its extent, rounded.
fn detail(extent: i64) -> i32 {
    ((extent as f64).ln() / 2f64.ln()).round() as i32
}

fn zero_extent(name: &str, (z, x, y): Zxy) -> Error {
    Error::new(
        EXIT_MVT,
        format!(
            "Layer {} of tile {}/{}/{} has an extent of 0",
            name, z, x, y
        ),
    )
}

/// One feature of a tile at `src` with `extent`, made into a feature of the
/// tile `dst` as `overzoom_source_tiles` makes it, or None if it is clipped
/// or cleaned away.
fn feature(
    points: impl Iterator<Item = Point>,
    t: i32,
    extent: i64,
    src: Zxy,
    dst: Zxy,
    buffer: i32,
    det: i32,
) -> Option<DrawVec> {
    // to world coordinates
    let tilesize = 1i64.wrapping_shl((32 - src.0) as u32);
    let mut geom = DrawVec::new();
    let mut ring_closure = Draw::new(0, 0, 0);
    for g in points {
        if g.op == MVT_CLOSEPATH {
            geom.push(ring_closure);
        } else {
            geom.push(Draw::new(
                g.op as i8,
                g.x * tilesize / extent + src.1 * tilesize,
                g.y * tilesize / extent + src.2 * tilesize,
            ));
            if g.op == MVT_MOVETO {
                ring_closure = geom[geom.len() - 1];
                ring_closure.op = VT_LINETO;
            }
        }
    }

    // to the output tile, still at world scale
    let outtilesize = 1i64.wrapping_shl((32 - dst.0) as u32);
    for g in geom.iter_mut() {
        g.x -= dst.1 * outtilesize;
        g.y -= dst.2 * outtilesize;
    }

    let (mut xmin, mut ymin, mut xmax, mut ymax) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for g in &geom {
        xmin = xmin.min(g.x);
        ymin = ymin.min(g.y);
        xmax = xmax.max(g.x);
        ymax = ymax.max(g.y);
    }
    let b = outtilesize * buffer as i64 / 256;
    if xmax < -b || ymax < -b || xmin > outtilesize + b || ymin > outtilesize + b {
        return None;
    }
    if t == VT_LINE {
        geom = clip_lines(&mut geom, dst.0, buffer as i64);
    } else if t == VT_POLYGON {
        let mut dv = DrawVec::new();
        geom = simple_clip_poly(&mut geom, dst.0, buffer, &mut dv, false);
    } else if t == VT_POINT {
        geom = clip_point(&mut geom, dst.0, buffer as i64);
    }
    if geom.is_empty() {
        return None;
    }

    to_tile_scale(&mut geom, dst.0, det);
    // never the same tile: overzoomed tiles are deeper than their sources
    geom = remove_noop(geom, t, 0);
    if t == VT_POLYGON {
        geom = clean_or_clip_poly(&mut geom, 0, 0, false, false);
        geom = close_poly(&mut geom);
    }
    if geom.is_empty() {
        return None;
    }
    Some(geom)
}

/// The overzoomed tile `dst` made from `parent`, the tile `src`, as
/// tile-join's `retrieve_overzoom` makes it, uncompressed, and the children
/// of `dst` that will have features in them, as `overzoom_source_tiles`
/// lists them in `next_overzoomed_tiles`.
pub fn overzoom(
    parent: &Decoded<'_>,
    src: Zxy,
    dst: Zxy,
    buffer: i32,
    children: &mut Vec<(u32, u32)>,
) -> Result<Vec<u8>> {
    let mut out = Out::default();
    for layer in &parent.layers {
        let det = detail(layer.extent);
        let ol = match out.layers.iter().rposition(|l| l.name == layer.name) {
            Some(ol) => ol,
            None => {
                out.layers.push(OutLayer::new(
                    &layer.name,
                    layer.version,
                    1i64.wrapping_shl(det as u32),
                ));
                out.layers.len() - 1
            }
        };
        for feat in &layer.features {
            let commands = &parent.geoms[feat.geom.start as usize..feat.geom.end as usize];
            if layer.extent == 0 {
                let mut any = false;
                for_each_point(commands, |p| any |= p.op != MVT_CLOSEPATH);
                if any {
                    return Err(zero_extent(&layer.name, src));
                }
            }
            let mut points = Vec::new();
            for_each_point(commands, |p| points.push(p));
            let Some(geom) = feature(
                points.into_iter(),
                feat.type_,
                layer.extent,
                src,
                dst,
                buffer,
                det,
            ) else {
                continue;
            };

            // feature_out: the geometry as it is, and every attribute
            let start = out.points.len() as u32;
            out.points.extend(geom.iter().map(|g| Point {
                op: g.op as i32,
                x: g.x,
                y: g.y,
            }));
            let points = start..out.points.len() as u32;
            let tags = &parent.tags[feat.tags.start as usize..feat.tags.end as usize];
            let tags_start = out.tags.len() as u32;
            let mut i = 0;
            while i + 1 < tags.len() {
                let key = layer.keys.get(tags[i] as usize);
                let value = layer.values.get(tags[i + 1] as usize);
                let (Some(key), Some(value)) = (key, value) else {
                    return Err(Error::new(
                        tippecanoe::errors::EXIT_PROTOBUF,
                        format!(
                            "PBF decoding error in tile {}/{}/{}: a feature's key or value is out of range",
                            src.0, src.1, src.2
                        ),
                    ));
                };
                out.layers[ol].tag(key, value, &mut out.tags);
                i += 2;
            }
            out.layers[ol].features.push(OutFeature {
                id: feat.id,
                has_id: feat.has_id,
                type_: feat.type_,
                tags: tags_start..out.tags.len() as u32,
                geom: Geom::Points(points),
            });
        }
    }

    // will any child tiles have features in them?
    if !out.layers.is_empty() {
        for x in 0..2 {
            for y in 0..2 {
                let child = (dst.0 + 1, dst.1 * 2 + x, dst.2 * 2 + y);
                if any_feature(&out, dst, child, buffer) {
                    children.push((child.1 as u32, child.2 as u32));
                }
            }
        }
    }

    let mut pbf = Vec::new();
    out.encode(&[], &mut Scratch::default(), &mut pbf)?;
    Ok(pbf)
}

/// Whether overzooming `tile`, the tile `src`, into `dst` leaves a feature.
fn any_feature(tile: &Out<'_>, src: Zxy, dst: Zxy, buffer: i32) -> bool {
    for layer in &tile.layers {
        let det = detail(layer.extent);
        for f in &layer.features {
            let Geom::Points(r) = &f.geom else {
                continue;
            };
            let points = tile.points[r.start as usize..r.end as usize]
                .iter()
                .copied();
            if feature(points, f.type_, layer.extent, src, dst, buffer, det).is_some() {
                return true;
            }
        }
    }
    false
}
