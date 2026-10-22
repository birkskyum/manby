//! Port of geojson.hpp and geojson.cpp: serializing the features of a GeoJSON
//! stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::cstd::strtoull;
use crate::geojson_loop::JsonFeatureAction;
use crate::geometry::{DrawVec, VT_MOVETO};
use crate::globals::{QUIET, additional, get};
use crate::jsonpull::{JsonPull, JsonRef, JsonType};
use crate::milo::dtoa_milo;
use crate::options::A_CONVERT_NUMERIC_IDS;
use crate::projection::projection;
use crate::read_json::{
    GEOM_TYPES, GEOMETRY_NAMES, MB_GEOMETRY, json_context, parse_coordinates, stringify_value,
};
use crate::serial::{KeyPool, SerialFeature, SerialVal, SerializationState, serialize_feature_sst};
use crate::text::integer_zoom;

/// `struct parse_json_args`, what each parsing thread works on.
pub struct ParseJsonArgs<'j, 's, 'a> {
    pub jp: JsonPull<'j>,
    pub layer: i32,
    pub layername: &'j str,

    pub sst: &'s mut SerializationState<'a>,
}

impl<'j, 's, 'a> ParseJsonArgs<'j, 's, 'a> {
    pub fn new(
        jp1: JsonPull<'j>,
        layer1: i32,
        layername1: &'j str,
        sst1: &'s mut SerializationState<'a>,
    ) -> ParseJsonArgs<'j, 's, 'a> {
        ParseJsonArgs {
            jp: jp1,
            layer: layer1,
            layername: layername1,
            sst: sst1,
        }
    }
}

/// The length of the C string that `s.c_str()` would give.
fn cstr_len(s: &str) -> usize {
    s.bytes().position(|c| c == 0).unwrap_or(s.len())
}

static WARNED_NULL_GEOMETRY: AtomicI32 = AtomicI32::new(0);
static WARNED_FRAC: AtomicBool = AtomicBool::new(false);
static WARNED_TOO_LARGE: AtomicBool = AtomicBool::new(false);
static WARNED_NEG: AtomicBool = AtomicBool::new(false);
static WARNED_STRING_FRAC: AtomicBool = AtomicBool::new(false);
static WARNED_STRING_TOO_LARGE: AtomicBool = AtomicBool::new(false);
static WARNED_NAN: AtomicBool = AtomicBool::new(false);

#[allow(clippy::too_many_arguments)]
pub fn serialize_geojson_feature(
    sst: &mut SerializationState<'_>,
    geometry: JsonRef<'_>,
    properties: Option<JsonRef<'_>>,
    id: Option<JsonRef<'_>>,
    layer: i32,
    tippecanoe: Option<JsonRef<'_>>,
    feature: JsonRef<'_>,
    layername: &str,
) -> i32 {
    let geometry_type = match geometry.hash_get("type") {
        Some(t) => t,
        None => {
            if WARNED_NULL_GEOMETRY.load(Ordering::Relaxed) == 0 {
                eprint!(
                    "{}:{}: null geometry (additional not reported): ",
                    sst.fname, sst.line
                );
                json_context(feature);
                WARNED_NULL_GEOMETRY.store(1, Ordering::Relaxed);
            }

            return 0;
        }
    };

    if geometry_type.type_() != JsonType::String {
        eprint!(
            "{}:{}: geometry type is not a string: ",
            sst.fname, sst.line
        );
        json_context(feature);
        return 0;
    }

    let coordinates = match geometry.hash_get("coordinates") {
        Some(c) if c.type_() == JsonType::Array => c,
        _ => {
            eprint!(
                "{}:{}: feature without coordinates array: ",
                sst.fname, sst.line
            );
            json_context(feature);
            return 0;
        }
    };

    let mut t = 0;
    while t < GEOM_TYPES {
        if geometry_type.string() == GEOMETRY_NAMES[t] {
            break;
        }
        t += 1;
    }
    if t >= GEOM_TYPES {
        eprint!(
            "{}:{}: Can't handle geometry type {}: ",
            sst.fname,
            sst.line,
            &geometry_type.string()[..cstr_len(geometry_type.string())]
        );
        json_context(feature);
        return 0;
    }

    let mut tippecanoe_minzoom = -1;
    let mut tippecanoe_maxzoom = -1;
    let mut tippecanoe_layername = layername.to_string();

    if let Some(tippecanoe) = tippecanoe {
        if let Some(min) = tippecanoe.hash_get("minzoom") {
            if min.type_() == JsonType::Number {
                tippecanoe_minzoom = integer_zoom(sst.fname, &dtoa_milo(min.number()));
            }
        }

        if let Some(max) = tippecanoe.hash_get("maxzoom") {
            if max.type_() == JsonType::Number {
                tippecanoe_maxzoom = integer_zoom(sst.fname, &dtoa_milo(max.number()));
            }
        }

        if let Some(ln) = tippecanoe.hash_get("layer") {
            if ln.type_() == JsonType::String {
                tippecanoe_layername = ln.string().to_string();
            }
        }
    }

    let mut has_id = false;
    let mut id_value: u64 = 0;
    if let Some(id) = id {
        if id.type_() == JsonType::Number {
            if id.number() >= 0.0 {
                let id_number = dtoa_milo(id.number());
                let (v, consumed) = strtoull(&id_number, 10);
                id_value = v;

                if id.large_unsigned() != 0 {
                    id_value = id.large_unsigned();
                }

                if consumed < cstr_len(&id_number) {
                    if !WARNED_FRAC.load(Ordering::Relaxed) {
                        eprintln!(
                            "Warning: Can't represent non-integer feature ID {}",
                            dtoa_milo(id.number())
                        );
                        WARNED_FRAC.store(true, Ordering::Relaxed);
                    }
                } else if id.large_unsigned() == 0 && id_value.to_string() != dtoa_milo(id.number())
                {
                    if !WARNED_TOO_LARGE.load(Ordering::Relaxed) {
                        eprintln!(
                            "Warning: Can't represent too-large feature ID {}",
                            dtoa_milo(id.number())
                        );
                        WARNED_TOO_LARGE.store(true, Ordering::Relaxed);
                    }
                } else {
                    has_id = true;
                }
            } else if !WARNED_NEG.load(Ordering::Relaxed) {
                eprintln!(
                    "Warning: Can't represent negative feature ID {}",
                    dtoa_milo(id.number())
                );
                WARNED_NEG.store(true, Ordering::Relaxed);
            }
        } else {
            let mut converted = false;

            if additional(A_CONVERT_NUMERIC_IDS) != 0 && id.type_() == JsonType::String {
                let s = id.string();
                let (v, consumed) = strtoull(s, 10);
                id_value = v;

                if consumed < cstr_len(s) {
                    if !WARNED_STRING_FRAC.load(Ordering::Relaxed) {
                        eprintln!(
                            "Warning: Can't represent non-integer feature ID {}",
                            &s[..cstr_len(s)]
                        );
                        WARNED_STRING_FRAC.store(true, Ordering::Relaxed);
                    }
                } else if id_value.to_string() != s {
                    if !WARNED_STRING_TOO_LARGE.load(Ordering::Relaxed) {
                        eprintln!(
                            "Warning: Can't represent too-large feature ID {}",
                            &s[..cstr_len(s)]
                        );
                        WARNED_STRING_TOO_LARGE.store(true, Ordering::Relaxed);
                    }
                } else {
                    has_id = true;
                    converted = true;
                }
            }

            if !converted && !WARNED_NAN.load(Ordering::Relaxed) {
                let s = id.stringify();
                eprintln!(
                    "Warning: Can't represent non-numeric feature ID {}",
                    &s[..cstr_len(&s)]
                );
                WARNED_NAN.store(true, Ordering::Relaxed);
            }
        }
    }

    let mut full_keys: Vec<Arc<String>> = Vec::new();
    let mut values: Vec<SerialVal> = Vec::new();
    let mut key_pool = KeyPool::default();

    if let Some(properties) = properties {
        if properties.type_() == JsonType::Hash {
            let entries = properties.entries();
            full_keys.reserve(entries.len());
            values.reserve(entries.len());

            for (key, value) in entries {
                if key.type_() == JsonType::String {
                    let sv = stringify_value(Some(value), sst.fname, sst.line, feature);

                    full_keys.push(key_pool.pool(key.string()));
                    values.push(sv);
                }
            }
        }
    }

    let mut dv = DrawVec::new();
    parse_coordinates(
        t as i32,
        coordinates,
        &mut dv,
        VT_MOVETO,
        sst.fname,
        sst.line,
        feature,
    );

    let mut sf = SerialFeature {
        layer: layer as i64,
        segment: sst.segment,
        t: MB_GEOMETRY[t] as i8,
        has_id,
        id: id_value,
        tippecanoe_minzoom,
        tippecanoe_maxzoom,
        geometry: dv,
        feature_minzoom: 0, // Will be filled in during index merging
        seq: sst.layer_seq,
        full_keys,
        full_values: values,
        ..Default::default()
    };

    serialize_feature_sst(sst, &mut sf, &tippecanoe_layername)
}

pub fn check_crs(j: JsonRef<'_>, reading: &str) {
    if let Some(crs) = j.hash_get("crs") {
        if let Some(properties) = crs.hash_get("properties") {
            if let Some(name) = properties.hash_get("name") {
                if name.type_() == JsonType::String && name.string() != projection().alias {
                    if get(&QUIET) == 0 {
                        let n = &name.string()[..cstr_len(name.string())];
                        eprintln!(
                            "{}: Warning: GeoJSON specified projection \"{}\", not the expected \"{}\".",
                            reading,
                            n,
                            projection().alias
                        );
                        eprintln!(
                            "{}: If \"{}\" is not the expected projection, use -s to specify the right one.",
                            reading,
                            projection().alias
                        );
                    }
                }
            }
        }
    }
}

struct JsonSerializeAction<'s, 'a> {
    fname: String,
    sst: &'s mut SerializationState<'a>,
    layer: i32,
    layername: String,
}

impl JsonFeatureAction for JsonSerializeAction<'_, '_> {
    fn fname(&self) -> &str {
        &self.fname
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
        self.sst.line = line;
        if geometrycollection {
            let mut ret = 1;
            for g in geometry.array() {
                ret &= serialize_geojson_feature(
                    self.sst,
                    g,
                    properties,
                    id,
                    self.layer,
                    tippecanoe,
                    feature,
                    &self.layername,
                );
            }
            ret
        } else {
            serialize_geojson_feature(
                self.sst,
                geometry,
                properties,
                id,
                self.layer,
                tippecanoe,
                feature,
                &self.layername,
            )
        }
    }

    fn check_crs(&mut self, j: JsonRef<'_>) {
        check_crs(j, &self.fname);
    }
}

pub fn parse_json(
    sst: &mut SerializationState<'_>,
    jp: &mut JsonPull<'_>,
    layer: i32,
    layername: &str,
) {
    let mut jsa = JsonSerializeAction {
        fname: sst.fname.to_string(),
        sst,
        layer,
        layername: layername.to_string(),
    };

    crate::geojson_loop::parse_json(&mut jsa, jp);
}

pub fn run_parse_json(pja: &mut ParseJsonArgs<'_, '_, '_>) {
    parse_json(pja.sst, &mut pja.jp, pja.layer, pja.layername);
}

/// `json_begin_map`: a parser that reads from a memory-mapped part of the input.
pub fn json_begin_map(map: &[u8]) -> JsonPull<'_> {
    JsonPull::begin_file(Box::new(map))
}

pub fn json_end_map(jp: JsonPull<'_>) {
    drop(jp);
}
