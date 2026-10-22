//! Port of jsonpull/jsonpull.h and jsonpull.cpp, the streaming JSON parser.
//!
//! The C++ links nodes with owning pointers down and raw `parent` pointers up.
//! Here all the nodes of one top-level value live in one arena, a `JsonTree`, and
//! links are indexes into it. A `JsonRef` is the counterpart of a borrowed
//! `json_object *`, and a `JsonTree` returned by `read_tree` or `disconnect` is
//! the counterpart of an owned `json_object_ptr`.
//!
//! String values keep the text as a `String`. If the input bytes were not valid
//! UTF-8, the `String` holds a lossy copy and `raw_string_bytes` returns the
//! original bytes, so callers can run the C++ `check_utf8` on them.

use std::io::Read;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JsonType {
    // These types can be returned by json_read()
    Hash,
    Array,
    Number,
    String,
    True,
    False,
    Null,

    // These and Hash and Array can be called back by json_read_with_separators()
    Comma,
    Colon,

    // These are only used internally as expectations of what comes next
    Item,
    Key,
    Value,
}

/// Index of a node in a `JsonTree`.
pub type JsonId = usize;

/// An arena holding one JSON value and everything inside it.
#[derive(Default, Clone, Debug)]
pub struct JsonTree {}

impl JsonTree {
    /// The top-level value. Panics on an empty tree.
    pub fn root(&self) -> JsonRef<'_> {
        todo!("JsonTree::root")
    }

    pub fn get(&self, id: JsonId) -> JsonRef<'_> {
        todo!("JsonTree::get")
    }

    /// `json_free` on a node of a detached tree: splices it out of its parent.
    pub fn free(&mut self, id: JsonId) {
        todo!("JsonTree::free")
    }
}

/// A borrowed node, the counterpart of `json_object *`.
#[derive(Clone, Copy)]
pub struct JsonRef<'a> {
    tree: &'a JsonTree,
    id: JsonId,
}

impl<'a> JsonRef<'a> {
    pub fn id(&self) -> JsonId {
        self.id
    }

    pub fn tree(&self) -> &'a JsonTree {
        self.tree
    }

    pub fn type_(&self) -> JsonType {
        todo!("JsonRef::type_")
    }

    /// `json_object::string()`. Panics if this is not a string.
    pub fn string(&self) -> &'a str {
        todo!("JsonRef::string")
    }

    /// The string's bytes as they appeared in the input, for `check_utf8`.
    pub fn raw_string_bytes(&self) -> &'a [u8] {
        todo!("JsonRef::raw_string_bytes")
    }

    pub fn number(&self) -> f64 {
        todo!("JsonRef::number")
    }

    /// 0 when the number is not held as a large unsigned integer.
    pub fn large_unsigned(&self) -> u64 {
        todo!("JsonRef::large_unsigned")
    }

    /// 0 when the number is not held as a large signed integer.
    pub fn large_signed(&self) -> i64 {
        todo!("JsonRef::large_signed")
    }

    /// The elements of an array, in order. Panics if this is not an array.
    pub fn array(&self) -> Vec<JsonRef<'a>> {
        todo!("JsonRef::array")
    }

    /// The key/value pairs of a hash, in source order. Pairs whose value is
    /// still being parsed are left out. Panics if this is not a hash.
    pub fn entries(&self) -> Vec<(JsonRef<'a>, JsonRef<'a>)> {
        todo!("JsonRef::entries")
    }

    /// `json_hash_get`. `None` if this is not a hash, the key is absent, or the
    /// hash is mid-parse with that value slot unfilled. A JSON `null` comes
    /// back as a node of type `Null`.
    pub fn hash_get(&self, key: &str) -> Option<JsonRef<'a>> {
        todo!("JsonRef::hash_get")
    }

    pub fn parent(&self) -> Option<JsonRef<'a>> {
        todo!("JsonRef::parent")
    }

    /// `json_stringify`.
    pub fn stringify(&self) -> String {
        todo!("JsonRef::stringify")
    }
}

/// `json_pull`.
pub struct JsonPull<'a> {
    pub error: Option<&'static str>,
    pub line: i32,
    source: Option<Box<dyn Read + 'a>>,
}

impl<'a> JsonPull<'a> {
    /// `json_begin_file`, for any reader.
    pub fn begin_file(r: Box<dyn Read + 'a>) -> JsonPull<'a> {
        todo!("JsonPull::begin_file")
    }

    /// `json_begin_string`.
    pub fn begin_string(s: &'a [u8]) -> JsonPull<'a> {
        todo!("JsonPull::begin_string")
    }

    /// `json_read`: the next completed node, or `None` at the end of input or on
    /// an error (then `error` is set).
    pub fn read(&mut self) -> Option<JsonId> {
        todo!("JsonPull::read")
    }

    /// `json_read_separators`.
    pub fn read_separators(
        &mut self,
        cb: &mut dyn FnMut(JsonType, &JsonPull<'a>),
    ) -> Option<JsonId> {
        todo!("JsonPull::read_separators")
    }

    /// `json_read_tree`: drains the next top-level value and hands it over.
    pub fn read_tree(&mut self) -> Option<JsonTree> {
        todo!("JsonPull::read_tree")
    }

    /// The tree the parser is currently building.
    pub fn tree(&self) -> &JsonTree {
        todo!("JsonPull::tree")
    }

    pub fn get(&self, id: JsonId) -> JsonRef<'_> {
        todo!("JsonPull::get")
    }

    /// `jp->root`, the most recently completed or in-progress top-level value.
    pub fn root(&self) -> Option<JsonRef<'_>> {
        todo!("JsonPull::root")
    }

    /// `jp->root.reset()`.
    pub fn clear_root(&mut self) {
        todo!("JsonPull::clear_root")
    }

    /// `json_free`: splices the node out of its parent, or clears the root.
    pub fn free(&mut self, id: JsonId) {
        todo!("JsonPull::free")
    }

    /// `json_disconnect`: splices the node out and hands it over.
    pub fn disconnect(&mut self, id: JsonId) -> JsonTree {
        todo!("JsonPull::disconnect")
    }
}
