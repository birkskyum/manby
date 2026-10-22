//! Port of write_json.cpp.

use std::collections::BTreeSet;
use std::io::Write;

use crate::mvt::MvtLayer;

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

    pub fn json_write_array(&mut self) {
        todo!("JsonWriter::json_write_array")
    }
    pub fn json_end_array(&mut self) {
        todo!("JsonWriter::json_end_array")
    }
    pub fn json_write_hash(&mut self) {
        todo!("JsonWriter::json_write_hash")
    }
    pub fn json_end_hash(&mut self) {
        todo!("JsonWriter::json_end_hash")
    }
    pub fn json_write_string(&mut self, s: &str) {
        todo!("JsonWriter::json_write_string")
    }
    pub fn json_write_json(&mut self, s: &str) {
        todo!("JsonWriter::json_write_json")
    }
    pub fn json_write_number(&mut self, d: f64) {
        todo!("JsonWriter::json_write_number")
    }
    pub fn json_write_float(&mut self, d: f64) {
        todo!("JsonWriter::json_write_float")
    }
    pub fn json_write_unsigned(&mut self, v: u64) {
        todo!("JsonWriter::json_write_unsigned")
    }
    pub fn json_write_signed(&mut self, v: i64) {
        todo!("JsonWriter::json_write_signed")
    }
    pub fn json_write_stringified(&mut self, s: &str) {
        todo!("JsonWriter::json_write_stringified")
    }
    pub fn json_write_bool(&mut self, b: bool) {
        todo!("JsonWriter::json_write_bool")
    }
    pub fn json_write_null(&mut self) {
        todo!("JsonWriter::json_write_null")
    }
    pub fn json_write_newline(&mut self) {
        todo!("JsonWriter::json_write_newline")
    }
    pub fn json_comma_newline(&mut self) {
        todo!("JsonWriter::json_comma_newline")
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

#[allow(clippy::too_many_arguments)]
pub fn layer_to_geojson(
    layer: &MvtLayer,
    z: u32,
    x: u32,
    y: u32,
    comma: bool,
    name: bool,
    zoom: bool,
    dropped: bool,
    index: u64,
    sequence: i64,
    extent: i64,
    complain: bool,
    state: &mut JsonWriter<'_>,
    scale: f64,
    include_attr: &BTreeSet<String>,
) {
    todo!("write_json::layer_to_geojson")
}

/// Writes `s` as a quoted JSON string.
pub fn fprintq(f: &mut dyn Write, s: &str) {
    todo!("write_json::fprintq")
}
