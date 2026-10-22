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
//!
//! Ownership model
//!
//! Every node has exactly one owner: its parent (an index in the parent's
//! array or hash entry), the parser (its tree's root) for a top-level value,
//! or the caller once `read_tree` / `disconnect` hands the tree over.
//! `read` and `hash_get` return borrowed nodes, valid while the owning
//! container is intact. Freeing a node puts its slots, and those of everything
//! inside it, on the arena's free list, so streaming a huge FeatureCollection
//! and freeing each feature keeps the arena at the size of one feature.
//!
//! A detached tree has no parser. The `parent` of its root is cleared, while
//! the interior `parent` links stay, so it is still walkable upwards and
//! `free` / `disconnect` still work inside it.

use std::io::Read;

use crate::cstd::{atof, errno, set_errno, strtoll, strtoull};
use crate::milo::dtoa_milo;

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

/// `json_number::repr_t` and its value.
#[derive(Clone, Copy, Debug)]
enum NumberRepr {
    Double(f64),
    LargeUnsigned(u64),
    LargeSigned(i64),
}

/// One key/value pair in a hash, held in source order. The value is `None`
/// while it is still being parsed.
#[derive(Clone, Copy, Debug)]
struct JsonEntry {
    key: JsonId,
    value: Option<JsonId>,
}

#[derive(Clone, Debug)]
enum Payload {
    /// A slot on the arena's free list.
    Free,
    /// True, false and null have no extra fields.
    Plain,
    Number(NumberRepr),
    /// `raw` holds the original bytes when they are not valid UTF-8.
    String {
        s: String,
        raw: Option<Box<[u8]>>,
    },
    Array(Vec<JsonId>),
    Hash(Vec<JsonEntry>),
}

#[derive(Clone, Debug)]
struct JsonNode {
    type_: JsonType,
    parent: Option<JsonId>,
    payload: Payload,
}

/// An arena holding one JSON value and everything inside it.
#[derive(Default, Clone, Debug)]
pub struct JsonTree {
    nodes: Vec<JsonNode>,
    free_slots: Vec<JsonId>,
    root: Option<JsonId>,
}

impl JsonTree {
    /// The top-level value. Panics on an empty tree.
    pub fn root(&self) -> JsonRef<'_> {
        self.get(self.root.expect("JsonTree::root on an empty tree"))
    }

    /// Whether the tree holds no value, like a null `json_object_ptr`.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn get(&self, id: JsonId) -> JsonRef<'_> {
        JsonRef { tree: self, id }
    }

    /// `json_free` on a node of a detached tree: splices it out of its parent.
    ///
    /// Like the C++, freeing the root of a detached tree does nothing, since
    /// the tree's owner, not a parser, holds it.
    pub fn free(&mut self, id: JsonId) {
        if self.take_from_owner(id) {
            self.release_subtree(id);
        }
    }

    /// `json_disconnect` on a node of a detached tree: splices it out of its
    /// parent and hands it over. Empty if nothing in the tree owns it, which
    /// is the case for the root.
    pub fn disconnect(&mut self, id: JsonId) -> JsonTree {
        if !self.take_from_owner(id) {
            return JsonTree::default();
        }
        let out = self.extract(id);
        self.release_subtree(id);
        out
    }

    /// `new json_string(parent, parser)` with the given text, not yet linked
    /// into the parent, for code like jsontool that adds to a tree.
    pub fn new_string(&mut self, parent: Option<JsonId>, s: &[u8]) -> JsonId {
        let id = self.alloc(JsonType::String, parent);
        self.set_string(id, s);
        id
    }

    /// `new json_number(parent, parser)` followed by `set_number(d)`, not yet
    /// linked into the parent.
    pub fn new_number(&mut self, parent: Option<JsonId>, d: f64) -> JsonId {
        let id = self.alloc(JsonType::Number, parent);
        self.set_number(id, d);
        id
    }

    /// `entries().push_back({key, value})` on a hash.
    pub fn hash_push(&mut self, hash: JsonId, key: JsonId, value: JsonId) {
        match &mut self.nodes[hash].payload {
            Payload::Hash(entries) => entries.push(JsonEntry {
                key,
                value: Some(value),
            }),
            _ => panic!("json_object::entries() on a non-hash"),
        }
    }

    /// `json_object::set_number`.
    pub fn set_number(&mut self, id: JsonId, d: f64) {
        self.set_repr(id, NumberRepr::Double(d));
    }

    /// `json_object::set_large_unsigned`.
    pub fn set_large_unsigned(&mut self, id: JsonId, u: u64) {
        self.set_repr(id, NumberRepr::LargeUnsigned(u));
    }

    /// `json_object::set_large_signed`.
    pub fn set_large_signed(&mut self, id: JsonId, s: i64) {
        self.set_repr(id, NumberRepr::LargeSigned(s));
    }

    fn set_repr(&mut self, id: JsonId, repr: NumberRepr) {
        match &mut self.nodes[id].payload {
            Payload::Number(n) => *n = repr,
            _ => panic!("json_object::set_number() on a non-number"),
        }
    }

    fn set_string(&mut self, id: JsonId, val: &[u8]) {
        let (s, raw) = match std::str::from_utf8(val) {
            Ok(s) => (s.to_owned(), None),
            Err(_) => (
                String::from_utf8_lossy(val).into_owned(),
                Some(val.to_vec().into_boxed_slice()),
            ),
        };
        match &mut self.nodes[id].payload {
            Payload::String { s: ss, raw: rr } => {
                *ss = s;
                *rr = raw;
            }
            _ => panic!("json_object::string() on a non-string"),
        }
    }

    /// Drops every node, keeping the arena's capacity.
    fn clear(&mut self) {
        self.nodes.clear();
        self.free_slots.clear();
        self.root = None;
    }

    // JSON_TRUE / JSON_FALSE / JSON_NULL and the parse-token types are bare
    // json_objects; the value-bearing types each get their own payload.
    fn alloc(&mut self, type_: JsonType, parent: Option<JsonId>) -> JsonId {
        let payload = match type_ {
            JsonType::Number => Payload::Number(NumberRepr::Double(0.0)),
            JsonType::String => Payload::String {
                s: String::new(),
                raw: None,
            },
            // 2 slots: coordinate pairs dominate the parse workload.
            JsonType::Array => Payload::Array(Vec::with_capacity(2)),
            // 4 slots: the typical GeoJSON property hash.
            JsonType::Hash => Payload::Hash(Vec::with_capacity(4)),
            _ => Payload::Plain,
        };
        let node = JsonNode {
            type_,
            parent,
            payload,
        };

        match self.free_slots.pop() {
            Some(id) => {
                self.nodes[id] = node;
                id
            }
            None => {
                self.nodes.push(node);
                self.nodes.len() - 1
            }
        }
    }

    /// Destroys `id` and everything inside it, putting their slots on the free list.
    fn release_subtree(&mut self, id: JsonId) {
        let mut pending = vec![id];

        while let Some(n) = pending.pop() {
            match std::mem::replace(&mut self.nodes[n].payload, Payload::Free) {
                Payload::Array(arr) => pending.extend(arr),
                Payload::Hash(entries) => {
                    for e in entries {
                        pending.push(e.key);
                        if let Some(v) = e.value {
                            pending.push(v);
                        }
                    }
                }
                _ => {}
            }
            self.nodes[n].parent = None;
            self.free_slots.push(n);
        }
    }

    // Unlink `o` from the vector slot or hash entry of its parent that holds
    // it. False if nothing tracked owns it -- already detached, or a root,
    // which the caller deals with.
    //
    // Detaching one half of a hash entry would disturb the surrounding key/value
    // pairing, so the extracted half is replaced by a JSON_NULL placeholder and the
    // entry is erased only once both halves are gone.
    fn take_from_owner(&mut self, o: JsonId) -> bool {
        let parent = match self.nodes[o].parent {
            Some(p) => p,
            None => return false,
        };

        match &mut self.nodes[parent].payload {
            Payload::Array(arr) => {
                if let Some(i) = arr.iter().position(|&e| e == o) {
                    arr.remove(i);
                    return true;
                }
            }
            Payload::Hash(entries) => {
                let found = entries
                    .iter()
                    .position(|e| e.key == o || e.value == Some(o));
                if let Some(i) = found {
                    let placeholder = self.alloc(JsonType::Null, Some(parent));
                    let entry = self.hash_entries(parent)[i];

                    let erase = if entry.key == o {
                        self.hash_entries_mut(parent)[i].key = placeholder;
                        match entry.value {
                            Some(v) => self.nodes[v].type_ == JsonType::Null,
                            None => false,
                        }
                    } else {
                        self.hash_entries_mut(parent)[i].value = Some(placeholder);
                        self.nodes[entry.key].type_ == JsonType::Null
                    };

                    if erase {
                        // Both halves are placeholders or nulls by now; the
                        // half being taken is no longer in the entry.
                        let e = self.hash_entries_mut(parent).remove(i);
                        self.release_subtree(e.key);
                        if let Some(v) = e.value {
                            self.release_subtree(v);
                        }
                    }
                    return true;
                }
            }
            _ => {}
        }

        false
    }

    fn hash_entries(&self, id: JsonId) -> &Vec<JsonEntry> {
        match &self.nodes[id].payload {
            Payload::Hash(entries) => entries,
            _ => panic!("json_object::entries() on a non-hash"),
        }
    }

    fn hash_entries_mut(&mut self, id: JsonId) -> &mut Vec<JsonEntry> {
        match &mut self.nodes[id].payload {
            Payload::Hash(entries) => entries,
            _ => panic!("json_object::entries() on a non-hash"),
        }
    }

    /// A copy of the subtree at `id` as a tree of its own, whose root has no parent.
    fn extract(&self, id: JsonId) -> JsonTree {
        let mut out = JsonTree::default();
        let root = self.copy_into(id, None, &mut out);
        out.root = Some(root);
        out
    }

    fn copy_into(&self, id: JsonId, parent: Option<JsonId>, out: &mut JsonTree) -> JsonId {
        let n = &self.nodes[id];
        let new_id = out.nodes.len();
        out.nodes.push(JsonNode {
            type_: n.type_,
            parent,
            payload: Payload::Plain,
        });

        let payload = match &n.payload {
            Payload::Array(arr) => Payload::Array(
                arr.iter()
                    .map(|&e| self.copy_into(e, Some(new_id), out))
                    .collect(),
            ),
            Payload::Hash(entries) => Payload::Hash(
                entries
                    .iter()
                    .map(|e| JsonEntry {
                        key: self.copy_into(e.key, Some(new_id), out),
                        value: e.value.map(|v| self.copy_into(v, Some(new_id), out)),
                    })
                    .collect(),
            ),
            other => other.clone(),
        };
        out.nodes[new_id].payload = payload;
        new_id
    }
}

/// A borrowed node, the counterpart of `json_object *`.
#[derive(Clone, Copy)]
pub struct JsonRef<'a> {
    tree: &'a JsonTree,
    id: JsonId,
}

/// Pointer equality, like comparing two `json_object *`.
impl PartialEq for JsonRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.tree, other.tree) && self.id == other.id
    }
}

impl Eq for JsonRef<'_> {}

impl std::fmt::Debug for JsonRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JsonRef({}, {})", self.id, self.stringify())
    }
}

impl<'a> JsonRef<'a> {
    pub fn id(&self) -> JsonId {
        self.id
    }

    pub fn tree(&self) -> &'a JsonTree {
        self.tree
    }

    fn node(&self) -> &'a JsonNode {
        &self.tree.nodes[self.id]
    }

    pub fn type_(&self) -> JsonType {
        self.node().type_
    }

    /// `json_object::string()`. Panics if this is not a string.
    pub fn string(&self) -> &'a str {
        match &self.node().payload {
            Payload::String { s, .. } => s,
            _ => panic!("json_object::string() on a non-string"),
        }
    }

    /// The string's bytes as they appeared in the input, for `check_utf8`.
    pub fn raw_string_bytes(&self) -> &'a [u8] {
        match &self.node().payload {
            Payload::String { s, raw } => match raw {
                Some(raw) => raw,
                None => s.as_bytes(),
            },
            _ => panic!("json_object::string() on a non-string"),
        }
    }

    fn repr(&self) -> NumberRepr {
        match &self.node().payload {
            Payload::Number(n) => *n,
            _ => panic!("json_object::number() on a non-number"),
        }
    }

    pub fn number(&self) -> f64 {
        match self.repr() {
            NumberRepr::LargeUnsigned(u) => u as f64,
            NumberRepr::LargeSigned(s) => s as f64,
            NumberRepr::Double(d) => d,
        }
    }

    /// 0 when the number is not held as a large unsigned integer.
    pub fn large_unsigned(&self) -> u64 {
        match self.repr() {
            NumberRepr::LargeUnsigned(u) => u,
            _ => 0,
        }
    }

    /// 0 when the number is not held as a large signed integer.
    pub fn large_signed(&self) -> i64 {
        match self.repr() {
            NumberRepr::LargeSigned(s) => s,
            _ => 0,
        }
    }

    fn array_ids(&self) -> &'a [JsonId] {
        match &self.node().payload {
            Payload::Array(arr) => arr,
            _ => panic!("json_object::array() on a non-array"),
        }
    }

    /// The elements of an array, in order. Panics if this is not an array.
    pub fn array(&self) -> Vec<JsonRef<'a>> {
        self.array_iter().collect()
    }

    /// `array().size()` without collecting the elements. Panics if this is not an array.
    pub fn array_len(&self) -> usize {
        self.array_ids().len()
    }

    /// `array()[i]`. Panics if this is not an array or `i` is out of range.
    pub fn array_get(&self, i: usize) -> JsonRef<'a> {
        self.tree.get(self.array_ids()[i])
    }

    /// The elements of an array, in order, without collecting them.
    pub fn array_iter(&self) -> impl ExactSizeIterator<Item = JsonRef<'a>> + 'a {
        let tree = self.tree;
        self.array_ids().iter().map(move |&id| tree.get(id))
    }

    fn entry_slice(&self) -> &'a [JsonEntry] {
        match &self.node().payload {
            Payload::Hash(entries) => entries,
            _ => panic!("json_object::entries() on a non-hash"),
        }
    }

    /// The key/value pairs of a hash, in source order. Pairs whose value is
    /// still being parsed are left out. Panics if this is not a hash.
    pub fn entries(&self) -> Vec<(JsonRef<'a>, JsonRef<'a>)> {
        self.entries_iter().collect()
    }

    /// `entries()` without collecting them.
    pub fn entries_iter(&self) -> impl Iterator<Item = (JsonRef<'a>, JsonRef<'a>)> + 'a {
        let tree = self.tree;
        self.entry_slice()
            .iter()
            .filter_map(move |e| e.value.map(|v| (tree.get(e.key), tree.get(v))))
    }

    /// `json_hash_get`. `None` if this is not a hash, the key is absent, or the
    /// hash is mid-parse with that value slot unfilled. A JSON `null` comes
    /// back as a node of type `Null`.
    pub fn hash_get(&self, key: &str) -> Option<JsonRef<'a>> {
        let entries = match &self.node().payload {
            Payload::Hash(entries) => entries,
            _ => return None,
        };

        for e in entries {
            let k = self.tree.get(e.key);
            if k.type_() == JsonType::String && k.raw_string_bytes() == key.as_bytes() {
                return e.value.map(|v| self.tree.get(v));
            }
        }

        None
    }

    pub fn parent(&self) -> Option<JsonRef<'a>> {
        self.node().parent.map(|p| self.tree.get(p))
    }

    /// `json_stringify`. Strings that were not valid UTF-8 come out lossily;
    /// `stringify_raw` has the exact bytes.
    pub fn stringify(&self) -> String {
        let val = self.stringify_raw();
        match String::from_utf8(val) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }
    }

    /// `json_stringify`, byte for byte.
    pub fn stringify_raw(&self) -> Vec<u8> {
        let mut val = Vec::new();
        json_print(&mut val, Some(*self));
        val
    }

    /// A copy of this node and everything inside it as a detached tree,
    /// for callers that need to change it.
    pub fn to_tree(&self) -> JsonTree {
        self.tree.extract(self.id)
    }
}

fn json_print_one(val: &mut Vec<u8>, o: Option<JsonRef<'_>>) {
    let o = match o {
        None => {
            val.extend_from_slice(b"...");
            return;
        }
        Some(o) => o,
    };

    match o.type_() {
        JsonType::String => {
            val.push(b'"');

            // Every byte, not up to a NUL: the value may contain an embedded NUL,
            // which the control-character branch below escapes like any other.
            for &c in o.raw_string_bytes() {
                if c == b'\\' || c == b'"' {
                    val.push(b'\\');
                    val.push(c);
                } else if c < b' ' {
                    val.extend_from_slice(format!("\\u{:04x}", c).as_bytes());
                } else {
                    val.push(c);
                }
            }

            val.push(b'"');
        }
        JsonType::Number => {
            if o.large_signed() != 0 {
                val.extend_from_slice(o.large_signed().to_string().as_bytes());
            } else if o.large_unsigned() != 0 {
                val.extend_from_slice(o.large_unsigned().to_string().as_bytes());
            } else {
                val.extend_from_slice(dtoa_milo(o.number()).as_bytes());
            }
        }
        JsonType::Null => val.extend_from_slice(b"null"),
        JsonType::True => val.extend_from_slice(b"true"),
        JsonType::False => val.extend_from_slice(b"false"),
        // JSON_HASH and JSON_ARRAY never reach here: json_print handles both
        // itself and only delegates to json_print_one for the scalar types.
        _ => {}
    }
}

fn json_print(val: &mut Vec<u8>, o: Option<JsonRef<'_>>) {
    match o {
        None => {
            // Hash value in incompletely read hash
            val.extend_from_slice(b"...");
        }
        Some(o) if o.type_() == JsonType::Hash => {
            val.push(b'{');

            let entries = o.entry_slice();
            for (i, e) in entries.iter().enumerate() {
                json_print(val, Some(o.tree.get(e.key)));
                val.push(b':');
                json_print(val, e.value.map(|v| o.tree.get(v)));
                if i + 1 < entries.len() {
                    val.push(b',');
                }
            }
            val.push(b'}');
        }
        Some(o) if o.type_() == JsonType::Array => {
            val.push(b'[');
            let arr = o.array_ids();
            for (i, &e) in arr.iter().enumerate() {
                json_print(val, Some(o.tree.get(e)));
                if i + 1 < arr.len() {
                    val.push(b',');
                }
            }
            val.push(b']');
        }
        Some(o) => json_print_one(val, Some(o)),
    }
}

const BUFFER: usize = 10000;
const EOF: i32 = -1;

/// A currently-open container, with the token it expects next.
struct ParseFrame {
    container: JsonId,
    expect: JsonType,
}

/// `json_separator_callback`, with the closure's captures as its `state`.
pub type JsonSeparatorCallback<'b, 'a> = dyn FnMut(JsonType, &JsonPull<'a>) + 'b;

/// `json_pull`.
pub struct JsonPull<'a> {
    pub error: Option<&'static str>,
    pub line: i32,
    source: Option<Box<dyn Read + 'a>>,
    /// Set once the source has returned no more data, which then stays the
    /// case, as with `fread` on a `FILE` at its end.
    at_eof: bool,
    buffer: Vec<u8>,
    buffer_tail: usize,
    buffer_head: usize,

    // Currently-open containers, innermost last, each with the token it
    // expects next.
    container_stack: Vec<ParseFrame>,

    // Most recently completed top-level value, owned until the next one
    // starts parsing or the caller takes it.
    tree: JsonTree,

    // Reused across tokens, cleared but not shrunk, so they stop
    // reallocating once grown to the largest token seen.
    number_buffer: String,
    string_buffer: Vec<u8>,
}

impl<'a> JsonPull<'a> {
    fn begin(source: Option<Box<dyn Read + 'a>>) -> JsonPull<'a> {
        JsonPull {
            error: None,
            line: 1,
            source,
            at_eof: false,
            buffer: vec![0; BUFFER],
            buffer_tail: 0,
            buffer_head: 0,
            container_stack: Vec::new(),
            tree: JsonTree::default(),
            number_buffer: String::new(),
            string_buffer: Vec::new(),
        }
    }

    /// `json_begin_file`, for any reader.
    pub fn begin_file(r: Box<dyn Read + 'a>) -> JsonPull<'a> {
        JsonPull::begin(Some(r))
    }

    /// `json_begin_string`. Like the C++, the string ends at its first NUL.
    pub fn begin_string(s: &'a [u8]) -> JsonPull<'a> {
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        let s: &'a [u8] = &s[..end];
        JsonPull::begin(Some(Box::new(s)))
    }

    /// The parser's `read` callback: up to `BUFFER` bytes into the buffer.
    fn read_source(&mut self) -> usize {
        if self.at_eof {
            return 0;
        }
        let source = match &mut self.source {
            Some(s) => s,
            None => return 0,
        };
        loop {
            match source.read(&mut self.buffer[..BUFFER]) {
                Ok(0) => break,
                Ok(n) => return n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        self.at_eof = true;
        0
    }

    #[inline]
    fn peek(&mut self) -> i32 {
        if self.buffer_head < self.buffer_tail {
            self.buffer[self.buffer_head] as i32
        } else {
            self.buffer_head = 0;
            self.buffer_tail = self.read_source();
            if self.buffer_head >= self.buffer_tail {
                return EOF;
            }
            self.buffer[self.buffer_head] as i32
        }
    }

    #[inline]
    fn next(&mut self) -> i32 {
        if self.buffer_head < self.buffer_tail {
            let c = self.buffer[self.buffer_head] as i32;
            self.buffer_head += 1;
            c
        } else {
            self.buffer_head = 0;
            self.buffer_tail = self.read_source();
            if self.buffer_head >= self.buffer_tail {
                return EOF;
            }
            let c = self.buffer[self.buffer_head] as i32;
            self.buffer_head += 1;
            c
        }
    }

    #[inline]
    fn read_wrap(&mut self) -> i32 {
        let c = self.next();

        if c == '\n' as i32 {
            self.line += 1;
        }

        c
    }

    /// Drops the parser's top-level value. The C++ leaves any open containers
    /// dangling then; here they are forgotten too.
    fn reset_root(&mut self) {
        self.tree.clear();
        self.container_stack.clear();
    }

    // Install a new node of `type` in the current container, or as the parser's
    // root if the stack is empty. Returns it, or None after setting
    // self.error.
    fn add_object(&mut self, type_: JsonType) -> Option<JsonId> {
        let f = match self.container_stack.last_mut() {
            Some(f) => f,
            None => {
                // Replacing the parser's root destroys the previous top-level
                // value (if no one called disconnect / read_tree to
                // take ownership of it).
                self.tree.clear();
                let o = self.tree.alloc(type_, None);
                self.tree.root = Some(o);
                return Some(o);
            }
        };
        let c = f.container;

        match self.tree.nodes[c].type_ {
            JsonType::Array => {
                if f.expect == JsonType::Item {
                    f.expect = JsonType::Comma;
                    let o = self.tree.alloc(type_, Some(c));
                    match &mut self.tree.nodes[c].payload {
                        Payload::Array(arr) => arr.push(o),
                        _ => unreachable!(),
                    }
                    Some(o)
                } else {
                    self.error = Some("Expected a comma, not a list item");
                    None
                }
            }
            JsonType::Hash => {
                if f.expect == JsonType::Value {
                    // A colon is the only thing that sets JSON_VALUE, and it
                    // requires a key already pushed.
                    f.expect = JsonType::Comma;
                    let o = self.tree.alloc(type_, Some(c));
                    match &mut self.tree.nodes[c].payload {
                        Payload::Hash(entries) => {
                            entries.last_mut().expect("JSON_VALUE without a key").value = Some(o)
                        }
                        _ => unreachable!(),
                    }
                    Some(o)
                } else if f.expect == JsonType::Key {
                    if type_ != JsonType::String {
                        self.error = Some("Hash key is not a string");
                        return None;
                    }

                    f.expect = JsonType::Colon;
                    let o = self.tree.alloc(type_, Some(c));
                    match &mut self.tree.nodes[c].payload {
                        Payload::Hash(entries) => entries.push(JsonEntry {
                            key: o,
                            value: None,
                        }),
                        _ => unreachable!(),
                    }
                    Some(o)
                } else {
                    self.error = Some("Expected a comma or colon");
                    None
                }
            }
            _ => unreachable!("open container that is not an array or hash"),
        }
    }

    /// `json_read`: the next completed node, or `None` at the end of input or on
    /// an error (then `error` is set).
    pub fn read(&mut self) -> Option<JsonId> {
        self.read_with(None)
    }

    /// `json_read_separators`.
    pub fn read_separators(
        &mut self,
        cb: &mut dyn FnMut(JsonType, &JsonPull<'a>),
    ) -> Option<JsonId> {
        self.read_with(Some(cb))
    }

    fn read_with(&mut self, mut cb: Option<&mut JsonSeparatorCallback<'_, 'a>>) -> Option<JsonId> {
        let mut c: i32;

        // In case there is an error at the top level
        if self.container_stack.is_empty() {
            self.reset_root();
        }

        loop {
            // again:
            c = self.read_wrap();
            if c == EOF {
                if !self.container_stack.is_empty() {
                    self.error = Some("Reached EOF without all containers being closed");
                }

                return None;
            }

            match c {
                /////////////////////////// Byte order mark
                0xEF => {
                    let mut c2 = self.peek();
                    if c2 == 0xBB {
                        self.read_wrap();
                        c2 = self.peek();
                        if c2 == 0xBF {
                            self.read_wrap();
                            continue;
                        }
                    }
                    self.error = Some("Corrupt byte-order mark found");
                    return None;
                }

                /////////////////////////// Whitespace
                0x20 | 0x09 | 0x0D | 0x0A | 0x1E => continue,

                /////////////////////////// Arrays
                0x5B => {
                    // '['
                    let o = self.add_object(JsonType::Array)?;
                    self.container_stack.push(ParseFrame {
                        container: o,
                        expect: JsonType::Item,
                    });

                    if let Some(cb) = cb.as_mut() {
                        cb(JsonType::Array, self);
                    }

                    continue;
                }

                0x5D => {
                    // ']'
                    let f = match self.container_stack.last() {
                        Some(f) => f,
                        None => {
                            self.error = Some("Found ] at top level");
                            return None;
                        }
                    };

                    let cc = f.container;
                    let arr = match &self.tree.nodes[cc].payload {
                        Payload::Array(arr) => arr,
                        _ => {
                            self.error = Some("Found ] not in an array");
                            return None;
                        }
                    };

                    if f.expect != JsonType::Comma
                        && !(f.expect == JsonType::Item && arr.is_empty())
                    {
                        self.error = Some("Found ] without final element");
                        return None;
                    }

                    self.container_stack.pop();
                    return Some(cc);
                }

                /////////////////////////// Hashes
                0x7B => {
                    // '{'
                    let o = self.add_object(JsonType::Hash)?;
                    self.container_stack.push(ParseFrame {
                        container: o,
                        expect: JsonType::Key,
                    });

                    if let Some(cb) = cb.as_mut() {
                        cb(JsonType::Hash, self);
                    }

                    continue;
                }

                0x7D => {
                    // '}'
                    let f = match self.container_stack.last() {
                        Some(f) => f,
                        None => {
                            self.error = Some("Found } at top level");
                            return None;
                        }
                    };

                    let cc = f.container;
                    let entries = match &self.tree.nodes[cc].payload {
                        Payload::Hash(entries) => entries,
                        _ => {
                            self.error = Some("Found } not in a hash");
                            return None;
                        }
                    };

                    if f.expect != JsonType::Comma
                        && !(f.expect == JsonType::Key && entries.is_empty())
                    {
                        self.error = Some("Found } without final element");
                        return None;
                    }

                    self.container_stack.pop();
                    return Some(cc);
                }

                /////////////////////////// Null
                0x6E => {
                    // 'n'
                    if self.read_wrap() != 'u' as i32
                        || self.read_wrap() != 'l' as i32
                        || self.read_wrap() != 'l' as i32
                    {
                        self.error = Some("Found misspelling of null");
                        return None;
                    }

                    return self.add_object(JsonType::Null);
                }

                /////////////////////////// NaN
                0x4E => {
                    // 'N'
                    if self.read_wrap() != 'a' as i32 || self.read_wrap() != 'N' as i32 {
                        self.error = Some("Found misspelling of NaN");
                        return None;
                    }

                    self.error = Some("JSON does not allow NaN");
                    return None;
                }

                /////////////////////////// Infinity
                0x49 => {
                    // 'I'
                    if self.read_wrap() != 'n' as i32
                        || self.read_wrap() != 'f' as i32
                        || self.read_wrap() != 'i' as i32
                        || self.read_wrap() != 'n' as i32
                        || self.read_wrap() != 'i' as i32
                        || self.read_wrap() != 't' as i32
                        || self.read_wrap() != 'y' as i32
                    {
                        self.error = Some("Found misspelling of Infinity");
                        return None;
                    }

                    self.error = Some("JSON does not allow Infinity");
                    return None;
                }

                /////////////////////////// True
                0x74 => {
                    // 't'
                    if self.read_wrap() != 'r' as i32
                        || self.read_wrap() != 'u' as i32
                        || self.read_wrap() != 'e' as i32
                    {
                        self.error = Some("Found misspelling of true");
                        return None;
                    }

                    return self.add_object(JsonType::True);
                }

                /////////////////////////// False
                0x66 => {
                    // 'f'
                    if self.read_wrap() != 'a' as i32
                        || self.read_wrap() != 'l' as i32
                        || self.read_wrap() != 's' as i32
                        || self.read_wrap() != 'e' as i32
                    {
                        self.error = Some("Found misspelling of false");
                        return None;
                    }

                    return self.add_object(JsonType::False);
                }

                /////////////////////////// Comma
                0x2C => {
                    // ','
                    let tree = &self.tree;
                    if let Some(f) = self.container_stack.last_mut() {
                        if f.expect != JsonType::Comma {
                            self.error = Some("Found unexpected comma");
                            return None;
                        }

                        if tree.nodes[f.container].type_ == JsonType::Hash {
                            f.expect = JsonType::Key;
                        } else {
                            f.expect = JsonType::Item;
                        }
                    }

                    if let Some(cb) = cb.as_mut() {
                        cb(JsonType::Comma, self);
                    }

                    continue;
                }

                /////////////////////////// Colon
                0x3A => {
                    // ':'
                    let f = match self.container_stack.last_mut() {
                        Some(f) => f,
                        None => {
                            self.error = Some("Found colon at top level");
                            return None;
                        }
                    };

                    if f.expect != JsonType::Colon {
                        self.error = Some("Found unexpected colon");
                        return None;
                    }

                    f.expect = JsonType::Value;

                    if let Some(cb) = cb.as_mut() {
                        cb(JsonType::Colon, self);
                    }

                    continue;
                }

                /////////////////////////// Numbers
                0x2D | 0x30..=0x39 => {
                    // '-', '0' to '9'
                    return self.read_number(c);
                }

                /////////////////////////// Strings
                0x22 => {
                    // '"'
                    return self.read_string();
                }

                _ => {}
            }

            self.error = Some("Found unexpected character");
            return None;
        }
    }

    fn read_number(&mut self, mut c: i32) -> Option<JsonId> {
        let is_digit = |c: i32| c >= '0' as i32 && c <= '9' as i32;

        self.number_buffer.clear();
        let mut decimal = false;

        if c == '-' as i32 {
            self.number_buffer.push(c as u8 as char);
            c = self.read_wrap();
        }

        if c == '0' as i32 {
            self.number_buffer.push(c as u8 as char);
        } else if c >= '1' as i32 && c <= '9' as i32 {
            self.number_buffer.push(c as u8 as char);
            c = self.peek();

            while is_digit(c) {
                let d = self.read_wrap();
                self.number_buffer.push(d as u8 as char);
                c = self.peek();
            }
        }

        if self.peek() == '.' as i32 {
            let d = self.read_wrap();
            self.number_buffer.push(d as u8 as char);
            decimal = true;

            c = self.peek();
            if !is_digit(c) {
                self.error = Some("Decimal point without digits");
                return None;
            }
            while is_digit(c) {
                let d = self.read_wrap();
                self.number_buffer.push(d as u8 as char);
                c = self.peek();
            }
        }

        c = self.peek();
        if c == 'e' as i32 || c == 'E' as i32 {
            let d = self.read_wrap();
            self.number_buffer.push(d as u8 as char);
            decimal = true;

            c = self.peek();
            if c == '+' as i32 || c == '-' as i32 {
                let d = self.read_wrap();
                self.number_buffer.push(d as u8 as char);
            }

            c = self.peek();
            if !is_digit(c) {
                self.error = Some("Exponent without digits");
                return None;
            }
            while is_digit(c) {
                let d = self.read_wrap();
                self.number_buffer.push(d as u8 as char);
                c = self.peek();
            }
        }

        let n = self.add_object(JsonType::Number);
        if let Some(n) = n {
            let d = atof(&self.number_buffer);
            self.tree.set_number(n, d);

            const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;
            const MIN_SAFE_INTEGER: f64 = -9007199254740991.0;

            if !decimal && d > MAX_SAFE_INTEGER {
                set_errno(0);
                let (ull, used) = strtoull(&self.number_buffer, 10);
                if errno() == 0 && used == self.number_buffer.len() {
                    self.tree.set_large_unsigned(n, ull);
                }
            }
            if !decimal && d < MIN_SAFE_INTEGER {
                set_errno(0);
                let (ll, used) = strtoll(&self.number_buffer, 10);
                if errno() == 0 && used == self.number_buffer.len() {
                    self.tree.set_large_signed(n, ll);
                }
            }
        }
        n
    }

    fn read_string(&mut self) -> Option<JsonId> {
        // Reused across tokens; see JsonPull::string_buffer.
        let mut val = std::mem::take(&mut self.string_buffer);
        val.clear();
        let ret = self.read_string_into(&mut val);
        self.string_buffer = val;
        ret
    }

    fn read_string_into(&mut self, val: &mut Vec<u8>) -> Option<JsonId> {
        fn push_surrogate(val: &mut Vec<u8>, surrogate: i32) {
            val.push((0xE0 | (surrogate >> 12)) as u8);
            val.push((0x80 | ((surrogate >> 6) & 0x3F)) as u8);
            val.push((0x80 | (surrogate & 0x3F)) as u8);
        }

        let mut c: i32;
        let mut surrogate: i32 = -1;
        loop {
            c = self.read_wrap();
            if c == EOF {
                break;
            }

            if c == '"' as i32 {
                if surrogate >= 0 {
                    push_surrogate(val, surrogate);
                }

                break;
            } else if c == '\\' as i32 {
                c = self.read_wrap();

                if c == 'u' as i32 {
                    let mut hex = [0u8; 4];
                    for h in hex.iter_mut() {
                        // `char hex[]`: EOF and bytes from 0x80 up are not hex digits
                        let x = self.read_wrap();
                        if !(x >= 0 && (x as u8).is_ascii_hexdigit()) {
                            self.error = Some("Invalid \\u hex character");
                            return None;
                        }
                        *h = x as u8;
                    }

                    let mut ch: u64 = 0;
                    for &h in &hex {
                        ch = ch * 16 + (h as char).to_digit(16).unwrap() as u64;
                    }
                    if (0xd800..=0xdbff).contains(&ch) {
                        if surrogate < 0 {
                            surrogate = ch as i32;
                        } else {
                            // Impossible surrogate, so output the first half,
                            // keep what might be a legitimate new first half.
                            push_surrogate(val, surrogate);
                            surrogate = ch as i32;
                        }
                        continue;
                    } else if (0xdc00..=0xdfff).contains(&ch) && surrogate >= 0 {
                        let c1 = (surrogate - 0xd800) as u64;
                        let c2 = ch - 0xdc00;
                        ch = ((c1 << 10) | c2) + 0x010000;
                        surrogate = -1;
                    }

                    if surrogate >= 0 {
                        push_surrogate(val, surrogate);
                        surrogate = -1;
                    }

                    if ch <= 0x7F {
                        val.push(ch as u8);
                    } else if ch <= 0x7FF {
                        val.push((0xC0 | (ch >> 6)) as u8);
                        val.push((0x80 | (ch & 0x3F)) as u8);
                    } else if ch <= 0xFFFF {
                        val.push((0xE0 | (ch >> 12)) as u8);
                        val.push((0x80 | ((ch >> 6) & 0x3F)) as u8);
                        val.push((0x80 | (ch & 0x3F)) as u8);
                    } else {
                        // Only reachable for a code point assembled from a
                        // surrogate pair above, since `ch` on its own comes
                        // from four hex digits and so cannot exceed 0xFFFF.
                        val.push((0xF0 | (ch >> 18)) as u8);
                        val.push((0x80 | ((ch >> 12) & 0x3F)) as u8);
                        val.push((0x80 | ((ch >> 6) & 0x3F)) as u8);
                        val.push((0x80 | (ch & 0x3F)) as u8);
                    }
                } else {
                    if surrogate >= 0 {
                        push_surrogate(val, surrogate);
                        surrogate = -1;
                    }

                    if c == '"' as i32 {
                        val.push(b'"');
                    } else if c == '\\' as i32 {
                        val.push(b'\\');
                    } else if c == '/' as i32 {
                        val.push(b'/');
                    } else if c == 'b' as i32 {
                        val.push(0x08);
                    } else if c == 'f' as i32 {
                        val.push(0x0C);
                    } else if c == 'n' as i32 {
                        val.push(b'\n');
                    } else if c == 'r' as i32 {
                        val.push(b'\r');
                    } else if c == 't' as i32 {
                        val.push(b'\t');
                    } else {
                        self.error = Some("Found backslash followed by unknown character");
                        return None;
                    }
                }
            } else if c < ' ' as i32 {
                self.error = Some("Found control character in string");
                return None;
            } else {
                if surrogate >= 0 {
                    push_surrogate(val, surrogate);
                    surrogate = -1;
                }

                val.push(c as u8);
            }
        }
        if c == EOF {
            self.error = Some("String without closing quote mark");
            return None;
        }

        let s = self.add_object(JsonType::String);
        if let Some(s) = s {
            self.tree.set_string(s, val);
        }
        s
    }

    /// `json_read_tree`: drains the next top-level value and hands it over.
    pub fn read_tree(&mut self) -> Option<JsonTree> {
        while let Some(j) = self.read() {
            if self.tree.nodes[j].parent.is_none() {
                // Everything in the parser's tree belongs to this value.
                return Some(std::mem::take(&mut self.tree));
            }
        }

        None
    }

    /// The tree the parser is currently building.
    pub fn tree(&self) -> &JsonTree {
        &self.tree
    }

    pub fn get(&self, id: JsonId) -> JsonRef<'_> {
        self.tree.get(id)
    }

    /// `jp->root`, the most recently completed or in-progress top-level value.
    pub fn root(&self) -> Option<JsonRef<'_>> {
        self.tree.root.map(|r| self.tree.get(r))
    }

    /// `jp->root.reset()`.
    pub fn clear_root(&mut self) {
        self.reset_root();
    }

    /// `json_free`: splices the node out of its parent, or clears the root.
    pub fn free(&mut self, id: JsonId) {
        if self.tree.nodes[id].parent.is_none() {
            // Top-level value: the parser owns it via root.
            if self.tree.root == Some(id) {
                self.reset_root();
            }
            return;
        }

        self.tree.free(id);
    }

    /// `json_disconnect`: splices the node out and hands it over.
    pub fn disconnect(&mut self, id: JsonId) -> JsonTree {
        if self.tree.nodes[id].parent.is_none() {
            if self.tree.root == Some(id) {
                self.container_stack.clear();
                return std::mem::take(&mut self.tree);
            }
            return JsonTree::default();
        }

        self.tree.disconnect(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A high surrogate followed by a non-surrogate used to be combined into one
    // code point, because the range check tested the outer-loop byte instead of
    // the parsed code unit. The stale surrogate should come out as standalone
    // CESU-8, then U+E000 encoded normally.
    #[test]
    fn surrogate_pair_regression() {
        let mut jp = JsonPull::begin_string(b"\"\\uD83D\\uE000\"");
        let o = jp.read_tree();

        assert!(jp.error.is_none());
        let o = o.expect("tree");
        assert_eq!(o.root().type_(), JsonType::String);

        let expected = b"\xED\xA0\xBD\xEE\x80\x80";
        assert_eq!(o.root().raw_string_bytes(), expected);

        // Sanity check: the buggy output (a single 4-byte UTF-8 sequence for
        // U+1F400) must not be what we got.
        let buggy = b"\xF0\x9F\x90\x80";
        assert_ne!(o.root().raw_string_bytes(), buggy);
    }

    // What geojson-loop does: free each feature once serialized, so they do not
    // accumulate while the rest of the document is parsed.
    #[test]
    fn free_prunes_a_subtree_from_its_parent() {
        let mut jp = JsonPull::begin_string(b"[[1, 2], [3, 4], [5, 6]]");

        let mut outer = None;
        let mut arrays_seen = 0;

        while let Some(j) = jp.read() {
            if jp.get(j).type_() != JsonType::Array {
                continue;
            }
            arrays_seen += 1;
            if arrays_seen == 2 {
                // This is [3, 4]; verify, then ask the parser to drop it.
                let a = jp.get(j).array();
                assert_eq!(a.len(), 2);
                assert_eq!(a[0].number(), 3.0);
                assert_eq!(a[1].number(), 4.0);
                jp.free(j);
            } else if jp.get(j).parent().is_none() {
                // The completed outer array; the parser still owns it
                // via its root, so the id stays valid.
                outer = Some(j);
                break;
            }
        }

        let outer = jp.get(outer.expect("outer"));
        assert_eq!(outer.type_(), JsonType::Array);
        assert_eq!(outer.array_len(), 2);

        // First surviving element: [1, 2].
        assert_eq!(outer.array_get(0).type_(), JsonType::Array);
        assert_eq!(outer.array_get(0).array_len(), 2);
        assert_eq!(outer.array_get(0).array_get(0).number(), 1.0);
        assert_eq!(outer.array_get(0).array_get(1).number(), 2.0);

        // Second surviving element (previously third): [5, 6].
        assert_eq!(outer.array_get(1).type_(), JsonType::Array);
        assert_eq!(outer.array_get(1).array_len(), 2);
        assert_eq!(outer.array_get(1).array_get(0).number(), 5.0);
        assert_eq!(outer.array_get(1).array_get(1).number(), 6.0);
    }

    // A top-level value has no parent, so free has to drop the parser's
    // reference instead.
    #[test]
    fn free_releases_a_top_level_value_held_by_the_parser() {
        let mut jp = JsonPull::begin_string(br#"{"a": 1, "b": [2, 3]}"#);

        // read streams atoms first (1, 2, 3, [2,3], ...); the top-level
        // hash is returned by the final `}` token.
        let mut top = None;
        while let Some(j) = jp.read() {
            if jp.get(j).parent().is_none() {
                top = Some(j);
                break;
            }
        }

        let top = top.expect("top");
        assert_eq!(jp.get(top).type_(), JsonType::Hash);
        assert_eq!(jp.root().map(|r| r.id()), Some(top));

        jp.free(top);

        assert!(jp.root().is_none());
    }

    #[test]
    fn disconnect_hands_a_subtree_to_the_caller() {
        let mut taken = None;
        let mut outer = None;

        let mut jp = JsonPull::begin_string(b"[[1, 2], [3, 4]]");

        let mut arrays_seen = 0;
        while let Some(j) = jp.read() {
            if jp.get(j).type_() != JsonType::Array {
                continue;
            }
            arrays_seen += 1;
            if arrays_seen == 2 {
                // This is [3, 4]; take it away from the enclosing array.
                let t = jp.disconnect(j);
                assert!(!t.is_empty());
                taken = Some(t);
            } else if jp.get(j).parent().is_none() {
                outer = Some(j);
                break;
            }
        }

        // The outer array is left holding only the element we didn't take,
        // and the parser is still the owner of it.
        let outer = outer.expect("outer");
        assert_eq!(jp.root().map(|r| r.id()), Some(outer));
        let o = jp.get(outer);
        assert_eq!(o.array_len(), 1);
        assert_eq!(o.array_get(0).array_len(), 2);
        assert_eq!(o.array_get(0).array_get(0).number(), 1.0);
        assert_eq!(o.array_get(0).array_get(1).number(), 2.0);

        // The detached root no longer points out at the array it was spliced
        // from, but the parent links *within* it are left intact so the tree
        // stays navigable upwards.
        let taken = taken.expect("taken");
        let t = taken.root();
        assert!(t.parent().is_none());
        assert_eq!(t.array_get(0).parent(), Some(t));
        assert_eq!(t.array_get(1).parent(), Some(t));

        // So the subtree stays valid once the parser, and the tree the parser
        // still owns, are destroyed.
        drop(jp);

        let t = taken.root();
        assert_eq!(t.type_(), JsonType::Array);
        assert_eq!(t.array_len(), 2);
        assert_eq!(t.array_get(0).number(), 3.0);
        assert_eq!(t.array_get(1).number(), 4.0);
    }

    // free finds a node's owner through its parent, so clearing the interior
    // parent links on detach would make this a silent no-op.
    #[test]
    fn free_prunes_an_interior_node_of_a_detached_tree() {
        let mut jp = JsonPull::begin_string(b"[[1, 2], [3, 4], [5, 6]]");
        let mut tree = jp.read_tree().expect("tree");

        assert_eq!(tree.root().type_(), JsonType::Array);
        assert_eq!(tree.root().array_len(), 3);

        // Destroy the parser first, so this is unambiguously operating on a
        // tree that no longer has one.
        drop(jp);

        let drop_id = tree.root().array_get(1).id();
        assert_eq!(tree.get(drop_id).array_get(0).number(), 3.0);
        assert_eq!(tree.get(drop_id).parent(), Some(tree.root()));

        tree.free(drop_id);

        assert_eq!(tree.root().array_len(), 2);
        assert_eq!(tree.root().array_get(0).array_get(0).number(), 1.0);
        assert_eq!(tree.root().array_get(1).array_get(0).number(), 5.0);
    }

    // Not a removal: the key stays with a JSON_NULL stand-in so the surrounding
    // pairs keep their alignment, and the entry goes only when both halves do.
    #[test]
    fn free_of_a_hash_value_leaves_a_null_placeholder() {
        let mut jp = JsonPull::begin_string(br#"{"keep": 1, "drop": [2, 3]}"#);
        let mut tree = jp.read_tree().expect("tree");

        assert_eq!(tree.root().entries().len(), 2);

        let drop_ref = tree.root().hash_get("drop").expect("drop");
        assert_eq!(drop_ref.type_(), JsonType::Array);
        let drop_id = drop_ref.id();

        tree.free(drop_id);

        // The key survives, now paired with a null rather than the array.
        assert_eq!(tree.root().entries().len(), 2);
        let after = tree.root().hash_get("drop").expect("after");
        assert_eq!(after.type_(), JsonType::Null);

        // The neighbouring pair is untouched.
        assert_eq!(tree.root().hash_get("keep").unwrap().number(), 1.0);
    }

    // The array-splicing fix: every survivor keeps its identity, in order.
    #[test]
    fn free_prunes_a_non_final_element() {
        let mut jp = JsonPull::begin_string(b"[[1], [2], [3], [4], [5], [6], [7], [8]]");
        let mut tree = jp.read_tree().expect("tree");

        assert_eq!(tree.root().type_(), JsonType::Array);
        assert_eq!(tree.root().array_len(), 8);

        let first = tree.root().array_get(0).id();
        tree.free(first);

        let root = tree.root();
        assert_eq!(root.array_len(), 7);
        for i in 0..root.array_len() {
            let e = root.array_get(i);
            assert_eq!(e.type_(), JsonType::Array);
            assert_eq!(e.array_len(), 1);
            assert_eq!(e.array_get(0).number(), (i + 2) as f64);

            if i + 1 < root.array_len() {
                assert_ne!(e, root.array_get(i + 1));
            }
        }
    }

    // The mirror of the value case above.
    #[test]
    fn free_of_a_hash_key_then_of_both_halves() {
        let mut jp = JsonPull::begin_string(br#"{"a": 1, "b": 2, "c": 3}"#);
        let mut tree = jp.read_tree().expect("tree");

        assert_eq!(tree.root().entries().len(), 3);

        // Free the key of the middle pair. The entry stays, with a null key,
        // and its value is still reachable positionally.
        let key = tree.root().entries()[1].0.id();
        tree.free(key);

        assert_eq!(tree.root().entries().len(), 3);
        assert_eq!(tree.root().entries()[1].0.type_(), JsonType::Null);
        assert_eq!(tree.root().entries()[1].1.number(), 2.0);

        // The neighbours are untouched, and the now-keyless pair is no longer
        // findable by name.
        assert!(tree.root().hash_get("b").is_none());
        assert_eq!(tree.root().hash_get("a").unwrap().number(), 1.0);
        assert_eq!(tree.root().hash_get("c").unwrap().number(), 3.0);

        // Freeing the other half too retires the whole entry.
        let value = tree.root().entries()[1].1.id();
        tree.free(value);

        assert_eq!(tree.root().entries().len(), 2);
        assert_eq!(tree.root().hash_get("a").unwrap().number(), 1.0);
        assert_eq!(tree.root().hash_get("c").unwrap().number(), 3.0);
    }

    // What the filter loaders and -L / -E do. Each detached tree has to survive
    // the next read_tree call and the parser's destruction.
    #[test]
    fn repeated_read_tree_on_a_line_delimited_stream() {
        let mut jp = JsonPull::begin_string(b"{\"n\": 1}\n{\"n\": 2}\n{\"n\": 3}\n");

        let mut trees = Vec::new();
        for i in 0..3 {
            let t = jp.read_tree().expect("tree");
            assert_eq!(t.root().type_(), JsonType::Hash);
            // Reading the next tree must not disturb the ones already taken.
            assert_eq!(t.root().hash_get("n").unwrap().number(), (i + 1) as f64);
            trees.push(t);
        }

        assert!(jp.read_tree().is_none());

        // All three outlive the parser they came from.
        drop(jp);
        for (i, t) in trees.iter().enumerate() {
            assert_eq!(t.root().hash_get("n").unwrap().number(), (i + 1) as f64);
        }
    }

    // What json_context() prints on the error paths: a hash whose last key has no
    // value yet renders that slot as "...".
    #[test]
    fn stringify_of_a_partially_parsed_tree() {
        let mut jp = JsonPull::begin_string(b"{\"a\": [1, 2], \"b\":");

        // Read until the parser runs out of input mid-hash.
        while jp.read().is_some() {}
        assert!(jp.error.is_some());
        assert!(jp.root().is_some());

        let s = jp.root().unwrap().stringify();
        assert_eq!(s, "{\"a\":[1,2],\"b\":...}");
    }

    // Values hold every byte, so an embedded NUL is legal and stringify has to
    // walk past it.
    #[test]
    fn stringify_keeps_text_after_an_embedded_nul() {
        let mut jp = JsonPull::begin_string(b"\"a\\u0000b\"");
        let o = jp.read_tree().expect("tree");

        assert_eq!(o.root().type_(), JsonType::String);
        assert_eq!(o.root().string().len(), 3);

        assert_eq!(o.root().stringify(), "\"a\\u0000b\"");
    }

    // U+FFFF used to fall through the `< 0xFFFF` test into the four-byte branch
    // and come out as the overlong F0 8F BF BF, which check_utf8 does not catch.
    #[test]
    fn encodes_u_ffff_as_three_bytes() {
        let mut jp = JsonPull::begin_string(b"\"a\\uFFFFb\"");
        let o = jp.read_tree();

        assert!(jp.error.is_none());
        let o = o.expect("tree");
        assert_eq!(o.root().type_(), JsonType::String);

        assert_eq!(o.root().raw_string_bytes(), b"a\xEF\xBF\xBFb");
        assert_ne!(o.root().raw_string_bytes(), b"a\xF0\x8F\xBF\xBFb");

        // The boundary below it, and a genuine supplementary code point built
        // from a surrogate pair, both keep their existing encodings.
        let mut jp2 = JsonPull::begin_string(b"\"\\uFFFE\"");
        let o2 = jp2.read_tree().expect("tree");
        assert_eq!(o2.root().raw_string_bytes(), b"\xEF\xBF\xBE");

        let mut jp3 = JsonPull::begin_string(b"\"\\uD83D\\uDC00\"");
        let o3 = jp3.read_tree().expect("tree");
        assert_eq!(o3.root().raw_string_bytes(), b"\xF0\x9F\x90\x80");
        assert_eq!(o3.root().string(), "\u{1F400}");
    }

    #[test]
    fn numbers() {
        let mut jp = JsonPull::begin_string(
            b"[0, -0, 1.5e3, 9007199254740993, -9007199254740993, 18446744073709551616, 12345678901234567890]",
        );
        let t = jp.read_tree().expect("tree");
        let a = t.root().array();
        assert_eq!(a[0].number(), 0.0);
        assert!(a[1].number().is_sign_negative());
        assert_eq!(a[2].number(), 1500.0);
        assert_eq!(a[3].large_unsigned(), 9007199254740993);
        assert_eq!(a[4].large_signed(), -9007199254740993);
        assert_eq!(a[5].large_unsigned(), 0);
        assert_eq!(a[6].large_unsigned(), 12345678901234567890);
        assert_eq!(
            t.root().stringify(),
            "[0,0,1500,9007199254740993,-9007199254740993,18446744073709553000,12345678901234567890]"
        );
    }

    #[test]
    fn errors_and_lines() {
        let cases: &[(&[u8], &str, i32)] = &[
            (b"[1,\n2,\n]", "Found ] without final element", 3),
            (b"{\"a\" 1}", "Expected a comma or colon", 1),
            (b"{1:2}", "Hash key is not a string", 1),
            (b"[1 2]", "Expected a comma, not a list item", 1),
            (b"nul", "Found misspelling of null", 1),
            (b"NaN", "JSON does not allow NaN", 1),
            (b"Infinity", "JSON does not allow Infinity", 1),
            (b"\"abc", "String without closing quote mark", 1),
            (
                b"\"a\\x\"",
                "Found backslash followed by unknown character",
                1,
            ),
            (b"\"a\\u12g4\"", "Invalid \\u hex character", 1),
            (b"\"a\tb\"", "Found control character in string", 1),
            (b"1.", "Decimal point without digits", 1),
            (b"1e+", "Exponent without digits", 1),
            (b"\xEF\xBB", "Corrupt byte-order mark found", 1),
            (b":", "Found colon at top level", 1),
            (b"]", "Found ] at top level", 1),
            (
                b"[\n\n",
                "Reached EOF without all containers being closed",
                3,
            ),
            (b"@", "Found unexpected character", 1),
        ];
        for (input, error, line) in cases {
            let mut jp = JsonPull::begin_string(input);
            while jp.read().is_some() {}
            assert_eq!(
                jp.error,
                Some(*error),
                "{:?}",
                String::from_utf8_lossy(input)
            );
            assert_eq!(jp.line, *line, "{:?}", String::from_utf8_lossy(input));
        }
    }

    #[test]
    fn separators() {
        let mut jp = JsonPull::begin_string(br#"{"a": [1, 2]}"#);
        let mut seen = Vec::new();
        while jp.read_separators(&mut |t, _| seen.push(t)).is_some() {}
        assert_eq!(
            seen,
            vec![
                JsonType::Hash,
                JsonType::Colon,
                JsonType::Array,
                JsonType::Comma
            ]
        );
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut input = b"{\"type\": \"FeatureCollection\", \"features\": [".to_vec();
        for i in 0..1000 {
            if i != 0 {
                input.push(b',');
            }
            input.extend_from_slice(
                br#"{"type": "Feature", "properties": {"a": 1}, "geometry": {"type": "Point", "coordinates": [1, 2]}}"#,
            );
        }
        input.extend_from_slice(b"]}");

        let mut jp = JsonPull::begin_string(&input);
        while let Some(j) = jp.read() {
            if jp.get(j).hash_get("type").map(|t| t.string()) == Some("Feature") {
                jp.free(j);
            }
        }
        assert!(jp.error.is_none());
        assert!(jp.tree().nodes.len() < 40);
    }
}
