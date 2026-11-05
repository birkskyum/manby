//! Differential test driver, the counterpart of a C++ program that runs the vendored wagyu.
//!
//! Reads from stdin: the number of cases, then for each case `mode nrings`, then for each ring
//! `type npts x1 y1 ... xn yn` (type is 0 for subject and 1 for clip). Mode 0 is an
//! intersection and mode 1 a union, both with `fill_type_positive` as tippecanoe uses them;
//! mode `100 + clip * 16 + subject_fill * 4 + clip_fill` picks any other combination.
//! Prints `C i`, then either `E message` or one `P nrings` line per polygon with an
//! `R npts x y ...` line per ring.

use std::fmt::Write as _;
use std::io::{Read, Write};

use wagyu::{ClipType, FillType, Point, PolygonType, Wagyu};

const CLIP_TYPES: [ClipType; 4] = [
    ClipType::Intersection,
    ClipType::Union,
    ClipType::Difference,
    ClipType::XOr,
];
const FILL_TYPES: [FillType; 4] = [
    FillType::EvenOdd,
    FillType::NonZero,
    FillType::Positive,
    FillType::Negative,
];

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let mut it = input
        .split_ascii_whitespace()
        .map(|t| t.parse::<i64>().unwrap());
    let mut next = || it.next().unwrap();
    let ncases = next();
    let mut out = String::new();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for c in 0..ncases {
        let clip = next();
        let nrings = next();
        let mut rings = Vec::new();
        for _ in 0..nrings {
            let t = next();
            let npts = next();
            let ring: Vec<Point> = (0..npts).map(|_| Point::new(next(), next())).collect();
            rings.push((t, ring));
        }
        writeln!(out, "C {c}").unwrap();
        let mut w = Wagyu::new();
        let mut error = None;
        for (t, ring) in &rings {
            let p_type = if *t == 0 {
                PolygonType::Subject
            } else {
                PolygonType::Clip
            };
            if let Err(e) = w.try_add_ring(ring, p_type) {
                error = Some(e);
                break;
            }
        }
        let (cliptype, subject_fill, clip_fill) = if clip >= 100 {
            let m = clip - 100;
            (
                CLIP_TYPES[(m / 16) as usize],
                FILL_TYPES[((m / 4) % 4) as usize],
                FILL_TYPES[(m % 4) as usize],
            )
        } else if clip == 1 {
            (ClipType::Union, FillType::Positive, FillType::Positive)
        } else {
            (
                ClipType::Intersection,
                FillType::Positive,
                FillType::Positive,
            )
        };
        let result = match error {
            Some(e) => Err(e),
            None => w.execute(cliptype, subject_fill, clip_fill),
        };
        match result {
            Ok(result) => {
                for poly in &result {
                    writeln!(out, "P {}", poly.len()).unwrap();
                    for ring in poly {
                        write!(out, "R {}", ring.len()).unwrap();
                        for p in ring {
                            write!(out, " {} {}", p.x, p.y).unwrap();
                        }
                        out.push('\n');
                    }
                }
            }
            Err(e) => writeln!(out, "E {e}").unwrap(),
        }
        if out.len() > 1 << 19 {
            stdout.write_all(out.as_bytes()).unwrap();
            out.clear();
        }
    }
    stdout.write_all(out.as_bytes()).unwrap();
}
