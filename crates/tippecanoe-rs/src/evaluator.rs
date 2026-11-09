//! Port of evaluator.cpp, the feature filter expressions.

// The getters build their values the way the C++ does, field by field.
#![allow(clippy::field_reassign_with_default)]

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{EXIT_FILTER, EXIT_IMPOSSIBLE, EXIT_JSON, EXIT_OPEN};
use crate::jsonpull::{JsonPull, JsonRef, JsonTree, JsonType};
use crate::mvt::{
    MVT_BOOL, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINESTRING, MVT_NO_SUCH_KEY, MVT_NULL, MVT_POINT,
    MVT_POLYGON, MVT_SINT, MVT_STRING, MVT_UINT, MvtFeature, MvtLayer, MvtValue,
};

/// `strcmp`, on the parts of the strings before any NUL. Only the sign of the
/// result matters to the callers.
fn strcmp(a: &str, b: &str) -> i32 {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let a = &a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())];
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn compare(one: &MvtValue, two: JsonRef<'_>, fail: &mut bool) -> i32 {
    match one.type_ {
        MVT_STRING => {
            if two.type_() != JsonType::String {
                *fail = true;
                return false as i32; // string vs non-string
            }

            strcmp(one.get_str(), two.string())
        }

        MVT_DOUBLE | MVT_FLOAT | MVT_INT | MVT_UINT | MVT_SINT => {
            if two.type_() != JsonType::Number {
                *fail = true;
                return false as i32; // number vs non-number
            }

            let v: f64 = match one.type_ {
                MVT_DOUBLE => one.numeric_value.double_value(),
                MVT_FLOAT => one.numeric_value.float_value() as f64,
                MVT_INT => one.numeric_value.int_value() as f64,
                MVT_UINT => one.numeric_value.uint_value() as f64,
                MVT_SINT => one.numeric_value.sint_value() as f64,
                _ => {
                    eprintln!("Internal error: bad mvt type {}", one.type_);
                    std::process::exit(EXIT_IMPOSSIBLE);
                }
            };

            if v < two.number() {
                -1
            } else if v > two.number() {
                1
            } else {
                0
            }
        }

        MVT_BOOL => {
            if two.type_() != JsonType::True && two.type_() != JsonType::False {
                *fail = true;
                return false as i32; // bool vs non-bool
            }

            let b = two.type_() != JsonType::False;
            (one.numeric_value.bool_value() & !b) as i32
        }

        MVT_NULL => {
            if two.type_() != JsonType::Null {
                *fail = true;
                return false as i32; // null vs non-null
            }

            0 // null equals null
        }

        _ => {
            eprintln!("Internal error: bad mvt type {}", one.type_);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
}

/// `fprintf(stderr, "...%s\n", json_stringify(f).c_str())`, with the bytes of
/// the JSON as the C++ prints them.
fn json_message(prefix: &str, f: JsonRef<'_>) {
    let mut msg = prefix.as_bytes().to_vec();
    let json = f.stringify_raw();
    let n = json.iter().position(|&c| c == 0).unwrap_or(json.len());
    msg.extend_from_slice(&json[..n]);
    msg.push(b'\n');
    let _ = std::io::stderr().write_all(&msg);
}

static COMPARISON_NOT_FOUND_WARNED: AtomicBool = AtomicBool::new(false);
static COMPARISON_MISMATCH_WARNED: AtomicBool = AtomicBool::new(false);
static IN_NOT_FOUND_WARNED: AtomicBool = AtomicBool::new(false);
static IN_MISMATCH_WARNED: AtomicBool = AtomicBool::new(false);

/// Runs a C++ `static bool warned` check: true the first time only.
fn first_warning(warned: &AtomicBool) -> bool {
    !warned.swap(true, Ordering::Relaxed)
}

// 0: false
// 1: true
// -1: incomparable (sql null), treated as false in final output
#[allow(clippy::only_used_in_recursion)]
fn eval(
    feature: &dyn Fn(&str) -> MvtValue,
    f: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    unidecode_data: &[String],
) -> i32 {
    if f.type_() == JsonType::True {
        return 1;
    } else if f.type_() == JsonType::False {
        return 0;
    } else if f.type_() == JsonType::Null {
        return 1;
    }

    if f.type_() == JsonType::Number {
        if f.number() == 0.0 {
            return 0;
        } else {
            return 1;
        }
    }

    if f.type_() == JsonType::String {
        if f.string().is_empty() {
            return 0;
        } else {
            return 1;
        }
    }

    if f.type_() != JsonType::Array {
        json_message("Filter is not an array: ", f);
        std::process::exit(EXIT_FILTER);
    }

    let a = f.array();

    if a.is_empty() {
        json_message("Array too small in filter: ", f);
        std::process::exit(EXIT_FILTER);
    }

    if a[0].type_() != JsonType::String {
        json_message("Filter operation is not a string: ", f);
        std::process::exit(EXIT_FILTER);
    }

    let op = a[0].string();

    if op == "has" || op == "!has" {
        if a.len() != 2 {
            json_message("Wrong number of array elements in filter: ", f);
            std::process::exit(EXIT_FILTER);
        }

        if op == "has" {
            if a[1].type_() != JsonType::String {
                json_message("\"has\" key is not a string: ", f);
                std::process::exit(EXIT_FILTER);
            }
            return (feature(a[1].string()).type_ != MVT_NO_SUCH_KEY) as i32;
        }

        if op == "!has" {
            if a[1].type_() != JsonType::String {
                json_message("\"!has\" key is not a string: ", f);
                std::process::exit(EXIT_FILTER);
            }
            return (feature(a[1].string()).type_ == MVT_NO_SUCH_KEY) as i32;
        }
    }

    if op == "==" || op == "!=" || op == ">" || op == ">=" || op == "<" || op == "<=" {
        if a.len() != 3 {
            json_message("Wrong number of array elements in filter: ", f);
            std::process::exit(EXIT_FILTER);
        }
        if a[1].type_() != JsonType::String {
            json_message("comparison key is not a string: ", f);
            std::process::exit(EXIT_FILTER);
        }

        let ff = feature(a[1].string());
        if ff.type_ == MVT_NO_SUCH_KEY {
            if first_warning(&COMPARISON_NOT_FOUND_WARNED) {
                json_message("Warning: attribute not found for comparison: ", f);
            }
            if op == "!=" {
                return true as i32; //  attributes that aren't found are not equal
            }
            return false as i32; // not found: comparison is false
        }

        let mut fail = false;
        let cmp = compare(&ff, a[2], &mut fail);

        if fail {
            if first_warning(&COMPARISON_MISMATCH_WARNED) {
                json_message("Warning: mismatched type in comparison: ", f);
            }
            if op == "!=" {
                return true as i32; // mismatched types are not equal
            }
            return false as i32;
        }

        if op == "==" {
            return (cmp == 0) as i32;
        }
        if op == "!=" {
            return (cmp != 0) as i32;
        }
        if op == ">" {
            return (cmp > 0) as i32;
        }
        if op == ">=" {
            return (cmp >= 0) as i32;
        }
        if op == "<" {
            return (cmp < 0) as i32;
        }
        if op == "<=" {
            return (cmp <= 0) as i32;
        }

        json_message("Internal error: can't happen: ", f);
        std::process::exit(EXIT_IMPOSSIBLE);
    }

    if op == "all" || op == "any" || op == "none" {
        let mut v = op == "all";

        for elem in a.iter().skip(1) {
            let out = eval(feature, *elem, exclude_attributes, unidecode_data);

            if out >= 0 {
                // nulls are ignored in boolean and/or expressions
                if op == "all" {
                    v = v && out != 0;
                    if !v {
                        break;
                    }
                } else {
                    v = v || out != 0;
                    if v {
                        break;
                    }
                }
            }
        }

        if op == "none" {
            return (!v) as i32;
        } else {
            return v as i32;
        }
    }

    if op == "in" || op == "!in" {
        if a.len() < 2 {
            json_message("Array too small in filter: ", f);
            std::process::exit(EXIT_FILTER);
        }

        if a[1].type_() != JsonType::String {
            json_message("\"!in\" key is not a string: ", f);
            std::process::exit(EXIT_FILTER);
        }

        let ff = feature(a[1].string());
        if ff.type_ == MVT_NO_SUCH_KEY {
            if first_warning(&IN_NOT_FOUND_WARNED) {
                json_message("Warning: attribute not found for comparison: ", f);
            }
            if op == "!in" {
                return true as i32; // attributes that aren't found are not in
            }
            return false as i32; // not found: comparison is false
        }

        let mut found = false;
        for elem in a.iter().skip(2) {
            let mut fail = false;
            let mut cmp = compare(&ff, *elem, &mut fail);

            if fail {
                if first_warning(&IN_MISMATCH_WARNED) {
                    json_message("Warning: mismatched type in comparison: ", f);
                }
                cmp = 1;
            }

            if cmp == 0 {
                found = true;
                break;
            }
        }

        if op == "in" {
            return found as i32;
        } else {
            return (!found) as i32;
        }
    }

    if op == "attribute-filter" {
        if a.len() != 3 {
            json_message("Wrong number of array elements in filter: ", f);
            std::process::exit(EXIT_FILTER);
        }

        if a[1].type_() != JsonType::String {
            json_message("\"attribute-filter\" key is not a string: ", f);
            std::process::exit(EXIT_FILTER);
        }

        let ok = eval(feature, a[2], exclude_attributes, unidecode_data) > 0;
        if !ok {
            exclude_attributes.insert(a[1].string().to_string());
        }

        return true as i32;
    }

    json_message("Unknown filter ", f);
    std::process::exit(EXIT_FILTER);
}

fn evaluate_getter(
    feature: &dyn Fn(&str) -> MvtValue,
    layer: &str,
    filter: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    unidecode_data: &[String],
) -> bool {
    if filter.type_() != JsonType::Hash {
        json_message("Error: filter is not a hash: ", filter);
        std::process::exit(EXIT_JSON);
    }

    let mut ok = true;

    // the C++ looks the layer up by its C string
    let f = filter.hash_get(layer.split('\0').next().unwrap_or(""));
    if ok && let Some(f) = f {
        ok = eval(feature, f, exclude_attributes, unidecode_data) > 0;
    }

    let f = filter.hash_get("*");
    if ok && let Some(f) = f {
        ok = eval(feature, f, exclude_attributes, unidecode_data) > 0;
    }

    ok
}

pub fn read_filter(fname: &str) -> JsonTree {
    let fp = match std::fs::File::open(fname) {
        Ok(fp) => fp,
        Err(_) => {
            crate::cstd::perror(fname);
            std::process::exit(EXIT_OPEN);
        }
    };

    let mut jp = JsonPull::begin_file(Box::new(std::io::BufReader::new(fp)));
    let Some(filter) = jp.read_tree() else {
        eprintln!("{}: {}", fname, jp.error.unwrap_or("(null)"));
        std::process::exit(EXIT_JSON);
    };
    filter
}

pub fn parse_filter(s: &str) -> JsonTree {
    let mut jp = JsonPull::begin_string(s.as_bytes());
    let Some(filter) = jp.read_tree() else {
        eprintln!("Could not parse filter {}", s);
        eprintln!("{}", jp.error.unwrap_or("(null)"));
        std::process::exit(EXIT_JSON);
    };
    filter
}

/// The value a getter returns for an attribute that isn't there.
fn no_such_key() -> MvtValue {
    let mut v = MvtValue::default();
    v.type_ = MVT_NO_SUCH_KEY;
    v.numeric_value.set_null_value(0);
    v
}

/// `evaluate` over a map of attribute values.
pub fn evaluate(
    feature: &HashMap<String, MvtValue>,
    layer: &str,
    filter: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    unidecode_data: &[String],
) -> bool {
    let getter = |key: &str| -> MvtValue {
        match feature.get(key) {
            Some(f) => f.clone(),
            None => no_such_key(),
        }
    };

    evaluate_getter(&getter, layer, filter, exclude_attributes, unidecode_data)
}

/// `evaluate` over a feature of a decoded tile.
pub fn evaluate_feature(
    feat: &MvtFeature,
    layer: &MvtLayer,
    filter: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    z: i32,
    unidecode_data: &[String],
) -> bool {
    let getter = |key: &str| -> MvtValue {
        if key == "$id" && feat.has_id {
            let mut v = MvtValue::default();
            v.type_ = MVT_UINT;
            v.numeric_value.set_uint_value(feat.id);
            return v;
        }

        if key == "$type" {
            let mut v = MvtValue::default();
            v.type_ = MVT_STRING;

            if feat.type_ == MVT_POINT {
                v.set_string_value("Point");
            } else if feat.type_ == MVT_LINESTRING {
                v.set_string_value("LineString");
            } else if feat.type_ == MVT_POLYGON {
                v.set_string_value("Polygon");
            }
            return v;
        }

        if key == "$zoom" {
            let mut v2 = MvtValue::default();
            v2.type_ = MVT_UINT;
            v2.numeric_value.set_uint_value(z as u64);
            return v2;
        }

        let mut i = 0;
        while i + 1 < feat.tags.len() {
            if layer.keys[feat.tags[i] as usize] == key {
                return layer.values[feat.tags[i + 1] as usize].clone();
            }
            i += 2;
        }

        no_such_key()
    };

    evaluate_getter(
        &getter,
        &layer.name,
        filter,
        exclude_attributes,
        unidecode_data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::stringified_to_mvt_value;

    /// Results from the C++ for a feature with n=5, s="abc" and b=false in
    /// layer "layer", and the attributes each filter excluded.
    #[test]
    fn filters_match_the_cpp() {
        let mut attrs = HashMap::new();
        attrs.insert("n".to_string(), stringified_to_mvt_value(MVT_DOUBLE, "5"));
        attrs.insert("s".to_string(), stringified_to_mvt_value(MVT_STRING, "abc"));
        attrs.insert("b".to_string(), stringified_to_mvt_value(MVT_BOOL, "false"));

        let cases: &[(&str, bool, &[&str])] = &[
            (r#"{"layer": ["==", "n", 5]}"#, true, &[]),
            (r#"{"*": ["<", "s", "abd"]}"#, true, &[]),
            (r#"{"layer": [">", "b", true]}"#, false, &[]),
            (r#"{"layer": [">", "b", false]}"#, false, &[]),
            // false compares equal to true, as in the C++
            (r#"{"layer": ["==", "b", true]}"#, true, &[]),
            (r#"{"layer": ["in", "n", 1, 2, 5]}"#, true, &[]),
            (r#"{"layer": ["!in", "n", "x", 5]}"#, false, &[]),
            (r#"{"layer": ["has", "n"]}"#, true, &[]),
            (r#"{"layer": ["!has", "zz"]}"#, true, &[]),
            (
                r#"{"layer": ["any", ["==", "n", 4], ["==", "s", "abc"]]}"#,
                true,
                &[],
            ),
            (r#"{"layer": ["none", ["==", "n", 4]]}"#, true, &[]),
            (
                r#"{"layer": ["all", ["attribute-filter", "s", ["==", "n", 4]], true]}"#,
                true,
                &["s"],
            ),
            (r#"{"layer": 0}"#, false, &[]),
            (r#"{"layer": "x"}"#, true, &[]),
            (r#"{"layer": null}"#, true, &[]),
            (r#"{"other": false}"#, true, &[]),
        ];

        for &(filter, expected, excluded) in cases {
            let tree = parse_filter(filter);
            let mut exclude = BTreeSet::new();
            assert_eq!(
                evaluate(&attrs, "layer", tree.root(), &mut exclude, &[]),
                expected,
                "{filter}"
            );
            let excluded: BTreeSet<String> = excluded.iter().map(|s| s.to_string()).collect();
            assert_eq!(exclude, excluded, "{filter}");
        }
    }

    #[test]
    fn feature_getters() {
        let mut layer = MvtLayer {
            name: "l".to_string(),
            ..Default::default()
        };
        let mut feat = MvtFeature {
            type_: MVT_LINESTRING,
            id: 1234,
            has_id: true,
            ..Default::default()
        };
        layer.tag(&mut feat, "k", &stringified_to_mvt_value(MVT_STRING, "v"));

        let check = |filter: &str, z: i32| {
            let tree = parse_filter(filter);
            let mut exclude = BTreeSet::new();
            evaluate_feature(&feat, &layer, tree.root(), &mut exclude, z, &[])
        };
        assert!(check(r#"{"l": ["==", "$type", "LineString"]}"#, 0));
        assert!(check(r#"{"l": ["==", "$id", 1234]}"#, 0));
        assert!(check(r#"{"l": [">=", "$zoom", 3]}"#, 3));
        assert!(!check(r#"{"l": [">=", "$zoom", 3]}"#, 2));
        assert!(check(r#"{"*": ["==", "k", "v"]}"#, 0));
    }
}
