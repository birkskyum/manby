//! The parts of JavaScript that `encode.js` and `bin/json2geobuf` rely on:
//! `JSON.parse`, `JSON.stringify`, property access, `for...in`, `ToNumber`,
//! `ToString`, `Math.round` and `Math.max`, with the semantics of V8.
//!
//! An uncaught exception in Node prints the error and exits with status 1
//! before anything reaches stdout, which `throw` does too.

use std::borrow::Cow;
use std::collections::HashMap;

/// A JavaScript string, as UTF-16 code units that may include lone surrogates.
pub type JsString = Box<[u16]>;

/// A value as `JSON.parse` returns it, plus `undefined` for missing properties.
/// (`Clone` is only there for `Cow`; nothing clones a parsed value.)
#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    String(JsString),
    Array(Vec<Value>),
    /// Own properties in the order of OrdinaryOwnPropertyKeys. Keys that are
    /// array indices come first in ascending numeric order, then the other
    /// keys in the order they were first created.
    Object(Vec<(JsString, Value)>),
}

const UNDEFINED: Value = Value::Undefined;

pub fn throw(error: &str) -> ! {
    eprintln!("{error}");
    std::process::exit(1);
}

pub fn utf16(s: &str) -> JsString {
    s.encode_utf16().collect()
}

fn eq_ascii(k: &[u16], s: &str) -> bool {
    k.len() == s.len() && k.iter().zip(s.bytes()).all(|(&a, b)| a == b as u16)
}

/// `v === "s"`
pub fn is_str(v: &Value, s: &str) -> bool {
    matches!(v, Value::String(k) if eq_ascii(k, s))
}

pub fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undefined => "undefined",
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Boolean(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

/// ToBoolean, for `if (v)`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Boolean(b) => *b,
        Value::Number(n) => !(*n == 0.0 || n.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn cannot_read(v: &Value, key: &str) -> ! {
    let what = if matches!(v, Value::Null) {
        "null"
    } else {
        "undefined"
    };
    throw(&format!(
        "TypeError: Cannot read properties of {what} (reading '{key}')"
    ))
}

/// `v.name`, for the property names encode.js uses. None of them is an array
/// index, and none of them is inherited from a prototype except `length`.
pub fn get<'a>(v: &'a Value, name: &str) -> Cow<'a, Value> {
    match v {
        Value::Undefined | Value::Null => cannot_read(v, name),
        Value::Object(props) => match props.iter().find(|(k, _)| eq_ascii(k, name)) {
            Some((_, value)) => Cow::Borrowed(value),
            None => Cow::Borrowed(&UNDEFINED),
        },
        Value::Array(a) if name == "length" => Cow::Owned(Value::Number(a.len() as f64)),
        Value::String(s) if name == "length" => Cow::Owned(Value::Number(s.len() as f64)),
        _ => Cow::Borrowed(&UNDEFINED),
    }
}

/// `v[i]` for a loop counter `i`.
pub fn get_index(v: &Value, i: usize) -> Cow<'_, Value> {
    match v {
        Value::Undefined | Value::Null => cannot_read(v, &i.to_string()),
        Value::Array(a) => match a.get(i) {
            Some(value) => Cow::Borrowed(value),
            None => Cow::Borrowed(&UNDEFINED),
        },
        Value::String(s) => match s.get(i) {
            Some(&c) => Cow::Owned(Value::String(Box::new([c]))),
            None => Cow::Borrowed(&UNDEFINED),
        },
        Value::Object(props) => {
            let name = i.to_string();
            match props.iter().find(|(k, _)| eq_ascii(k, &name)) {
                Some((_, value)) => Cow::Borrowed(value),
                None => Cow::Borrowed(&UNDEFINED),
            }
        }
        _ => Cow::Borrowed(&UNDEFINED),
    }
}

/// The keys `for (key in v)` visits, each with `v[key]`. Nothing that
/// JSON.parse makes inherits an enumerable property, so these are the own
/// enumerable keys. Those are the keys of an object in their order, and the
/// indices of an array or of a string (one per UTF-16 code unit).
#[allow(clippy::type_complexity)]
pub fn for_in(v: &Value) -> Vec<(Cow<'_, [u16]>, Cow<'_, Value>)> {
    match v {
        Value::Object(props) => props
            .iter()
            .map(|(k, value)| (Cow::Borrowed(&k[..]), Cow::Borrowed(value)))
            .collect(),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, value)| {
                (
                    Cow::Owned(utf16(&i.to_string()).into()),
                    Cow::Borrowed(value),
                )
            })
            .collect(),
        Value::String(s) => s
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                (
                    Cow::Owned(utf16(&i.to_string()).into()),
                    Cow::Owned(Value::String(Box::new([c]))),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// OrdinaryToPrimitive for a parsed object, which always ends at
/// Object.prototype.toString unless the object has an own `toString` (or
/// `valueOf`), which JSON can only make a non-callable one.
fn object_to_string(props: &[(JsString, Value)]) -> JsString {
    if props.iter().any(|(k, _)| eq_ascii(k, "toString")) {
        throw("TypeError: Cannot convert object to primitive value");
    }
    utf16("[object Object]")
}

/// ToString (also ToPropertyKey, since there are no symbols here).
pub fn to_string(v: &Value) -> JsString {
    match v {
        Value::Undefined => utf16("undefined"),
        Value::Null => utf16("null"),
        Value::Boolean(b) => utf16(if *b { "true" } else { "false" }),
        Value::Number(n) => utf16(&number_to_string(*n)),
        Value::String(s) => s.clone(),
        // Array.prototype.toString is Array.prototype.join(",")
        Value::Array(a) => {
            let mut out = Vec::new();
            for (i, element) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',' as u16);
                }
                if !matches!(element, Value::Undefined | Value::Null) {
                    out.extend_from_slice(&to_string(element));
                }
            }
            out.into()
        }
        Value::Object(props) => object_to_string(props),
    }
}

/// ToNumber
pub fn to_number(v: &Value) -> f64 {
    match v {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Boolean(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => *n,
        Value::String(s) => string_to_number(s),
        Value::Array(_) | Value::Object(_) => string_to_number(&to_string(v)),
    }
}

fn is_str_whitespace(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

/// StringToNumber, which reads a StringNumericLiteral between white space.
fn string_to_number(s: &[u16]) -> f64 {
    let start = s
        .iter()
        .position(|&c| !is_str_whitespace(c))
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|&c| !is_str_whitespace(c))
        .map_or(start, |p| p + 1);
    let s = &s[start..end];
    if s.is_empty() {
        return 0.0;
    }
    if s.iter().any(|&c| c >= 0x80) {
        return f64::NAN;
    }
    let s: String = s.iter().map(|&c| c as u8 as char).collect();

    let radix = match s.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = &s[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return match u128::from_str_radix(digits, radix) {
            Ok(n) => n as f64,
            Err(_) => digits.chars().fold(0.0, |n, c| {
                n * radix as f64 + c.to_digit(radix).unwrap() as f64
            }),
        };
    }

    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(&s);
    if unsigned == "Infinity" {
        return if s.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let b = unsigned.as_bytes();
    let mut i = 0;
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    i += int_digits;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += frac_digits;
    }
    if int_digits == 0 && frac_digits == 0 {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let exp_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp_digits == 0 {
            return f64::NAN;
        }
        i += exp_digits;
    }
    if i != b.len() {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}

/// Number::toString(x) with radix 10: the shortest digits that round-trip,
/// laid out by the rules of the ECMAScript spec.
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    if x < 0.0 {
        return format!("-{}", number_to_string(-x));
    }
    if x.is_infinite() {
        return "Infinity".to_string();
    }

    let (digits, exponent) = shortest_digits(x);
    let k = digits.len() as i32;
    let n = exponent + 1;

    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    }
}

/// Splits a `{:e}` rendering into its digits and its exponent.
fn split_exponential(s: &str) -> (String, i32) {
    let (mantissa, exponent) = s.split_once('e').unwrap();
    let digits = mantissa.chars().filter(|&c| c != '.').collect();
    (digits, exponent.parse().unwrap())
}

/// The shortest digits that round-trip to `x` (positive and finite), and the
/// exponent of the first digit. Among equally short candidates the spec takes
/// the one closest to `x`, and the even one if `x` lies exactly halfway
/// between two. Rust's shortest formatting rounds that tie up instead.
fn shortest_digits(x: f64) -> (String, i32) {
    let (digits, exponent) = split_exponential(&format!("{x:e}"));
    let k = digits.len();

    // x is halfway between two k-digit candidates only if its exact decimal
    // expansion has k + 1 significant digits and the last one is 5.
    let (rounded, _) = split_exponential(&format!("{x:.k$e}"));
    if !rounded.ends_with('5') {
        return (digits, exponent);
    }
    let (exact, exact_exponent) = split_exponential(&format!("{x:.766e}"));
    if exact.as_bytes()[k] != b'5' || exact[k + 1..].bytes().any(|c| c != b'0') {
        return (digits, exponent);
    }

    let mut even = exact.as_bytes()[..k].to_vec();
    if (even[k - 1] - b'0') % 2 != 0 {
        // the upper candidate, unless the increment carries out of all k digits
        let mut i = k;
        loop {
            if i == 0 {
                return (digits, exponent);
            }
            i -= 1;
            if even[i] == b'9' {
                even[i] = b'0';
            } else {
                even[i] += 1;
                break;
            }
        }
    }
    let even = String::from_utf8(even).unwrap();
    let candidate = format!("{}.{}e{exact_exponent}", &even[..1], &even[1..]);
    if candidate.parse::<f64>() == Ok(x) {
        (even, exact_exponent)
    } else {
        (digits, exponent)
    }
}

/// Math.round as V8 implements it, which rounds up and steps back if that went
/// more than half past `x`. Halves round toward +Infinity.
pub fn math_round(x: f64) -> f64 {
    let r = x.ceil();
    if r - 0.5 <= x { r } else { r - 1.0 }
}

/// Math.max(a, b)
pub fn math_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        // +0 is larger than -0
        if a.is_sign_negative() { b } else { a }
    } else if a > b {
        a
    } else {
        b
    }
}

/// ToInt32
pub fn to_int32(x: f64) -> i32 {
    to_uint32(x) as i32
}

/// ToUint32
pub fn to_uint32(x: f64) -> u32 {
    if !x.is_finite() {
        return 0;
    }
    x.trunc().rem_euclid(4294967296.0) as u32
}

// === JSON.stringify ==========================================================

/// JSON.stringify(v) for an object or an array.
pub fn json_stringify(v: &Value) -> Vec<u16> {
    let mut out = Vec::new();
    serialize(v, &mut out);
    out
}

fn push_ascii(out: &mut Vec<u16>, s: &str) {
    out.extend(s.bytes().map(u16::from));
}

fn serialize(v: &Value, out: &mut Vec<u16>) {
    match v {
        Value::Null | Value::Undefined => push_ascii(out, "null"),
        Value::Boolean(b) => push_ascii(out, if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_finite() {
                push_ascii(out, &number_to_string(*n));
            } else {
                push_ascii(out, "null");
            }
        }
        Value::String(s) => quote_json_string(s, out),
        Value::Array(a) => {
            out.push(b'[' as u16);
            for (i, element) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',' as u16);
                }
                serialize(element, out);
            }
            out.push(b']' as u16);
        }
        Value::Object(props) => {
            out.push(b'{' as u16);
            for (i, (k, value)) in props.iter().enumerate() {
                if i > 0 {
                    out.push(b',' as u16);
                }
                quote_json_string(k, out);
                out.push(b':' as u16);
                serialize(value, out);
            }
            out.push(b'}' as u16);
        }
    }
}

/// QuoteJSONString, which escapes lone surrogates (ES2019).
fn quote_json_string(s: &[u16], out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            0x08 => push_ascii(out, "\\b"),
            0x09 => push_ascii(out, "\\t"),
            0x0A => push_ascii(out, "\\n"),
            0x0C => push_ascii(out, "\\f"),
            0x0D => push_ascii(out, "\\r"),
            0x22 => push_ascii(out, "\\\""),
            0x5C => push_ascii(out, "\\\\"),
            0xD800..=0xDBFF if i + 1 < s.len() && (0xDC00..=0xDFFF).contains(&s[i + 1]) => {
                out.push(c);
                out.push(s[i + 1]);
                i += 1;
            }
            0..=0x1F | 0xD800..=0xDFFF => push_ascii(out, &format!("\\u{c:04x}")),
            _ => out.push(c),
        }
        i += 1;
    }
    out.push(b'"' as u16);
}

// === JSON.parse ==============================================================

/// JSON.parse(text)
///
/// This follows the spec where V8 14.6 (Node 26) does not. V8 can store a key
/// written with an escape under the name of a different key, when that name
/// equals the raw source text of the escaped key. It finds that name among
/// the map transitions of earlier objects, comparing the raw text cut to the
/// decoded length (`ExpectedTransition` in `JSDataObjectBuilder`). Such a name
/// has a backslash in it, so documents without one in any key are unaffected.
pub fn json_parse(text: &str) -> Value {
    let mut parser = Parser {
        s: text.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value();
    parser.skip_whitespace();
    if parser.pos < parser.s.len() {
        parser.unexpected();
    }
    value
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

/// Whether `k` is an array index, the canonical decimal form of an integer
/// from 0 to 2^32 - 2. Those keys come first in an object, in numeric order.
fn array_index(k: &[u16]) -> Option<u32> {
    if k.is_empty() || k.len() > 10 || (k.len() > 1 && k[0] == b'0' as u16) {
        return None;
    }
    let mut n: u64 = 0;
    for &c in k {
        if !(b'0' as u16..=b'9' as u16).contains(&c) {
            return None;
        }
        n = n * 10 + (c - b'0' as u16) as u64;
    }
    if n <= 4294967294 {
        Some(n as u32)
    } else {
        None
    }
}

impl Parser<'_> {
    fn unexpected(&self) -> ! {
        if self.pos >= self.s.len() {
            throw("SyntaxError: Unexpected end of JSON input");
        }
        throw(&format!(
            "SyntaxError: Unexpected token in JSON at position {}",
            self.pos
        ))
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.s.len() && matches!(self.s[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: u8) {
        if self.pos < self.s.len() && self.s[self.pos] == c {
            self.pos += 1;
        } else {
            self.unexpected();
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Value {
        for c in word.bytes() {
            self.expect(c);
        }
        value
    }

    fn parse_value(&mut self) -> Value {
        match self.s.get(self.pos) {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Value::String(self.parse_string()),
            Some(b'-' | b'0'..=b'9') => Value::Number(self.parse_number()),
            Some(b't') => self.literal("true", Value::Boolean(true)),
            Some(b'f') => self.literal("false", Value::Boolean(false)),
            Some(b'n') => self.literal("null", Value::Null),
            _ => self.unexpected(),
        }
    }

    fn parse_object(&mut self) -> Value {
        self.expect(b'{');
        let mut props: Vec<(JsString, Value)> = Vec::new();
        // For objects with many keys, a map from key to position finds duplicates.
        let mut index: Option<HashMap<JsString, usize>> = None;
        self.skip_whitespace();
        if self.s.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Value::Object(props);
        }
        loop {
            self.skip_whitespace();
            if self.s.get(self.pos) != Some(&b'"') {
                self.unexpected();
            }
            let key = self.parse_string();
            self.skip_whitespace();
            self.expect(b':');
            self.skip_whitespace();
            let value = self.parse_value();

            // A repeated key keeps its first position and takes the last value.
            let existing = match &index {
                Some(index) => index.get(&key).copied(),
                None => props.iter().position(|(k, _)| *k == key),
            };
            match existing {
                Some(i) => props[i].1 = value,
                None => {
                    if let Some(index) = &mut index {
                        index.insert(key.clone(), props.len());
                    }
                    props.push((key, value));
                    if index.is_none() && props.len() > 16 {
                        index = Some(
                            props
                                .iter()
                                .enumerate()
                                .map(|(i, (k, _))| (k.clone(), i))
                                .collect(),
                        );
                    }
                }
            }

            self.skip_whitespace();
            match self.s.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => self.unexpected(),
            }
        }

        if props.iter().any(|(k, _)| array_index(k).is_some()) {
            let (mut indices, names): (Vec<_>, Vec<_>) = props
                .into_iter()
                .partition(|(k, _)| array_index(k).is_some());
            indices.sort_by_key(|(k, _)| array_index(k));
            indices.extend(names);
            props = indices;
        }
        Value::Object(props)
    }

    fn parse_array(&mut self) -> Value {
        self.expect(b'[');
        let mut elements = Vec::new();
        self.skip_whitespace();
        if self.s.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Value::Array(elements);
        }
        loop {
            self.skip_whitespace();
            elements.push(self.parse_value());
            self.skip_whitespace();
            match self.s.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => self.unexpected(),
            }
        }
        Value::Array(elements)
    }

    fn hex_digit(&mut self) -> u16 {
        let digit = self.s.get(self.pos).and_then(|&c| (c as char).to_digit(16));
        match digit {
            Some(d) => {
                self.pos += 1;
                d as u16
            }
            None => self.unexpected(),
        }
    }

    fn parse_string(&mut self) -> JsString {
        self.expect(b'"');
        let mut out: Vec<u16> = Vec::new();
        loop {
            let start = self.pos;
            while self.pos < self.s.len() && !matches!(self.s[self.pos], b'"' | b'\\' | 0..=0x1F) {
                self.pos += 1;
            }
            // The run stops at an ASCII byte, so it is whole UTF-8 characters.
            let run = std::str::from_utf8(&self.s[start..self.pos]).unwrap();
            out.extend(run.encode_utf16());
            match self.s.get(self.pos) {
                Some(b'"') => {
                    self.pos += 1;
                    return out.into();
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = match self.s.get(self.pos) {
                        Some(b'"') => b'"' as u16,
                        Some(b'\\') => b'\\' as u16,
                        Some(b'/') => b'/' as u16,
                        Some(b'b') => 0x08,
                        Some(b'f') => 0x0C,
                        Some(b'n') => 0x0A,
                        Some(b'r') => 0x0D,
                        Some(b't') => 0x09,
                        Some(b'u') => {
                            self.pos += 1;
                            let mut c = 0;
                            for _ in 0..4 {
                                c = c * 16 + self.hex_digit();
                            }
                            out.push(c);
                            continue;
                        }
                        _ => self.unexpected(),
                    };
                    self.pos += 1;
                    out.push(c);
                }
                // a control character, or the end of the input
                _ => self.unexpected(),
            }
        }
    }

    fn parse_number(&mut self) -> f64 {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while p.pos < p.s.len() && p.s[p.pos].is_ascii_digit() {
                p.pos += 1;
            }
            p.pos - from
        };
        if self.s.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.s.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => self.unexpected(),
        }
        if self.s.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if digits(self) == 0 {
                self.unexpected();
            }
        }
        if matches!(self.s.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.s.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                self.unexpected();
            }
        }
        // The literal is ASCII, and Rust's parse rounds correctly, like V8.
        std::str::from_utf8(&self.s[start..self.pos])
            .unwrap()
            .parse()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_strings() {
        for (x, s) in [
            (1e21, "1e+21"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (-1.5e-10, "-1.5e-10"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (-0.0, "0"),
            (2.5, "2.5"),
            (100.0, "100"),
            // exactly halfway between ...824.2 and ...824.3, so the even one
            (622594461077824.25, "622594461077824.2"),
        ] {
            assert_eq!(number_to_string(x), s);
        }
    }

    #[test]
    fn rounding() {
        assert_eq!(math_round(2.5), 3.0);
        assert_eq!(math_round(-2.5), -2.0);
        assert_eq!(math_round(0.49999999999999994), 0.0);
        assert!(math_round(-0.5).is_sign_negative());
    }
}
