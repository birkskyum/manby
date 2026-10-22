//! A port of the `getopt_long` behaviour the C++ gets from libc, including
//! argument permutation, abbreviated long options and the error messages.
//! Follows glibc, which produced the golden test output.
//!
//! This follows `_getopt_internal_r` and `process_long_option` from glibc's
//! posix/getopt.c, including its handling of an optstring starting with `+`,
//! `-` or `:`, of `POSIXLY_CORRECT` in the environment, and of `W;`.

use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};

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

/// Describe how to deal with options that follow non-option ARGV-elements.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ordering_ {
    RequireOrder,
    Permute,
    ReturnInOrder,
}

/// Parser state, the C++ `optind`, `optarg`, `optopt` and `opterr` globals.
pub struct Getopt {
    pub optind: usize,
    pub optarg: Option<String>,
    pub optopt: i32,
    pub opterr: bool,

    // The rest of the ARGV-element being scanned for short options, the
    // C `__nextchar`. None is a NULL pointer.
    nextchar: Option<Vec<u8>>,
    ordering: Ordering_,
    initialized: bool,
    // The ARGV-elements from first_nonopt to last_nonopt are non-options
    // that have been skipped and will be moved to the end by exchange().
    first_nonopt: usize,
    last_nonopt: usize,
}

/// Whether two `flag` pointers are the same.
fn same_flag(a: Option<&'static AtomicI32>, b: Option<&'static AtomicI32>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

/// Writes an error message to stderr as one write, byte for byte.
fn print_error(msg: &[u8]) {
    let _ = std::io::stderr().write_all(msg);
}

/// `optstring[0] == ':'`, which also stands in for a NUL at the end.
fn byte_at(s: &[u8], i: usize) -> u8 {
    if i < s.len() { s[i] } else { 0 }
}

impl Getopt {
    pub fn new() -> Getopt {
        Getopt {
            optind: 1,
            optarg: None,
            optopt: 0,
            opterr: true,
            nextchar: None,
            ordering: Ordering_::Permute,
            initialized: false,
            first_nonopt: 0,
            last_nonopt: 0,
        }
    }

    /// Exchange two adjacent subsequences of ARGV.
    /// One subsequence is elements [first_nonopt,last_nonopt)
    /// which contains all the non-options that have been skipped so far.
    /// The other is elements [last_nonopt,optind), which contains all
    /// the options processed since those non-options were skipped.
    ///
    /// `first_nonopt` and `last_nonopt` are relocated so that they describe
    /// the new indices of the non-options in ARGV after they are moved.
    fn exchange(&mut self, argv: &mut [String]) {
        let mut bottom = self.first_nonopt;
        let middle = self.last_nonopt;
        let mut top = self.optind;

        // Exchange the shorter segment with the far end of the longer segment.
        // That puts the shorter segment into the right place.
        // It leaves the longer segment in the right place overall,
        // but it consists of two parts that need to be swapped next.

        while top > middle && middle > bottom {
            if top - middle > middle - bottom {
                // Bottom segment is the short one.
                let len = middle - bottom;

                // Swap it with the top part of the top segment.
                for i in 0..len {
                    argv.swap(bottom + i, top - (middle - bottom) + i);
                }
                // Exclude the moved bottom segment from further swapping.
                top -= len;
            } else {
                // Top segment is the short one.
                let len = top - middle;

                // Swap it with the bottom part of the bottom segment.
                for i in 0..len {
                    argv.swap(bottom + i, middle + i);
                }
                // Exclude the moved top segment from further swapping.
                bottom += len;
            }
        }

        // Update records for the slots the non-options now occupy.

        self.first_nonopt += self.optind - self.last_nonopt;
        self.last_nonopt = self.optind;
    }

    /// Process the argument starting with nextchar as a long option.
    /// optind should *not* have been advanced over this argument.
    ///
    /// If the value returned is -1, it was not actually a long option, the
    /// state is unchanged, and the argument should be processed as a set
    /// of short options (this can only happen when long_only is true).
    /// Otherwise, the option (and its argument, if any) have been consumed
    /// and the return value is the value to return from getopt_long.
    #[allow(clippy::too_many_arguments)]
    fn process_long_option(
        &mut self,
        argv: &[String],
        optstring: &[u8],
        longopts: &[LongOption],
        longind: Option<&mut usize>,
        long_only: bool,
        print_errors: bool,
        prefix: &str,
    ) -> i32 {
        let argc = argv.len();
        let nextchar = self.nextchar.clone().unwrap_or_default();

        let namelen = nextchar
            .iter()
            .position(|&c| c == b'=')
            .unwrap_or(nextchar.len());
        let name = &nextchar[..namelen];
        let nameend = &nextchar[namelen..];

        let mut pfound: Option<&LongOption> = None;
        let mut option_index: usize = 0;
        let n_options = longopts.len();

        // First look for an exact match, counting the options as a side
        // effect.
        for (i, p) in longopts.iter().enumerate() {
            if p.name.as_bytes() == name {
                // Exact match found.
                pfound = Some(p);
                option_index = i;
                break;
            }
        }

        if pfound.is_none() {
            // Didn't find an exact match, so look for abbreviations.
            let mut ambig_set: Option<Vec<bool>> = None;
            let mut ambig_fallback = false;
            let mut indfound: usize = 0;

            for (i, p) in longopts.iter().enumerate() {
                if p.name.as_bytes().starts_with(name) {
                    match pfound {
                        None => {
                            // First nonexact match found.
                            pfound = Some(p);
                            indfound = i;
                        }
                        Some(found) => {
                            if long_only
                                || found.has_arg != p.has_arg
                                || !same_flag(found.flag, p.flag)
                                || found.val != p.val
                            {
                                // Second or later nonexact match found.
                                if !ambig_fallback {
                                    if !print_errors {
                                        // Don't waste effort tracking the ambig set if
                                        // we're not going to print it anyway.
                                        ambig_fallback = true;
                                    } else if ambig_set.is_none() {
                                        let mut set = vec![false; n_options];
                                        set[indfound] = true;
                                        ambig_set = Some(set);
                                    }
                                    if let Some(set) = ambig_set.as_mut() {
                                        set[i] = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if ambig_set.is_some() || ambig_fallback {
                if print_errors {
                    let mut msg = Vec::new();
                    if ambig_fallback {
                        msg.extend_from_slice(argv[0].as_bytes());
                        msg.extend_from_slice(b": option '");
                        msg.extend_from_slice(prefix.as_bytes());
                        msg.extend_from_slice(&nextchar);
                        msg.extend_from_slice(b"' is ambiguous\n");
                    } else {
                        msg.extend_from_slice(argv[0].as_bytes());
                        msg.extend_from_slice(b": option '");
                        msg.extend_from_slice(prefix.as_bytes());
                        msg.extend_from_slice(&nextchar);
                        msg.extend_from_slice(b"' is ambiguous; possibilities:");

                        let set = ambig_set.as_ref().unwrap();
                        for (option_index, &ambiguous) in set.iter().enumerate() {
                            if ambiguous {
                                msg.extend_from_slice(b" '");
                                msg.extend_from_slice(prefix.as_bytes());
                                msg.extend_from_slice(longopts[option_index].name.as_bytes());
                                msg.push(b'\'');
                            }
                        }

                        msg.push(b'\n');
                    }
                    print_error(&msg);
                }
                self.nextchar = Some(Vec::new());
                self.optind += 1;
                self.optopt = 0;
                return '?' as i32;
            }

            option_index = indfound;
        }

        let pfound = match pfound {
            Some(p) => p,
            None => {
                // Can't find it as a long option.  If this is not getopt_long_only,
                // or the option starts with '--' or is not a valid short option,
                // then it's an error.
                if !long_only
                    || byte_at(argv[self.optind].as_bytes(), 1) == b'-'
                    || !optstring.contains(&byte_at(&nextchar, 0))
                {
                    if print_errors {
                        let mut msg = Vec::new();
                        msg.extend_from_slice(argv[0].as_bytes());
                        msg.extend_from_slice(b": unrecognized option '");
                        msg.extend_from_slice(prefix.as_bytes());
                        msg.extend_from_slice(&nextchar);
                        msg.extend_from_slice(b"'\n");
                        print_error(&msg);
                    }

                    self.nextchar = None;
                    self.optind += 1;
                    self.optopt = 0;
                    return '?' as i32;
                }

                // Otherwise interpret it as a short option.
                return -1;
            }
        };

        // We have found a matching long option.  Consume it.
        self.optind += 1;
        self.nextchar = None;
        if !nameend.is_empty() {
            if pfound.has_arg != 0 {
                self.optarg = Some(String::from_utf8_lossy(&nameend[1..]).into_owned());
            } else {
                if print_errors {
                    let mut msg = Vec::new();
                    msg.extend_from_slice(argv[0].as_bytes());
                    msg.extend_from_slice(b": option '");
                    msg.extend_from_slice(prefix.as_bytes());
                    msg.extend_from_slice(pfound.name.as_bytes());
                    msg.extend_from_slice(b"' doesn't allow an argument\n");
                    print_error(&msg);
                }

                self.optopt = pfound.val;
                return '?' as i32;
            }
        } else if pfound.has_arg == 1 {
            if self.optind < argc {
                self.optarg = Some(argv[self.optind].clone());
                self.optind += 1;
            } else {
                if print_errors {
                    let mut msg = Vec::new();
                    msg.extend_from_slice(argv[0].as_bytes());
                    msg.extend_from_slice(b": option '");
                    msg.extend_from_slice(prefix.as_bytes());
                    msg.extend_from_slice(pfound.name.as_bytes());
                    msg.extend_from_slice(b"' requires an argument\n");
                    print_error(&msg);
                }

                self.optopt = pfound.val;
                return if byte_at(optstring, 0) == b':' {
                    ':' as i32
                } else {
                    '?' as i32
                };
            }
        }

        if let Some(longind) = longind {
            *longind = option_index;
        }
        if let Some(flag) = pfound.flag {
            flag.store(pfound.val, Ordering::Relaxed);
            return 0;
        }
        pfound.val
    }

    /// Initialize internal data upon the first call to getopt.
    fn getopt_initialize<'s>(&mut self, optstring: &'s [u8]) -> &'s [u8] {
        let mut optstring = optstring;

        // Start processing options with ARGV-element 1 (since ARGV-element 0
        // is the program name); the sequence of previously skipped
        // non-option ARGV-elements is empty.
        if self.optind == 0 {
            self.optind = 1;
        }

        self.first_nonopt = self.optind;
        self.last_nonopt = self.optind;
        self.nextchar = None;

        // Determine how to handle the ordering of options and nonoptions.
        if byte_at(optstring, 0) == b'-' {
            self.ordering = Ordering_::ReturnInOrder;
            optstring = &optstring[1..];
        } else if byte_at(optstring, 0) == b'+' {
            self.ordering = Ordering_::RequireOrder;
            optstring = &optstring[1..];
        } else if std::env::var_os("POSIXLY_CORRECT").is_some() {
            self.ordering = Ordering_::RequireOrder;
        } else {
            self.ordering = Ordering_::Permute;
        }

        self.initialized = true;
        optstring
    }

    /// `getopt_long`. May permute `args` as glibc does. Returns -1 when the
    /// options are done, `'?' as i32` on an error, 0 when a long option set its
    /// `flag`, and otherwise the option's `val`.
    #[allow(clippy::ptr_arg)]
    pub fn getopt_long(
        &mut self,
        args: &mut Vec<String>,
        optstring: &str,
        longopts: &[LongOption],
        longindex: Option<&mut usize>,
    ) -> i32 {
        self.getopt_internal(args, optstring, longopts, longindex, false)
    }

    /// `getopt_long_only`.
    #[allow(clippy::ptr_arg)]
    pub fn getopt_long_only(
        &mut self,
        args: &mut Vec<String>,
        optstring: &str,
        longopts: &[LongOption],
        longindex: Option<&mut usize>,
    ) -> i32 {
        self.getopt_internal(args, optstring, longopts, longindex, true)
    }

    fn getopt_internal(
        &mut self,
        argv: &mut [String],
        optstring: &str,
        longopts: &[LongOption],
        mut longind: Option<&mut usize>,
        long_only: bool,
    ) -> i32 {
        let mut optstring = optstring.as_bytes();
        let mut print_errors = self.opterr;
        let argc = argv.len();

        if argc < 1 {
            return -1;
        }

        self.optarg = None;

        if self.optind == 0 || !self.initialized {
            optstring = self.getopt_initialize(optstring);
        } else if byte_at(optstring, 0) == b'-' || byte_at(optstring, 0) == b'+' {
            optstring = &optstring[1..];
        }

        if byte_at(optstring, 0) == b':' {
            print_errors = false;
        }

        // Test whether ARGV[optind] points to a non-option argument.
        let nonoption_p = |argv: &[String], optind: usize| -> bool {
            let a = argv[optind].as_bytes();
            byte_at(a, 0) != b'-' || byte_at(a, 1) == b'\0'
        };

        if self.nextchar.as_ref().is_none_or(|n| n.is_empty()) {
            // Advance to the next ARGV-element.

            // Give FIRST_NONOPT & LAST_NONOPT rational values if OPTIND has been
            // moved back by the user (who may also have changed the arguments).
            if self.last_nonopt > self.optind {
                self.last_nonopt = self.optind;
            }
            if self.first_nonopt > self.optind {
                self.first_nonopt = self.optind;
            }

            if self.ordering == Ordering_::Permute {
                // If we have just processed some options following some non-options,
                // exchange them so that the options come first.

                if self.first_nonopt != self.last_nonopt && self.last_nonopt != self.optind {
                    self.exchange(argv);
                } else if self.last_nonopt != self.optind {
                    self.first_nonopt = self.optind;
                }

                // Skip any additional non-options
                // and extend the range of non-options previously skipped.

                while self.optind < argc && nonoption_p(argv, self.optind) {
                    self.optind += 1;
                }
                self.last_nonopt = self.optind;
            }

            // The special ARGV-element '--' means premature end of options.
            // Skip it like a null option,
            // then exchange with previous non-options as if it were an option,
            // then skip everything else like a non-option.

            if self.optind != argc && argv[self.optind] == "--" {
                self.optind += 1;

                if self.first_nonopt != self.last_nonopt && self.last_nonopt != self.optind {
                    self.exchange(argv);
                } else if self.first_nonopt == self.last_nonopt {
                    self.first_nonopt = self.optind;
                }
                self.last_nonopt = argc;

                self.optind = argc;
            }

            // If we have done all the ARGV-elements, stop the scan
            // and back over any non-options that we skipped and permuted.

            if self.optind == argc {
                // Set the next-arg-index to point at the non-options
                // that we previously skipped, so the caller will digest them.
                if self.first_nonopt != self.last_nonopt {
                    self.optind = self.first_nonopt;
                }
                return -1;
            }

            // If we have come to a non-option and did not permute it,
            // either stop the scan or describe it to the caller and pass it by.

            if nonoption_p(argv, self.optind) {
                if self.ordering == Ordering_::RequireOrder {
                    return -1;
                }
                self.optarg = Some(argv[self.optind].clone());
                self.optind += 1;
                return 1;
            }

            // We have found another option-ARGV-element.
            // Check whether it might be a long option.
            {
                let arg = argv[self.optind].as_bytes();
                if byte_at(arg, 1) == b'-' {
                    // "--foo" is always a long option.  The special option
                    // "--" was handled above.
                    self.nextchar = Some(arg[2..].to_vec());
                    return self.process_long_option(
                        argv,
                        optstring,
                        longopts,
                        longind.as_deref_mut(),
                        long_only,
                        print_errors,
                        "--",
                    );
                }

                // If long_only and the ARGV-element has the form "-f",
                // where f is a valid short option, don't consider it an
                // abbreviated form of a long option that starts with f.
                // Otherwise there would be no way to give the -f short
                // option.
                //
                // On the other hand, if there's a long option "fubar" and
                // the ARGV-element is "-fu", do consider that an
                // abbreviation of the long option, just like "--fu", and
                // not "-f" with arg "u".
                //
                // This distinction seems to be the most useful approach.
                if long_only && (byte_at(arg, 2) != 0 || !optstring.contains(&byte_at(arg, 1))) {
                    self.nextchar = Some(arg[1..].to_vec());
                    let code = self.process_long_option(
                        argv,
                        optstring,
                        longopts,
                        longind.as_deref_mut(),
                        long_only,
                        print_errors,
                        "-",
                    );
                    if code != -1 {
                        return code;
                    }
                }
            }

            // It is not a long option.  Skip the initial punctuation.
            self.nextchar = Some(argv[self.optind].as_bytes()[1..].to_vec());
        }

        self.short_option(argv, optstring, longopts, longind, print_errors)
    }

    /// Look at and handle the next short option-character.
    fn short_option(
        &mut self,
        argv: &[String],
        optstring: &[u8],
        longopts: &[LongOption],
        longind: Option<&mut usize>,
        print_errors: bool,
    ) -> i32 {
        let argc = argv.len();

        let mut next = self.nextchar.take().unwrap_or_default();
        let mut c = byte_at(&next, 0);
        if !next.is_empty() {
            next.remove(0);
        }
        self.nextchar = Some(next);
        let rest_empty = self.nextchar.as_ref().unwrap().is_empty();
        let temp = optstring.iter().position(|&o| o == c);

        // Increment 'optind' when we start to process its last character.
        if rest_empty {
            self.optind += 1;
        }

        let temp = match temp {
            Some(t) if c != b':' && c != b';' => t,
            _ => {
                if print_errors {
                    let mut msg = Vec::new();
                    msg.extend_from_slice(argv[0].as_bytes());
                    msg.extend_from_slice(b": invalid option -- '");
                    msg.push(c);
                    msg.extend_from_slice(b"'\n");
                    print_error(&msg);
                }
                self.optopt = c as i32;
                return '?' as i32;
            }
        };

        // Convenience. Treat POSIX -W foo same as long option --foo
        if byte_at(optstring, temp) == b'W' && byte_at(optstring, temp + 1) == b';' {
            // This is an option that requires an argument.
            if !rest_empty {
                self.optarg =
                    Some(String::from_utf8_lossy(self.nextchar.as_ref().unwrap()).into_owned());
            } else if self.optind == argc {
                if print_errors {
                    let mut msg = Vec::new();
                    msg.extend_from_slice(argv[0].as_bytes());
                    msg.extend_from_slice(b": option requires an argument -- '");
                    msg.push(c);
                    msg.extend_from_slice(b"'\n");
                    print_error(&msg);
                }

                self.optopt = c as i32;
                if byte_at(optstring, 0) == b':' {
                    c = b':';
                } else {
                    c = b'?';
                }
                return c as i32;
            } else {
                self.optarg = Some(argv[self.optind].clone());
            }

            self.nextchar = self.optarg.take().map(|s| s.into_bytes());
            return self.process_long_option(
                argv,
                optstring,
                longopts,
                longind,
                false, // long_only
                print_errors,
                "-W ",
            );
        }
        if byte_at(optstring, temp + 1) == b':' {
            if byte_at(optstring, temp + 2) == b':' {
                // This is an option that accepts an argument optionally.
                if !rest_empty {
                    self.optarg =
                        Some(String::from_utf8_lossy(self.nextchar.as_ref().unwrap()).into_owned());
                    self.optind += 1;
                } else {
                    self.optarg = None;
                }
                self.nextchar = None;
            } else {
                // This is an option that requires an argument.
                if !rest_empty {
                    self.optarg =
                        Some(String::from_utf8_lossy(self.nextchar.as_ref().unwrap()).into_owned());
                    // If we end this ARGV-element by taking the rest as an arg,
                    // we must advance to the next element now.
                    self.optind += 1;
                } else if self.optind == argc {
                    if print_errors {
                        let mut msg = Vec::new();
                        msg.extend_from_slice(argv[0].as_bytes());
                        msg.extend_from_slice(b": option requires an argument -- '");
                        msg.push(c);
                        msg.extend_from_slice(b"'\n");
                        print_error(&msg);
                    }

                    self.optopt = c as i32;
                    if byte_at(optstring, 0) == b':' {
                        c = b':';
                    } else {
                        c = b'?';
                    }
                } else {
                    // We already incremented 'optind' once;
                    // increment it again when taking next ARGV-elt as argument.
                    self.optarg = Some(argv[self.optind].clone());
                    self.optind += 1;
                }
                self.nextchar = None;
            }
        }
        c as i32
    }
}

impl Default for Getopt {
    fn default() -> Self {
        Getopt::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FLAG: AtomicI32 = AtomicI32::new(0);

    static LONGOPTS: &[LongOption] = &[
        LongOption {
            name: "output",
            has_arg: REQUIRED_ARGUMENT,
            flag: None,
            val: 'o' as i32,
        },
        LongOption {
            name: "output-to-directory",
            has_arg: REQUIRED_ARGUMENT,
            flag: None,
            val: 'e' as i32,
        },
        LongOption {
            name: "force",
            has_arg: NO_ARGUMENT,
            flag: None,
            val: 'f' as i32,
        },
        LongOption {
            name: "hilbert",
            has_arg: NO_ARGUMENT,
            flag: Some(&FLAG),
            val: 1,
        },
        LongOption {
            name: "smallest-maximum-zoom-guess",
            has_arg: REQUIRED_ARGUMENT,
            flag: None,
            val: '~' as i32,
        },
        LongOption {
            name: "smallest-other",
            has_arg: REQUIRED_ARGUMENT,
            flag: None,
            val: '~' as i32,
        },
    ];

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn permutes_and_parses() {
        let mut argv = args(&[
            "prog", "in.json", "-o", "out", "--force", "-fe", "dir", "--hilb", "x", "--", "-f",
        ]);
        let mut g = Getopt::new();
        let mut index = usize::MAX;
        let mut seen = Vec::new();
        loop {
            let c = g.getopt_long(&mut argv, "o:e:f~:", LONGOPTS, Some(&mut index));
            if c == -1 {
                break;
            }
            seen.push((c, g.optarg.clone()));
        }
        assert_eq!(
            seen,
            vec![
                ('o' as i32, Some("out".to_string())),
                ('f' as i32, None),
                ('f' as i32, None),
                ('e' as i32, Some("dir".to_string())),
                (0, None),
            ]
        );
        assert_eq!(FLAG.load(Ordering::Relaxed), 1);
        assert_eq!(index, 3);
        assert_eq!(&argv[g.optind..], &args(&["in.json", "x", "-f"])[..]);
    }

    #[test]
    fn long_option_arguments() {
        let mut argv = args(&[
            "prog",
            "--out=a",
            "--output",
            "b",
            "--smallest=3",
            "--output",
        ]);
        let mut g = Getopt::new();
        let mut index = 0;
        assert_eq!(
            g.getopt_long(&mut argv, "o:", LONGOPTS, Some(&mut index)),
            '?' as i32
        );
        assert_eq!(
            g.getopt_long(&mut argv, "o:", LONGOPTS, Some(&mut index)),
            'o' as i32
        );
        assert_eq!(g.optarg.as_deref(), Some("b"));
        // Both matches share has_arg, flag and val, so this is not ambiguous
        assert_eq!(
            g.getopt_long(&mut argv, "o:", LONGOPTS, Some(&mut index)),
            '~' as i32
        );
        assert_eq!((g.optarg.as_deref(), index), (Some("3"), 4));
        // Missing argument at the end
        assert_eq!(
            g.getopt_long(&mut argv, "o:", LONGOPTS, Some(&mut index)),
            '?' as i32
        );
        assert_eq!(g.optopt, 'o' as i32);
        assert_eq!(
            g.getopt_long(&mut argv, "o:", LONGOPTS, Some(&mut index)),
            -1
        );
    }
}
