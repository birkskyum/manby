//! C library behaviour that tippecanoe relies on and Rust's standard library does
//! not reproduce. The parsers accept a numeric prefix and ignore the rest of the
//! string, returning 0 if there is none, like their libc namesakes.
//!
//! They call the libc functions themselves on a NUL-terminated copy of the
//! string, so the results, including the rounding of `strtod` and the setting
//! of `errno`, are the ones the C++ gets. As in C, the string ends at its first
//! NUL byte.

use std::ffi::{CStr, c_char};

/// Runs `f` on a NUL-terminated copy of `s`, cut at the first NUL like a C string.
fn with_cstr<R>(s: &[u8], f: impl FnOnce(*const c_char) -> R) -> R {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    let s = &s[..end];

    let mut stack = [0u8; 128];
    if s.len() < stack.len() {
        stack[..s.len()].copy_from_slice(s);
        stack[s.len()] = 0;
        f(stack.as_ptr() as *const c_char)
    } else {
        let mut heap = Vec::with_capacity(s.len() + 1);
        heap.extend_from_slice(s);
        heap.push(0);
        f(heap.as_ptr() as *const c_char)
    }
}

/// libc `atof`.
pub fn atof(s: &str) -> f64 {
    // SAFETY: the pointer is a NUL-terminated string that outlives the call.
    with_cstr(s.as_bytes(), |p| unsafe { libc::atof(p) })
}

/// libc `atoi`.
pub fn atoi(s: &str) -> i32 {
    // SAFETY: the pointer is a NUL-terminated string that outlives the call.
    with_cstr(s.as_bytes(), |p| unsafe { libc::atoi(p) })
}

/// libc `atol` (64 bits on LP64).
pub fn atol(s: &str) -> i64 {
    // SAFETY: the pointer is a NUL-terminated string that outlives the call.
    with_cstr(s.as_bytes(), |p| unsafe { libc::atol(p) }) as i64
}

/// libc `atoll`.
pub fn atoll(s: &str) -> i64 {
    // SAFETY: the pointer is a NUL-terminated string that outlives the call.
    with_cstr(s.as_bytes(), |p| unsafe { libc::atoll(p) })
}

/// libc `strtod`. Returns the value and the number of bytes consumed (0 if no conversion).
pub fn strtod(s: &str) -> (f64, usize) {
    with_cstr(s.as_bytes(), |p| {
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: p is a NUL-terminated string that outlives the call, and end
        // is set to a pointer into it.
        let v = unsafe { libc::strtod(p, &mut end) };
        (v, end as usize - p as usize)
    })
}

/// libc `strtoll`. Returns the value and the number of bytes consumed. Sets errno to ERANGE on overflow, like libc.
pub fn strtoll(s: &str, base: u32) -> (i64, usize) {
    with_cstr(s.as_bytes(), |p| {
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: p is a NUL-terminated string that outlives the call, and end
        // is set to a pointer into it.
        let v = unsafe { libc::strtoll(p, &mut end, base as libc::c_int) };
        (v, end as usize - p as usize)
    })
}

/// libc `strtoull`. Returns the value and the number of bytes consumed. Sets errno to ERANGE on overflow, like libc.
pub fn strtoull(s: &str, base: u32) -> (u64, usize) {
    with_cstr(s.as_bytes(), |p| {
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: p is a NUL-terminated string that outlives the call, and end
        // is set to a pointer into it.
        let v = unsafe { libc::strtoull(p, &mut end, base as libc::c_int) };
        (v, end as usize - p as usize)
    })
}

/// libc `strtoul` (64 bits on LP64). Returns the value and the number of bytes consumed.
pub fn strtoul(s: &str, base: u32) -> (u64, usize) {
    with_cstr(s.as_bytes(), |p| {
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: p is a NUL-terminated string that outlives the call, and end
        // is set to a pointer into it.
        let v = unsafe { libc::strtoul(p, &mut end, base as libc::c_int) };
        (v as u64, end as usize - p as usize)
    })
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: __error has no preconditions and returns this thread's errno.
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: __errno_location has no preconditions and returns this thread's errno.
    unsafe { libc::__errno_location() }
}

/// The current `errno`.
pub fn errno() -> i32 {
    // SAFETY: the pointer is this thread's errno, valid for the thread's lifetime.
    unsafe { *errno_location() }
}

/// Sets `errno`, for code that clears it before a libc-style call.
pub fn set_errno(v: i32) {
    // SAFETY: the pointer is this thread's errno, valid for the thread's lifetime.
    unsafe { *errno_location() = v }
}

/// libc `strerror`.
pub fn strerror(errnum: i32) -> String {
    // SAFETY: strerror returns a NUL-terminated string. It may be overwritten by
    // a later call, so it is copied at once.
    unsafe { CStr::from_ptr(libc::strerror(errnum)) }
        .to_string_lossy()
        .into_owned()
}

/// libc `perror`: prints `s: <strerror(errno)>` to stderr.
pub fn perror(s: &str) {
    let e = errno();
    perror_errno(s, e);
}

/// `perror` for an error the caller already holds, such as the
/// `raw_os_error()` of a `std::io::Error`, rather than the current `errno`.
pub fn perror_errno(s: &str, errnum: i32) {
    let msg = strerror(errnum);
    if s.is_empty() {
        eprintln!("{}", msg);
    } else {
        eprintln!("{}: {}", s, msg);
    }
}

/// `perror` for a `std::io::Error`, printing what libc would for its errno.
pub fn perror_io(s: &str, e: &std::io::Error) {
    match e.raw_os_error() {
        Some(errnum) => perror_errno(s, errnum),
        None => {
            if s.is_empty() {
                eprintln!("{}", e);
            } else {
                eprintln!("{}: {}", s, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixes() {
        assert_eq!(atof("1.5abc"), 1.5);
        assert_eq!(atof("  -2e3"), -2000.0);
        assert_eq!(atof("abc"), 0.0);
        assert_eq!(atof("1\u{0}5"), 1.0);
        assert_eq!(atoi("42xyz"), 42);
        assert_eq!(atoi(" -7"), -7);
        assert_eq!(atol("9000000000"), 9000000000);
        assert_eq!(atoll("-9000000000 "), -9000000000);
        assert_eq!(strtod("3.25 rest"), (3.25, 4));
        assert_eq!(strtod("nope"), (0.0, 0));
        assert_eq!(strtoll("0x1f", 0), (31, 4));
        assert_eq!(strtoull("18446744073709551615", 10), (u64::MAX, 20));
        assert_eq!(strtoul("ffff", 16), (0xffff, 4));
    }

    #[test]
    fn sets_errno() {
        set_errno(0);
        assert_eq!(errno(), 0);
        let (v, n) = strtoull("18446744073709551616", 10);
        assert_eq!((v, n), (u64::MAX, 20));
        assert_eq!(errno(), libc::ERANGE);

        set_errno(0);
        let (v, _) = strtoll("-9223372036854775809", 10);
        assert_eq!(v, i64::MIN);
        assert_eq!(errno(), libc::ERANGE);

        set_errno(0);
        let (v, _) = strtoll("123", 10);
        assert_eq!(v, 123);
        assert_eq!(errno(), 0);
    }

    #[test]
    fn long_strings() {
        let s = format!("{}1", "0".repeat(300));
        assert_eq!(atoll(&s), 1);
        assert_eq!(strtod(&s), (1.0, 301));
    }

    #[test]
    fn messages() {
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
    }
}
