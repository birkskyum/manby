//! The functions that the package's C code calls where manby is built into
//! the package, in webR and from CRAN: manby's command line, the reports of
//! `manby stats` as text, and `manby serve` on threads of its own.
//!
//! A panic is caught and is exit status 70, so that it doesn't end R.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The exit status of a panic, tippecanoe's "can't happen".
const PANICKED: c_int = 70;

/// What `f` gives, or the status of a panic.
fn guarded(f: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(PANICKED)
}

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
    let argv = unsafe { args(argc, argv) };
    guarded(|| manby::command::run(&argv))
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
    let out = AssertUnwindSafe(out);
    guarded(move || {
        match manby::stats::parse_args(&argv).and_then(|o| manby::stats::text(&o)) {
            Ok(text) => {
                // SAFETY: the caller gives somewhere to write.
                unsafe { *out.0 = into_c(text) };
                0
            }
            Err(e) => {
                if !e.message.is_empty() {
                    eprintln!("{}", e.message);
                }
                e.exit_code
            }
        }
    })
}

/// Text for C, to be freed with `manby_r_free`.
fn into_c(text: String) -> *mut c_char {
    CString::new(text.replace('\0', ""))
        .unwrap_or_default()
        .into_raw()
}

/// Starts `manby serve` from its arguments, the first being the program's
/// name, on threads of its own. Writes the map's address and each tileset's
/// TileJSON address, a line each, into `*out`, to be freed with
/// `manby_r_free`, and the server into `*server`, to be stopped with
/// `manby_r_serve_stop`. Gives the exit status, after printing any error.
///
/// # Safety
///
/// `argv` has `argc` NUL-terminated strings, and `out` and `server` can be
/// written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn manby_r_serve(
    argc: c_int,
    argv: *const *const c_char,
    out: *mut *mut c_char,
    server: *mut *mut c_void,
) -> c_int {
    // SAFETY: as the caller promises.
    let argv = unsafe { args(argc, argv) };
    let (out, server) = (AssertUnwindSafe(out), AssertUnwindSafe(server));
    guarded(move || {
        match manby::serve::parse_args(&argv).and_then(|o| manby::serve::Server::bind(&o)) {
            Ok(s) => {
                let mut text = format!("{}\n", s.url);
                for t in s.tilesets() {
                    text += &format!("{}: {}{}.json\n", t.name, s.url, t.name);
                }
                let running = Box::new(s.spawn());
                // SAFETY: the caller gives somewhere to write.
                unsafe {
                    *out.0 = into_c(text);
                    *server.0 = Box::into_raw(running).cast();
                }
                0
            }
            Err(e) => {
                if !e.message.is_empty() {
                    eprintln!("{}", e.message);
                }
                e.exit_code
            }
        }
    })
}

/// Stops a server from `manby_r_serve`.
///
/// # Safety
///
/// `server` came from `manby_r_serve` and is stopped once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn manby_r_serve_stop(server: *mut c_void) {
    if !server.is_null() {
        // SAFETY: the server came from Box::into_raw.
        let running = unsafe { Box::from_raw(server.cast::<manby::serve::Running>()) };
        let _ = catch_unwind(AssertUnwindSafe(move || running.stop()));
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
