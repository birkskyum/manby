//! Port of jsontool.cpp, the `tippecanoe-json-tool` program.
//!
//! The C++ joins CSV columns into a feature by appending new entries to its
//! parsed `properties` hash and then stringifying the feature. The Rust parse
//! tree can't be modified that way, so `join_csv` returns the new entries and
//! `stringify_joined` writes the feature as `json_stringify` would with them
//! appended to the properties.

use std::fs::File;
use std::io::{BufReader, Read, StdoutLock, Write};
use std::sync::atomic::{AtomicI32, Ordering};

use crate::cstd::{atof, perror_io};
use crate::csv::{csv_dequote, csv_getline, csv_split, is_number};
use crate::errors::{EXIT_ARGS, EXIT_CSV, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_SUCCESS, EXIT_UTF8};
use crate::geojson_loop::{JsonFeatureAction, parse_json};
use crate::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use crate::jsonpull::{JsonId, JsonPull, JsonRef, JsonType};
use crate::milo::dtoa_milo;
use crate::text::check_utf8;
use crate::usage::{getopt_string, print_usage, strip_usage_headings};

static PE: AtomicI32 = AtomicI32::new(0);

/// The other file-level globals of jsontool.cpp, including the `static`
/// variables inside its functions.
struct Globals {
    fail: i32,
    wrap: bool,
    extract: Option<String>,

    csvfile: Option<BufReader<File>>,
    header: Vec<String>,
    fields: Vec<String>,

    buffered: Vec<u8>,
    buffered_type: i32,
    // 0: nothing yet
    // 1: buffered a line
    // 2: wrote the line and the wrapper
    buffer_state: i32,

    prev_joinkey: Vec<u8>,

    /// `warned` in `out()`
    out_warned: bool,
    /// `warned` in `join_csv()`
    join_warned: bool,

    stdout: StdoutLock<'static>,
}

/// A value that `join_csv` adds to the properties: a `json_string` or a `json_number`.
enum JoinedValue {
    String(String),
    Number(f64),
}

struct JoinedEntry {
    key: String,
    value: JoinedValue,
}

/// The bytes of `s` up to its first NUL, as `c_str()` would pass them on.
fn c_str(s: &[u8]) -> &[u8] {
    match s.iter().position(|&c| c == 0) {
        Some(n) => &s[..n],
        None => s,
    }
}

fn decode32(s: &[u8]) -> Vec<u64> {
    let s = c_str(s);
    // Reading past the end gives the NUL terminator, as in the C++
    let at = |i: usize| -> u64 { s.get(i).copied().unwrap_or(0) as u64 };
    let mut utf32 = Vec::new();
    let mut i = 0;

    while at(i) != 0 {
        let b = at(i) & 0xFF;
        i += 1;

        if b < 0x80 {
            utf32.push(b);
        } else if (b & 0xe0) == 0xc0 {
            let mut c = (b & 0x1f) << 6;
            let b1 = at(i) & 0xFF;
            i += 1;

            if (b1 & 0xc0) == 0x80 {
                c |= b1 & 0x3f;
                utf32.push(c);
            } else {
                i -= 1;
                utf32.push(0xfffd);
            }
        } else if (b & 0xf0) == 0xe0 {
            let mut c = (b & 0x0f) << 12;
            let b1 = at(i) & 0xFF;
            i += 1;

            if (b1 & 0xc0) == 0x80 {
                c |= (b1 & 0x3f) << 6;
                let b2 = at(i) & 0xFF;
                i += 1;

                if (b2 & 0xc0) == 0x80 {
                    c |= b2 & 0x3f;
                    utf32.push(c);
                } else {
                    i -= 2;
                    utf32.push(0xfffd);
                }
            } else {
                i -= 1;
                utf32.push(0xfffd);
            }
        } else if (b & 0xf8) == 0xf0 {
            let mut c = (b & 0x07) << 18;
            let b1 = at(i) & 0xFF;
            i += 1;

            if (b1 & 0xc0) == 0x80 {
                c |= (b1 & 0x3f) << 12;
                let b2 = at(i) & 0xFF;
                i += 1;

                if (b2 & 0xc0) == 0x80 {
                    c |= (b2 & 0x3f) << 6;
                    let b3 = at(i) & 0xFF;
                    i += 1;

                    if (b3 & 0xc0) == 0x80 {
                        c |= b3 & 0x3f;

                        utf32.push(c);
                    } else {
                        i -= 3;
                        utf32.push(0xfffd);
                    }
                } else {
                    i -= 2;
                    utf32.push(0xfffd);
                }
            } else {
                i -= 1;
                utf32.push(0xfffd);
            }
        } else {
            utf32.push(0xfffd);
        }
    }

    utf32
}

// This uses a really weird encoding for strings
// so that they will sort in UTF-32 order in spite of quoting

fn sort_quote(s: &[u8]) -> Vec<u8> {
    let utf32 = decode32(s);
    let mut ret = Vec::new();

    for &c in &utf32 {
        if c < 0xD800 {
            ret.extend_from_slice(format!("\\u{:04}", c).as_bytes());
        } else if c <= 0x7f {
            ret.push(c as u8);
        } else if c <= 0x7ff {
            ret.push((0xc0 | (c >> 6)) as u8);
            ret.push((0x80 | (c & 0x3f)) as u8);
        } else if c <= 0xffff {
            ret.push((0xe0 | (c >> 12)) as u8);
            ret.push((0x80 | ((c >> 6) & 0x3f)) as u8);
            ret.push((0x80 | (c & 0x3f)) as u8);
        } else {
            ret.push((0xf0 | (c >> 18)) as u8);
            ret.push((0x80 | ((c >> 12) & 0x3f)) as u8);
            ret.push((0x80 | ((c >> 6) & 0x3f)) as u8);
            ret.push((0x80 | (c & 0x3f)) as u8);
        }
    }

    ret
}

/// What `json_hash_get(properties, extract)` finds once the joined entries
/// have been appended to the properties.
enum Found<'a> {
    Node(JsonRef<'a>),
    Joined(&'a JoinedValue),
}

fn out(
    g: &mut Globals,
    s: &[u8],
    type_: i32,
    properties: Option<JsonRef<'_>>,
    joined: &[JoinedEntry],
) {
    if let Some(extract) = &g.extract {
        let mut extracted = sort_quote(b"null");
        let mut found = false;

        let o = match properties.and_then(|p| p.hash_get(extract)) {
            Some(o) => Some(Found::Node(o)),
            None => joined
                .iter()
                .find(|e| e.key == *extract)
                .map(|e| Found::Joined(&e.value)),
        };
        if let Some(o) = o {
            found = true;
            match o {
                Found::Node(o) if o.type_() == JsonType::String => {
                    extracted = sort_quote(o.raw_string_bytes());
                }
                // Numbers, booleans, null, and any other non-string
                // values are rendered via json_stringify(); calling
                // o->string() here would assert because the type-tagged
                // accessor requires JSON_STRING.
                Found::Node(o) => extracted = sort_quote(o.stringify().as_bytes()),
                Found::Joined(JoinedValue::String(v)) => extracted = sort_quote(v.as_bytes()),
                Found::Joined(JoinedValue::Number(d)) => {
                    extracted = sort_quote(dtoa_milo(*d).as_bytes())
                }
            }
        }

        if !found && !g.out_warned {
            eprintln!("Warning: extract key \"{}\" not found in JSON", extract);
            g.out_warned = true;
        }

        let line = [b"{\"", c_str(&extracted), b"\":", c_str(s), b"}\n"].concat();
        let _ = g.stdout.write_all(&line);
        return;
    }

    if !g.wrap {
        let line = [c_str(s), b"\n"].concat();
        let _ = g.stdout.write_all(&line);
        return;
    }

    if g.buffer_state == 0 {
        g.buffered = s.to_vec();
        g.buffered_type = type_;
        g.buffer_state = 1;
        return;
    }

    if g.buffer_state == 1 {
        if g.buffered_type == 1 {
            let _ = g
                .stdout
                .write_all(b"{\"type\":\"FeatureCollection\",\"features\":[\n");
        } else {
            let _ = g
                .stdout
                .write_all(b"{\"type\":\"GeometryCollection\",\"geometries\":[\n");
        }

        let line = [c_str(&g.buffered), b"\n"].concat();
        let _ = g.stdout.write_all(&line);
        g.buffer_state = 2;
    }

    let line = [b",\n", c_str(s), b"\n"].concat();
    let _ = g.stdout.write_all(&line);

    if type_ != g.buffered_type {
        eprintln!("Error: mix of bare geometries and features");
        std::process::exit(EXIT_IMPOSSIBLE);
    }
}

/// Reads the next line of the CSV file, checks that it is UTF-8, and splits it into dequoted fields.
/// `None` at the end of the file.
fn csv_fields(csvfile: &mut BufReader<File>) -> Option<(Vec<String>, String)> {
    let s = csv_getline(csvfile);
    if s.is_empty() {
        return None;
    }

    let err = check_utf8(&s);
    if !err.is_empty() {
        eprintln!("{}", err);
        std::process::exit(EXIT_UTF8);
    }

    let s = String::from_utf8_lossy(c_str(&s)).into_owned();
    let mut fields = csv_split(&s);

    for f in fields.iter_mut() {
        *f = csv_dequote(std::mem::take(f));
    }

    Some((fields, s))
}

/// Writes `s` to stderr up to its first NUL, as `%s` would.
fn eprint_c_str(s: &[u8]) {
    let _ = std::io::stderr().write_all(c_str(s));
}

/// Returns the entries to append to the feature's properties.
fn join_csv(g: &mut Globals, j: JsonRef<'_>) -> Vec<JoinedEntry> {
    let mut joined = Vec::new();
    let csvfile = g.csvfile.as_mut().unwrap();

    if g.header.is_empty() {
        let (header, s) = match csv_fields(csvfile) {
            Some(h) => h,
            None => {
                eprintln!("Couldn't get column header from CSV file");
                std::process::exit(EXIT_CSV);
            }
        };
        g.header = header;

        if g.header.is_empty() {
            eprintln!("No columns in CSV header \"{}\"", s);
            std::process::exit(EXIT_CSV);
        }
    }

    let properties = j.hash_get("properties");
    let mut key = None;

    if let Some(properties) = properties {
        key = properties.hash_get(&g.header[0]);
    }

    let key = match key {
        Some(key) => key,
        None => {
            if !g.join_warned {
                eprintln!("Warning: couldn't find CSV key \"{}\" in JSON", g.header[0]);
                g.join_warned = true;
            }
            return joined;
        }
    };

    let joinkey: Vec<u8> = if key.type_() == JsonType::String {
        key.raw_string_bytes().to_vec()
    } else if key.type_() == JsonType::Number {
        dtoa_milo(key.number()).into_bytes()
    } else {
        key.stringify().into_bytes()
    };

    if joinkey < g.prev_joinkey {
        eprint!("GeoJSON file is out of sort: \"");
        eprint_c_str(&joinkey);
        eprint!("\" follows \"");
        eprint_c_str(&g.prev_joinkey);
        eprintln!("\"");
        std::process::exit(EXIT_IMPOSSIBLE);
    }
    g.prev_joinkey = joinkey.clone();

    if g.fields.is_empty() || joinkey.as_slice() > g.fields[0].as_bytes() {
        let mut prevkey = String::new();
        if !g.fields.is_empty() {
            prevkey = g.fields[0].clone();
        }

        loop {
            g.fields = match csv_fields(csvfile) {
                Some((fields, _)) => fields,
                None => {
                    g.fields.clear();
                    break;
                }
            };

            if !g.fields.is_empty() && g.fields[0] < prevkey {
                eprintln!(
                    "CSV file is out of sort: \"{}\" follows \"{}\"",
                    g.fields[0], prevkey
                );
                std::process::exit(EXIT_CSV);
            }

            if !g.fields.is_empty() && g.fields[0].as_bytes() >= joinkey.as_slice() {
                break;
            }

            if !g.fields.is_empty() {
                prevkey = g.fields[0].clone();
            }
        }
    }

    if !g.fields.is_empty() && joinkey.as_slice() == g.fields[0].as_bytes() {
        for i in 1..g.fields.len() {
            let k = g.header[i].clone();
            let mut v = g.fields[i].clone();
            let mut attr_type = JsonType::String;

            if !v.is_empty() {
                if v.as_bytes()[0] == b'"' {
                    v = csv_dequote(v);
                } else if is_number(&v) {
                    attr_type = JsonType::Number;
                }
            } else if PE.load(Ordering::Relaxed) != 0 {
                attr_type = JsonType::Null;
            }

            if attr_type != JsonType::Null {
                let vo = if attr_type == JsonType::String {
                    JoinedValue::String(v)
                } else if attr_type == JsonType::Number {
                    JoinedValue::Number(atof(&v))
                } else {
                    std::process::abort();
                };

                joined.push(JoinedEntry { key: k, value: vo });
            }
        }
    }

    joined
}

/// `json_print_one` for a string.
fn print_string(val: &mut String, s: &str) {
    val.push('"');

    for c in s.chars() {
        if c == '\\' || c == '"' {
            val.push('\\');
            val.push(c);
        } else if (c as u32) < (' ' as u32) {
            val.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            val.push(c);
        }
    }

    val.push('"');
}

/// `json_stringify(o)` after the `joined` entries have been appended to the hash `properties`.
fn stringify_joined(o: JsonRef<'_>, properties: Option<JsonId>, joined: &[JoinedEntry]) -> String {
    let mut val = String::new();
    json_print(&mut val, o, properties, joined);
    val
}

fn json_print(
    val: &mut String,
    o: JsonRef<'_>,
    properties: Option<JsonId>,
    joined: &[JoinedEntry],
) {
    if joined.is_empty() {
        val.push_str(&o.stringify());
    } else if o.type_() == JsonType::Hash {
        val.push('{');

        let entries = o.entries();
        let extra: &[JoinedEntry] = if properties == Some(o.id()) {
            joined
        } else {
            &[]
        };
        let n = entries.len() + extra.len();
        for (i, (k, v)) in entries.iter().enumerate() {
            json_print(val, *k, properties, joined);
            val.push(':');
            json_print(val, *v, properties, joined);
            if i + 1 < n {
                val.push(',');
            }
        }
        for (i, e) in extra.iter().enumerate() {
            print_string(val, &e.key);
            val.push(':');
            match &e.value {
                JoinedValue::String(s) => print_string(val, s),
                JoinedValue::Number(d) => val.push_str(&dtoa_milo(*d)),
            }
            if entries.len() + i + 1 < n {
                val.push(',');
            }
        }
        val.push('}');
    } else if o.type_() == JsonType::Array {
        val.push('[');
        let arr = o.array();
        for (i, a) in arr.iter().enumerate() {
            json_print(val, *a, properties, joined);
            if i + 1 < arr.len() {
                val.push(',');
            }
        }
        val.push(']');
    } else {
        val.push_str(&o.stringify());
    }
}

struct JsonJoinAction<'g> {
    fname: String,
    g: &'g mut Globals,
}

impl JsonFeatureAction for JsonJoinAction<'_> {
    fn fname(&self) -> &str {
        &self.fname
    }

    fn add_feature(
        &mut self,
        _line: i32,
        geometry: JsonRef<'_>,
        _geometrycollection: bool,
        _properties: Option<JsonRef<'_>>,
        _id: Option<JsonRef<'_>>,
        _tippecanoe: Option<JsonRef<'_>>,
        feature: JsonRef<'_>,
    ) -> i32 {
        let same = std::ptr::eq(feature.tree(), geometry.tree()) && feature.id() == geometry.id();
        if !same {
            // a real feature, not a bare geometry
            let mut joined = Vec::new();
            if self.g.csvfile.is_some() {
                joined = join_csv(self.g, feature);
            }

            let properties = feature.hash_get("properties");
            let s = stringify_joined(feature, properties.map(|p| p.id()), &joined);
            out(self.g, s.as_bytes(), 1, properties, &joined);
        } else {
            out(self.g, geometry.stringify().as_bytes(), 2, None, &[]);
        }

        1
    }

    fn check_crs(&mut self, _j: JsonRef<'_>) {}
}

fn process(g: &mut Globals, fp: Box<dyn Read + '_>, fname: &str) {
    let mut jp = JsonPull::begin_file(fp);

    let mut jja = JsonJoinAction {
        fname: fname.to_string(),
        g,
    };
    parse_json(&mut jja, &mut jp);
}

const fn opt(
    name: &'static str,
    has_arg: i32,
    flag: Option<&'static AtomicI32>,
    val: u8,
) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag,
        val: val as i32,
    }
}

static LONG_OPTIONS: [LongOption; 8] = [
    opt("Wrapping the output", 0, None, 0),
    opt("wrap", NO_ARGUMENT, None, b'w'),
    //
    opt("Sorting and joining", 0, None, 0),
    opt("extract", REQUIRED_ARGUMENT, None, b'e'),
    opt("csv", REQUIRED_ARGUMENT, None, b'c'),
    opt("empty-csv-columns-are-null", NO_ARGUMENT, Some(&PE), 1),
    //
    opt("", 0, None, 0),
    opt("prevent", REQUIRED_ARGUMENT, None, b'p'),
];

fn usage(argv: &[String]) -> ! {
    let forms = ["[options] [file.json ...]"];

    print_usage(&mut std::io::stderr(), &argv[0], &forms, &LONG_OPTIONS, &[]);
    eprintln!("\nIf no files are named, the JSON is read from the standard input.");
    std::process::exit(EXIT_ARGS);
}

pub fn main() {
    restore_sigpipe();

    let mut argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argc = argv.len();
    let mut csv: Option<String> = None;

    let mut g = Globals {
        fail: EXIT_SUCCESS,
        wrap: false,
        extract: None,
        csvfile: None,
        header: Vec::new(),
        fields: Vec::new(),
        buffered: Vec::new(),
        buffered_type: -1,
        buffer_state: 0,
        prev_joinkey: Vec::new(),
        out_warned: false,
        join_warned: false,
        stdout: std::io::stdout().lock(),
    };

    // the options above, with the usage message headings removed
    let real_long_options = strip_usage_headings(&LONG_OPTIONS);
    let getopt_str = getopt_string(&real_long_options);

    let mut go = Getopt::new();
    loop {
        let i = go.getopt_long(&mut argv, &getopt_str, &real_long_options, None);
        if i == -1 {
            break;
        }
        let optarg = go.optarg.clone().unwrap_or_default();

        match u8::try_from(i) {
            Ok(0) => {}

            Ok(b'w') => g.wrap = true,

            Ok(b'e') => g.extract = Some(optarg),

            Ok(b'c') => csv = Some(optarg),

            Ok(b'p') => {
                if optarg == "e" {
                    PE.store(1, Ordering::Relaxed);
                } else {
                    eprintln!("{}: Unknown option for -p{}", argv[0], optarg);
                    std::process::exit(EXIT_ARGS);
                }
            }

            _ => usage(&argv),
        }
    }

    if g.extract.is_some() && g.wrap {
        eprintln!("{}: --wrap and --extract not supported together", argv[0]);
        std::process::exit(EXIT_ARGS);
    }

    if let Some(csv) = &csv {
        match File::open(csv) {
            Ok(f) => g.csvfile = Some(BufReader::new(f)),
            Err(e) => {
                perror_io(csv, &e);
                std::process::exit(EXIT_OPEN);
            }
        }
    }

    let optind = go.optind;
    if optind >= argc {
        process(&mut g, Box::new(std::io::stdin().lock()), "standard input");
    } else {
        for arg in &argv[optind..argc] {
            let f = match File::open(arg) {
                Ok(f) => f,
                Err(e) => {
                    perror_io(arg, &e);
                    std::process::exit(EXIT_OPEN);
                }
            };

            process(&mut g, Box::new(f), arg);
        }
    }

    if g.buffer_state == 1 {
        let line = [c_str(&g.buffered), b"\n"].concat();
        let _ = g.stdout.write_all(&line);
    } else if g.buffer_state == 2 {
        let _ = g.stdout.write_all(b"]}\n");
    }

    // PORT: the C++ checks the result of fclose() on the CSV file, which can't
    // fail for a file opened for reading; Rust closes it when it is dropped.
    g.csvfile = None;

    let _ = g.stdout.flush();
    std::process::exit(g.fail);
}

/// Restores the default `SIGPIPE` action that a C program starts with, so that
/// writing to a closed pipe ends the program quietly instead of failing the write.
fn restore_sigpipe() {
    // SAFETY: setting a signal to its default action has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_quote_uses_decimal_escapes() {
        assert_eq!(sort_quote(b"A"), b"\\u0065".to_vec());
        assert_eq!(
            sort_quote(b"null"),
            b"\\u0110\\u0117\\u0108\\u0108".to_vec()
        );
        // U+00E9 is below 0xD800, so it is escaped in decimal too
        assert_eq!(sort_quote("\u{e9}".as_bytes()), b"\\u0233".to_vec());
        // U+FFFD for a broken sequence is passed through as UTF-8
        assert_eq!(sort_quote(b"\xc3"), "\u{fffd}".as_bytes().to_vec());
        assert_eq!(
            sort_quote("\u{1F600}".as_bytes()),
            "\u{1F600}".as_bytes().to_vec()
        );
    }

    #[test]
    fn decode32_backs_up_after_bad_continuation() {
        // the bad third byte backs up to the second, which is then a stray continuation byte
        assert_eq!(decode32(b"\xe2\x82A"), vec![0xfffd, 0xfffd, 65]);
        assert_eq!(decode32(b"\xc3\xa9\xc3"), vec![0xe9, 0xfffd]);
        assert_eq!(decode32(b"ab\0cd"), vec![97, 98]);
    }

    #[test]
    fn print_string_escapes_like_json_print_one() {
        let mut v = String::new();
        print_string(&mut v, "a\"b\\c\u{1}\u{e9}");
        assert_eq!(v, "\"a\\\"b\\\\c\\u0001\u{e9}\"");
    }
}
