//! Port of text.cpp.

use crate::cstd::{atof, perror};
use crate::errors::{EXIT_JSON, EXIT_OPEN};
use std::io::Read;

/// Returns an empty string if `s` is valid UTF-8 by the C++ rules, otherwise the
/// error message. Takes bytes because the input may not be valid Rust UTF-8.
///
/// The message quotes `s`, so it is not valid UTF-8 itself. Here the quoted
/// input is converted lossily; `check_utf8_raw` returns the exact bytes.
pub fn check_utf8(s: &[u8]) -> String {
    String::from_utf8_lossy(&check_utf8_raw(s)).into_owned()
}

/**
 * Returns an empty string if `s` is valid utf8;
 * otherwise returns an error message.
 */
pub fn check_utf8_raw(s: &[u8]) -> Vec<u8> {
    let mut i = 0;
    while i < s.len() {
        let mut fail: usize = 0;

        if (s[i] & 0x80) == 0x80 {
            if (s[i] & 0xE0) == 0xC0 {
                if i + 1 >= s.len() || (s[i + 1] & 0xC0) != 0x80 {
                    fail = 2;
                } else {
                    i += 1;
                }
            } else if (s[i] & 0xF0) == 0xE0 {
                if i + 2 >= s.len() || (s[i + 1] & 0xC0) != 0x80 || (s[i + 2] & 0xC0) != 0x80 {
                    fail = 3;
                } else {
                    i += 2;
                }
            } else if (s[i] & 0xF8) == 0xF0 {
                if i + 3 >= s.len()
                    || (s[i + 1] & 0xC0) != 0x80
                    || (s[i + 2] & 0xC0) != 0x80
                    || (s[i + 3] & 0xC0) != 0x80
                {
                    fail = 4;
                } else {
                    i += 3;
                }
            } else {
                fail = 1;
            }
        }

        if fail != 0 {
            let mut out: Vec<u8> = Vec::new();
            out.push(b'"');
            out.extend_from_slice(s);
            out.extend_from_slice(b"\" is not valid UTF-8 (");
            let mut j = 0;
            while j < fail && i + j < s.len() {
                if j != 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(format!("0x{:02X}", s[i + j]).as_bytes());
                j += 1;
            }
            out.push(b')');
            return out;
        }

        i += 1;
    }

    Vec::new()
}

/// Byte `i` of a C string held in a slice: past the end reads as the NUL terminator.
fn at(s: &[u8], i: usize) -> u8 {
    if i < s.len() { s[i] } else { 0 }
}

/// Decodes the next character of a NUL-terminated (or slice-terminated) string.
/// Sets `c` to the code point, or -1 at the end, and returns the rest of the
/// string, or `None` at the end.
pub fn utf8_next<'a>(s: &'a [u8], c: &mut i64) -> Option<&'a [u8]> {
    if s.is_empty() || s[0] == b'\0' {
        *c = -1;
        return None;
    }

    let advance;
    if (s[0] & 0x80) == 0x80 {
        if (s[0] & 0xE0) == 0xC0 {
            if (at(s, 1) & 0xC0) != 0x80 {
                *c = 0xFFFD;
                advance = 1;
            } else {
                *c = (((s[0] & 0x1F) as i64) << 6) | ((s[1] & 0x7F) as i64);
                advance = 2;
            }
        } else if (s[0] & 0xF0) == 0xE0 {
            if (at(s, 1) & 0xC0) != 0x80 || (at(s, 2) & 0xC0) != 0x80 {
                *c = 0xFFFD;
                advance = 1;
            } else {
                *c = (((s[0] & 0x0F) as i64) << 12)
                    | (((s[1] & 0x7F) as i64) << 6)
                    | ((s[2] & 0x7F) as i64);
                advance = 3;
            }
        } else if (s[0] & 0xF8) == 0xF0 {
            if (at(s, 1) & 0xC0) != 0x80 || (at(s, 2) & 0xC0) != 0x80 || (at(s, 3) & 0xC0) != 0x80 {
                *c = 0xFFFD;
                advance = 1;
            } else {
                *c = (((s[0] & 0x0F) as i64) << 18)
                    | (((s[1] & 0x7F) as i64) << 12)
                    | (((s[2] & 0x7F) as i64) << 6)
                    | ((s[3] & 0x7F) as i64);
                advance = 4;
            }
        } else {
            *c = 0xFFFD;
            advance = 1;
        }
    } else {
        *c = s[0] as i64;
        advance = 1;
    }

    Some(&s[advance..])
}

pub fn truncate16(s: &str, runes: usize) -> String {
    let start = s.as_bytes();
    let mut cp = start;
    let mut lastgood = 0;
    let mut len: usize = 0;
    let mut c: i64 = 0;

    while let Some(rest) = utf8_next(cp, &mut c) {
        cp = rest;
        if c <= 0xFFFF {
            len += 1;
        } else {
            len += 2;
        }

        if len <= runes {
            lastgood = start.len() - cp.len();
        } else {
            break;
        }
    }

    String::from_utf8_lossy(&start[..lastgood]).into_owned()
}

#[allow(clippy::manual_range_contains)]
pub fn integer_zoom(where_: &str, text: &str) -> i32 {
    let d = atof(text);
    if !d.is_finite() || d != d.floor() || d < 0.0 || d > 32.0 {
        eprintln!(
            "{}: Expected integer zoom level in \"tippecanoe\" GeoJSON extension, not {}",
            where_, text
        );
        std::process::exit(EXIT_JSON);
    }
    d as i32
}

pub fn format_commandline(argv: &[String]) -> String {
    let mut out: Vec<u8> = Vec::new();

    for (i, arg) in argv.iter().enumerate() {
        // The argument as a C string, which ends at a NUL
        let arg = arg.as_bytes();
        let arg = &arg[..arg.iter().position(|&c| c == 0).unwrap_or(arg.len())];

        let mut need_quote = false;
        for &cp in arg {
            if !cp.is_ascii_alphabetic()
                && !cp.is_ascii_digit()
                && cp != b'/'
                && cp != b'-'
                && cp != b'_'
                && cp != b'@'
                && cp != b':'
                && cp != b'.'
                && cp != b'%'
                && cp != b','
            {
                need_quote = true;
                break;
            }
        }

        if need_quote {
            out.push(b'\'');
            for &cp in arg {
                if cp == b'\'' {
                    out.extend_from_slice(b"'\"'\"'");
                } else {
                    out.push(cp);
                }
            }
            out.push(b'\'');
        } else {
            out.extend_from_slice(arg);
        }

        if i + 1 < argv.len() {
            out.push(b' ');
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

// to work with data from https://github.com/kmike/text-unidecode
pub fn read_unidecode(fname: &str) -> Vec<String> {
    let mut data: Vec<u8> = Vec::new();

    let mut f = match std::fs::File::open(fname) {
        Ok(f) => f,
        Err(e) => {
            crate::cstd::perror_io(fname, &e);
            std::process::exit(EXIT_OPEN);
        }
    };

    let mut buf = vec![0u8; 2000];

    loop {
        let nread = match f.read(&mut buf) {
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => 0,
        };
        if nread == 0 {
            break;
        }
        data.extend_from_slice(&buf[..nread]);
    }

    drop(f);

    let mut out: Vec<Vec<u8>> = Vec::new();
    out.push(Vec::new()); // because the data file is 1-indexed
    out.push(Vec::new()); // ascii 001

    for mut c in data {
        if c == b'\0' {
            out.push(Vec::new());
        } else {
            if c <= b'~' {
                c = c.to_ascii_lowercase();
            }
            out.last_mut().unwrap().push(c);
        }
    }

    out.into_iter()
        .map(|v| match String::from_utf8(v) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        })
        .collect()
}

pub fn unidecode_smash(unidecode_data: &[String], s: &str) -> String {
    let mut s = s.as_bytes();
    s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];

    if unidecode_data.is_empty() {
        return String::from_utf8_lossy(s).into_owned();
    }

    let mut out: Vec<u8> = Vec::with_capacity(s.len());

    let mut c: i64 = 0;
    loop {
        let os = s;
        s = match utf8_next(s, &mut c) {
            None => break,
            Some(rest) => rest,
        };

        if c >= 0 && c < unidecode_data.len() as i64 {
            out.extend_from_slice(unidecode_data[c as usize].as_bytes());
        } else {
            // pass through anything that is out of unidecode range literally
            out.extend_from_slice(&os[..os.len() - s.len()]);
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// `fnv1a(std::string const &)`.
pub fn fnv1a(s: &[u8]) -> u64 {
    // Store tiles by a hash of their contents (fnv1a 64-bit)
    // http://www.isthe.com/chongo/tech/comp/fnv/
    const FNV_OFFSET_BASIS: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut h = FNV_OFFSET_BASIS;
    for &c in s {
        h ^= c as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// `fnv1a(const char *, char additional)`: hashes up to the first NUL, then `additional`.
// The "additional" is to make it easier to hash a serial_val attribute value and type together
pub fn fnv1a_cstr(s: &[u8], additional: u8) -> u64 {
    // http://www.isthe.com/chongo/tech/comp/fnv/
    const FNV_OFFSET_BASIS: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut h = FNV_OFFSET_BASIS;
    for &c in s {
        if c == b'\0' {
            break;
        }
        h ^= c as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h ^= additional as u64;
    h = h.wrapping_mul(FNV_PRIME);
    h
}

/// `fnv1a(size_t size, void *p)`.
pub fn fnv1a_mem(p: &[u8]) -> u64 {
    // http://www.isthe.com/chongo/tech/comp/fnv/
    const FNV_OFFSET_BASIS: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut h = FNV_OFFSET_BASIS;
    for &c in p {
        h ^= c as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

// This function reverses the order of the bits in a 64-bit word.
// Instead of shifting individual bits in a loop, it shifts them
// in blocks, starting with swapping the halfwords, and working downward
// until it is swapping individual pairs of adjacent bits.
//
// The purpose is to permute the order in which features are visited:
// instead of working in an orderly fashion from the top left to the
// bottom right of the tile, instead jump around to minimize adjacency,
// like a hash function, but taking advantage of the knowledge that we
// are operating on a fixed-size input that can be directly inverted.
// https://en.wikipedia.org/wiki/Bit-reversal_permutation
//
// This allows calculating an appropriate set of features to appear
// at a fractional zoom level: at what is effectively z4.25, for example,
// we can bring in a quarter of the features that will be added in the
// transition from z4 to z5, and have them be spatially distributed
// across the tile rather than clumped together.

pub fn bit_reverse(mut v: u64) -> u64 {
    v = ((v & 0x00000000FFFFFFFF) << 32) | ((v & 0xFFFFFFFF00000000) >> 32);
    v = ((v & 0x0000FFFF0000FFFF) << 16) | ((v & 0xFFFF0000FFFF0000) >> 16);
    v = ((v & 0x00FF00FF00FF00FF) << 8) | ((v & 0xFF00FF00FF00FF00) >> 8);
    v = ((v & 0x0F0F0F0F0F0F0F0F) << 4) | ((v & 0xF0F0F0F0F0F0F0F0) >> 4);
    v = ((v & 0x3333333333333333) << 2) | ((v & 0xCCCCCCCCCCCCCCCC) >> 2);
    v = ((v & 0x5555555555555555) << 1) | ((v & 0xAAAAAAAAAAAAAAAA) >> 1);
    v
}

pub fn truncate_string(s: &str, len: usize) -> String {
    if s.len() <= len {
        return s.to_string();
    }

    // find the initial byte of a UTF-8 character
    let b = s.as_bytes();
    let mut i = len;
    while i > 0 {
        if (b[i] & 0x80) == 0 || (b[i] & 0xC0) == 0xC0 {
            break;
        }
        i -= 1;
    }

    String::from_utf8_lossy(&b[..i]).into_owned()
}

pub fn starts_with(s: &str, prefix: &str) -> bool {
    s.as_bytes().starts_with(prefix.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_enforcement() {
        assert_eq!(check_utf8(b""), "");
        assert_eq!(check_utf8(b"hello world"), "");
        assert_eq!(check_utf8("Καλημέρα κόσμε".as_bytes()), "");
        assert_eq!(check_utf8("こんにちは 世界".as_bytes()), "");
        assert_eq!(check_utf8("👋🌏".as_bytes()), "");
        assert_eq!(
            check_utf8_raw(b"Hola m\xF3n"),
            b"\"Hola m\xF3n\" is not valid UTF-8 (0xF3 0x6E)".to_vec()
        );
        assert_eq!(
            check_utf8(b"Hola m\xF3n"),
            "\"Hola m\u{FFFD}n\" is not valid UTF-8 (0xF3 0x6E)"
        );
    }

    #[test]
    fn utf8_truncation() {
        assert_eq!(truncate16("0123456789abcdefghi", 16), "0123456789abcdef");
        assert_eq!(truncate16("0123456789éîôüéîôüç", 16), "0123456789éîôüéî");
        assert_eq!(
            truncate16("0123456789😀😬😁😂😃😄😅😆", 16),
            "0123456789😀😬😁"
        );
        assert_eq!(
            truncate16("0123456789😀😬😁😂😃😄😅😆", 17),
            "0123456789😀😬😁"
        );
        assert_eq!(
            truncate16("0123456789あいうえおかきくけこさ", 16),
            "0123456789あいうえおか"
        );

        assert_eq!(truncate_string("789éîôüéîôüç", 3), "789");
        assert_eq!(truncate_string("789éîôüéîôüç", 4), "789");
        assert_eq!(truncate_string("789éîôüéîôüç", 5), "789é");
        assert_eq!(truncate_string("789éîôüéîôüç", 6), "789é");
        assert_eq!(truncate_string("789éîôüéîôüç", 7), "789éî");
        assert_eq!(truncate_string("789éîôüéîôüç", 8), "789éî");

        assert_eq!(
            truncate_string("0123456789😀😬😁😂😃😄😅😆", 10),
            "0123456789"
        );
        assert_eq!(
            truncate_string("0123456789😀😬😁😂😃😄😅😆", 11),
            "0123456789"
        );
        assert_eq!(
            truncate_string("0123456789😀😬😁😂😃😄😅😆", 12),
            "0123456789"
        );
        assert_eq!(
            truncate_string("0123456789😀😬😁😂😃😄😅😆", 13),
            "0123456789"
        );
        assert_eq!(
            truncate_string("0123456789😀😬😁😂😃😄😅😆", 14),
            "0123456789😀"
        );

        assert_eq!(truncate_string("😀", 4), "😀");
        assert_eq!(truncate_string("😀", 3), "");
        assert_eq!(truncate_string("😀", 2), "");
        assert_eq!(truncate_string("😀", 1), "");
        assert_eq!(truncate_string("😀", 0), "");
    }

    #[test]
    fn bit_reversal() {
        assert_eq!(bit_reverse(1), 0x8000000000000000);
        assert_eq!(bit_reverse(0x1234567812489BCF), 0xF3D912481E6A2C48);
        assert_eq!(bit_reverse(0xF3D912481E6A2C48), 0x1234567812489BCF);
    }

    #[test]
    fn next_character() {
        let mut c = 0;
        let s = "aé😀".as_bytes();
        let s = utf8_next(s, &mut c).unwrap();
        assert_eq!(c, 'a' as i64);
        let s = utf8_next(s, &mut c).unwrap();
        assert_eq!(c, 'é' as i64);
        let s = utf8_next(s, &mut c).unwrap();
        assert_eq!(c, '😀' as i64);
        assert!(utf8_next(s, &mut c).is_none());
        assert_eq!(c, -1);

        // a truncated sequence reads the terminator as a bad continuation byte
        let s = utf8_next(b"\xE3\x81", &mut c).unwrap();
        assert_eq!(c, 0xFFFD);
        assert_eq!(s, b"\x81");
        assert!(utf8_next(b"\0abc", &mut c).is_none());
    }

    #[test]
    fn hashes() {
        assert_eq!(fnv1a(b""), 14695981039346656037);
        assert_eq!(fnv1a(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_mem(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_cstr(b"", b'a'), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_cstr(b"a\0b", 0), fnv1a(b"a\0"));
    }

    #[test]
    fn commandline() {
        let argv: Vec<String> = [
            "tippecanoe",
            "-o",
            "out.mbtiles",
            "-l",
            "it's",
            "a b",
            "--x=1,2",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            format_commandline(&argv),
            "tippecanoe -o out.mbtiles -l 'it'\"'\"'s' 'a b' '--x=1,2'"
        );
    }

    #[test]
    fn smash() {
        let data: Vec<String> = (0..128)
            .map(|i| {
                if i == 'A' as usize {
                    "a".to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        assert_eq!(unidecode_smash(&data, "AéA"), "aéa");
        assert_eq!(unidecode_smash(&[], "AéA"), "AéA");
    }
}
