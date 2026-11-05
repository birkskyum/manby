//! Cases checked against the vendored C++ wagyu.
//!
//! `data/cases.txt` holds the inputs in the format of `examples/manby_wagyu_diff.rs`, and
//! `data/expected.txt` is what the C++ printed for them, built with
//! `clang++ -std=c++17 -O3 -DNDEBUG -ffp-contract=off`. The cases come from the random
//! generators and the tippecanoe GeoJSON fixtures used in the differential runs (see `LABELS`).

use std::fmt::Write as _;

use manby_wagyu::{ClipType, FillType, MultiPolygon, Point, PolygonType, Wagyu};

/// Where each case in `data/cases.txt` came from.
const LABELS: [&str; 39] = [
    "random 344",
    "random 520",
    "random 644",
    "random 738",
    "random 1161",
    "random 1390",
    "random 2047",
    "random 2416",
    "random 2533",
    "random 2662",
    "random, other clip and fill types 326",
    "random, other clip and fill types 484",
    "random, other clip and fill types 521",
    "random, other clip and fill types 2089",
    "nested rings 352",
    "nested rings 708",
    "nested rings 1289",
    "nested rings 1968",
    "nested rings 2518",
    "nested rings 2886",
    "nested rings, other clip and fill types 559",
    "nested rings, other clip and fill types 1293",
    "nested rings, other clip and fill types 1666",
    "many overlapping rings 153",
    "many overlapping rings 1327",
    "many overlapping rings 1848",
    "many overlapping rings 2747",
    "large self-intersecting rings 37",
    "fixtures 895",
    "fixtures 1343",
    "fixtures 1744",
    "fixtures 2853",
    "fixtures 6527",
    "fixtures 8164",
    "fixtures 11407",
    "fixtures 13646",
    "sliver that depends on floating point contraction",
    "unit.cpp: Polygon cleaning drops a hole that no ring can parent",
    "unit.cpp: Polygon cleaning drops a hole that no ring can parent, scaled by 16",
];

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

struct Case {
    mode: i64,
    rings: Vec<(i64, Vec<Point>)>,
}

fn parse_cases(text: &str) -> Vec<Case> {
    let mut it = text
        .split_ascii_whitespace()
        .map(|t| t.parse::<i64>().unwrap());
    let mut next = || it.next().unwrap();
    let ncases = next();
    (0..ncases)
        .map(|_| {
            let mode = next();
            let nrings = next();
            let rings = (0..nrings)
                .map(|_| {
                    let t = next();
                    let npts = next();
                    (t, (0..npts).map(|_| Point::new(next(), next())).collect())
                })
                .collect();
            Case { mode, rings }
        })
        .collect()
}

/// Mode 0 is an intersection and mode 1 a union, both with positive fills, as tippecanoe
/// calls wagyu. Mode `100 + clip * 16 + subject_fill * 4 + clip_fill` is any other combination.
fn run(case: &Case) -> Result<MultiPolygon, manby_wagyu::Error> {
    let mut w = Wagyu::new();
    for (t, ring) in &case.rings {
        let p_type = if *t == 0 {
            PolygonType::Subject
        } else {
            PolygonType::Clip
        };
        w.try_add_ring(ring, p_type)?;
    }
    let (cliptype, subject_fill, clip_fill) = if case.mode >= 100 {
        let m = (case.mode - 100) as usize;
        (
            CLIP_TYPES[m / 16],
            FILL_TYPES[(m / 4) % 4],
            FILL_TYPES[m % 4],
        )
    } else if case.mode == 1 {
        (ClipType::Union, FillType::Positive, FillType::Positive)
    } else {
        (
            ClipType::Intersection,
            FillType::Positive,
            FillType::Positive,
        )
    };
    w.execute(cliptype, subject_fill, clip_fill)
}

fn format_result(i: usize, result: &Result<MultiPolygon, manby_wagyu::Error>) -> String {
    let mut out = format!("C {i}\n");
    match result {
        Ok(mp) => {
            for poly in mp {
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
    out
}

/// Splits the C++ output into one block per case.
fn split_expected(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with("C ") {
            blocks.push(String::new());
        }
        let block = blocks.last_mut().unwrap();
        block.push_str(line);
        block.push('\n');
    }
    blocks
}

#[test]
fn matches_cpp() {
    let cases = parse_cases(include_str!("data/cases.txt"));
    let expected = split_expected(include_str!("data/expected.txt"));
    assert_eq!(cases.len(), LABELS.len());
    assert_eq!(cases.len(), expected.len());
    for (i, case) in cases.iter().enumerate() {
        let actual = format_result(i, &run(case));
        assert_eq!(actual, expected[i], "case {i} ({})", LABELS[i]);
    }
}

fn ring(points: &[(i64, i64)]) -> Vec<Point> {
    points.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

/// unit.cpp: "Polygon cleaning drops a hole that no ring can parent". Two mutually reversed
/// self-intersecting rings whose union leaves a hole that topology correction cannot assign
/// to any parent. With tippecanoe's change to `correct_tree` the hole is dropped instead of
/// failing, and nothing is left.
#[test]
fn drops_a_hole_that_no_ring_can_parent() {
    let mut w = Wagyu::new();
    w.add_ring(
        &ring(&[(0, 5), (5, 4), (5, 1), (4, 4), (4, 2), (7, 1), (0, 5)]),
        PolygonType::Subject,
    );
    w.add_ring(
        &ring(&[
            (0, 5),
            (7, 1),
            (4, 2),
            (4, 4),
            (5, 1),
            (5, 4),
            (0, 0),
            (0, 5),
        ]),
        PolygonType::Subject,
    );
    let result = w
        .execute(ClipType::Union, FillType::Positive, FillType::Positive)
        .unwrap();
    assert_eq!(result, MultiPolygon::new());
}

/// Apple clang on arm64 fuses multiply-adds unless built with `-ffp-contract=off`, and for
/// this sliver the fused build keeps an extra three point ring. The port matches the
/// unfused build.
#[test]
fn sliver_matches_unfused_cpp() {
    let mut w = Wagyu::new();
    w.add_ring(
        &ring(&[
            (141491127, 321643025),
            (97240511, 321643026),
            (141491127, 321643025),
            (90566969, 321643026),
            (141491127, 321643025),
            (145503241, 321643024),
        ]),
        PolygonType::Subject,
    );
    w.add_ring(
        &ring(&[
            (835221150, 673624914),
            (835221149, 719710867),
            (835221150, 669085290),
            (842020606, 669085291),
            (842020606, 669085291),
            (821510684, 669085290),
        ]),
        PolygonType::Subject,
    );
    let result = w
        .execute(ClipType::Union, FillType::Positive, FillType::Positive)
        .unwrap();
    assert_eq!(
        result,
        vec![vec![ring(&[
            (835221150, 669085290),
            (842020606, 669085291),
            (835221150, 669085291),
            (835221150, 669085290)
        ])]]
    );
}

#[test]
fn empty_input() {
    let mut w = Wagyu::new();
    assert!(!w.add_ring(&ring(&[(0, 0), (1, 1)]), PolygonType::Subject));
    assert!(!w.add_ring(
        &ring(&[(0, 0), (1, 1), (2, 2), (0, 0)]),
        PolygonType::Subject
    ));
    let mut solution = MultiPolygon::new();
    assert_eq!(
        w.execute_into(
            ClipType::Union,
            &mut solution,
            FillType::Positive,
            FillType::Positive
        ),
        Ok(false)
    );
    assert!(solution.is_empty());
}
