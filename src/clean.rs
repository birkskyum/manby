//! Copies of tippecanoe-rs's `clean_or_clip_poly` and `coalesce_polygon`, which
//! clean polygons with manby's own copy of wagyu (`crates/wagyu`), so that wagyu
//! can be made faster without changing tippecanoe-rs. The polygons are the same.

use std::io::Write;
use std::process::exit;

use tippecanoe::clip::{get_area, remove_noop};
use tippecanoe::errors::EXIT_IMPOSSIBLE;
use tippecanoe::geometry::{Draw, DrawVec, VT_LINETO, VT_MOVETO, VT_POLYGON};
use wagyu::{
    ClipType as WagyuClipType, FillType, LinearRing, MultiPolygon, Point, PolygonType, Wagyu,
};

fn decode_clipped(t: &MultiPolygon, out: &mut DrawVec, scale: f64) {
    out.clear();

    for polygon in t {
        for (j, lr) in polygon.iter().enumerate() {
            let mut ring = DrawVec::new();

            for (k, p) in lr.iter().enumerate() {
                ring.push(Draw::new(
                    if k == 0 { VT_MOVETO } else { VT_LINETO },
                    (p.x as f64 / scale).round() as i64,
                    (p.y as f64 / scale).round() as i64,
                ));
            }

            if !ring.is_empty() && ring[ring.len() - 1] != ring[0] {
                eprintln!("Had to close ring");
                ring.push(Draw::new(VT_LINETO, ring[0].x, ring[0].y));
            }

            let area = get_area(&ring, 0, ring.len());

            if (j == 0 && area < 0.0) || (j != 0 && area > 0.0) {
                eprintln!("Ring area has wrong sign: {:.6} for {}", area, j);
                exit(EXIT_IMPOSSIBLE);
            }

            out.extend_from_slice(&ring);
        }
    }
}

/// The ring wagyu would be given, scaled, if the geometry has just one.
fn single_ring(geom: &DrawVec, scale: f64) -> Option<LinearRing> {
    let mut found = None;
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() && geom[j].op == VT_LINETO {
                j += 1;
            }
            if j >= i + 4 {
                if found.is_some() {
                    return None;
                }
                found = Some(i..j);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let range = found?;
    Some(
        geom[range]
            .iter()
            .map(|g| Point::new((g.x as f64 * scale) as i64, (g.y as f64 * scale) as i64))
            .collect(),
    )
}

pub fn clean_or_clip_poly(
    geom: &mut DrawVec,
    z: i32,
    buffer: i32,
    clip: bool,
    try_scaling: bool,
) -> DrawVec {
    *geom = remove_noop(std::mem::take(geom), VT_POLYGON, 0);
    let mut result: MultiPolygon = MultiPolygon::new();

    let mut scale = 16.0;
    if !try_scaling {
        scale = 1.0;
    }

    let mut again = true;
    while again {
        again = false;

        // A single ring that is already simple comes out of wagyu as it went
        // in, which wagyu::union_of_simple_ring tells without the sweep.
        if !clip
            && let Some(r) =
                single_ring(geom, scale).and_then(|lr| wagyu::union_of_simple_ring(&lr))
        {
            result = r;
            break;
        }

        let mut wagyu = Wagyu::new();

        let mut i = 0;
        while i < geom.len() {
            if geom[i].op == VT_MOVETO {
                let mut j = i + 1;
                while j < geom.len() {
                    if geom[j].op != VT_LINETO {
                        break;
                    }
                    j += 1;
                }

                if j >= i + 4 {
                    let mut lr = LinearRing::new();

                    for g in &geom[i..j] {
                        lr.push(Point::new(
                            (g.x as f64 * scale) as i64,
                            (g.y as f64 * scale) as i64,
                        ));
                    }

                    if lr.len() >= 3 {
                        wagyu.add_ring(&lr, PolygonType::Subject);
                    }
                }

                i = j - 1;
            }
            i += 1;
        }

        if clip {
            let mut area: i64 = 0xFFFFFFFF;
            if z != 0 {
                area = 1i64 << (32 - z);
            }
            let clip_buffer: i64 = buffer as i64 * area / 256;

            let lr: LinearRing = vec![
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (area + clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (area + clip_buffer) as f64) as i64,
                    (scale * (area + clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (area + clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
                Point::new(
                    (scale * (-clip_buffer) as f64) as i64,
                    (scale * (-clip_buffer) as f64) as i64,
                ),
            ];

            wagyu.add_ring(&lr, PolygonType::Clip);
        }

        result.clear();
        match wagyu.execute(WagyuClipType::Union, FillType::Positive, FillType::Positive) {
            Ok(r) => result = r,
            Err(e) => {
                let mut f = String::new();
                f.push_str(&format!("{}\n", e));
                eprintln!("{}", e);
                f.push('[');

                let mut i = 0;
                while i < geom.len() {
                    if geom[i].op == VT_MOVETO {
                        let mut j = i + 1;
                        while j < geom.len() {
                            if geom[j].op != VT_LINETO {
                                break;
                            }
                            j += 1;
                        }

                        if j >= i + 4 {
                            if i != 0 {
                                f.push(',');
                            }
                            f.push('[');

                            for (k, g) in geom[i..j].iter().enumerate() {
                                if k != 0 {
                                    f.push(',');
                                }
                                f.push_str(&format!("[{},{}]", g.x, g.y));
                            }

                            f.push(']');
                        }

                        i = j - 1;
                    }
                    i += 1;
                }

                f.push(']');
                f.push_str("\n\n\n\n\n");

                if let Ok(mut file) = std::fs::File::create("/tmp/wagyu.log") {
                    let _ = file.write_all(f.as_bytes());
                }
                eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                exit(EXIT_IMPOSSIBLE);
            }
        }

        if scale != 1.0 {
            for outer in &result {
                for ring in outer {
                    for p in ring {
                        if p.x as f64 / scale != (p.x as f64 / scale).round()
                            || p.y as f64 / scale != (p.y as f64 / scale).round()
                        {
                            scale = 1.0;
                            again = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    let mut ret = DrawVec::new();
    decode_clipped(&result, &mut ret, scale);
    ret
}

pub fn coalesce_polygon(geom: &mut DrawVec, scale_up: bool) {
    // wagyu should be able to straightforwardly handle
    // anything under a few hundred thousand vertices
    if geom.len() < 100000 {
        *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
        return;
    }

    // These geometries were assembled in geometric order,
    // so sub-batches of them should hopefully union into
    // reasonable sets.
    //
    // Find the first outer ring after halfway point.

    let mut i = geom.len() / 2;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            if get_area(geom, i, j) > 0.0 {
                // If we have an outer ring, split there
                // and coalesce the two halves

                // Copy second half to new vector
                let mut geom2: Vec<Draw> = geom[i..].to_vec();

                // Resize vector to include only first half
                geom.truncate(i);

                // Clean each half individually
                coalesce_polygon(geom, scale_up);
                coalesce_polygon(&mut geom2, scale_up);

                // Copy second half back with first
                geom.extend_from_slice(&geom2);
                geom2.clear();

                // Clean the combined geometry
                *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
                return;
            }

            i = j - 1;
        }
        i += 1;
    }

    // Can't find a breakpoint; take what we can get.
    *geom = clean_or_clip_poly(geom, 0, 0, false, scale_up);
}
