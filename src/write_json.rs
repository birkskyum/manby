//! Port of write_json.cpp.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{EXIT_IMPOSSIBLE, EXIT_JSON};
use crate::geometry::{VT_CLOSEPATH, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON};
use crate::mvt::{
    MVT_BOOL, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_NULL, MVT_SINT, MVT_STRING, MVT_UINT, MvtLayer,
};
use crate::serial::FEATURE_DROPPED;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonWriteTok {
    Hash,
    HashKey,
    HashValue,
    Array,
    ArrayElement,
    Top,
}

/// Where a `JsonWriter` sends its output: a `FILE *` or a `std::string *` in the C++.
pub enum JsonOut<'a> {
    File(&'a mut dyn Write),
    String(&'a mut String),
}

pub struct JsonWriter<'a> {
    pub state: Vec<JsonWriteTok>,
    pub nospace: bool,
    pub wantnl: bool,
    pub out: JsonOut<'a>,
}

/// `printf("%f", d)`. Rust spells NaN differently from libc.
fn printf_f(d: f64) -> String {
    if d.is_nan() {
        "nan".to_string()
    } else {
        format!("{:.6}", d)
    }
}

impl<'a> JsonWriter<'a> {
    pub fn new_file(f: &'a mut dyn Write) -> JsonWriter<'a> {
        JsonWriter {
            state: Vec::new(),
            nospace: false,
            wantnl: false,
            out: JsonOut::File(f),
        }
    }

    pub fn new_string(s: &'a mut String) -> JsonWriter<'a> {
        JsonWriter {
            state: Vec::new(),
            nospace: false,
            wantnl: false,
            out: JsonOut::String(s),
        }
    }

    fn json_adjust(&mut self) {
        if self.state.is_empty() {
            self.state.push(JsonWriteTok::Top);
            return;
        }

        let last = self.state.len() - 1;
        match self.state[last] {
            JsonWriteTok::Top => {
                self.addc(b'\n');
                self.state[last] = JsonWriteTok::Top;
            }
            JsonWriteTok::Hash => {
                if !self.nospace {
                    self.addc(b' ');
                }
                self.nospace = false;
                self.state[last] = JsonWriteTok::HashKey;
            }
            JsonWriteTok::HashKey => {
                self.adds(":");
                if !self.nospace {
                    self.addc(b' ');
                    self.nospace = false;
                }
                self.state[last] = JsonWriteTok::HashValue;
            }
            JsonWriteTok::HashValue => {
                if self.wantnl {
                    self.adds(",\n");
                    self.nospace = false;
                } else if self.nospace {
                    self.addc(b',');
                    self.nospace = false;
                } else {
                    self.adds(", ");
                }
                self.wantnl = false;
                self.state[last] = JsonWriteTok::HashKey;
            }
            JsonWriteTok::Array => {
                if !self.nospace {
                    self.addc(b' ');
                }
                self.nospace = false;
                self.state[last] = JsonWriteTok::ArrayElement;
            }
            JsonWriteTok::ArrayElement => {
                if self.wantnl {
                    self.adds(",\n");
                    self.nospace = false;
                } else if self.nospace {
                    self.addc(b',');
                    self.nospace = false;
                } else {
                    self.adds(", ");
                }
                self.wantnl = false;
                self.state[last] = JsonWriteTok::ArrayElement;
            }
        }
    }

    pub fn json_write_array(&mut self) {
        self.json_adjust();
        self.addc(b'[');

        self.state.push(JsonWriteTok::Array);
    }

    pub fn json_end_array(&mut self) {
        let Some(tok) = self.state.pop() else {
            eprintln!("End JSON array at top level");
            std::process::exit(EXIT_JSON);
        };

        if tok == JsonWriteTok::Array || tok == JsonWriteTok::ArrayElement {
            if !self.nospace {
                self.addc(b' ');
            }
            self.nospace = false;
            self.addc(b']');
        } else {
            eprintln!("End JSON array with unexpected state");
            std::process::exit(EXIT_JSON);
        }
    }

    pub fn json_write_hash(&mut self) {
        self.json_adjust();
        self.addc(b'{');

        self.state.push(JsonWriteTok::Hash);
    }

    pub fn json_end_hash(&mut self) {
        let Some(tok) = self.state.pop() else {
            eprintln!("End JSON hash at top level");
            std::process::exit(EXIT_JSON);
        };

        if tok == JsonWriteTok::Hash {
            if !self.nospace {
                self.adds("  "); // Preserve accidental extra space from before
            }
            self.nospace = false;
            self.addc(b'}');
        } else if tok == JsonWriteTok::HashValue {
            if !self.nospace {
                self.addc(b' ');
            }
            self.nospace = false;
            self.addc(b'}');
        } else {
            eprintln!("End JSON hash with unexpected state");
            std::process::exit(EXIT_JSON);
        }
    }

    pub fn json_write_string(&mut self, s: &str) {
        self.json_adjust();

        // The C++ adds a byte at a time. Bytes from 0x80 up are copied
        // unchanged, so this goes a character at a time instead.
        let mut buf = String::with_capacity(s.len() + 2);
        buf.push('"');
        for c in s.chars() {
            if c == '\\' || c == '"' {
                buf.push('\\');
                buf.push(c);
            } else if (c as u32) < 0x20 {
                buf.push_str(&format!("\\u{:04x}", c as u32));
            } else {
                buf.push(c);
            }
        }
        buf.push('"');
        self.adds(&buf);
    }

    pub fn json_write_json(&mut self, s: &str) {
        self.json_adjust();
        self.adds(s);
    }

    pub fn json_write_number(&mut self, d: f64) {
        self.json_adjust();

        self.adds(&crate::milo::dtoa_milo(d));
    }

    // Just to avoid json_writer:: changing expected output format
    pub fn json_write_float(&mut self, d: f64) {
        self.json_adjust();

        self.aprintf(&printf_f(d));
    }

    pub fn json_write_unsigned(&mut self, v: u64) {
        self.json_adjust();

        self.aprintf(&v.to_string());
    }

    pub fn json_write_signed(&mut self, v: i64) {
        self.json_adjust();

        self.aprintf(&v.to_string());
    }

    pub fn json_write_stringified(&mut self, s: &str) {
        self.json_adjust();

        self.adds(s);
    }

    pub fn json_write_bool(&mut self, b: bool) {
        self.json_adjust();

        if b {
            self.adds("true");
        } else {
            self.adds("false");
        }
    }

    pub fn json_write_null(&mut self) {
        self.json_adjust();

        self.adds("null");
    }

    pub fn json_write_newline(&mut self) {
        self.addc(b'\n');
        self.nospace = true;
    }

    pub fn json_comma_newline(&mut self) {
        self.wantnl = true;
    }

    /// `aprintf`, which the C++ calls with a format; the callers here format first.
    fn aprintf(&mut self, s: &str) {
        self.adds(s);
    }

    /// Only ever called with ASCII.
    fn addc(&mut self, c: u8) {
        match &mut self.out {
            JsonOut::File(f) => {
                let _ = f.write_all(&[c]);
            }
            JsonOut::String(s) => {
                s.push(c as char);
            }
        }
    }

    fn adds(&mut self, s: &str) {
        match &mut self.out {
            JsonOut::File(f) => {
                // fputs() stops at a NUL
                let b = s.as_bytes();
                let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                let _ = f.write_all(&b[..n]);
            }
            JsonOut::String(out) => {
                out.push_str(s);
            }
        }
    }
}

/// The C++ destructor complains if the JSON was left open.
impl Drop for JsonWriter<'_> {
    fn drop(&mut self) {
        if !self.state.is_empty() && (self.state.len() != 1 || self.state[0] != JsonWriteTok::Top) {
            eprintln!("JSON not closed at end");
            std::process::exit(crate::errors::EXIT_FAILURE);
        }
    }
}

#[derive(Clone, Copy)]
struct Lonlat {
    op: i32,
    lon: f64,
    lat: f64,
    x: i64,
    y: i64,
}

impl Lonlat {
    fn new(nop: i32, nlon: f64, nlat: f64, nx: i64, ny: i64) -> Lonlat {
        Lonlat {
            op: nop,
            lon: nlon,
            lat: nlat,
            x: nx,
            y: ny,
        }
    }
}

fn write_coords(state: &mut JsonWriter<'_>, ll: &Lonlat, scale: f64) {
    if scale == 0.0 {
        state.json_write_float(ll.lon);
        state.json_write_float(ll.lat);
    } else {
        state.json_write_number(ll.x as f64 / scale);
        state.json_write_number(ll.y as f64 / scale);
    }
}

static RING_WARNED: AtomicBool = AtomicBool::new(false);
static INNER_WARNED: AtomicBool = AtomicBool::new(false);

#[allow(clippy::too_many_arguments, clippy::if_same_then_else)]
pub fn layer_to_geojson(
    layer: &MvtLayer,
    z: u32,
    x: u32,
    y: u32,
    comma: bool,
    name: bool,
    zoom: bool,
    write_dropped: bool,
    index: u64,
    sequence: i64,
    extent: i64,
    complain: bool,
    state: &mut JsonWriter<'_>,
    scale: f64,
    include_attr: &BTreeSet<String>,
) {
    for feat in &layer.features {
        state.json_write_hash();
        state.json_write_string("type");
        state.json_write_string("Feature");

        if feat.has_id {
            state.json_write_string("id");
            state.json_write_unsigned(feat.id);
        }

        if name || zoom || index != 0 || sequence != 0 || extent != 0 {
            state.json_write_string("tippecanoe");
            state.json_write_hash();

            if name {
                state.json_write_string("layer");
                state.json_write_string(&layer.name);
            }

            if zoom {
                state.json_write_string("minzoom");
                state.json_write_unsigned(z as u64);

                state.json_write_string("maxzoom");
                state.json_write_unsigned(z as u64);
            }

            if write_dropped {
                state.json_write_string("dropped");
                state.json_write_bool(feat.dropped == FEATURE_DROPPED);
            }

            if index != 0 {
                state.json_write_string("index");
                state.json_write_unsigned(index);
            }

            if sequence != 0 {
                state.json_write_string("sequence");
                state.json_write_signed(sequence);
            }

            if extent != 0 {
                state.json_write_string("extent");
                state.json_write_signed(extent);
            }

            state.json_end_hash();
        }

        state.json_write_string("properties");
        state.json_write_hash();

        let mut t = 0;
        while t + 1 < feat.tags.len() {
            if feat.tags[t] as usize >= layer.keys.len() {
                eprintln!(
                    "Error: out of bounds feature key ({} in {})",
                    feat.tags[t],
                    layer.keys.len()
                );
                std::process::exit(EXIT_IMPOSSIBLE);
            }
            if feat.tags[t + 1] as usize >= layer.values.len() {
                eprintln!(
                    "Error: out of bounds feature value ({} in {})",
                    feat.tags[t + 1],
                    layer.values.len()
                );
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            if !include_attr.is_empty()
                && !include_attr.contains(&layer.keys[feat.tags[t] as usize])
            {
                t += 2;
                continue;
            }

            // the C++ passes the key as a C string
            let full_key = &layer.keys[feat.tags[t] as usize];
            let key = match full_key.find('\0') {
                Some(n) => &full_key[..n],
                None => full_key.as_str(),
            };
            let val = &layer.values[feat.tags[t + 1] as usize];

            match val.type_ {
                MVT_STRING => {
                    state.json_write_string(key);
                    state.json_write_string(&val.get_string_value());
                }
                MVT_INT => {
                    state.json_write_string(key);
                    state.json_write_signed(val.numeric_value.int_value());
                }
                MVT_DOUBLE => {
                    state.json_write_string(key);
                    state.json_write_number(val.numeric_value.double_value());
                }
                MVT_FLOAT => {
                    state.json_write_string(key);
                    state.json_write_number(val.numeric_value.float_value() as f64);
                }
                MVT_SINT => {
                    state.json_write_string(key);
                    state.json_write_signed(val.numeric_value.sint_value());
                }
                MVT_UINT => {
                    state.json_write_string(key);
                    state.json_write_unsigned(val.numeric_value.uint_value());
                }
                MVT_BOOL => {
                    state.json_write_string(key);
                    state.json_write_bool(val.numeric_value.bool_value());
                }
                MVT_NULL => {
                    state.json_write_string(key);
                    state.json_write_null();
                }
                _ => {
                    eprintln!("Internal error: property with unknown type");
                    std::process::exit(EXIT_IMPOSSIBLE);
                }
            }

            t += 2;
        }

        state.json_end_hash();

        state.json_write_string("geometry");
        state.json_write_hash();

        let mut ops: Vec<Lonlat> = Vec::new();

        for g in &feat.geometry {
            let op = g.op;
            let px = g.x;
            let py = g.y;

            if op == VT_MOVETO as i32 || op == VT_LINETO as i32 {
                let wscale: i64 = 1i64 << (32 - z as i32);
                let wx = wscale * x as i64 + (wscale / layer.extent) * px;
                let wy = wscale * y as i64 + (wscale / layer.extent) * py;

                let (lon, lat) = (crate::projection::projection().unproject)(wx, wy, 32);

                ops.push(Lonlat::new(op, lon, lat, px, py));
            } else {
                ops.push(Lonlat::new(op, 0.0, 0.0, 0, 0));
            }
        }

        if feat.type_ == VT_POINT {
            if ops.len() == 1 {
                state.json_write_string("type");
                state.json_write_string("Point");

                state.json_write_string("coordinates");

                state.json_write_array();
                write_coords(state, &ops[0], scale);
                state.json_end_array();
            } else {
                state.json_write_string("type");
                state.json_write_string("MultiPoint");

                state.json_write_string("coordinates");
                state.json_write_array();

                for op in &ops {
                    state.json_write_array();
                    write_coords(state, op, scale);
                    state.json_end_array();
                }

                state.json_end_array();
            }
        } else if feat.type_ == VT_LINE {
            let mut movetos = 0;
            for op in &ops {
                if op.op == VT_MOVETO as i32 {
                    movetos += 1;
                }
            }

            if movetos < 2 {
                state.json_write_string("type");
                state.json_write_string("LineString");

                state.json_write_string("coordinates");
                state.json_write_array();

                for op in &ops {
                    state.json_write_array();
                    write_coords(state, op, scale);
                    state.json_end_array();
                }

                state.json_end_array();
            } else {
                state.json_write_string("type");
                state.json_write_string("MultiLineString");

                state.json_write_string("coordinates");
                state.json_write_array();
                state.json_write_array();

                let mut sstate = 0;
                for op in &ops {
                    if op.op == VT_MOVETO as i32 {
                        if sstate == 0 {
                            state.json_write_array();
                            write_coords(state, op, scale);
                            state.json_end_array();

                            sstate = 1;
                        } else {
                            state.json_end_array();
                            state.json_write_array();

                            state.json_write_array();
                            write_coords(state, op, scale);
                            state.json_end_array();

                            sstate = 1;
                        }
                    } else {
                        state.json_write_array();
                        write_coords(state, op, scale);
                        state.json_end_array();
                    }
                }

                state.json_end_array();
                state.json_end_array();
            }
        } else if feat.type_ == VT_POLYGON {
            let mut rings: Vec<Vec<Lonlat>> = Vec::new();
            let mut areas: Vec<f64> = Vec::new();

            for i in 0..ops.len() {
                if ops[i].op == VT_MOVETO as i32 {
                    rings.push(Vec::new());
                    areas.push(0.0);
                }

                let n = rings.len() as i32 - 1;
                if n >= 0 {
                    let n = n as usize;
                    if ops[i].op == VT_CLOSEPATH as i32 {
                        let first = rings[n][0];
                        rings[n].push(first);
                    } else {
                        rings[n].push(ops[i]);
                    }
                }

                if (i + 1 >= ops.len() || ops[i + 1].op == VT_MOVETO as i32)
                    && ops[i].op != VT_CLOSEPATH as i32
                    && !RING_WARNED.load(Ordering::Relaxed)
                {
                    eprintln!("Ring does not end with closepath (ends with {})", ops[i].op);
                    if complain {
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    RING_WARNED.store(true, Ordering::Relaxed);
                }
            }

            let mut outer = 0;

            for i in 0..rings.len() {
                let mut area: f64 = 0.0;
                let ring = &rings[i];
                for k in 0..ring.len() {
                    if ring[k].op != VT_CLOSEPATH as i32 {
                        area += ring[k].x as f64 * ring[(k + 1) % ring.len()].y as f64;
                        area -= ring[k].y as f64 * ring[(k + 1) % ring.len()].x as f64;
                    }
                }
                area /= 2.0;

                areas[i] = area;
                if areas[i] >= 0.0 || i == 0 {
                    outer += 1;
                }

                // fprintf("\"area\": %Lf,", area);
            }

            if outer > 1 {
                state.json_write_string("type");
                state.json_write_string("MultiPolygon");

                state.json_write_string("coordinates");
                state.json_write_array();
                state.json_write_array();
                state.json_write_array();
            } else {
                state.json_write_string("type");
                state.json_write_string("Polygon");

                state.json_write_string("coordinates");
                state.json_write_array();
                state.json_write_array();
            }

            let mut sstate = 0;
            for i in 0..rings.len() {
                if i == 0 && areas[i] < 0.0 && !INNER_WARNED.load(Ordering::Relaxed) {
                    eprintln!("Polygon begins with an inner ring");
                    if complain {
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }

                    INNER_WARNED.store(true, Ordering::Relaxed);
                }

                if areas[i] >= 0.0 {
                    if sstate != 0 {
                        // new multipolygon
                        state.json_end_array();
                        state.json_end_array();

                        state.json_write_array();
                        state.json_write_array();
                    }
                    sstate = 1;
                }

                if sstate == 2 {
                    // new ring in the same polygon
                    state.json_end_array();
                    state.json_write_array();
                }

                for ll in &rings[i] {
                    if ll.op != VT_CLOSEPATH as i32 {
                        state.json_write_array();
                        write_coords(state, ll, scale);
                        state.json_end_array();
                    } else {
                        state.json_write_array();
                        write_coords(state, ll, scale);
                        state.json_end_array();
                    }
                }

                sstate = 2;
            }

            if outer > 1 {
                state.json_end_array();
                state.json_end_array();
                state.json_end_array();
            } else {
                state.json_end_array();
                state.json_end_array();
            }
        }

        state.json_end_hash();
        state.json_end_hash();

        if comma {
            state.json_write_newline();
            state.json_comma_newline();
        }
    }
}

/// Writes `s` as a quoted JSON string. The C++ takes a C string, so this
/// stops at a NUL.
pub fn fprintq(f: &mut dyn Write, s: &str) {
    let _ = f.write_all(b"\"");
    for &c in s.as_bytes() {
        if c == 0 {
            break;
        }
        if c == b'\\' || c == b'"' {
            let _ = f.write_all(&[b'\\', c]);
        } else if c < b' ' {
            let _ = write!(f, "\\u{:04x}", c);
        } else {
            let _ = f.write_all(&[c]);
        }
    }
    let _ = f.write_all(b"\"");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{MVT_CLOSEPATH, MVT_LINETO, MVT_MOVETO, MvtFeature, MvtGeometry};

    #[test]
    fn writer_matches_the_cpp() {
        let mut s = String::new();
        {
            let mut w = JsonWriter::new_string(&mut s);
            w.json_write_hash();
            w.json_write_string("a");
            w.json_write_array();
            w.json_write_number(1.5);
            w.json_write_float(-0.0000005);
            w.json_write_float(f64::NAN);
            w.json_write_unsigned(18446744073709551615);
            w.json_write_signed(-5);
            w.json_write_bool(true);
            w.json_write_null();
            w.json_write_stringified("{\"x\":1}");
            w.json_end_array();
            w.json_write_string("empty");
            w.json_write_hash();
            w.json_end_hash();
            w.json_write_string("e2");
            w.json_write_array();
            w.json_end_array();
            w.json_write_string("s\t\"\\\x01\u{e9}");
            w.json_write_json("[1,2]");
            w.json_end_hash();
            w.json_write_newline();
            w.json_write_hash();
            w.json_write_string("k");
            w.json_write_string("v");
            w.json_write_newline();
            w.json_comma_newline();
            w.json_write_string("k2");
            w.json_write_number(1e21);
            w.json_end_hash();
        }
        assert_eq!(
            s,
            "{ \"a\": [ 1.5, -0.000000, nan, 18446744073709551615, -5, true, null, {\"x\":1} ], \"empty\": {  }, \"e2\": [ ], \"s\\u0009\\\"\\\\\\u0001\u{e9}\": [1,2] }\n\n{\"k\": \"v\"\n,\n\"k2\": 1e+21 }"
        );
    }

    #[test]
    fn file_output_stops_at_nul() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = JsonWriter::new_file(&mut out);
            w.json_write_array();
            w.json_write_string("a\0b");
            w.json_write_json("x\0y");
            w.json_end_array();
        }
        assert_eq!(out, b"[ \"a\\u0000b\", x ]");
    }

    #[test]
    fn fprintq_matches_the_cpp() {
        let mut out: Vec<u8> = Vec::new();
        fprintq(&mut out, "a\"b\\c\n\x7f\u{e9}\0z");
        assert_eq!(out, "\"a\\\"b\\\\c\\u000a\x7f\u{e9}\"".as_bytes());
    }

    #[test]
    fn polygon_with_hole_and_multipolygon() {
        let ring = |x0: i64, y0: i64, x1: i64, y1: i64, clockwise: bool| {
            let mut g = vec![MvtGeometry::new(MVT_MOVETO, x0, y0)];
            if clockwise {
                g.push(MvtGeometry::new(MVT_LINETO, x1, y0));
                g.push(MvtGeometry::new(MVT_LINETO, x1, y1));
                g.push(MvtGeometry::new(MVT_LINETO, x0, y1));
            } else {
                g.push(MvtGeometry::new(MVT_LINETO, x0, y1));
                g.push(MvtGeometry::new(MVT_LINETO, x1, y1));
                g.push(MvtGeometry::new(MVT_LINETO, x1, y0));
            }
            g.push(MvtGeometry::new(MVT_CLOSEPATH, 0, 0));
            g
        };

        let mut layer = MvtLayer {
            name: "l".to_string(),
            version: 2,
            extent: 4096,
            ..Default::default()
        };
        let mut g = ring(0, 0, 100, 100, true);
        g.extend(ring(10, 10, 20, 20, false));
        g.extend(ring(200, 200, 300, 300, true));
        layer.features.push(MvtFeature {
            type_: VT_POLYGON,
            geometry: g,
            ..Default::default()
        });

        let mut s = String::new();
        {
            let mut w = JsonWriter::new_string(&mut s);
            layer_to_geojson(
                &layer,
                0,
                0,
                0,
                false,
                false,
                false,
                false,
                0,
                0,
                0,
                true,
                &mut w,
                1.0,
                &BTreeSet::new(),
            );
        }
        assert_eq!(
            s,
            "{ \"type\": \"Feature\", \"properties\": {  }, \"geometry\": { \"type\": \"MultiPolygon\", \"coordinates\": [ [ [ [ 0, 0 ], [ 100, 0 ], [ 100, 100 ], [ 0, 100 ], [ 0, 0 ] ], [ [ 10, 10 ], [ 10, 20 ], [ 20, 20 ], [ 20, 10 ], [ 10, 10 ] ] ], [ [ [ 200, 200 ], [ 300, 200 ], [ 300, 300 ], [ 200, 300 ], [ 200, 200 ] ] ] ] } }"
        );
    }
}
