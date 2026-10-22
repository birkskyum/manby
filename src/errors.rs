//! Port of errors.hpp. Exit codes, avoiding 124, 125, 126, 127 and 137, which GNU timeout uses.

pub const EXIT_INCOMPLETE: i32 = 100;
pub const EXIT_ARGS: i32 = 101;
pub const EXIT_CLOSE: i32 = 102;
pub const EXIT_CSV: i32 = 103;
pub const EXIT_EXISTS: i32 = 104;
pub const EXIT_FILTER: i32 = 105;
pub const EXIT_IMPOSSIBLE: i32 = 106;
pub const EXIT_JSON: i32 = 107;
pub const EXIT_MEMORY: i32 = 108;
pub const EXIT_MVT: i32 = 109;
pub const EXIT_NODATA: i32 = 110;
pub const EXIT_OPEN: i32 = 111;
pub const EXIT_PROTOBUF: i32 = 112;
pub const EXIT_PTHREAD: i32 = 113;
pub const EXIT_READ: i32 = 114;
pub const EXIT_SEEK: i32 = 115;
pub const EXIT_SQLITE: i32 = 116;
pub const EXIT_STAT: i32 = 117;
pub const EXIT_UNLINK: i32 = 118;
pub const EXIT_UTF8: i32 = 119;
pub const EXIT_WRITE: i32 = 120;

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
