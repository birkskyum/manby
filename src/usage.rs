//! Port of usage.cpp.

use std::io::Write;

use crate::getopt::LongOption;

/// An option that must be specified rather than being optional, and the
/// placeholder to show for its argument in the usage message.
///
/// Options that share the same non-zero `alternation` are alternatives to
/// each other: one of them must be specified, but not more than one, and
/// they are listed together as `(--this=... | --that=...)`.
pub struct UsageRequiredOption {
    pub name: &'static str,
    pub placeholder: &'static str,
    pub alternation: i32,
}

/// Returns the short option string to pass to getopt_long() for the
/// options in `long_options`.
pub fn getopt_string(long_options: &[LongOption]) -> String {
    todo!("usage::getopt_string")
}

/// `long_options` without the headings of the usage message, which are not
/// real options and so must not be passed on to getopt_long().
pub fn strip_usage_headings(long_options: &[LongOption]) -> Vec<LongOption> {
    todo!("usage::strip_usage_headings")
}

/// Prints a usage message for `program` to `out`. See usage.hpp for the format.
pub fn print_usage(
    out: &mut dyn Write,
    program: &str,
    forms: &[&str],
    long_options: &[LongOption],
    required: &[UsageRequiredOption],
) {
    todo!("usage::print_usage")
}
