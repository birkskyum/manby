//! tile-join's command line: the options of tile-join.cpp's `main`, in its
//! order, with its usage message, messages and exit statuses, and manby's
//! own options at the end.

use std::sync::Arc;

use tippecanoe::errors::{EXIT_ARGS, EXIT_JSON, EXIT_OPEN};
use tippecanoe::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use tippecanoe::jsonpull::{JsonPull, JsonTree};
use tippecanoe::usage::{UsageRequiredOption, print_usage, strip_usage_headings};

use super::JoinOptions;
use super::csv::RawCsv;
use crate::options::Output;
use crate::{Error, Result};

/// Values for the options tile-join sets through `flag` pointers.
const PE: i32 = 0x100;
const PK: i32 = 0x101;
const PC: i32 = 0x102;
const PG: i32 = 0x103;

const REQ: i32 = REQUIRED_ARGUMENT;
const NO: i32 = NO_ARGUMENT;

const fn heading(name: &'static str) -> LongOption {
    LongOption {
        name,
        has_arg: 0,
        flag: None,
        val: 0,
    }
}

const fn opt(name: &'static str, has_arg: i32, val: i32) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag: None,
        val,
    }
}

const fn c(b: u8) -> i32 {
    b as i32
}

fn long_options_orig() -> Vec<LongOption> {
    vec![
        heading("Output tileset"),
        opt("output", REQ, c(b'o')),
        opt("output-to-directory", REQ, c(b'e')),
        opt("force", NO, c(b'f')),
        heading("Tileset description and attribution"),
        opt("name", REQ, c(b'n')),
        opt("attribution", REQ, c(b'A')),
        opt("description", REQ, c(b'N')),
        heading("Input tilesets"),
        opt("read-from", REQ, c(b'r')),
        heading("Zoom levels"),
        opt("maximum-zoom", REQ, c(b'z')),
        opt("minimum-zoom", REQ, c(b'Z')),
        opt("overzoom", NO, c(b'O')),
        opt("buffer", REQ, c(b'b')),
        heading("Layer names"),
        opt("layer", REQ, c(b'l')),
        opt("exclude-layer", REQ, c(b'L')),
        opt("rename-layer", REQ, c(b'R')),
        heading("Joining with a CSV file"),
        opt("csv", REQ, c(b'c')),
        opt("if-matched", NO, c(b'i')),
        opt("empty-csv-columns-are-null", NO, PE),
        heading("Filtering feature attributes"),
        opt("exclude", REQ, c(b'x')),
        opt("include", REQ, c(b'y')),
        opt("exclude-all", NO, c(b'X')),
        opt("exclude-all-tile-attributes", NO, c(b'~')),
        opt("exclude-all-tile-geometries", NO, c(b'~')),
        heading("Filtering features by attributes"),
        opt("feature-filter-file", REQ, c(b'J')),
        opt("feature-filter", REQ, c(b'j')),
        heading("Setting or disabling tile size limits"),
        opt("no-tile-size-limit", NO, PK),
        opt("no-tile-compression", NO, PC),
        opt("no-tile-stats", NO, PG),
        opt("tile-stats-attributes-limit", REQ, c(b'~')),
        opt("tile-stats-sample-values-limit", REQ, c(b'~')),
        opt("tile-stats-values-limit", REQ, c(b'~')),
        heading("Progress indicator"),
        opt("quiet", NO, c(b'q')),
        heading("manby"),
        opt("compression", REQ, c(b'~')),
        opt("threads", REQ, c(b'~')),
        heading(""),
        opt("prevent", REQ, c(b'p')),
        opt("unidecode-data", REQ, c(b'~')),
    ]
}

/// The short options for getopt: the options whose value is a printable letter.
fn getopt_string(long_options: &[LongOption]) -> String {
    let mut s = String::new();
    for lo in long_options {
        if lo.val > ' ' as i32 && lo.val < 0x7F {
            s.push(lo.val as u8 as char);
            if lo.has_arg == REQUIRED_ARGUMENT {
                s.push(':');
            }
        }
    }
    s
}

/// The usage message.
fn usage(argv0: &str) -> String {
    let forms = [
        "[options] source.mbtiles ...",
        "[options] --read-from=inputfile.txt",
    ];
    let required = [
        UsageRequiredOption {
            name: "output",
            placeholder: "new.mbtiles",
            alternation: 1,
        },
        UsageRequiredOption {
            name: "output-to-directory",
            placeholder: "directory",
            alternation: 1,
        },
    ];
    // Under tile-join's name, the usage is tile-join's, without manby's own options.
    let mut options = long_options_orig();
    if argv0.rsplit('/').next() == Some("tile-join") {
        let from = options.iter().position(|o| o.name == "manby").unwrap_or(0);
        let to = options
            .iter()
            .position(|o| o.name.is_empty())
            .unwrap_or(from);
        options.drain(from..to);
    }
    let mut out: Vec<u8> = Vec::new();
    print_usage(&mut out, argv0, &forms, &options, &required);
    let mut msg = String::from_utf8_lossy(&out).into_owned();
    if msg.ends_with('\n') {
        msg.pop();
    }
    msg
}

fn usage_error(argv0: &str, before: Option<String>) -> Error {
    let mut msg = before.map(|m| m + "\n").unwrap_or_default();
    msg += &usage(argv0);
    Error::new(EXIT_ARGS, msg)
}

/// The lines that `getline()` returns one by one.
fn getline_lines(content: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = content.split(|&c| c == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// `read_filter`, returning errors instead of exiting.
pub fn read_filter(fname: &str) -> Result<JsonTree> {
    let fp = std::fs::File::open(fname).map_err(|e| {
        Error::new(
            EXIT_OPEN,
            format!("{}: {}", fname, crate::options::os_message(&e)),
        )
    })?;
    let mut jp = JsonPull::begin_file(Box::new(std::io::BufReader::new(fp)));
    match jp.read_tree() {
        Some(t) => Ok(t),
        None => Err(Error::new(
            EXIT_JSON,
            format!("{}: {}", fname, jp.error.unwrap_or("(null)")),
        )),
    }
}

/// `parse_filter`, returning errors instead of exiting.
pub fn parse_filter(s: &str) -> Result<JsonTree> {
    let mut jp = JsonPull::begin_string(s.as_bytes());
    match jp.read_tree() {
        Some(t) => Ok(t),
        None => Err(Error::new(
            EXIT_JSON,
            format!(
                "Could not parse filter {}\n{}",
                s,
                jp.error.unwrap_or("(null)")
            ),
        )),
    }
}

/// Parses a tile-join command line. `argv[0]` names the program in messages,
/// and `commandline` is what the metadata records as the command.
pub fn parse_args(argv: &[String], commandline: String) -> Result<JoinOptions> {
    let mut argv: Vec<String> = argv.to_vec();
    let argv0 = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "tile-join".to_string());
    let argc = argv.len();

    let mut o = JoinOptions {
        program: argv0.clone(),
        commandline,
        ..Default::default()
    };
    // tile-join's number of threads, which --threads overrides
    if let Some(max) = std::env::var_os("TIPPECANOE_MAX_THREADS") {
        o.threads = tippecanoe::cstd::atoi(&max.to_string_lossy()).max(1) as usize;
    }
    let mut out_mbtiles: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut read_from: Option<Vec<String>> = None;

    let long_options_orig = long_options_orig();
    let long_options = strip_usage_headings(&long_options_orig);
    let getopt_str = getopt_string(&long_options);

    let mut g = Getopt::new();
    let mut option_index: usize = 0;
    loop {
        let i = g.getopt_long(
            &mut argv,
            &getopt_str,
            &long_options,
            Some(&mut option_index),
        );
        if i == -1 {
            break;
        }
        let optarg: String = g.optarg.clone().unwrap_or_default();
        let atoi = |s: &str| tippecanoe::cstd::atoi(s);

        match i {
            PE => o.empty_csv_columns_are_null = true,
            PK => o.no_tile_size_limit = true,
            PC => o.no_tile_compression = true,
            PG => o.no_tile_stats = true,
            _ if i == c(b'o') => out_mbtiles = Some(optarg),
            _ if i == c(b'e') => out_dir = Some(optarg),
            _ if i == c(b'f') => o.force = true,
            _ if i == c(b'O') => o.overzoom = true,
            _ if i == c(b'b') => o.buffer = atoi(&optarg),
            _ if i == c(b'i') => o.ifmatched = true,
            _ if i == c(b'A') => o.attribution = optarg,
            _ if i == c(b'n') => o.name = optarg,
            _ if i == c(b'N') => o.description = optarg,
            _ if i == c(b'z') => o.maxzoom = atoi(&optarg),
            _ if i == c(b'Z') => o.minzoom = atoi(&optarg),
            _ if i == c(b'J') => o.filter = Some(read_filter(&optarg)?),
            _ if i == c(b'j') => o.filter = Some(parse_filter(&optarg)?),
            _ if i == c(b'p') => match optarg.as_str() {
                "k" => o.no_tile_size_limit = true,
                "C" => o.no_tile_compression = true,
                "g" => o.no_tile_stats = true,
                "e" => o.empty_csv_columns_are_null = true,
                _ => {
                    return Err(Error::new(
                        EXIT_ARGS,
                        format!("{}: Unknown option for -p{}", argv0, optarg),
                    ));
                }
            },
            _ if i == c(b'c') => {
                if o.csv.is_some() {
                    return Err(Error::new(EXIT_ARGS, "Only one -c for now"));
                }
                o.csv = Some(Arc::new(RawCsv::read(&optarg)?));
            }
            _ if i == c(b'x') => {
                o.exclude.insert(optarg);
            }
            _ if i == c(b'X') => o.exclude_all = true,
            _ if i == c(b'y') => {
                o.exclude_all = true;
                o.include.insert(optarg);
            }
            _ if i == c(b'l') => {
                o.keep_layers.insert(optarg);
            }
            _ if i == c(b'L') => {
                o.remove_layers.insert(optarg);
            }
            _ if i == c(b'R') => match optarg.find(':') {
                None | Some(0) => {
                    return Err(Error::new(
                        EXIT_ARGS,
                        format!("{}: -R requires old:new", argv0),
                    ));
                }
                Some(cp) => {
                    o.renames
                        .entry(optarg[..cp].to_string())
                        .or_insert_with(|| optarg[cp + 1..].to_string());
                }
            },
            _ if i == c(b'r') => {
                // a file that cannot be opened is passed over, as tile-join does
                if let Ok(content) = std::fs::read(&optarg) {
                    let list = read_from.get_or_insert_with(Vec::new);
                    for line in getline_lines(&content) {
                        list.push(String::from_utf8_lossy(line).into_owned());
                    }
                }
            }
            _ if i == c(b'q') => o.quiet = true,
            _ if i == c(b'~') => {
                let name = long_options[option_index].name;
                match name {
                    "tile-stats-attributes-limit" => {
                        o.max_tilestats_attributes = atoi(&optarg) as usize
                    }
                    "tile-stats-sample-values-limit" => {
                        o.max_tilestats_sample_values = atoi(&optarg) as usize
                    }
                    "tile-stats-values-limit" => o.max_tilestats_values = atoi(&optarg) as usize,
                    "unidecode-data" => {
                        o.unidecode_data = tippecanoe::text::read_unidecode(&optarg)
                    }
                    "exclude-all-tile-attributes" => o.exclude_all_tile_attributes = true,
                    "exclude-all-tile-geometries" => o.exclude_all_tile_geometries = true,
                    "compression" => match optarg.as_str() {
                        "zlib" => o.zlib_tiles = true,
                        "libdeflate" => o.zlib_tiles = false,
                        _ => {
                            return Err(Error::new(
                                EXIT_ARGS,
                                format!(
                                    "{}: --compression must be zlib or libdeflate, not {}",
                                    argv0, optarg
                                ),
                            ));
                        }
                    },
                    "threads" => {
                        let n = atoi(&optarg);
                        if n < 0 || optarg.is_empty() || !optarg.bytes().all(|b| b.is_ascii_digit())
                        {
                            return Err(Error::new(
                                EXIT_ARGS,
                                format!("{}: Threads must be a number (got {})", argv0, optarg),
                            ));
                        }
                        o.threads = n as usize;
                    }
                    _ => {
                        return Err(Error::new(
                            EXIT_ARGS,
                            format!("{}: Unrecognized option --{}", argv0, name),
                        ));
                    }
                }
            }
            _ => return Err(usage_error(&argv0, None)),
        }
    }

    let optind = g.optind;
    if argc - optind.min(argc) < 1 && read_from.is_none() {
        return Err(usage_error(&argv0, None));
    }
    match (&out_mbtiles, &out_dir) {
        (None, None) => {
            return Err(usage_error(
                &argv0,
                Some(format!(
                    "{}: must specify -o out.mbtiles or -e directory",
                    argv0
                )),
            ));
        }
        (Some(_), Some(_)) => {
            return Err(usage_error(
                &argv0,
                Some(format!(
                    "{}: Options -o and -e cannot be used together",
                    argv0
                )),
            ));
        }
        _ => {}
    }
    if o.minzoom > o.maxzoom {
        return Err(Error::new(
            EXIT_ARGS,
            format!(
                "{}: Minimum zoom -Z{} cannot be greater than maxzoom -z{}",
                argv0, o.minzoom, o.maxzoom
            ),
        ));
    }
    if o.buffer < 0 {
        return Err(Error::new(
            EXIT_ARGS,
            format!("{}: buffer cannot be less than 0", argv0),
        ));
    }

    o.output = match (out_mbtiles, out_dir) {
        (Some(out), _) if tippecanoe::pmtiles_file::pmtiles_has_suffix(&out) => {
            Output::Pmtiles(out)
        }
        (Some(out), _) => Output::Mbtiles(out),
        (None, Some(dir)) => Output::Directory(dir),
        (None, None) => Output::None,
    };
    if read_from.is_none() {
        o.inputs = argv[optind.min(argc)..].to_vec();
    }
    o.read_from = read_from;
    Ok(o)
}
