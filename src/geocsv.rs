//! Port of geocsv.hpp and geocsv.cpp: point features from the lat and lon
//! columns of a CSV file.

// Kept in the shape of the C++ rather than in the shape clippy prefers.
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::identity_op,
    clippy::if_same_then_else,
    clippy::needless_late_init,
    clippy::only_used_in_recursion,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::vec_init_then_push,
    clippy::int_plus_one,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::cstd::{atof, perror_io};
use crate::csv::{csv_dequote, csv_getline, csv_split, is_number};
use crate::errors::{EXIT_CSV, EXIT_OPEN, EXIT_UTF8};
use crate::geometry::{Draw, DrawVec, VT_MOVETO};
use crate::globals::prevent;
use crate::mvt::{MVT_DOUBLE, MVT_NULL, MVT_STRING};
use crate::options::P_EMPTY_CSV_COLUMNS;
use crate::projection::projection;
use crate::serial::{KeyPool, SerialFeature, SerialVal, SerializationState, serialize_feature_sst};
use crate::text::check_utf8;

/// `s.c_str()` of a line: the text up to its first NUL.
fn c_str(s: &[u8]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

static WARNED: AtomicI32 = AtomicI32::new(0);

pub fn parse_geocsv(sst: &mut [SerializationState<'_>], fname: &str, layer: i32, layername: &str) {
    let stdin = std::io::stdin();
    let mut f: Box<dyn BufRead> = if fname.is_empty() {
        Box::new(stdin.lock())
    } else {
        match File::open(fname) {
            Ok(file) => Box::new(BufReader::new(file)),
            Err(e) => {
                perror_io(fname, &e);
                std::process::exit(EXIT_OPEN);
            }
        }
    };

    let mut header: Vec<String> = Vec::new();
    let mut latcol: isize = -1;
    let mut loncol: isize = -1;

    let s = csv_getline(&mut *f);
    if !s.is_empty() {
        let err = check_utf8(&s);
        if !err.is_empty() {
            eprintln!("{}: {}", fname, err);
            std::process::exit(EXIT_UTF8);
        }

        header = csv_split(&c_str(&s));

        for i in 0..header.len() {
            header[i] = csv_dequote(std::mem::take(&mut header[i]));

            let lower = header[i].to_ascii_lowercase();

            if lower == "y" || lower == "lat" || lower.contains("latitude") {
                latcol = i as isize;
            }
            if lower == "x"
                || lower == "lon"
                || lower == "lng"
                || lower == "long"
                || lower.contains("longitude")
            {
                loncol = i as isize;
            }
        }
    }

    if latcol < 0 || loncol < 0 {
        eprintln!("{}: Can't find \"lat\" and \"lon\" columns", fname);
        std::process::exit(EXIT_CSV);
    }

    let mut seq: usize = 0;
    let mut key_pool = KeyPool::default();
    loop {
        let s = csv_getline(&mut *f);
        if s.is_empty() {
            break;
        }

        let err = check_utf8(&s);
        if !err.is_empty() {
            eprintln!("{}: {}", fname, err);
            std::process::exit(EXIT_UTF8);
        }

        seq += 1;
        let mut line = csv_split(&c_str(&s));

        if line.len() != header.len() {
            eprintln!(
                "{}:{}: Mismatched column count: {} in line, {} in header",
                fname,
                seq + 1,
                line.len(),
                header.len()
            );
            std::process::exit(EXIT_CSV);
        }

        if line[loncol as usize].is_empty() || line[latcol as usize].is_empty() {
            if WARNED.load(Ordering::Relaxed) == 0 {
                eprintln!(
                    "{}:{}: null geometry (additional not reported)",
                    fname,
                    seq + 1
                );
                WARNED.store(1, Ordering::Relaxed);
            }
            continue;
        }
        let lon = atof(&line[loncol as usize]);
        let lat = atof(&line[latcol as usize]);

        let (x, y) = (projection().project)(lon, lat, 32);
        let mut dv = DrawVec::new();
        dv.push(Draw::new(VT_MOVETO, x, y));

        let mut full_keys: Vec<Arc<String>> = Vec::new();
        let mut full_values: Vec<SerialVal> = Vec::new();

        for i in 0..line.len() {
            if i != latcol as usize && i != loncol as usize {
                line[i] = csv_dequote(std::mem::take(&mut line[i]));

                let mut sv = SerialVal::default();
                if is_number(&line[i]) {
                    sv.type_ = MVT_DOUBLE;
                } else if line[i].is_empty() && prevent(P_EMPTY_CSV_COLUMNS) != 0 {
                    sv.type_ = MVT_NULL;
                    line[i] = "null".to_string();
                } else {
                    sv.type_ = MVT_STRING;
                }
                sv.s = line[i].clone();

                full_keys.push(key_pool.pool(&header[i]));
                full_values.push(sv);
            }
        }

        let mut sf = SerialFeature {
            layer: layer as i64,
            segment: sst[0].segment,
            has_id: false,
            id: 0,
            tippecanoe_minzoom: -1,
            tippecanoe_maxzoom: -1,
            feature_minzoom: 0,
            seq: sst[0].layer_seq,
            geometry: dv,
            t: 1, // POINT
            full_keys,
            full_values,
            ..Default::default()
        };

        serialize_feature_sst(&mut sst[0], &mut sf, layername);
    }

    // The file, if it is not standard input, is closed when it is dropped.
    drop(f);
}
