//! A port of the `getopt_long` behaviour the C++ gets from libc, including
//! argument permutation, abbreviated long options and the error messages.
//! Follows glibc, which produced the golden test output.

use std::sync::atomic::AtomicI32;

pub const NO_ARGUMENT: i32 = 0;
pub const REQUIRED_ARGUMENT: i32 = 1;
pub const OPTIONAL_ARGUMENT: i32 = 2;

/// `struct option`. An entry whose `val` is 0 and `flag` is `None` is a heading
/// in the usage message (see usage.rs), and is removed before parsing.
#[derive(Clone, Copy)]
pub struct LongOption {
    pub name: &'static str,
    pub has_arg: i32,
    pub flag: Option<&'static AtomicI32>,
    pub val: i32,
}

/// Parser state, the C++ `optind`, `optarg`, `optopt` and `opterr` globals.
pub struct Getopt {
    pub optind: usize,
    pub optarg: Option<String>,
    pub optopt: i32,
    pub opterr: bool,
}

impl Getopt {
    pub fn new() -> Getopt {
        Getopt {
            optind: 1,
            optarg: None,
            optopt: 0,
            opterr: true,
        }
    }

    /// `getopt_long`. May permute `args` as glibc does. Returns -1 when the
    /// options are done, `'?' as i32` on an error, 0 when a long option set its
    /// `flag`, and otherwise the option's `val`.
    pub fn getopt_long(
        &mut self,
        args: &mut Vec<String>,
        optstring: &str,
        longopts: &[LongOption],
        longindex: Option<&mut usize>,
    ) -> i32 {
        todo!("getopt::getopt_long")
    }
}

impl Default for Getopt {
    fn default() -> Self {
        Getopt::new()
    }
}
