//! Port of usage.cpp.

use std::collections::BTreeSet;
use std::io::Write;

use crate::getopt::{LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};

// Options are wrapped to fit within this many columns
const USAGE_WIDTH: usize = 80;

// The indentation of the continuation lines of the option list
const USAGE_INDENT: usize = 8;

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
    let mut getopt_str = String::new();

    for lo in long_options {
        if lo.val > ' ' as i32 {
            getopt_str.push(lo.val as u8 as char);

            if lo.has_arg == REQUIRED_ARGUMENT {
                getopt_str.push(':');
            }
        }
    }

    getopt_str
}

/// `long_options` without the headings of the usage message, which are not
/// real options and so must not be passed on to getopt_long().
pub fn strip_usage_headings(long_options: &[LongOption]) -> Vec<LongOption> {
    long_options
        .iter()
        .filter(|lo| lo.val != 0)
        .copied()
        .collect()
}

// The entry for `name` in the list of options that must be specified,
// or None if it is an optional option
fn required_for<'r>(
    name: &str,
    required: &'r [UsageRequiredOption],
) -> Option<&'r UsageRequiredOption> {
    required.iter().find(|r| r.name == name)
}

// "--option", or "--option=placeholder" if the option takes an argument
fn option_text(opt: &LongOption, req: Option<&UsageRequiredOption>) -> String {
    let mut text = format!("--{}", opt.name);

    if opt.has_arg != NO_ARGUMENT {
        text += "=";
        text += match req {
            Some(req) => req.placeholder,
            None => "...",
        };
    }

    text
}

/// The options to list: up to the first one with an empty name.
fn listed(long_options: &[LongOption]) -> impl Iterator<Item = &LongOption> {
    long_options.iter().take_while(|lo| !lo.name.is_empty())
}

// The alternatives that `req` belongs to, as "(--this=... | --that=...)"
fn alternation_text(
    long_options: &[LongOption],
    required: &[UsageRequiredOption],
    alternation: i32,
) -> String {
    let mut text = String::new();
    let mut found: usize = 0;

    for lo in listed(long_options) {
        let req = required_for(lo.name, required);

        if let Some(req) = req
            && req.alternation == alternation
        {
            if found > 0 {
                text += " | ";
            }
            found += 1;

            text += &option_text(lo, Some(req));
        }
    }

    if found > 1 {
        text = format!("({})", text);
    }

    text
}

/// Prints a usage message for `program` to `out`:
///
/// ```text
/// Usage: program forms[0]
///    or: program forms[1]
///         [--some-option] [--another-option=...] ...
/// ```
///
/// where `forms` lists the ways the non-option arguments can be given, and
/// the list of options is derived from `long_options`, the same table that
/// is passed to getopt_long(), so that the message stays in sync with the
/// options that are really accepted.
///
/// Options named in `required` are shown without brackets, using the
/// placeholder given there for their argument, and grouped with any
/// alternatives to them. An entry in `long_options` with no `val` is printed
/// as a heading for the options that follow it, and an entry with an empty
/// name ends the listing, hiding any options after it.
pub fn print_usage(
    out: &mut dyn Write,
    program: &str,
    forms: &[&str],
    long_options: &[LongOption],
    required: &[UsageRequiredOption],
) {
    let mut msg = String::new();

    for (f, form) in forms.iter().enumerate() {
        let lead = if f == 0 { "Usage: " } else { "\n   or: " };
        msg += &format!("{}{} {}", lead, program, form);
    }

    // whatever the forms took up, the option list starts on a line of its own
    let mut width = USAGE_WIDTH;
    let mut alternations_listed: BTreeSet<i32> = BTreeSet::new();

    for lo in listed(long_options) {
        if lo.val == 0 {
            msg += &format!("\n  {}\n{:width$}", lo.name, "", width = USAGE_INDENT);
            width = USAGE_INDENT;
            continue;
        }

        let req = required_for(lo.name, required);
        let text = match req {
            None => format!("[{}]", option_text(lo, None)),
            Some(req) if req.alternation == 0 => option_text(lo, Some(req)),
            Some(req) => {
                if alternations_listed.contains(&req.alternation) {
                    continue; // already listed with the first of its alternatives
                }
                alternations_listed.insert(req.alternation);

                alternation_text(long_options, required, req.alternation)
            }
        };

        if width + 1 + text.len() >= USAGE_WIDTH {
            msg += &format!("\n{:width$}", "", width = USAGE_INDENT);
            width = USAGE_INDENT;
        }

        msg += &format!(" {}", text);
        width += 1 + text.len();
    }

    msg += "\n";
    let _ = out.write_all(msg.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::getopt::OPTIONAL_ARGUMENT;
    use std::sync::atomic::AtomicI32;

    static FLAG: AtomicI32 = AtomicI32::new(0);

    fn opt(
        name: &'static str,
        has_arg: i32,
        flag: Option<&'static AtomicI32>,
        val: i32,
    ) -> LongOption {
        LongOption {
            name,
            has_arg,
            flag,
            val,
        }
    }

    #[test]
    fn usage_message() {
        let options = [
            opt("Output tileset", 0, None, 0),
            opt("output", REQUIRED_ARGUMENT, None, 'o' as i32),
            opt("output-to-directory", REQUIRED_ARGUMENT, None, 'e' as i32),
            opt("force", NO_ARGUMENT, None, 'f' as i32),
            opt("Zoom levels", 0, None, 0),
            opt("maximum-zoom", REQUIRED_ARGUMENT, None, 'z' as i32),
            opt(
                "extend-zooms-if-still-dropping",
                NO_ARGUMENT,
                Some(&FLAG),
                1,
            ),
            opt(
                "smallest-maximum-zoom-guess",
                REQUIRED_ARGUMENT,
                None,
                '~' as i32,
            ),
            opt("optional", OPTIONAL_ARGUMENT, None, 'q' as i32),
            opt("", 0, None, 0),
            opt("hidden", NO_ARGUMENT, None, 'H' as i32),
        ];
        let required = [
            UsageRequiredOption {
                name: "output",
                placeholder: "output.mbtiles",
                alternation: 1,
            },
            UsageRequiredOption {
                name: "output-to-directory",
                placeholder: "directory",
                alternation: 1,
            },
        ];

        let mut out = Vec::new();
        print_usage(
            &mut out,
            "tippecanoe",
            &["[options] [file.json ...]", "--other"],
            &options,
            &required,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Usage: tippecanoe [options] [file.json ...]\n   or: tippecanoe --other\n  Output tileset\n         (--output=output.mbtiles | --output-to-directory=directory) [--force]\n  Zoom levels\n         [--maximum-zoom=...] [--extend-zooms-if-still-dropping]\n         [--smallest-maximum-zoom-guess=...] [--optional=...]\n"
        );

        assert_eq!(
            getopt_string(&strip_usage_headings(&options)),
            "o:e:fz:~:qH"
        );
        assert_eq!(strip_usage_headings(&options).len(), 8);
    }
}
