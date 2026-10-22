//! Copies of tippecanoe-rs's `line_is_too_small`, `reduce_tiny_poly` and
//! `get_area` that take slices, so that candidates' geometry, which lives in
//! one array per tile, need not be copied into a vector of its own first.

use tippecanoe::geometry::{Draw, DrawVec, VT_LINETO, VT_MOVETO};

pub fn line_is_too_small(geometry: &[Draw], z: i32, detail: i32) -> bool {
    if geometry.is_empty() {
        return true;
    }

    let scale = 1i64.wrapping_shl((32 - detail - z) as u32) as f64;
    let mut x: i64 = 0;
    let mut y: i64 = 0;
    for g in geometry {
        if g.op == VT_MOVETO {
            x = (g.x as f64 / scale).round() as i64;
            y = (g.y as f64 / scale).round() as i64;
        } else {
            let xx = (g.x as f64 / scale).round() as i64;
            let yy = (g.y as f64 / scale).round() as i64;

            if xx != x || yy != y {
                return false;
            }
        }
    }

    true
}

fn get_area_scaled(geom: &[Draw], i: usize, j: usize) -> f64 {
    let max_exact_double = ((1i64 << 53) - 1) as f64;

    let mut scale: i64 = 2;
    loop {
        let bx = geom[i].x;
        let by = geom[i].y;
        let mut again = false;

        let mut area: f64 = 0.0;
        for k in i..j {
            let next = geom[i + ((k - i + 1) % (j - i))];
            area += ((geom[k].x - bx) / scale) as f64 * ((next.y - by) / scale) as f64;
            if area.abs() >= max_exact_double {
                again = true;
                break;
            }
            area -= ((geom[k].y - by) / scale) as f64 * ((next.x - bx) / scale) as f64;
            if area.abs() >= max_exact_double {
                again = true;
                break;
            }
        }

        if again && scale < (1i64 << 30) {
            scale *= 2;
            continue;
        }
        // tippecanoe exits if scaling never helps, which 40-bit coordinates rule out
        area /= 2.0;
        return area * scale as f64 * scale as f64;
    }
}

pub fn get_area(geom: &[Draw], i: usize, j: usize) -> f64 {
    let max_exact_double = ((1i64 << 53) - 1) as f64;

    let bx = geom[i].x;
    let by = geom[i].y;

    let mut area: f64 = 0.0;
    let mut overflow = false;
    for k in i..j {
        let next = geom[i + ((k - i + 1) % (j - i))];
        area += (geom[k].x - bx) as f64 * (next.y - by) as f64;
        if area.abs() >= max_exact_double {
            overflow = true;
        }
        area -= (geom[k].y - by) as f64 * (next.x - bx) as f64;
        if area.abs() >= max_exact_double {
            overflow = true;
        }
    }
    area /= 2.0;

    if overflow {
        let scaled_area = get_area_scaled(geom, i, j);
        if (area < 0.0 && scaled_area > 0.0) || (area > 0.0 && scaled_area < 0.0) {
            area = -area;
        }
    }

    area
}

pub fn get_mp_area(geom: &[Draw]) -> f64 {
    let mut ret = 0.0;

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

            ret += get_area(geom, i, j);
            i = j - 1;
        }
        i += 1;
    }

    ret
}

pub fn reduce_tiny_poly(
    geom: &[Draw],
    z: i32,
    detail: i32,
    still_needs_simplification: &mut bool,
    reduced_away: &mut bool,
    accum_area: &mut f64,
    tiny_polygon_size: f64,
) -> DrawVec {
    let mut out = DrawVec::new();
    let pixel = 1i64.wrapping_shl((32 - detail - z) as u32) as f64 * tiny_polygon_size;

    let mut included_last_outer = false;
    *still_needs_simplification = false;
    *reduced_away = false;

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

            let area = get_area(geom, i, j);

            if area != 0.0 {
                if (area > 0.0 && area <= pixel * pixel) || (area < 0.0 && !included_last_outer) {
                    *accum_area += area;
                    *reduced_away = true;

                    if area > 0.0 && *accum_area > pixel * pixel {
                        let x = geom[i].x as f64;
                        let y = geom[i].y as f64;
                        out.push(Draw::new(
                            VT_MOVETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0 + pixel) as i64,
                            (y - pixel / 2.0) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0 + pixel) as i64,
                            (y - pixel / 2.0 + pixel) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0 + pixel) as i64,
                        ));
                        out.push(Draw::new(
                            VT_LINETO,
                            (x - pixel / 2.0) as i64,
                            (y - pixel / 2.0) as i64,
                        ));

                        *accum_area -= pixel * pixel;
                    }

                    if area > 0.0 {
                        included_last_outer = false;
                    }
                } else {
                    out.extend_from_slice(&geom[i..j.min(geom.len())]);

                    *still_needs_simplification = true;

                    if area > 0.0 {
                        included_last_outer = true;
                    }
                }
            }

            i = j - 1;
        } else {
            eprintln!(
                "how did we get here with {} in {}?",
                geom[i].op as i32,
                geom.len() as i32
            );

            for g in geom {
                eprint!("{}/{}/{} ", g.op as i32, g.x, g.y);
            }
            eprintln!();

            out.push(geom[i]);
        }
        i += 1;
    }

    out
}
