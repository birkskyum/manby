//! Port of read_json.cpp.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clip::{clean_or_clip_poly, close_poly, fix_polygon, remove_noop};
use crate::errors::{EXIT_JSON, EXIT_UTF8};
use crate::geometry::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON};
use crate::jsonpull::{JsonPull, JsonRef, JsonType};
use crate::mvt::{
    MVT_BOOL, MVT_DOUBLE, MVT_NULL, MVT_STRING, MvtFeature, MvtGeometry, MvtLayer,
    stringified_to_mvt_value,
};
use crate::serial::SerialVal;
use crate::text::check_utf8_raw;

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
    -1,              /* point */
    GEOM_POINT,      /* multipoint */
    GEOM_POINT,      /* linestring */
    GEOM_LINESTRING, /* multilinestring */
    GEOM_LINESTRING, /* polygon */
    GEOM_POLYGON,    /* multipolygon */
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
    let mut s = j.stringify_raw();

    if s.len() >= 500 {
        s.truncate(497);
        s.extend_from_slice(b"...");
    }

    // fprintf("%s") stops at a NUL
    if let Some(n) = s.iter().position(|&c| c == 0) {
        s.truncate(n);
    }

    let mut out = b"in JSON object ".to_vec();
    out.extend_from_slice(&s);
    out.push(b'\n');
    let _ = std::io::stderr().write_all(&out);
}

static DIMENSIONS_WARNED: AtomicBool = AtomicBool::new(false);

#[allow(clippy::too_many_arguments)]
pub fn parse_coordinates(
    t: i32,
    j: JsonRef<'_>,
    out: &mut DrawVec,
    mut op: i8,
    fname: &str,
    line: i32,
    feature: JsonRef<'_>,
) {
    if j.type_() != JsonType::Array {
        eprint!(
            "{}:{}: expected array for geometry type {}: ",
            fname, line, t
        );
        json_context(feature);
        return;
    }

    let within = GEOMETRY_WITHIN[t as usize];
    if within >= 0 {
        for (i, elem) in j.array_iter().enumerate() {
            if within == GEOM_POINT {
                if i == 0 || MB_GEOMETRY[t as usize] == VT_POINT {
                    op = VT_MOVETO;
                } else {
                    op = VT_LINETO;
                }
            }

            parse_coordinates(within, elem, out, op, fname, line, feature);
        }
    } else if j.array_len() >= 2
        && j.array_get(0).type_() == JsonType::Number
        && j.array_get(1).type_() == JsonType::Number
    {
        let lon = j.array_get(0).number();
        let lat = j.array_get(1).number();
        let (x, y) = (crate::projection::projection().project)(lon, lat, 32);

        if j.array_len() > 2 && !DIMENSIONS_WARNED.load(Ordering::Relaxed) {
            eprint!("{}:{}: ignoring dimensions beyond two: ", fname, line);
            json_context(j);
            eprint!("{}:{}: ignoring dimensions beyond two: ", fname, line);
            json_context(feature);
            DIMENSIONS_WARNED.store(true, Ordering::Relaxed);
        }

        out.push(Draw::new(op, x, y));
    } else {
        eprint!("{}:{}: malformed point: ", fname, line);
        json_context(j);
        eprint!("{}:{}: malformed point: ", fname, line);
        json_context(feature);
        std::process::exit(EXIT_JSON);
    }

    if t == GEOM_POLYGON {
        // Note that this is not using the correct meaning of closepath.
        //
        // We are using it here to close an entire Polygon, to distinguish
        // the Polygons within a MultiPolygon from each other.
        //
        // This will be undone in fix_polygon(), which needs to know which
        // rings come from which Polygons so that it can make the winding order
        // of the outer ring be the opposite of the order of the inner rings.

        out.push(Draw::new(VT_CLOSEPATH, 0, 0));
    }
}

// This is used to convert a JSON attribute value into a serial_val-style
// type and stringified value. All numeric values, even if they are integers,
// even integers that are too large to fit in a double but will still be
// stringified with their original precision, are recorded here as mvt_double.
pub fn stringify_value(
    value: Option<JsonRef<'_>>,
    reading: &str,
    line: i32,
    feature: JsonRef<'_>,
) -> SerialVal {
    let mut sv = SerialVal::default();

    if let Some(value) = value {
        let vt = value.type_();

        if vt == JsonType::String {
            sv.type_ = MVT_STRING;
            sv.s = value.string().to_string();

            let err = check_utf8_raw(value.raw_string_bytes());
            if !err.is_empty() {
                // the message quotes the string with its invalid bytes
                let mut msg = format!("{}:{}: ", reading, line).into_bytes();
                msg.extend_from_slice(&err);
                msg.extend_from_slice(b": ");
                let _ = std::io::stderr().write_all(&msg);
                json_context(feature);
                std::process::exit(EXIT_UTF8);
            }
        } else if vt == JsonType::Number {
            sv.type_ = MVT_DOUBLE;

            if value.large_unsigned() != 0 {
                sv.s = value.large_unsigned().to_string();
            } else if value.large_signed() != 0 {
                sv.s = value.large_signed().to_string();
            } else {
                sv.s = crate::milo::dtoa_milo(value.number());
            }
        } else if vt == JsonType::True {
            sv.type_ = MVT_BOOL;
            sv.s = "true".to_string();
        } else if vt == JsonType::False {
            sv.type_ = MVT_BOOL;
            sv.s = "false".to_string();
        } else if vt == JsonType::Null {
            sv.type_ = MVT_NULL;
            sv.s = "null".to_string();
        } else {
            sv.type_ = MVT_STRING;
            sv.s = value.stringify();
        }
    }

    sv
}

// XXX deduplicate
fn to_feature(geom: &DrawVec) -> Vec<MvtGeometry> {
    let mut out = Vec::with_capacity(geom.len());

    for g in geom {
        out.push(MvtGeometry::new(g.op as i32, g.x, g.y));
    }

    out
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
    let Some(geometry_type) = geometry.hash_get("type") else {
        eprint!(
            "Filter output:{}: null geometry (additional not reported): ",
            jp.line
        );
        json_context(j);
        std::process::exit(EXIT_JSON);
    };

    if geometry_type.type_() != JsonType::String {
        eprint!("Filter output:{}: geometry type is not a string: ", jp.line);
        json_context(j);
        std::process::exit(EXIT_JSON);
    }

    let coordinates = match geometry.hash_get("coordinates") {
        Some(c) if c.type_() == JsonType::Array => c,
        _ => {
            eprint!(
                "Filter output:{}: geometry without coordinates array: ",
                jp.line
            );
            json_context(j);
            std::process::exit(EXIT_JSON);
        }
    };

    let mut t: i32 = 0;
    while (t as usize) < GEOM_TYPES {
        if geometry_type.string() == GEOMETRY_NAMES[t as usize] {
            break;
        }
        t += 1;
    }
    if t as usize >= GEOM_TYPES {
        eprint!(
            "Filter output:{}: Can't handle geometry type {}: ",
            jp.line,
            geometry_type.string()
        );
        json_context(j);
        std::process::exit(EXIT_JSON);
    }

    let mut dv = DrawVec::new();
    parse_coordinates(
        t,
        coordinates,
        &mut dv,
        VT_MOVETO,
        "Filter output",
        jp.line,
        j,
    );

    // handle longitude wraparound
    //
    // this is supposed to be data for a single tile,
    // so any jump from the left hand side edge of the world
    // to the right edge, or vice versa, is unexpected,
    // so move it to the other side.

    if fix_longitudes && MB_GEOMETRY[t as usize] == VT_POLYGON {
        const QUARTER_WORLD: i64 = 1i64 << 30;
        const WORLD: i64 = 1i64 << 32;

        let mut copy_to_left = false;
        let mut copy_to_right = false;

        for i in 0..dv.len() {
            // is this vertex on a different side of the world
            // than the first vertex? then shift this one to match
            if i > 0 {
                if (dv[0].x < QUARTER_WORLD) && (dv[i].x > 3 * QUARTER_WORLD) {
                    dv[i].x -= WORLD;
                }
                if (dv[0].x > 3 * QUARTER_WORLD) && (dv[i].x < QUARTER_WORLD) {
                    dv[i].x += WORLD;
                }
            }

            // does it stick off the edge of the world?
            // then we need another copy on the other side of the world
            if dv[i].x < 0 {
                copy_to_right = true;
            }
            if dv[i].x > WORLD {
                copy_to_left = true;
            }
        }

        if copy_to_left {
            let n = dv.len();
            for i in 0..n {
                let d = dv[i];
                dv.push(Draw::new(d.op, d.x - WORLD, d.y));
            }
        }
        if copy_to_right {
            let n = dv.len();
            for i in 0..n {
                let d = dv[i];
                dv.push(Draw::new(d.op, d.x + WORLD, d.y));
            }
        }
    }

    if MB_GEOMETRY[t as usize] == VT_POLYGON {
        dv = fix_polygon(&dv, false, false);
    }

    // Offset and scale geometry from global to tile
    for d in dv.iter_mut() {
        let scale: i64 = 1i64 << (32 - z);

        // offset to tile
        d.x -= scale * x as i64;
        d.y -= scale * y as i64;

        // scale to tile
        d.x = (d.x as f64 * (extent as f64 / scale as f64)).round() as i64;
        d.y = (d.y as f64 * (extent as f64 / scale as f64)).round() as i64;
    }

    if MB_GEOMETRY[t as usize] == VT_POLYGON {
        // don't try scaling up because we may have coordinates
        // on the other side of the world
        dv = clean_or_clip_poly(&mut dv, z, 256, true, false);
        if dv.len() < 3 {
            dv.clear();
        }
    }
    dv = remove_noop(dv, MB_GEOMETRY[t as usize], 0);

    if mvt_style && MB_GEOMETRY[t as usize] == VT_POLYGON {
        dv = close_poly(&mut dv);
    }

    (t, dv)
}

pub fn parse_layers(
    fp: Box<dyn Read + '_>,
    z: i32,
    x: u32,
    y: u32,
    extent: i32,
    fix_longitudes: bool,
) -> Vec<MvtLayer> {
    let mut ret: BTreeMap<String, MvtLayer> = BTreeMap::new();

    let mut jp = JsonPull::begin_file(fp);
    loop {
        let Some(jid) = jp.read() else {
            if let Some(error) = jp.error {
                eprint!("Filter output:{}: {}: ", jp.line, error);
                if let Some(root) = jp.root() {
                    json_context(root);
                } else {
                    eprintln!();
                }
                std::process::exit(EXIT_JSON);
            }

            jp.clear_root();
            break;
        };

        {
            let j = jp.get(jid);

            // Only complete Features are freed; see plugin.cpp::parse_feature.
            let Some(type_) = j.hash_get("type") else {
                continue;
            };
            if type_.type_() != JsonType::String {
                continue;
            }
            if type_.string() != "Feature" {
                continue;
            }

            let properties = match j.hash_get("properties") {
                Some(p) if p.type_() == JsonType::Hash || p.type_() == JsonType::Null => p,
                _ => {
                    eprint!(
                        "Filter output:{}: feature without properties hash: ",
                        jp.line
                    );
                    json_context(j);
                    std::process::exit(EXIT_JSON);
                }
            };

            let mut layername = "unknown".to_string();
            if let Some(tippecanoe) = j.hash_get("tippecanoe")
                && let Some(layer) = tippecanoe.hash_get("layer")
                && layer.type_() == JsonType::String
            {
                layername = layer.string().to_string();
            }

            let l = ret.entry(layername.clone()).or_insert_with(|| MvtLayer {
                name: layername,
                version: 2,
                extent: extent as i64,
                ..Default::default()
            });

            let Some(geometry) = j.hash_get("geometry") else {
                eprint!(
                    "Filter output:{}: filtered feature with no geometry: ",
                    jp.line
                );
                json_context(j);
                std::process::exit(EXIT_JSON);
            };

            let parsed_geometry = parse_geometry(
                geometry,
                &jp,
                j,
                z,
                x as i32,
                y as i32,
                extent as i64,
                fix_longitudes,
                true,
            );

            let t = parsed_geometry.0;
            let dv = &parsed_geometry.1;

            if !dv.is_empty() {
                let mut feature = MvtFeature {
                    type_: MB_GEOMETRY[t as usize],
                    geometry: to_feature(dv),
                    ..Default::default()
                };

                if let Some(id) = j.hash_get("id")
                    && id.type_() == JsonType::Number
                {
                    feature.id = id.number() as u64;
                    if id.large_unsigned() > 0 {
                        feature.id = id.large_unsigned();
                    }
                    feature.has_id = true;
                }

                if properties.type_() == JsonType::Hash {
                    for (key, value) in properties.entries() {
                        let sv = stringify_value(Some(value), "Filter output", jp.line, j);

                        // Nulls can be excluded here because this is the postfilter
                        // and it is nearly time to create the vector representation

                        if sv.type_ != MVT_NULL {
                            let v = stringified_to_mvt_value(sv.type_, &sv.s);
                            l.tag(&mut feature, key.string(), &v);
                        }
                    }
                }

                l.features.push(feature);
            }
        }

        jp.free(jid);
    }

    ret.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::MvtTile;

    fn encoded(json: &str, z: i32, x: u32, y: u32, extent: i32, fix_longitudes: bool) -> String {
        let layers = parse_layers(Box::new(json.as_bytes()), z, x, y, extent, fix_longitudes);
        let tile = MvtTile { layers };
        tile.encode().iter().map(|c| format!("{:02x}", c)).collect()
    }

    /// The expected encodings are what the C++ parse_layers gives for the
    /// same input, encoded as a tile.
    #[test]
    fn parse_layers_matches_the_cpp() {
        let attrs = r#"{"type":"Feature","properties":{"s":"x","n":1.5,"i":-3,"b":true,"z":null,"o":{"k":[1,2]},"big":18446744073709551615},"id":12,"tippecanoe":{"layer":"L"},"geometry":{"type":"MultiPoint","coordinates":[[1,2],[3,4]]}}"#;
        assert_eq!(
            encoded(attrs, 0, 0, 0, 4096, true),
            "1a6978020a014c2880201a01731a016e1a01691a01621a016f1a0362696722030a0178220d0a0b7b226b223a5b312c325d7d2205150000c03f220b28ffffffffffffffffff012202300522023801121b1801120c000001020204030504010503080c2207119620d21f2e2d"
        );
        assert_eq!(
            encoded(attrs, 3, 4, 3, 256, false),
            "1a6878020a014c2880021a01731a016e1a01691a01621a016f1a0362696722030a0178220d0a0b7b226b223a5b312c325d7d2205150000c03f220b28ffffffffffffffffff012202300522023801121a1801120c000001020204030504010503080c2206110cea031617"
        );

        let hole = r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,4],[4,4],[4,2],[2,2]]]}}"#;
        assert_eq!(
            encoded(hole, 0, 0, 0, 4096, true),
            "1a3078020a07756e6b6e6f776e28802012201803221c09e4219c1e1a00e401e3010000e3010f092eb6011a2e00002d2d000f"
        );

        let dateline = r#"{"type":"Feature","properties":{},"geometry":{"type":"MultiPolygon","coordinates":[[[[170,0],[-170,0],[-170,10],[170,10],[170,0]]]]}}"#;
        assert_eq!(
            encoded(dateline, 0, 0, 0, 4096, true),
            "1a3378020a07756e6b6e6f776e28802012231803221f09e4419c1e1a00e401c7030000e3010f09b73c001a00e401c7030000e3010f"
        );
    }

    #[test]
    fn stringify_values() {
        let mut jp = JsonPull::begin_string(br#"[ "s", 1.5, 18446744073709551615, -9223372036854775808, true, false, null, {"a": [1]} ]"#);
        let tree = jp.read_tree().unwrap();
        let root = tree.root();
        let got: Vec<(i32, String)> = root
            .array()
            .into_iter()
            .map(|v| {
                let sv = stringify_value(Some(v), "test", 1, root);
                (sv.type_, sv.s)
            })
            .collect();
        let want = [
            (MVT_STRING, "s"),
            (MVT_DOUBLE, "1.5"),
            (MVT_DOUBLE, "18446744073709551615"),
            (MVT_DOUBLE, "-9223372036854775808"),
            (MVT_BOOL, "true"),
            (MVT_BOOL, "false"),
            (MVT_NULL, "null"),
            (MVT_STRING, "{\"a\":[1]}"),
        ];
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want.iter()) {
            assert_eq!((g.0, g.1.as_str()), *w);
        }

        let sv = stringify_value(None, "test", 1, root);
        assert_eq!((sv.type_, sv.s.as_str()), (MVT_STRING, ""));
    }
}
