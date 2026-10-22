//! Port of main.cpp, the `tippecanoe` program: reading the input, sorting the
//! features, and handing them to tile.rs.

/// `progress_time()`: whether enough time has passed to print progress again.
pub fn progress_time() -> bool {
    todo!("tippecanoe_main::progress_time")
}

/// `mkstemp_cloexec`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
pub fn mkstemp_cloexec(name: &mut String) -> i32 {
    todo!("tippecanoe_main::mkstemp_cloexec")
}

/// The `tippecanoe` program.
pub fn main() {
    todo!("tippecanoe_main::main")
}
