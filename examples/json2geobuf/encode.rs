//! A port of geobuf 3.0.2 `encode.js`.
//!
//! The module variables of encode.js live in `Encoder`. Values keep their
//! JavaScript types, so malformed GeoJSON goes down the same paths as in
//! JavaScript. Missing coordinates turn into NaN and then 0, and reading a
//! property of null or undefined throws.

use std::collections::HashMap;

use crate::js::{
    JsString, Value, for_in, get, get_index, is_str, json_stringify, math_max, math_round, throw,
    to_number, to_string, to_uint32, truthy, type_of,
};
use crate::pbf::Pbf;

const MAX_PRECISION: f64 = 1e6;

const GEOMETRY_TYPES: [&str; 7] = [
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Polygon",
    "MultiPolygon",
    "GeometryCollection",
];

/// The properties of Object.prototype in Node. `keys` is a plain object, so
/// `keys[key]` finds these even before `saveKey` has stored anything.
const OBJECT_PROTOTYPE: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

struct Encoder {
    keys: HashMap<JsString, f64>,
    keys_num: f64,
    keys_arr: Vec<JsString>,
    dim: f64,
    e: f64,
}

fn is_named(key: &[u16], names: &[&str]) -> bool {
    names.iter().any(|name| {
        key.len() == name.len() && key.iter().zip(name.bytes()).all(|(&a, b)| a == b as u16)
    })
}

/// `geometryTypes[type]` as writeVarint converts it to a number. An unknown
/// type (or one that names a property of Object.prototype) is NaN.
fn geometry_type(ty: &Value) -> f64 {
    let key = to_string(ty);
    for (i, name) in GEOMETRY_TYPES.iter().enumerate() {
        if is_named(&key, &[name]) {
            return i as f64;
        }
    }
    f64::NAN
}

/// `x.length` as a number, as the loops and arithmetic of encode.js use it.
fn length(v: &Value) -> f64 {
    to_number(&get(v, "length"))
}

pub fn encode(obj: &Value, mut pbf: Pbf) -> Vec<u8> {
    let mut enc = Encoder {
        keys: HashMap::new(),
        keys_arr: Vec::new(),
        keys_num: 0.0,
        dim: 0.0,
        e: 1.0,
    };

    enc.analyze(obj);

    enc.e = enc.e.min(MAX_PRECISION);
    let precision = (enc.e.ln() / std::f64::consts::LN_10).ceil();

    for key in &enc.keys_arr {
        pbf.write_string_field(1, key);
    }
    if enc.dim != 2.0 {
        pbf.write_varint_field(2, enc.dim);
    }
    if precision != 6.0 {
        pbf.write_varint_field(3, precision);
    }

    let ty = get(obj, "type");
    if is_str(&ty, "FeatureCollection") {
        pbf.write_message(4, |pbf| enc.write_feature_collection(obj, pbf));
    } else if is_str(&ty, "Feature") {
        pbf.write_message(5, |pbf| enc.write_feature(obj, pbf));
    } else {
        pbf.write_message(6, |pbf| enc.write_geometry(obj, pbf));
    }

    pbf.finish()
}

impl Encoder {
    fn analyze(&mut self, obj: &Value) {
        let ty = get(obj, "type");

        if is_str(&ty, "FeatureCollection") {
            let features = get(obj, "features");
            let mut i = 0;
            while (i as f64) < length(&features) {
                self.analyze(&get_index(&features, i));
                i += 1;
            }
        } else if is_str(&ty, "Feature") {
            let geometry = get(obj, "geometry");
            if !matches!(*geometry, Value::Null) {
                self.analyze(&geometry);
            }
            for (key, _) in for_in(&get(obj, "properties")) {
                self.save_key(&key);
            }
        } else if is_str(&ty, "Point") {
            self.analyze_point(&get(obj, "coordinates"));
        } else if is_str(&ty, "MultiPoint") {
            self.analyze_points(&get(obj, "coordinates"));
        } else if is_str(&ty, "GeometryCollection") {
            let geometries = get(obj, "geometries");
            let mut i = 0;
            while (i as f64) < length(&geometries) {
                self.analyze(&get_index(&geometries, i));
                i += 1;
            }
        } else if is_str(&ty, "LineString") {
            self.analyze_points(&get(obj, "coordinates"));
        } else if is_str(&ty, "Polygon") || is_str(&ty, "MultiLineString") {
            self.analyze_multi_line(&get(obj, "coordinates"));
        } else if is_str(&ty, "MultiPolygon") {
            let coordinates = get(obj, "coordinates");
            let mut i = 0;
            while (i as f64) < length(&coordinates) {
                self.analyze_multi_line(&get_index(&coordinates, i));
                i += 1;
            }
        }

        for (key, _) in for_in(obj) {
            if !is_special_key(&key, &ty) {
                self.save_key(&key);
            }
        }
    }

    fn analyze_multi_line(&mut self, coords: &Value) {
        let mut i = 0;
        while (i as f64) < length(coords) {
            self.analyze_points(&get_index(coords, i));
            i += 1;
        }
    }

    fn analyze_points(&mut self, coords: &Value) {
        let mut i = 0;
        while (i as f64) < length(coords) {
            self.analyze_point(&get_index(coords, i));
            i += 1;
        }
    }

    fn analyze_point(&mut self, point: &Value) {
        self.dim = math_max(self.dim, length(point));

        // find max precision
        let mut i = 0;
        while (i as f64) < length(point) {
            let coord = get_index(point, i);
            // Math.round(point[i] * e) / e !== point[i], which is always true
            // for a coordinate that is not a number
            let differs = |e: f64| {
                let rounded = math_round(to_number(&coord) * e) / e;
                !matches!(*coord, Value::Number(n) if rounded == n)
            };
            while differs(self.e) && self.e < MAX_PRECISION {
                self.e *= 10.0;
            }
            i += 1;
        }
    }

    /// `keys[key]`, which is undefined for a key not saved yet.
    fn lookup_key(&self, key: &[u16]) -> Option<f64> {
        if is_named(key, &OBJECT_PROTOTYPE) {
            // a function, or Object.prototype itself, which become NaN as numbers
            return Some(f64::NAN);
        }
        self.keys.get(key).copied()
    }

    fn save_key(&mut self, key: &[u16]) {
        if self.lookup_key(key).is_none() {
            self.keys_arr.push(key.into());
            self.keys.insert(key.into(), self.keys_num);
            self.keys_num += 1.0;
        }
    }

    fn write_feature_collection(&self, obj: &Value, pbf: &mut Pbf) {
        let features = get(obj, "features");
        let mut i = 0;
        while (i as f64) < length(&features) {
            pbf.write_message(1, |pbf| self.write_feature(&get_index(&features, i), pbf));
            i += 1;
        }
        self.write_props(obj, pbf, true);
    }

    fn write_feature(&self, feature: &Value, pbf: &mut Pbf) {
        let geometry = get(feature, "geometry");
        if !matches!(*geometry, Value::Null) {
            pbf.write_message(1, |pbf| self.write_geometry(&geometry, pbf));
        }

        let id = get(feature, "id");
        if !matches!(*id, Value::Undefined) {
            match *id {
                Value::Number(n) if n % 1.0 == 0.0 => pbf.write_svarint_field(12, n),
                _ => pbf.write_string_field(11, &to_string(&id)),
            }
        }

        let properties = get(feature, "properties");
        if truthy(&properties) {
            self.write_props(&properties, pbf, false);
        }
        self.write_props(feature, pbf, true);
    }

    fn write_geometry(&self, geom: &Value, pbf: &mut Pbf) {
        let ty = get(geom, "type");
        pbf.write_varint_field(1, geometry_type(&ty));

        let coords = get(geom, "coordinates");

        if is_str(&ty, "Point") {
            self.write_point(&coords, pbf);
        } else if is_str(&ty, "MultiPoint") {
            self.write_line(&coords, pbf);
        } else if is_str(&ty, "LineString") {
            self.write_line(&coords, pbf);
        } else if is_str(&ty, "MultiLineString") {
            self.write_multi_line(&coords, pbf, false);
        } else if is_str(&ty, "Polygon") {
            self.write_multi_line(&coords, pbf, true);
        } else if is_str(&ty, "MultiPolygon") {
            self.write_multi_polygon(&coords, pbf);
        } else if is_str(&ty, "GeometryCollection") {
            let geometries = get(geom, "geometries");
            let mut i = 0;
            while (i as f64) < length(&geometries) {
                pbf.write_message(4, |pbf| {
                    self.write_geometry(&get_index(&geometries, i), pbf)
                });
                i += 1;
            }
        }

        self.write_props(geom, pbf, true);
    }

    fn write_props(&self, props: &Value, pbf: &mut Pbf, is_custom: bool) {
        let mut indexes = Vec::new();
        let mut value_index = 0.0;

        for (key, value) in for_in(props) {
            if is_custom && is_special_key(&key, &get(props, "type")) {
                continue;
            }
            pbf.write_message(13, |pbf| write_value(&value, pbf));
            indexes.push(self.lookup_key(&key).unwrap_or(f64::NAN));
            indexes.push(value_index);
            value_index += 1.0;
        }
        pbf.write_packed_varint(if is_custom { 15 } else { 14 }, &indexes);
    }

    fn write_point(&self, point: &Value, pbf: &mut Pbf) {
        let mut coords = Vec::new();
        let mut i = 0;
        while (i as f64) < self.dim {
            coords.push(math_round(to_number(&get_index(point, i)) * self.e));
            i += 1;
        }
        pbf.write_packed_svarint(3, &coords);
    }

    fn write_line(&self, line: &Value, pbf: &mut Pbf) {
        let mut coords = Vec::new();
        self.populate_line(&mut coords, line, false);
        pbf.write_packed_svarint(3, &coords);
    }

    fn write_multi_line(&self, lines: &Value, pbf: &mut Pbf, closed: bool) {
        let len = get(lines, "length");
        if !matches!(*len, Value::Number(n) if n == 1.0) {
            let mut lengths = Vec::new();
            let mut i = 0;
            while (i as f64) < to_number(&len) {
                lengths.push(length(&get_index(lines, i)) - if closed { 1.0 } else { 0.0 });
                i += 1;
            }
            pbf.write_packed_varint(2, &lengths);
        }
        let mut coords = Vec::new();
        let mut i = 0;
        while (i as f64) < to_number(&len) {
            self.populate_line(&mut coords, &get_index(lines, i), closed);
            i += 1;
        }
        pbf.write_packed_svarint(3, &coords);
    }

    fn write_multi_polygon(&self, polygons: &Value, pbf: &mut Pbf) {
        let len = get(polygons, "length");
        let len_is_one = matches!(*len, Value::Number(n) if n == 1.0);
        if !len_is_one
            || !matches!(*get(&get_index(polygons, 0), "length"), Value::Number(n) if n == 1.0)
        {
            let mut lengths = vec![to_number(&len)];
            let mut i = 0;
            while (i as f64) < to_number(&len) {
                let polygon = get_index(polygons, i);
                lengths.push(length(&polygon));
                let mut j = 0;
                while (j as f64) < length(&polygon) {
                    lengths.push(length(&get_index(&polygon, j)) - 1.0);
                    j += 1;
                }
                i += 1;
            }
            pbf.write_packed_varint(2, &lengths);
        }

        let mut coords = Vec::new();
        let mut i = 0;
        while (i as f64) < to_number(&len) {
            let polygon = get_index(polygons, i);
            let mut j = 0;
            while (j as f64) < length(&polygon) {
                self.populate_line(&mut coords, &get_index(&polygon, j), true);
                j += 1;
            }
            i += 1;
        }
        pbf.write_packed_svarint(3, &coords);
    }

    fn populate_line(&self, coords: &mut Vec<f64>, line: &Value, closed: bool) {
        let len = length(line) - if closed { 1.0 } else { 0.0 };
        // new Array(dim)
        if to_uint32(self.dim) as f64 != self.dim {
            throw("RangeError: Invalid array length");
        }
        let dim = self.dim as usize;
        let mut sum = vec![0.0; dim];
        let mut i = 0;
        while (i as f64) < len {
            let point = get_index(line, i);
            for (j, s) in sum.iter_mut().enumerate() {
                let n = math_round(to_number(&get_index(&point, j)) * self.e) - *s;
                coords.push(n);
                *s += n;
            }
            i += 1;
        }
    }
}

fn write_value(value: &Value, pbf: &mut Pbf) {
    if matches!(value, Value::Null) {
        return;
    }

    let ty = type_of(value);

    if let Value::String(s) = value {
        pbf.write_string_field(1, s);
    } else if let Value::Boolean(b) = value {
        pbf.write_boolean_field(5, *b);
    } else if ty == "object" {
        pbf.write_string_field(6, &json_stringify(value));
    } else if let Value::Number(n) = *value {
        if n % 1.0 != 0.0 {
            pbf.write_double_field(2, n);
        } else if n >= 0.0 {
            pbf.write_varint_field(3, n);
        } else {
            pbf.write_varint_field(4, -n);
        }
    }
}

fn is_special_key(key: &[u16], ty: &Value) -> bool {
    if is_named(key, &["type"]) {
        true
    } else if is_str(ty, "FeatureCollection") {
        is_named(key, &["features"])
    } else if is_str(ty, "Feature") {
        is_named(key, &["id", "properties", "geometry"])
    } else if is_str(ty, "GeometryCollection") {
        is_named(key, &["geometries"])
    } else {
        is_named(key, &["coordinates"])
    }
}
