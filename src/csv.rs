//! Port of csv.cpp.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};

use crate::errors::{EXIT_OPEN, EXIT_UTF8};
use crate::text::check_utf8_raw;

/// The byte at `i` of a C string, or NUL past its end.
fn byte_at(s: &[u8], i: usize) -> u8 {
    if i < s.len() { s[i] } else { 0 }
}

/// C `isspace` in the C locale.
fn isspace(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == 0x0B || c == 0x0C || c == b'\r'
}

/// The C++ takes a C string, so a NUL ends the line.
pub fn csv_split(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut ret: Vec<String> = Vec::new();
    let mut i = 0;

    while byte_at(b, i) != 0 && byte_at(b, i) != b'\n' && byte_at(b, i) != b'\r' {
        let start = i;
        let mut within = false;

        while byte_at(b, i) != 0 && byte_at(b, i) != b'\n' && byte_at(b, i) != b'\r' {
            if b[i] == b'"' {
                within = !within;
            }

            if b[i] == b',' && !within {
                break;
            }
            i += 1;
        }

        // The split points are all ASCII, so these are whole UTF-8 sequences.
        let v = s[start..i].to_string();
        ret.push(v);

        if byte_at(b, i) == b',' {
            i += 1;

            while byte_at(b, i) != 0 && isspace(b[i]) {
                i += 1;
            }

            if byte_at(b, i) == b'\0' || byte_at(b, i) == b'\r' || byte_at(b, i) == b'\n' {
                ret.push(String::new());
                break;
            }
        }
    }

    ret
}

pub fn csv_dequote(s: String) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    for i in 0..b.len() {
        if b[i] == b'"' {
            if i + 1 < b.len() && b[i + 1] == b'"' {
                out.push(b'"');
            }
        } else {
            out.push(b[i]);
        }
    }
    // Only ASCII quotes were dropped, so this is still UTF-8.
    String::from_utf8(out).unwrap()
}

/// Reads one line including its newline, like the C++ loop over `getc`.
/// Returns an empty string at the end of the file.
pub fn csv_getline(f: &mut dyn BufRead) -> Vec<u8> {
    let mut out = Vec::new();
    // A read error ends the line, as EOF from getc() would.
    let _ = f.read_until(b'\n', &mut out);
    out
}

/// Prints the `check_utf8` message, which quotes the line with its invalid
/// bytes, and exits.
fn utf8_error(fn_: &str, err: &[u8]) -> ! {
    let mut msg = format!("{}: ", fn_).into_bytes();
    msg.extend_from_slice(err);
    msg.push(b'\n');
    let _ = std::io::stderr().write_all(&msg);
    std::process::exit(EXIT_UTF8);
}

pub fn readcsv(fn_: &str, header: &mut Vec<String>, mapping: &mut BTreeMap<String, Vec<String>>) {
    let f = match std::fs::File::open(fn_) {
        Ok(f) => f,
        Err(_) => {
            crate::cstd::perror(fn_);
            std::process::exit(EXIT_OPEN);
        }
    };
    let mut f = BufReader::new(f);

    let s = csv_getline(&mut f);
    if !s.is_empty() {
        let err = check_utf8_raw(&s);
        if !err.is_empty() {
            utf8_error(fn_, &err);
        }
        // PORT: check_utf8 lets through a few sequences that Rust strings
        // can't hold (surrogates, overlong forms); those become U+FFFD.
        let s = String::from_utf8_lossy(&s);

        *header = csv_split(&s);

        for h in header.iter_mut() {
            *h = csv_dequote(std::mem::take(h));
        }
    }
    loop {
        let s = csv_getline(&mut f);
        if s.is_empty() {
            break;
        }

        let err = check_utf8_raw(&s);
        if !err.is_empty() {
            utf8_error(fn_, &err);
        }
        let s = String::from_utf8_lossy(&s);

        let mut line = csv_split(&s);
        if !line.is_empty() {
            line[0] = csv_dequote(std::mem::take(&mut line[0]));
        }

        let mut i = 0;
        while i < line.len() && i < header.len() {
            // printf("putting %s\n", line[0].c_str());
            // std::map::insert leaves an existing entry alone
            if !mapping.contains_key(&line[0]) {
                mapping.insert(line[0].clone(), line.clone());
            }
            i += 1;
        }
    }

    // fclose(): closing a file that was only read has nothing to report.
    drop(f);
}

// Follow JSON rules for what looks like a number
pub fn is_number(s: &str) -> bool {
    // The C++ reads a NUL-terminated string, so past the end is NUL.
    let b = s.as_bytes();
    let mut cp = 0;
    let mut c = byte_at(b, cp);
    cp += 1;

    if c == b'-' || c.is_ascii_digit() {
        if c == b'-' {
            c = byte_at(b, cp);
            cp += 1;
        }

        if c == b'0' {
        } else if (b'1'..=b'9').contains(&c) {
            c = byte_at(b, cp);

            while c.is_ascii_digit() {
                cp += 1;
                c = byte_at(b, cp);
            }
        }

        if byte_at(b, cp) == b'.' {
            cp += 1;

            c = byte_at(b, cp);
            if !c.is_ascii_digit() {
                return false;
            }
            while c.is_ascii_digit() {
                cp += 1;
                c = byte_at(b, cp);
            }
        }

        c = byte_at(b, cp);
        if c == b'e' || c == b'E' {
            cp += 1;

            c = byte_at(b, cp);
            if c == b'+' || c == b'-' {
                cp += 1;
            }

            c = byte_at(b, cp);
            if !c.is_ascii_digit() {
                return false;
            }
            while c.is_ascii_digit() {
                cp += 1;
                c = byte_at(b, cp);
            }
        }

        if byte_at(b, cp) == b'\0' {
            return true;
        } else {
            // Something non-numeric at the end
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert_eq!(csv_split("a,b,c\n"), vec!["a", "b", "c"]);
        assert_eq!(csv_split("\"a,b\",c\r\n"), vec!["\"a,b\"", "c"]);
        assert_eq!(csv_split("a,  b"), vec!["a", "b"]);
        assert_eq!(csv_split("a,\n"), vec!["a", ""]);
        assert_eq!(csv_split("a,  \n"), vec!["a", ""]);
        assert_eq!(csv_split(""), Vec::<String>::new());
        assert_eq!(csv_split("a\0,b"), vec!["a"]);
    }

    #[test]
    fn dequote() {
        assert_eq!(csv_dequote("\"a\"\"b\"".to_string()), "a\"b");
        assert_eq!(csv_dequote("\"x\"".to_string()), "x");
    }

    #[test]
    fn getline() {
        let mut r: &[u8] = b"one\ntwo";
        assert_eq!(csv_getline(&mut r), b"one\n");
        assert_eq!(csv_getline(&mut r), b"two");
        assert_eq!(csv_getline(&mut r), b"");
    }

    #[test]
    fn numbers() {
        for s in ["0", "-0", "12", "1.5", "-1.5e10", "1E+3", "0.5e-2", "-a"] {
            assert!(is_number(s), "{s}");
        }
        for s in ["", "a", "01", "1.", ".5", "1e", "1e+", "12x", "+1", "1.5.5"] {
            assert!(!is_number(s), "{s}");
        }
    }
}
