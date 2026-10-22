//! Port of geojson-loop.cpp, which walks a stream of JSON and hands each GeoJSON
//! feature or bare geometry to a `JsonFeatureAction`.

use std::io::Write;

use crate::jsonpull::{JsonPull, JsonRef, JsonType};

/// `json_feature_action`. `line` is the parser's line number when the feature
/// was completed, which the C++ reads through the node's `parser` pointer.
pub trait JsonFeatureAction {
    fn fname(&self) -> &str;
    #[allow(clippy::too_many_arguments)]
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

// XXX duplicated
const GEOM_TYPES: usize = 6;
static GEOMETRY_NAMES: [&str; GEOM_TYPES] = [
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Polygon",
    "MultiPolygon",
];

// XXX duplicated
fn json_context(j: JsonRef<'_>) {
    let mut s = j.stringify_raw();

    if s.len() >= 500 {
        s.truncate(497);
        s.extend_from_slice(b"...");
    }

    // The bytes as they are, like fprintf, even if not valid UTF-8
    let mut out = b"in JSON object ".to_vec();
    out.extend_from_slice(&s);
    out.push(b'\n');
    let _ = std::io::stderr().write_all(&out);
}

/// Whether some ancestor of `j`, or `j` itself, is the value of a "properties" key.
fn inside_properties(j: JsonRef<'_>) -> bool {
    let mut jo = Some(j);
    while let Some(o) = jo {
        if let Some(parent) = o.parent()
            && parent.type_() == JsonType::Hash
            && parent.hash_get("properties") == Some(o)
        {
            // Ancestor is the value corresponding to a properties key
            return true;
        }
        jo = o.parent();
    }
    false
}

pub fn parse_json(jfa: &mut dyn JsonFeatureAction, jp: &mut JsonPull<'_>) {
    let mut found_hashes: i64 = 0;
    let mut found_features: i64 = 0;
    let mut found_geometries: i64 = 0;

    loop {
        let jid = match jp.read() {
            Some(j) => j,
            None => {
                if let Some(error) = jp.error {
                    eprint!("{}:{}: {}: ", jfa.fname(), jp.line, error);
                    match jp.root() {
                        Some(root) => json_context(root),
                        None => eprintln!(),
                    }
                }

                jp.clear_root();
                break;
            }
        };
        let j = jp.get(jid);

        if j.type_() == JsonType::Hash {
            found_hashes += 1;

            if found_hashes == 50 && found_features == 0 && found_geometries == 0 {
                eprintln!(
                    "{}:{}: Warning: not finding any GeoJSON features or geometries in input yet after 50 objects.",
                    jfa.fname(),
                    jp.line
                );
            }
        }

        let type_ = match j.hash_get("type") {
            Some(t) if t.type_() == JsonType::String => t,
            _ => continue,
        };

        if found_features == 0 {
            let mut is_geometry = false;
            for name in GEOMETRY_NAMES.iter() {
                if type_.raw_string_bytes() == name.as_bytes() {
                    is_geometry = true;
                    break;
                }
            }

            if is_geometry && let Some(parent) = j.parent() {
                if parent.type_() == JsonType::Array && parent.parent().is_some() {
                    let grandparent = parent.parent().unwrap();
                    if grandparent.type_() == JsonType::Hash {
                        let geometries = grandparent.hash_get("geometries");
                        if geometries.is_some() {
                            // Parent of Parent must be a GeometryCollection
                            is_geometry = false;
                        }
                    }
                } else if parent.type_() == JsonType::Hash {
                    let geometry = parent.hash_get("geometry");
                    if geometry.is_some() {
                        // Parent must be a Feature
                        is_geometry = false;
                    }
                }
            }

            if is_geometry && inside_properties(j) {
                is_geometry = false;
            }

            if is_geometry {
                if found_features != 0 && found_geometries == 0 {
                    eprintln!(
                        "{}:{}: Warning: found a mixture of features and bare geometries",
                        jfa.fname(),
                        jp.line
                    );
                }
                found_geometries += 1;

                jfa.add_feature(jp.line, j, false, None, None, None, j);
                jp.free(jid);
                continue;
            }
        }

        if type_.raw_string_bytes() != b"Feature" {
            if type_.raw_string_bytes() == b"FeatureCollection" {
                jfa.check_crs(j);
                jp.free(jid);
            }

            continue;
        }

        if found_features == 0 && found_geometries != 0 {
            eprintln!(
                "{}:{}: Warning: found a mixture of features and bare geometries",
                jfa.fname(),
                jp.line
            );
        }
        found_features += 1;

        let geometry = match j.hash_get("geometry") {
            Some(g) => g,
            None => {
                eprint!("{}:{}: feature with no geometry: ", jfa.fname(), jp.line);
                json_context(j);
                jp.free(jid);
                continue;
            }
        };

        let properties = j.hash_get("properties");
        if properties.is_none_or(|p| p.type_() != JsonType::Hash && p.type_() != JsonType::Null) {
            eprint!(
                "{}:{}: feature without properties hash: ",
                jfa.fname(),
                jp.line
            );
            json_context(j);
            jp.free(jid);
            continue;
        }

        let is_feature = !inside_properties(j);
        if !is_feature {
            continue;
        }

        let tippecanoe = j.hash_get("tippecanoe");
        let id = j.hash_get("id");

        let geometries = geometry.hash_get("geometries");
        match geometries {
            Some(geometries) if geometries.type_() == JsonType::Array => {
                jfa.add_feature(jp.line, geometries, true, properties, id, tippecanoe, j);
            }
            _ => {
                jfa.add_feature(jp.line, geometry, false, properties, id, tippecanoe, j);
            }
        }

        jp.free(jid);

        /* XXX check for any non-features in the outer object */
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Collect {
        calls: Vec<String>,
    }

    impl JsonFeatureAction for Collect {
        fn fname(&self) -> &str {
            "test"
        }

        fn add_feature(
            &mut self,
            line: i32,
            geometry: JsonRef<'_>,
            geometrycollection: bool,
            properties: Option<JsonRef<'_>>,
            id: Option<JsonRef<'_>>,
            tippecanoe: Option<JsonRef<'_>>,
            feature: JsonRef<'_>,
        ) -> i32 {
            self.calls.push(format!(
                "{} {} {} {} {} {} {}",
                line,
                geometry.stringify(),
                geometrycollection,
                properties.map(|p| p.stringify()).unwrap_or_default(),
                id.map(|p| p.stringify()).unwrap_or_default(),
                tippecanoe.map(|p| p.stringify()).unwrap_or_default(),
                feature == geometry
            ));
            1
        }

        fn check_crs(&mut self, j: JsonRef<'_>) {
            self.calls.push(format!(
                "crs {}",
                j.hash_get("crs").map(|c| c.stringify()).unwrap_or_default()
            ));
        }
    }

    #[test]
    fn features_and_geometries() {
        let input = br#"{"type": "FeatureCollection", "crs": {"type": "name"}, "features": [
{"type": "Feature", "id": 7, "properties": {"a": {"type": "Point", "coordinates": [0, 0]}}, "geometry": {"type": "Point", "coordinates": [1, 2]}},
{"type": "Feature", "properties": null, "tippecanoe": {"minzoom": 3}, "geometry": {"type": "GeometryCollection", "geometries": [{"type": "Point", "coordinates": [3, 4]}]}}
]}
"#;
        let mut jp = JsonPull::begin_string(input);
        let mut c = Collect { calls: Vec::new() };
        parse_json(&mut c, &mut jp);
        assert_eq!(
            c.calls,
            vec![
                "2 {\"type\":\"Point\",\"coordinates\":[1,2]} false {\"a\":{\"type\":\"Point\",\"coordinates\":[0,0]}} 7  false",
                "3 [{\"type\":\"Point\",\"coordinates\":[3,4]}] true null  {\"minzoom\":3} false",
                "crs {\"type\":\"name\"}",
            ]
        );

        let mut jp = JsonPull::begin_string(b"{\"type\": \"Point\", \"coordinates\": [5, 6]}\n[{\"type\": \"LineString\", \"coordinates\": []}]");
        let mut c = Collect { calls: Vec::new() };
        parse_json(&mut c, &mut jp);
        assert_eq!(
            c.calls,
            vec![
                "1 {\"type\":\"Point\",\"coordinates\":[5,6]} false    true",
                "2 {\"type\":\"LineString\",\"coordinates\":[]} false    true",
            ]
        );
    }
}
