//! The functions that the package's C code calls in webR: manby's command
//! line, and the reports of `manby stats` as text.

use std::ffi::{CStr, CString, c_char, c_int};

/// The arguments, the first being the program's name.
///
/// # Safety
///
/// `argv` has `argc` NUL-terminated strings.
unsafe fn args(argc: c_int, argv: *const *const c_char) -> Vec<String> {
    (0..argc as usize)
        // SAFETY: the caller passes argc valid strings.
        .map(|i| {
            unsafe { CStr::from_ptr(*argv.add(i)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// Runs manby's command line and gives its exit status.
///
/// # Safety
///
/// `argv` has `argc` NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn manby_r_run(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: as the caller promises.
    manby::command::run(&unsafe { args(argc, argv) })
}

/// Makes `manby stats`'s reports from its arguments, the first being the
/// program's name, into `*out`, to be freed with `manby_r_free`. Gives the
/// exit status, after printing any error.
///
/// # Safety
///
/// `argv` has `argc` NUL-terminated strings, and `out` can be written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn manby_r_stats(
    argc: c_int,
    argv: *const *const c_char,
    out: *mut *mut c_char,
) -> c_int {
    // SAFETY: as the caller promises.
    let argv = unsafe { args(argc, argv) };
    match manby::stats::parse_args(&argv).and_then(|o| manby::stats::text(&o)) {
        Ok(text) => {
            let text = CString::new(text.replace('\0', "")).unwrap_or_default();
            // SAFETY: the caller gives somewhere to write.
            unsafe { *out = text.into_raw() };
            0
        }
        Err(e) => {
            if !e.message.is_empty() {
                eprintln!("{}", e.message);
            }
            e.exit_code
        }
    }
}

/// Frees a string from `manby_r_stats`.
///
/// # Safety
///
/// `s` came from `manby_r_stats` and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn manby_r_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: the string came from CString::into_raw.
        drop(unsafe { CString::from_raw(s) });
    }
}
