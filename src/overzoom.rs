//! Port of overzoom.cpp, the `tippecanoe-overzoom` program.
//!
//! The C++ keeps the settings in file-level globals that only `main` uses;
//! here they are local variables of `main`.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::AtomicI32;

use crate::attribute::{AttributeOp, set_attribute_accum_arg};
use crate::clip::overzoom;
use crate::cstd::{atof, atoi, perror_io, strtoll};
use crate::errors::{EXIT_FAILURE, EXIT_OPEN};
use crate::evaluator::parse_filter;
use crate::geometry::InputTile;
use crate::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use crate::text::read_unidecode;
use crate::usage::{UsageRequiredOption, getopt_string, print_usage, strip_usage_headings};

const fn opt(
    name: &'static str,
    has_arg: i32,
    flag: Option<&'static AtomicI32>,
    val: u8,
) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag,
        val: val as i32,
    }
}

static LONG_OPTIONS: [LongOption; 25] = [
    opt("Output tile", 0, None, 0),
    opt("output", REQUIRED_ARGUMENT, None, b'o'),
    opt("source-tile", REQUIRED_ARGUMENT, None, b't'),
    opt("no-tile-compression", NO_ARGUMENT, None, b'd' & 0x1F),
    //
    opt("Tile resolution", 0, None, 0),
    opt("full-detail", REQUIRED_ARGUMENT, None, b'd'),
    opt("buffer", REQUIRED_ARGUMENT, None, b'b'),
    //
    opt("Filtering feature attributes", 0, None, 0),
    opt("include", REQUIRED_ARGUMENT, None, b'y'),
    opt("exclude", REQUIRED_ARGUMENT, None, b'x'),
    opt("exclude-prefix", REQUIRED_ARGUMENT, None, b'x' & 0x1F),
    //
    opt("Modifying feature attributes", 0, None, 0),
    opt("accumulate-attribute", REQUIRED_ARGUMENT, None, b'E'),
    //
    opt("Filtering features", 0, None, 0),
    opt("feature-filter", REQUIRED_ARGUMENT, None, b'j'),
    opt("feature-filter-file", REQUIRED_ARGUMENT, None, b'J'),
    opt("filter-points-multiplier", NO_ARGUMENT, None, b'm'),
    opt("deduplicate-by-id", NO_ARGUMENT, None, b'i' & 0x1F),
    //
    opt("Line and polygon simplification", 0, None, 0),
    opt("line-simplification", REQUIRED_ARGUMENT, None, b'S'),
    opt("tiny-polygon-size", REQUIRED_ARGUMENT, None, b's' & 0x1F),
    //
    opt("Reordering features within the tile", 0, None, 0),
    opt("preserve-input-order", NO_ARGUMENT, None, b'o' & 0x1F),
    //
    opt("", 0, None, 0),
    opt("unidecode-data", REQUIRED_ARGUMENT, None, b'u' & 0x1F),
];

fn usage(argv: &[String]) -> ! {
    let forms = [
        "[options] tile.pbf.gz oz/ox/oy nz/nx/ny",
        "[options] --source-tile=nz/nx/ny tile.pbf.gz oz/ox/oy ...",
    ];
    let required = [UsageRequiredOption {
        name: "output",
        placeholder: "newtile.pbf.gz",
        alternation: 0,
    }];

    print_usage(
        &mut std::io::stderr(),
        &argv[0],
        &forms,
        &LONG_OPTIONS,
        &required,
    );
    eprintln!("\nThe tile nz/nx/ny is created from the tile or tiles oz/ox/oy that contain it.");
    eprintln!("In the second form, each source tile is named by a file name and a z/x/y pair.");
    std::process::exit(EXIT_FAILURE);
}

/// `optarg` is the global the C++ passes to `perror()`, which is the same as `fname`.
fn read_json_file(fname: &str, optarg: &str) -> String {
    let mut out = Vec::new();

    let mut f = match File::open(fname) {
        Ok(f) => f,
        Err(e) => {
            perror_io(optarg, &e);
            std::process::exit(EXIT_OPEN);
        }
    };

    let mut buf = [0u8; 2000];
    loop {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(nread) => out.extend_from_slice(&buf[..nread]),
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// `sscanf(s, "%d/%d/%d", &z, &x, &y) == 3`.
fn scan_zxy(s: &str) -> Option<(i32, i32, i32)> {
    let mut out = [0i32; 3];
    let mut rest = s;
    for (i, v) in out.iter_mut().enumerate() {
        if i > 0 {
            rest = rest.strip_prefix('/')?;
        }
        let (n, used) = strtoll(rest, 10);
        if used == 0 {
            return None;
        }
        *v = n as i32;
        rest = &rest[used..];
    }
    Some((out[0], out[1], out[2]))
}

pub fn main() {
    let mut argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argc = argv.len();
    let mut outtile: Option<String> = None;
    let mut outfile: Option<String> = None;
    let mut simplification: f64 = 0.0;
    let mut tiny_polygon_size: f64 = 0.0;

    let mut detail = 12; // tippecanoe-style: mvt extent == 1 << detail
    let mut buffer = 5; // tippecanoe-style: mvt buffer == extent * buffer / 256;
    let mut demultiply = false;
    let mut do_compress = true;
    let mut deduplicate_by_id = false;

    let mut filter = String::new();
    let mut preserve_input_order = false;
    let mut attribute_accum: HashMap<String, AttributeOp> = HashMap::new();
    let mut unidecode_data: Vec<String> = Vec::new();

    let mut keep: BTreeSet<String> = BTreeSet::new();
    let mut exclude: BTreeSet<String> = BTreeSet::new();
    let mut exclude_prefix: Vec<String> = Vec::new();

    let mut sources: Vec<InputTile> = Vec::new();

    // the options above, with the usage message headings removed
    let real_long_options = strip_usage_headings(&LONG_OPTIONS);
    let getopt_str = getopt_string(&real_long_options);

    let mut g = Getopt::new();
    let mut option_index = 0;
    loop {
        let i = g.getopt_long(
            &mut argv,
            &getopt_str,
            &real_long_options,
            Some(&mut option_index),
        );
        if i == -1 {
            break;
        }
        let optarg = g.optarg.clone().unwrap_or_default();

        const NO_TILE_COMPRESSION: u8 = b'd' & 0x1F;
        const EXCLUDE_PREFIX: u8 = b'x' & 0x1F;
        const PRESERVE_INPUT_ORDER: u8 = b'o' & 0x1F;
        const UNIDECODE_DATA: u8 = b'u' & 0x1F;
        const TINY_POLYGON_SIZE: u8 = b's' & 0x1F;
        const DEDUPLICATE_BY_ID: u8 = b'i' & 0x1F;

        match u8::try_from(i) {
            Ok(b'y') => {
                keep.insert(optarg);
            }

            Ok(b'x') => {
                exclude.insert(optarg);
            }

            Ok(EXCLUDE_PREFIX) => exclude_prefix.push(optarg),

            Ok(b'o') => outfile = Some(optarg),

            Ok(b'd') => detail = atoi(&optarg),

            Ok(b'b') => buffer = atoi(&optarg),

            Ok(b'm') => demultiply = true,

            Ok(b'j') => filter = optarg,

            Ok(b'J') => filter = read_json_file(&optarg, &optarg),

            Ok(PRESERVE_INPUT_ORDER) => preserve_input_order = true,

            Ok(b'E') => set_attribute_accum_arg(&mut attribute_accum, &optarg, &argv),

            Ok(UNIDECODE_DATA) => unidecode_data = read_unidecode(&optarg),

            Ok(b't') => outtile = Some(optarg),

            Ok(TINY_POLYGON_SIZE) => tiny_polygon_size = atof(&optarg),

            Ok(b'S') => simplification = atof(&optarg),

            Ok(NO_TILE_COMPRESSION) => do_compress = false,

            Ok(DEDUPLICATE_BY_ID) => {
                deduplicate_by_id = true;
            }

            _ => {
                let mut msg = b"Unrecognized flag -".to_vec();
                msg.push(i as u8);
                msg.push(b'\n');
                let _ = std::io::stderr().write_all(&msg);
                usage(&argv);
            }
        }
    }

    let optind = g.optind;
    let mut its: Vec<InputTile> = Vec::new();
    let nz;
    let nx;
    let ny;

    let outfile = match outfile {
        Some(outfile) => outfile,
        None => {
            eprintln!("{}: must specify -o newtile.pbf.gz", argv[0]);
            usage(&argv);
        }
    };

    match outtile {
        None => {
            // single input
            if argc - optind != 3 {
                eprintln!("Wrong number of arguments");
                usage(&argv);
            }

            let infile = &argv[optind];

            let (oz, ox, oy) = match scan_zxy(&argv[optind + 1]) {
                Some(zxy) => zxy,
                None => {
                    eprintln!("{}: not in z/x/y form", argv[optind + 1]);
                    usage(&argv);
                }
            };

            (nz, nx, ny) = match scan_zxy(&argv[optind + 2]) {
                Some(zxy) => zxy,
                None => {
                    eprintln!("{}: not in z/x/y form", argv[optind + 2]);
                    usage(&argv);
                }
            };

            sources.push(InputTile {
                tile: infile.as_bytes().to_vec(),
                z: oz,
                x: ox,
                y: oy,
            });
        }
        Some(outtile) => {
            // multiple inputs
            if (argc - optind) % 2 != 0 {
                eprintln!("Unpaired arguments");
                usage(&argv);
            }

            (nz, nx, ny) = match scan_zxy(&outtile) {
                Some(zxy) => zxy,
                None => {
                    eprintln!("{}: not in z/x/y form", outtile);
                    usage(&argv);
                }
            };

            let mut i = optind;
            while i + 1 < argc {
                let (oz, ox, oy) = match scan_zxy(&argv[i + 1]) {
                    Some(zxy) => zxy,
                    None => {
                        eprintln!("{}: not in z/x/y form", argv[i + 1]);
                        usage(&argv);
                    }
                };

                sources.push(InputTile {
                    tile: argv[i].as_bytes().to_vec(),
                    z: oz,
                    x: ox,
                    y: oy,
                });

                i += 2;
            }
        }
    }

    let out;

    {
        let json_filter = if !filter.is_empty() {
            Some(parse_filter(&filter))
        } else {
            None
        };

        for s in &sources {
            let mut tile = Vec::new();
            let mut buf = [0u8; 1000];

            // the file name is held in `tile` until the file is read
            let name = String::from_utf8_lossy(&s.tile).into_owned();
            let mut f = match File::open(&name) {
                Ok(f) => f,
                Err(e) => {
                    perror_io(&name, &e);
                    std::process::exit(EXIT_FAILURE);
                }
            };

            loop {
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(len) => tile.extend_from_slice(&buf[..len]),
                }
            }
            drop(f);

            let mut t = s.clone();
            t.tile = tile;
            its.push(t);
        }

        out = overzoom(
            &its,
            nz,
            nx,
            ny,
            detail,
            buffer,
            &keep,
            &exclude,
            &exclude_prefix,
            do_compress,
            None,
            demultiply,
            json_filter.as_ref().map(|f| f.root()),
            preserve_input_order,
            &attribute_accum,
            &unidecode_data,
            simplification,
            tiny_polygon_size,
            &[],
            "",
            "",
            usize::MAX,
            &[],
            deduplicate_by_id,
        );
    }

    let mut f = match File::create(&outfile) {
        Ok(f) => f,
        Err(e) => {
            perror_io(&outfile, &e);
            std::process::exit(EXIT_FAILURE);
        }
    };

    let _ = f.write_all(&out);
}
