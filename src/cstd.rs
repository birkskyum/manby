//! C library behaviour that tippecanoe relies on and Rust's standard library does
//! not reproduce. The parsers accept a numeric prefix and ignore the rest of the
//! string, returning 0 if there is none, like their libc namesakes.

/// libc `atof`.
pub fn atof(s: &str) -> f64 {
    todo!("cstd::atof")
}

/// libc `atoi`.
pub fn atoi(s: &str) -> i32 {
    todo!("cstd::atoi")
}

/// libc `atol` (64 bits on LP64).
pub fn atol(s: &str) -> i64 {
    todo!("cstd::atol")
}

/// libc `atoll`.
pub fn atoll(s: &str) -> i64 {
    todo!("cstd::atoll")
}

/// libc `strtod`. Returns the value and the number of bytes consumed (0 if no conversion).
pub fn strtod(s: &str) -> (f64, usize) {
    todo!("cstd::strtod")
}

/// libc `strtoll`. Returns the value and the number of bytes consumed. Sets errno to ERANGE on overflow, like libc.
pub fn strtoll(s: &str, base: u32) -> (i64, usize) {
    todo!("cstd::strtoll")
}

/// libc `strtoull`. Returns the value and the number of bytes consumed. Sets errno to ERANGE on overflow, like libc.
pub fn strtoull(s: &str, base: u32) -> (u64, usize) {
    todo!("cstd::strtoull")
}

/// The current `errno`.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets `errno`, for code that clears it before a libc-style call.
pub fn set_errno(v: i32) {
    todo!("cstd::set_errno")
}

/// libc `strerror`.
pub fn strerror(errnum: i32) -> String {
    todo!("cstd::strerror")
}

/// libc `perror`: prints `s: <strerror(errno)>` to stderr.
pub fn perror(s: &str) {
    todo!("cstd::perror")
}
