//! Port of text.cpp.

/// Returns an empty string if `s` is valid UTF-8 by the C++ rules, otherwise the
/// error message. Takes bytes because the input may not be valid Rust UTF-8.
pub fn check_utf8(s: &[u8]) -> String {
    todo!("text::check_utf8")
}

/// Decodes the next character of a NUL-terminated (or slice-terminated) string.
/// Sets `c` to the code point, or -1 at the end, and returns the rest of the
/// string, or `None` at the end.
pub fn utf8_next<'a>(s: &'a [u8], c: &mut i64) -> Option<&'a [u8]> {
    todo!("text::utf8_next")
}

pub fn truncate16(s: &str, runes: usize) -> String {
    todo!("text::truncate16")
}

pub fn integer_zoom(where_: &str, text: &str) -> i32 {
    todo!("text::integer_zoom")
}

pub fn format_commandline(argv: &[String]) -> String {
    todo!("text::format_commandline")
}

pub fn read_unidecode(fname: &str) -> Vec<String> {
    todo!("text::read_unidecode")
}

pub fn unidecode_smash(unidecode_data: &[String], s: &str) -> String {
    todo!("text::unidecode_smash")
}

/// `fnv1a(std::string const &)`.
pub fn fnv1a(s: &[u8]) -> u64 {
    todo!("text::fnv1a")
}

/// `fnv1a(const char *, char additional)`: hashes up to the first NUL, then `additional`.
pub fn fnv1a_cstr(s: &[u8], additional: u8) -> u64 {
    todo!("text::fnv1a_cstr")
}

/// `fnv1a(size_t size, void *p)`.
pub fn fnv1a_mem(p: &[u8]) -> u64 {
    todo!("text::fnv1a_mem")
}

pub fn bit_reverse(v: u64) -> u64 {
    todo!("text::bit_reverse")
}

pub fn truncate_string(s: &str, len: usize) -> String {
    todo!("text::truncate_string")
}

pub fn starts_with(s: &str, prefix: &str) -> bool {
    todo!("text::starts_with")
}
