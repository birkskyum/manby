//! A JSON pull parser with the semantics of tippecanoe's jsonpull, made for
//! speed: it parses straight from memory, keeps each top-level value in a
//! reusable arena, and borrows strings from the input when they have no escapes.
//!
//! What it accepts, where it stops with which error, the values of numbers
//! (including jsonpull's handling of integers beyond 2^53), the bytes of
//! strings, and the layout of the tree it builds (parents, the order of hash
//! entries, the placeholders that freeing leaves behind) all match jsonpull.

use tippecanoe::milo::dtoa_milo;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hash,
    Array,
    Number,
    String,
    True,
    False,
    Null,
}

/// How a number is held: jsonpull keeps integers beyond the safe range of a
/// double exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Double(f64),
    LargeUnsigned(u64),
    LargeSigned(i64),
}

#[derive(Clone, Copy, Debug)]
struct Node {
    kind: Kind,
    /// Open containers are still being parsed; their children are in `Doc::open`.
    open: bool,
    /// The string is in `Doc::strbuf` rather than in the input.
    owned: bool,
    parent: u32,
    /// String: offset into the input or `strbuf`. Closed container: offset of
    /// its children in `Doc::children`. Open container: its depth.
    start: usize,
    len: u32,
    num: Num,
    /// For containers: `children` and `strbuf` lengths when they opened, so
    /// that freeing them can give the space back.
    mark_children: usize,
    mark_strbuf: usize,
}

impl Node {
    fn new(kind: Kind, parent: u32) -> Node {
        Node {
            kind,
            open: false,
            owned: false,
            parent,
            start: 0,
            len: 0,
            num: Num::Double(0.0),
            mark_children: 0,
            mark_strbuf: 0,
        }
    }
}

/// What an open container expects next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Item,
    Comma,
    Key,
    Colon,
    Value,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    container: u32,
    expect: Expect,
}

/// The tree of the current top-level value.
pub struct Doc<'a> {
    input: &'a [u8],
    nodes: Vec<Node>,
    /// Children of closed containers. A hash's children are key and value
    /// pairs, with `NONE` for a value still being parsed.
    children: Vec<u32>,
    /// Children of the open containers, by depth.
    open: Vec<Vec<u32>>,
    strbuf: Vec<u8>,
    pub root: u32,
}

/// A node of the tree, the counterpart of jsonpull's `json_object *`.
#[derive(Clone, Copy)]
pub struct Ref<'d, 'a> {
    doc: &'d Doc<'a>,
    pub id: u32,
}

impl<'d, 'a> Ref<'d, 'a> {
    fn node(&self) -> &'d Node {
        &self.doc.nodes[self.id as usize]
    }

    pub fn kind(&self) -> Kind {
        self.node().kind
    }

    pub fn parent(&self) -> Option<Ref<'d, 'a>> {
        let p = self.node().parent;
        (p != NONE).then_some(Ref {
            doc: self.doc,
            id: p,
        })
    }

    /// The bytes of a string as jsonpull stores them.
    pub fn bytes(&self) -> &'d [u8] {
        let n = self.node();
        debug_assert_eq!(n.kind, Kind::String);
        let src: &'d [u8] = if n.owned {
            &self.doc.strbuf
        } else {
            self.doc.input
        };
        &src[n.start..n.start + n.len as usize]
    }

    pub fn num(&self) -> Num {
        self.node().num
    }

    /// `json_object::number()`
    pub fn number(&self) -> f64 {
        match self.node().num {
            Num::Double(d) => d,
            Num::LargeUnsigned(u) => u as f64,
            Num::LargeSigned(s) => s as f64,
        }
    }

    pub fn large_unsigned(&self) -> u64 {
        match self.node().num {
            Num::LargeUnsigned(u) => u,
            _ => 0,
        }
    }

    pub fn large_signed(&self) -> i64 {
        match self.node().num {
            Num::LargeSigned(s) => s,
            _ => 0,
        }
    }

    fn child_slice(&self) -> &'d [u32] {
        let n = self.node();
        if n.open {
            &self.doc.open[n.start]
        } else {
            &self.doc.children[n.start..n.start + n.len as usize]
        }
    }

    fn get(&self, id: u32) -> Ref<'d, 'a> {
        Ref { doc: self.doc, id }
    }

    /// The elements of an array.
    pub fn array(&self) -> impl ExactSizeIterator<Item = Ref<'d, 'a>> + 'd {
        let doc = self.doc;
        self.child_slice().iter().map(move |&id| Ref { doc, id })
    }

    pub fn array_len(&self) -> usize {
        self.child_slice().len()
    }

    pub fn array_get(&self, i: usize) -> Ref<'d, 'a> {
        self.get(self.child_slice()[i])
    }

    /// The key and value pairs of a hash whose values are complete, in order.
    pub fn entries(&self) -> impl Iterator<Item = (Ref<'d, 'a>, Ref<'d, 'a>)> + 'd {
        let doc = self.doc;
        self.child_slice()
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|kv| kv[1] != NONE)
            .map(move |kv| (Ref { doc, id: kv[0] }, Ref { doc, id: kv[1] }))
    }

    /// `json_hash_get`: the value of the first entry with this key, or None if
    /// this is not a hash, the key is absent, or its value is still being parsed.
    pub fn hash_get(&self, key: &[u8]) -> Option<Ref<'d, 'a>> {
        if self.node().kind != Kind::Hash {
            return None;
        }
        for kv in self.child_slice().as_chunks::<2>().0 {
            let k = self.get(kv[0]);
            if k.kind() == Kind::String && k.bytes() == key {
                return (kv[1] != NONE).then(|| self.get(kv[1]));
            }
        }
        None
    }

    /// `json_stringify`
    pub fn stringify(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.print(&mut out);
        out
    }

    fn print(&self, val: &mut Vec<u8>) {
        match self.kind() {
            Kind::Hash => {
                val.push(b'{');
                let c = self.child_slice();
                let n = c.len() / 2;
                for i in 0..n {
                    self.get(c[2 * i]).print(val);
                    val.push(b':');
                    if c[2 * i + 1] == NONE {
                        // Hash value in incompletely read hash
                        val.extend_from_slice(b"...");
                    } else {
                        self.get(c[2 * i + 1]).print(val);
                    }
                    if i + 1 < n {
                        val.push(b',');
                    }
                }
                val.push(b'}');
            }
            Kind::Array => {
                val.push(b'[');
                let c = self.child_slice();
                for (i, &e) in c.iter().enumerate() {
                    self.get(e).print(val);
                    if i + 1 < c.len() {
                        val.push(b',');
                    }
                }
                val.push(b']');
            }
            Kind::String => {
                val.push(b'"');
                for &c in self.bytes() {
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
            Kind::Number => match self.num() {
                Num::LargeSigned(s) => val.extend_from_slice(s.to_string().as_bytes()),
                Num::LargeUnsigned(u) => val.extend_from_slice(u.to_string().as_bytes()),
                Num::Double(d) => val.extend_from_slice(dtoa_milo(d).as_bytes()),
            },
            Kind::Null => val.extend_from_slice(b"null"),
            Kind::True => val.extend_from_slice(b"true"),
            Kind::False => val.extend_from_slice(b"false"),
        }
    }
}

impl<'a> Doc<'a> {
    fn new(input: &'a [u8]) -> Doc<'a> {
        Doc {
            input,
            nodes: Vec::new(),
            children: Vec::new(),
            open: Vec::new(),
            strbuf: Vec::new(),
            root: NONE,
        }
    }

    pub fn get(&self, id: u32) -> Ref<'_, 'a> {
        Ref { doc: self, id }
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.children.clear();
        self.strbuf.clear();
        for o in self.open.iter_mut() {
            o.clear();
        }
        self.root = NONE;
    }

    fn alloc(&mut self, kind: Kind, parent: u32) -> u32 {
        self.nodes.push(Node::new(kind, parent));
        (self.nodes.len() - 1) as u32
    }
}

const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;
const MIN_SAFE_INTEGER: f64 = -9007199254740991.0;

/// The parser, over one byte range of the input.
pub struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    pub line: i32,
    pub error: Option<&'static str>,
    pub doc: Doc<'a>,
    stack: Vec<Frame>,
    numbuf: Vec<u8>,
}

#[inline]
fn is_digit(c: i32) -> bool {
    (b'0' as i32..=b'9' as i32).contains(&c)
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a [u8]) -> Parser<'a> {
        Parser {
            input,
            pos: 0,
            line: 1,
            error: None,
            doc: Doc::new(input),
            stack: Vec::new(),
            numbuf: Vec::new(),
        }
    }

    /// Whether the parser has used up its input.
    pub fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// Whether any container is still open.
    pub fn in_container(&self) -> bool {
        !self.stack.is_empty()
    }

    #[inline]
    fn peek(&self) -> i32 {
        match self.input.get(self.pos) {
            Some(&c) => c as i32,
            None => -1,
        }
    }

    #[inline]
    fn read(&mut self) -> i32 {
        match self.input.get(self.pos) {
            Some(&c) => {
                self.pos += 1;
                if c == b'\n' {
                    self.line += 1;
                }
                c as i32
            }
            None => -1,
        }
    }

    /// `jp->root.reset()`
    pub fn clear_root(&mut self) {
        self.doc.clear();
    }

    fn add_object(&mut self, kind: Kind) -> Option<u32> {
        let f = match self.stack.last_mut() {
            Some(f) => f,
            None => {
                // Replacing the parser's root destroys the previous top-level value.
                self.doc.clear();
                let o = self.doc.alloc(kind, NONE);
                self.doc.root = o;
                return Some(o);
            }
        };
        let c = f.container;
        let depth = self.doc.nodes[c as usize].start;

        match self.doc.nodes[c as usize].kind {
            Kind::Array => {
                if f.expect == Expect::Item {
                    f.expect = Expect::Comma;
                    let o = self.doc.alloc(kind, c);
                    self.doc.open[depth].push(o);
                    Some(o)
                } else {
                    self.error = Some("Expected a comma, not a list item");
                    None
                }
            }
            _ => {
                if f.expect == Expect::Value {
                    f.expect = Expect::Comma;
                    let o = self.doc.alloc(kind, c);
                    *self.doc.open[depth].last_mut().unwrap() = o;
                    Some(o)
                } else if f.expect == Expect::Key {
                    if kind != Kind::String {
                        self.error = Some("Hash key is not a string");
                        return None;
                    }
                    f.expect = Expect::Colon;
                    let o = self.doc.alloc(kind, c);
                    let open = &mut self.doc.open[depth];
                    open.push(o);
                    open.push(NONE);
                    Some(o)
                } else {
                    self.error = Some("Expected a comma or colon");
                    None
                }
            }
        }
    }

    fn open_container(&mut self, kind: Kind, expect: Expect) -> Option<()> {
        let o = self.add_object(kind)?;
        let depth = self.stack.len();
        if self.doc.open.len() <= depth {
            self.doc.open.push(Vec::new());
        }
        self.doc.open[depth].clear();
        let mark_children = self.doc.children.len();
        let mark_strbuf = self.doc.strbuf.len();
        let n = &mut self.doc.nodes[o as usize];
        n.open = true;
        n.start = depth;
        n.mark_children = mark_children;
        n.mark_strbuf = mark_strbuf;
        self.stack.push(Frame {
            container: o,
            expect,
        });
        Some(())
    }

    fn close_container(&mut self) -> u32 {
        let f = self.stack.pop().unwrap();
        let depth = self.stack.len();
        let start = self.doc.children.len();
        let len = self.doc.open[depth].len();
        let Doc { open, children, .. } = &mut self.doc;
        children.extend_from_slice(&open[depth]);
        open[depth].clear();
        let n = &mut self.doc.nodes[f.container as usize];
        n.open = false;
        n.start = start;
        n.len = len as u32;
        f.container
    }

    /// `json_free` of the node that `read` just returned: splices it out of
    /// its parent, leaving a null placeholder if it was a hash value, or
    /// clears the root. Gives back the space of everything inside it.
    pub fn free_last(&mut self, id: u32) {
        let n = self.doc.nodes[id as usize];
        if n.parent == NONE {
            if self.doc.root == id {
                self.doc.clear();
            }
            return;
        }

        let p = self.doc.nodes[n.parent as usize];
        debug_assert!(p.open);
        let depth = p.start;
        let (mark_children, mark_strbuf) = match n.kind {
            Kind::Hash | Kind::Array => (n.mark_children, n.mark_strbuf),
            _ => (self.doc.children.len(), self.doc.strbuf.len()),
        };
        self.doc.nodes.truncate(id as usize);
        self.doc.children.truncate(mark_children);
        self.doc.strbuf.truncate(mark_strbuf);

        let open = &mut self.doc.open[depth];
        if p.kind == Kind::Array {
            debug_assert_eq!(open.last(), Some(&id));
            open.pop();
        } else {
            // The freed node was the value of the last entry; a null takes its place.
            let ph = self.doc.alloc(Kind::Null, n.parent);
            *self.doc.open[depth].last_mut().unwrap() = ph;
        }
    }

    /// `json_read`, but returning only completed hashes, which are all that the
    /// GeoJSON reader looks at. None at the end or on an error (see `error`).
    pub fn read_hash(&mut self) -> Option<u32> {
        loop {
            let (id, kind) = self.read_value()?;
            if kind == Kind::Hash {
                return Some(id);
            }
        }
    }

    /// `json_read`: the next completed node.
    pub fn read_value(&mut self) -> Option<(u32, Kind)> {
        if self.stack.is_empty() {
            // In case there is an error at the top level
            self.doc.clear();
        }

        loop {
            // skip whitespace quickly
            while let Some(&c) = self.input.get(self.pos) {
                match c {
                    b' ' | b'\t' | b'\r' | 0x1E => self.pos += 1,
                    b'\n' => {
                        self.pos += 1;
                        self.line += 1;
                    }
                    _ => break,
                }
            }

            let c = self.read();
            if c < 0 {
                if !self.stack.is_empty() {
                    self.error = Some("Reached EOF without all containers being closed");
                }
                return None;
            }

            match c as u8 {
                // Byte order mark
                0xEF => {
                    if self.peek() == 0xBB {
                        self.read();
                        if self.peek() == 0xBF {
                            self.read();
                            continue;
                        }
                    }
                    self.error = Some("Corrupt byte-order mark found");
                    return None;
                }

                b'[' => {
                    self.open_container(Kind::Array, Expect::Item)?;
                    continue;
                }

                b']' => {
                    let f = match self.stack.last() {
                        Some(f) => *f,
                        None => {
                            self.error = Some("Found ] at top level");
                            return None;
                        }
                    };
                    let node = &self.doc.nodes[f.container as usize];
                    if node.kind != Kind::Array {
                        self.error = Some("Found ] not in an array");
                        return None;
                    }
                    let empty = self.doc.open[node.start].is_empty();
                    if f.expect != Expect::Comma && !(f.expect == Expect::Item && empty) {
                        self.error = Some("Found ] without final element");
                        return None;
                    }
                    return Some((self.close_container(), Kind::Array));
                }

                b'{' => {
                    self.open_container(Kind::Hash, Expect::Key)?;
                    continue;
                }

                b'}' => {
                    let f = match self.stack.last() {
                        Some(f) => *f,
                        None => {
                            self.error = Some("Found } at top level");
                            return None;
                        }
                    };
                    let node = &self.doc.nodes[f.container as usize];
                    if node.kind != Kind::Hash {
                        self.error = Some("Found } not in a hash");
                        return None;
                    }
                    let empty = self.doc.open[node.start].is_empty();
                    if f.expect != Expect::Comma && !(f.expect == Expect::Key && empty) {
                        self.error = Some("Found } without final element");
                        return None;
                    }
                    return Some((self.close_container(), Kind::Hash));
                }

                b'n' => {
                    if self.read() != 'u' as i32
                        || self.read() != 'l' as i32
                        || self.read() != 'l' as i32
                    {
                        self.error = Some("Found misspelling of null");
                        return None;
                    }
                    return self.add_object(Kind::Null).map(|o| (o, Kind::Null));
                }

                b'N' => {
                    if self.read() != 'a' as i32 || self.read() != 'N' as i32 {
                        self.error = Some("Found misspelling of NaN");
                        return None;
                    }
                    self.error = Some("JSON does not allow NaN");
                    return None;
                }

                b'I' => {
                    for ch in b"nfinity" {
                        if self.read() != *ch as i32 {
                            self.error = Some("Found misspelling of Infinity");
                            return None;
                        }
                    }
                    self.error = Some("JSON does not allow Infinity");
                    return None;
                }

                b't' => {
                    if self.read() != 'r' as i32
                        || self.read() != 'u' as i32
                        || self.read() != 'e' as i32
                    {
                        self.error = Some("Found misspelling of true");
                        return None;
                    }
                    return self.add_object(Kind::True).map(|o| (o, Kind::True));
                }

                b'f' => {
                    if self.read() != 'a' as i32
                        || self.read() != 'l' as i32
                        || self.read() != 's' as i32
                        || self.read() != 'e' as i32
                    {
                        self.error = Some("Found misspelling of false");
                        return None;
                    }
                    return self.add_object(Kind::False).map(|o| (o, Kind::False));
                }

                b',' => {
                    if let Some(f) = self.stack.last_mut() {
                        if f.expect != Expect::Comma {
                            self.error = Some("Found unexpected comma");
                            return None;
                        }
                        f.expect = if self.doc.nodes[f.container as usize].kind == Kind::Hash {
                            Expect::Key
                        } else {
                            Expect::Item
                        };
                    }
                    continue;
                }

                b':' => {
                    let f = match self.stack.last_mut() {
                        Some(f) => f,
                        None => {
                            self.error = Some("Found colon at top level");
                            return None;
                        }
                    };
                    if f.expect != Expect::Colon {
                        self.error = Some("Found unexpected colon");
                        return None;
                    }
                    f.expect = Expect::Value;
                    continue;
                }

                b'-' | b'0'..=b'9' => {
                    return self.read_number(c).map(|o| (o, Kind::Number));
                }

                b'"' => {
                    return self.read_string().map(|o| (o, Kind::String));
                }

                _ => {}
            }

            self.error = Some("Found unexpected character");
            return None;
        }
    }

    fn read_number(&mut self, first: i32) -> Option<u32> {
        let start = self.pos - 1;
        let mut c = first;
        let mut decimal = false;
        // Whether the text is input[start..pos]; jsonpull swallows the
        // character after a '-' even if it is not a digit.
        let mut contiguous = true;
        self.numbuf.clear();

        if c == '-' as i32 {
            self.numbuf.push(b'-');
            c = self.read();
            if !is_digit(c) {
                contiguous = false;
            }
        }

        if c == '0' as i32 {
            self.numbuf.push(b'0');
        } else if is_digit(c) {
            self.numbuf.push(c as u8);
            while is_digit(self.peek()) {
                let d = self.read();
                self.numbuf.push(d as u8);
            }
        }

        if self.peek() == '.' as i32 {
            let d = self.read();
            self.numbuf.push(d as u8);
            decimal = true;

            if !is_digit(self.peek()) {
                self.error = Some("Decimal point without digits");
                return None;
            }
            while is_digit(self.peek()) {
                let d = self.read();
                self.numbuf.push(d as u8);
            }
        }

        c = self.peek();
        if c == 'e' as i32 || c == 'E' as i32 {
            let d = self.read();
            self.numbuf.push(d as u8);
            decimal = true;

            c = self.peek();
            if c == '+' as i32 || c == '-' as i32 {
                let d = self.read();
                self.numbuf.push(d as u8);
            }

            if !is_digit(self.peek()) {
                self.error = Some("Exponent without digits");
                return None;
            }
            while is_digit(self.peek()) {
                let d = self.read();
                self.numbuf.push(d as u8);
            }
        }

        let n = self.add_object(Kind::Number)?;

        let text: &[u8] = if contiguous {
            &self.input[start..self.pos]
        } else {
            &self.numbuf
        };
        // SAFETY: the text is ASCII digits, signs, '.' and 'e' or 'E'.
        let s = unsafe { std::str::from_utf8_unchecked(text) };
        // Rust's parse is correctly rounded, as strtod is; anything it does
        // not accept goes to atof itself.
        let d = s
            .parse::<f64>()
            .unwrap_or_else(|_| tippecanoe::cstd::atof(s));
        let mut num = Num::Double(d);

        if !decimal
            && d > MAX_SAFE_INTEGER
            && let Ok(u) = s.parse::<u64>()
        {
            num = Num::LargeUnsigned(u);
        }
        if !decimal
            && d < MIN_SAFE_INTEGER
            && let Ok(i) = s.parse::<i64>()
        {
            num = Num::LargeSigned(i);
        }
        self.doc.nodes[n as usize].num = num;
        Some(n)
    }

    fn read_string(&mut self) -> Option<u32> {
        let start = self.pos;
        // Fast path: no escapes before the closing quote.
        let mut i = start;
        let input = self.input;
        while i < input.len() {
            let c = input[i];
            if c == b'"' {
                let s = self.add_object(Kind::String)?;
                self.pos = i + 1;
                let n = &mut self.doc.nodes[s as usize];
                n.start = start;
                n.len = (i - start) as u32;
                return Some(s);
            }
            if c == b'\\' || c < b' ' {
                break;
            }
            i += 1;
        }

        // Slow path, as jsonpull does it, into the string buffer.
        let mut val = std::mem::take(&mut self.numbuf);
        val.clear();
        val.extend_from_slice(&input[start..i]);
        self.pos = i;
        let r = self.read_string_slow(&mut val);
        let ret = match r {
            Some(()) => match self.add_object(Kind::String) {
                Some(s) => {
                    let off = self.doc.strbuf.len();
                    self.doc.strbuf.extend_from_slice(&val);
                    let n = &mut self.doc.nodes[s as usize];
                    n.owned = true;
                    n.start = off;
                    n.len = val.len() as u32;
                    Some(s)
                }
                None => None,
            },
            None => None,
        };
        self.numbuf = val;
        ret
    }

    fn read_string_slow(&mut self, val: &mut Vec<u8>) -> Option<()> {
        fn push_surrogate(val: &mut Vec<u8>, surrogate: i32) {
            val.push((0xE0 | (surrogate >> 12)) as u8);
            val.push((0x80 | ((surrogate >> 6) & 0x3F)) as u8);
            val.push((0x80 | (surrogate & 0x3F)) as u8);
        }

        let mut c: i32;
        let mut surrogate: i32 = -1;
        loop {
            c = self.read();
            if c < 0 {
                break;
            }

            if c == '"' as i32 {
                if surrogate >= 0 {
                    push_surrogate(val, surrogate);
                }
                break;
            } else if c == '\\' as i32 {
                c = self.read();

                if c == 'u' as i32 {
                    let mut ch: u64 = 0;
                    for _ in 0..4 {
                        let x = self.read();
                        if !(x >= 0 && (x as u8).is_ascii_hexdigit()) {
                            self.error = Some("Invalid \\u hex character");
                            return None;
                        }
                        ch = ch * 16 + (x as u8 as char).to_digit(16).unwrap() as u64;
                    }

                    if (0xd800..=0xdbff).contains(&ch) {
                        if surrogate >= 0 {
                            // Impossible surrogate, so output the first half,
                            // keep what might be a legitimate new first half.
                            push_surrogate(val, surrogate);
                        }
                        surrogate = ch as i32;
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

                    let e = match c {
                        0x22 => b'"',
                        0x5C => b'\\',
                        0x2F => b'/',
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => b'\n',
                        0x72 => b'\r',
                        0x74 => b'\t',
                        _ => {
                            self.error = Some("Found backslash followed by unknown character");
                            return None;
                        }
                    };
                    val.push(e);
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
        if c < 0 {
            self.error = Some("String without closing quote mark");
            return None;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tippecanoe::jsonpull::{JsonPull, JsonRef, JsonType};

    fn kind_of(t: JsonType) -> Kind {
        match t {
            JsonType::Hash => Kind::Hash,
            JsonType::Array => Kind::Array,
            JsonType::Number => Kind::Number,
            JsonType::String => Kind::String,
            JsonType::True => Kind::True,
            JsonType::False => Kind::False,
            JsonType::Null => Kind::Null,
            _ => panic!("unexpected type"),
        }
    }

    fn same_tree(a: Ref<'_, '_>, b: JsonRef<'_>) {
        assert_eq!(a.kind(), kind_of(b.type_()));
        match a.kind() {
            Kind::String => assert_eq!(a.bytes(), b.raw_string_bytes()),
            Kind::Number => {
                assert_eq!(a.number().to_bits(), b.number().to_bits());
                assert_eq!(a.large_signed(), b.large_signed());
                assert_eq!(a.large_unsigned(), b.large_unsigned());
            }
            Kind::Array => {
                assert_eq!(a.array_len(), b.array_len());
                for (x, y) in a.array().zip(b.array_iter()) {
                    same_tree(x, y);
                }
            }
            Kind::Hash => {
                let ea: Vec<_> = a.entries().collect();
                let eb: Vec<_> = b.entries_iter().collect();
                assert_eq!(ea.len(), eb.len());
                for ((ka, va), (kb, vb)) in ea.into_iter().zip(eb) {
                    same_tree(ka, kb);
                    same_tree(va, vb);
                }
            }
            _ => {}
        }
        assert_eq!(a.stringify(), b.stringify_raw());
    }

    /// Reads every completed node with both parsers and compares them.
    fn compare(input: &[u8]) {
        let mut p = Parser::new(input);
        let mut jp = JsonPull::begin_string(input);
        loop {
            let a = p.read_value();
            let b = jp.read();
            match (a, b) {
                (None, None) => {
                    assert_eq!(
                        p.error,
                        jp.error,
                        "input {:?}",
                        String::from_utf8_lossy(input)
                    );
                    assert_eq!(p.line, jp.line);
                    break;
                }
                (Some((ida, _)), Some(idb)) => {
                    same_tree(p.doc.get(ida), jp.get(idb));
                    let ra = p.doc.root;
                    if ra != NONE {
                        assert_eq!(
                            p.doc.get(ra).stringify(),
                            jp.root().unwrap().stringify_raw()
                        );
                    }
                }
                _ => panic!(
                    "one parser ended early on {:?}: {:?} {:?}",
                    String::from_utf8_lossy(input),
                    p.error,
                    jp.error
                ),
            }
        }
    }

    #[test]
    fn matches_jsonpull() {
        let cases: &[&[u8]] = &[
            b"{\"type\":\"Feature\",\"properties\":{\"a\":1.5,\"b\":[1,2,{\"c\":null}]},\"geometry\":{\"type\":\"Point\",\"coordinates\":[-122.5,37.25]}}",
            b"[1, -0, 0.1, 1e5, 1E-5, -1.5e+300, 12345678901234567890, -12345678901234567890, 9007199254740993, 123456789012345678901234]",
            b"\"esc \\\" \\\\ \\/ \\b \\f \\n \\r \\t \\u0041 \\u00e9 \\u4e2d \\ud83d\\ude00 \\ud800x \\udc00 \\ud83d\\ud83d\"",
            b"{\"a\":1,}",
            b"[1,]",
            b"[1 2]",
            b"{\"a\" 1}",
            b"{1:2}",
            b"]",
            b"}",
            b":",
            b", 1, 2",
            b"nul",
            b"tru",
            b"NaN",
            b"Infinity",
            b"-x.5",
            b"-",
            b"1.",
            b"1e",
            b"01",
            b"\"abc",
            b"\"a\nb\"",
            b"\"\\q\"",
            b"\"\\u12\"",
            b"\xEF\xBB\xBF{\"a\":true}",
            b"\xEF\xBBx",
            b"{\"a\":[1,2,3]\n,\n\"b\":{\"c\":\"d\"}}\n{\"e\":false}\n\x1E[null]",
            b"{\"a\":{\"b\":",
            b"@",
        ];
        for c in cases {
            compare(c);
        }
    }

    #[test]
    fn freeing_matches_jsonpull() {
        // A feature collection whose features are freed as they complete,
        // as the GeoJSON reader does.
        let input = b"{\"type\":\"FeatureCollection\",\"features\":[{\"type\":\"Feature\",\"x\":\"a\\nb\"},{\"type\":\"Feature\",\"y\":[1,2]},],\"z\":{\"type\":\"Feature\"},\"w\":1}";
        let mut p = Parser::new(input);
        let mut jp = JsonPull::begin_string(input);
        loop {
            let a = p.read_value();
            let b = jp.read();
            match (a, b) {
                (None, None) => {
                    assert_eq!(p.error, jp.error);
                    break;
                }
                (Some((ida, ka)), Some(idb)) => {
                    let r = p.doc.get(ida);
                    if ka == Kind::Hash
                        && r.hash_get(b"type").map(|t| t.bytes()) == Some(b"Feature".as_slice())
                    {
                        p.free_last(ida);
                        jp.free(idb);
                    }
                    if p.doc.root != NONE {
                        assert_eq!(
                            p.doc.get(p.doc.root).stringify(),
                            jp.root().unwrap().stringify_raw()
                        );
                    }
                }
                _ => panic!("one parser ended early"),
            }
        }
    }
}
