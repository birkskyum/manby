//! Cases checked against the C++ Clipper2 1.4.0 vendored in tippecanoe.
//!
//! Each input is in the format of `examples/clip_diff.rs`, and each expected output is
//! what the C++ harness printed for it, built with `-ffp-contract=off`.

use clipper2::{ClipType, Clipper64, FillRule, Path64, Paths64, Point64};

fn run(input: &str) -> String {
    let mut tokens = input.split_ascii_whitespace();
    assert_eq!(tokens.next(), Some("case"));
    let ct = match tokens.next().unwrap() {
        "0" => ClipType::NoClip,
        "1" => ClipType::Intersection,
        "2" => ClipType::Union,
        "3" => ClipType::Difference,
        _ => ClipType::Xor,
    };
    let fr = match tokens.next().unwrap() {
        "0" => FillRule::EvenOdd,
        "1" => FillRule::NonZero,
        "2" => FillRule::Positive,
        _ => FillRule::Negative,
    };
    let (mut s, mut o, mut c) = (Paths64::new(), Paths64::new(), Paths64::new());
    let mut clipper = Clipper64::new();
    loop {
        let kind = tokens.next().unwrap();
        if kind == "end" {
            break;
        }
        if kind == "opt" {
            clipper.set_reverse_solution(tokens.next().unwrap() != "0");
            clipper.set_preserve_collinear(tokens.next().unwrap() != "0");
            continue;
        }
        let n: usize = tokens.next().unwrap().parse().unwrap();
        let path: Path64 = (0..n)
            .map(|_| {
                let x = tokens.next().unwrap().parse().unwrap();
                let y = tokens.next().unwrap().parse().unwrap();
                Point64::new(x, y)
            })
            .collect();
        match kind {
            "s" => s.push(path),
            "o" => o.push(path),
            _ => c.push(path),
        }
    }
    if !s.is_empty() {
        clipper.add_subject(&s);
    }
    clipper.add_open_subject(&o);
    clipper.add_clip(&c);
    let mut closed = Paths64::new();
    let mut open = Paths64::new();
    let ok = clipper.execute(ct, fr, &mut closed, &mut open);
    let mut out = format!("case {}\n", if ok { 1 } else { 0 });
    for (name, paths) in [("closed", &closed), ("open", &open)] {
        out += &format!("{} {}\n", name, paths.len());
        for path in paths {
            out += &path.len().to_string();
            for pt in path {
                out += &format!(" {} {}", pt.x, pt.y);
            }
            out += "\n";
        }
    }
    out
}

/// An open line crossing a square.
#[test]
fn line_across_square() {
    let input = "\
case 1 2
o 3 0 5 10 5 20 5
c 4 2 0 8 0 8 10 2 10
end
";
    let expected = "\
case 1
closed 0
open 1
2 2 5 8 5
";
    assert_eq!(run(input), expected);
}

/// Open lines along clip edges, through corners and across a hole.
#[test]
fn lines_on_edges_and_hole() {
    let input = "\
case 1 2
o 2 -5 50 105 50
o 4 -5 -5 50 50 105 105 105 -5
o 3 0 0 100 0 100 100
o 2 25 25 75 75
c 4 0 0 100 0 100 100 0 100
c 4 25 25 25 75 75 75 75 25
end
";
    let expected = "\
case 1
closed 0
open 5
2 75 75 100 100
3 0 0 100 0 100 100
2 0 50 25 50
2 75 50 100 50
2 0 0 25 25
";
    assert_eq!(run(input), expected);
}

/// Union with NonZero of closed squares sharing edges (horizontal joins).
#[test]
fn union_closed_subjects() {
    let input = "\
case 2 1
s 4 0 0 10 0 10 10 0 10
s 4 10 0 20 0 20 10 10 10
s 4 5 10 15 10 15 20 5 20
c 4 20 5 30 5 30 15 20 15
end
";
    let expected = "\
case 1
closed 1
13 15 20 5 20 5 10 0 10 0 0 10 0 20 0 20 5 30 5 30 15 20 15 20 10 15 10
open 0
";
    assert_eq!(run(input), expected);
}

/// Difference with EvenOdd, ReverseSolution and PreserveCollinear(false).
#[test]
fn difference_reverse_no_collinear() {
    let input = "\
case 3 0
opt 1 0
s 8 0 0 30 0 30 30 0 30 0 20 20 20 20 10 0 10
c 4 10 -5 15 -5 15 35 10 35
o 2 -5 15 35 15
end
";
    let expected = "\
case 1
closed 3
4 10 20 0 20 0 30 10 30
8 30 0 15 0 15 10 20 10 20 20 15 20 15 30 30 30
4 10 0 0 0 0 10 10 10
open 2
2 10 15 -5 15
2 35 15 15 15
";
    assert_eq!(run(input), expected);
}

/// Several intersections share a point. A stable sort of the intersection nodes
/// gives a different result here, so this needs libc++'s std::sort order.
#[test]
fn intersection_ties() {
    let input = "\
case 1 2
o 7 1 -1 4 -2 1 1 -1 1 1 -4 4 -4 4 4
c 6 3 3 2 -2 -3 1 2 -1 0 -4 2 -4
o 5 -2 3 -2 3 -4 1 1 -3 -4 -4
o 7 -3 -2 0 0 -1 -3 -4 -2 3 1 0 -4 3 4
o 1 0 -4
c 0
c 12 4 3 -3 3 1 0 3 3 -1 -4 3 0 -1 4 -1 2 3 -1 -4 -4 -2 -1 4 -3
end
";
    let expected = "\
case 1
closed 0
open 9
2 -2 3 -2 2
2 1 0 3 3
2 4 -3 4 3
4 2 -1 4 -2 1 1 0 1
2 1 -2 0 -1
2 1 -1 2 -1
2 0 -3 1 -2
3 0 -3 1 -4 2 -4
3 0 0 3 1 0 -4
";
    assert_eq!(run(input), expected);
}

/// The C++ built with -ffp-contract=on (Apple clang's default on arm64) returns
/// `2 -1669723912 745521155 1866319692 3471196474` here. The port matches -ffp-contract=off.
#[test]
fn fp_contract_off() {
    let input = "\
case 1 2
o 3 -3216802812 1582532647 -1669723912 745521155 1866319692 3471196474
c 3 1866319692 3471196474 -3216802812 1582532647 -1669723912 745521155
end
";
    let expected = "\
case 1
closed 0
open 1
2 -3216802812 1582532647 -1669723912 745521155
";
    assert_eq!(run(input), expected);
}

/// Near-horizontal edges whose intersection point rounds outside the scanbeam,
/// so AddNewIntersectNode moves it with GetClosestPointOnSegment.
#[test]
fn intersection_point_correction() {
    let input = "\
case 1 2
o 15 4493 -5 -158 2 -5987 2 5914 -4 -4465 0 7118 -5 -994 -1 -3079 -1 -2635 -4 -2305 4 -8918 0 -9051 3 3700 5 8954 3 -7499 -2
o 4 2621 -1 6335 4 -907 4 -3624 2
o 7 6789 5 3330 -4 2530 1 507 -4 -1149 3 6309 1 -127 4
o 4 5405 -5 -3175 5 7388 -2 6278 2
o 2 -4027 -2 9737 -2
o 4 -6685 4 8716 4 -4652 2 -1732 -2
c 11 6962 1 820 -3 5552 0 -8484 -4 -7937 4 7927 -4 -213 -1 6864 1 -157 -4 9821 -4 1007 2
o 1 6952 -5
o 13 4944 -1 6975 4 2226 5 -1208 -5 3925 -4 5448 1 4462 -3 -6997 0 -9516 -2 -7512 4 6638 4 -930 -2 744 -5
c 10 -4792 -2 6952 0 -6527 0 -4103 3 -3694 -1 -2664 -3 4671 -2 7905 1 2628 2 5671 -3
end
";
    let expected = "\
case 1
closed 0
open 45
2 2637 1 5215 1
2 3828 1 3572 1
2 4423 1 4575 1
2 5941 1 5215 1
2 5431 1 5355 1
2 4497 1 4006 1
2 5833 1 5909 1
2 6535 1 6480 1
2 6831 0 6606 0
2 509 0 433 0
2 1969 0 1115 0
2 1763 0 1171 0
2 1593 0 971 0
2 2125 0 1764 0
2 2717 0 2690 0
2 6838 0 6833 0
2 4676 0 4076 -2
2 4457 -2 4937 0
2 4944 -1 5045 0
2 4893 0 4897 0
2 5020 0 4719 -1
2 2803 0 2814 0
2 2844 0 2776 0
2 2356 -2 2397 -2
2 3961 -2 4671 -2
2 5062 -2 6883 -2
2 -2593 -2 -2561 -2
2 -712 -2 -534 -2
2 -324 -2 -415 -2
2 178 -2 145 -2
2 1088 -2 1120 -2
2 1578 -2 4801 -4
2 2177 -2 2197 -2
2 1884 -2 1864 -2
2 1764 -2 1749 -2
2 1800 -2 1786 -2
2 5090 -4 2434 -2
3 2359 -2 5914 -4 2543 -2
2 3829 -4 2548 -2
3 4489 -2 4462 -3 2111 -2
2 4547 -4 2983 -2
3 3900 -2 3330 -4 3041 -2
2 3925 -4 4328 -2
3 775 -3 507 -4 411 -3
2 88 -3 186 -4
";
    assert_eq!(run(input), expected);
}

/// The same correction with closed subjects, Xor and Negative.
#[test]
fn intersection_point_correction_xor() {
    let input = "\
case 4 3
s 6 8974 -6 9407 18 4431 -11 1067 -14 -5854 17 -6856 3
o 2 -8018 16 -8018 16
s 6 -2650 4 8457 -19 -6996 -1 -4260 3 8214 15 -7240 13
s 3 1890 -13 5404 -6 5757 -15
o 9 -1132 -6 6052 -7 -228 16 -7829 -8 -6720 17 -2897 16 678 -17 5237 7 -3319 -2
c 3 -4201 3 6340 16 4265 16
s 10 6299 -3 6633 -6 4411 11 8918 -18 4774 -18 2127 3 7523 -5 -1722 -15 2267 -5 -9972 -9
c 4 6624 -11 -8691 2 -5532 19 -7519 4
o 9 9933 -10 6781 20 9400 -15 -4870 4 3138 -15 5895 -15 -4317 4 -8093 14 -3653 4
o 10 -1145 -20 9843 17 3938 -3 7973 -10 -7893 7 -4055 -8 -4849 -15 -3518 -2 -5112 0 -1725 14
end
";
    let expected = "\
case 1
closed 3
5 -5233 1 -7519 4 -5532 19 -8691 2 -6320 0
3 6214 0 4411 11 6051 -1
34 7974 -18 8918 -18 6987 -5 7416 -5 6866 -4 6710 -3 6323 -3 6256 -3 6198 -3 5770 -3 5878 -2 2127 3 2820 -2 -392 0 -2650 4 -2837 4 -2999 4 -2128 0 -4986 1 -5233 1 -6320 0 -6996 -1 -2495 -6 -2284 -6 -21 -9 209 -10 -1722 -15 704 -12 1067 -14 2078 -13 4300 -14 4774 -18 7598 -18 8457 -19
open 20
3 -6975 11 -6720 17 -5941 16
3 -5827 16 -2897 16 -828 -3
4 2369 -6 6052 -7 -228 16 -4906 1
3 -6999 11 -8093 14 -6942 11
2 -4833 1 -1725 14
2 -6630 10 -3653 4
3 -71 -3 -4317 4 -6687 10
3 -7783 6 -7893 7 -7817 6
3 2623 -6 5237 7 -2587 -1
5 2748 -6 9843 17 3938 -3 7973 -10 -7217 6
2 -7306 3 -7184 6
2 -7384 5 -6978 3
3 5502 -10 -4870 4 -2728 -1
3 -5515 0 -7829 -8 -7432 0
4 -6037 0 -4055 -8 -4849 -15 -3554 -2
2 -1132 -6 1099 -6
4 -1428 -4 3138 -15 5895 -15 1329 -6
3 -639 -4 678 -17 2485 -7
2 -1145 -20 2553 -7
4 9933 -10 6781 20 9400 -15 5994 -10
";
    assert_eq!(run(input), expected);
}

/// Roads from tests/tl_2018_51685_roads clipped by a z24 tile.
#[test]
fn roads_tile_z24() {
    let input = "\
case 1 2
o 5 4779906 6425756 4779866 6425770 4779838 6425776 4779835 6425784 4779826 6425810
o 2 4779839 6425805 4779835 6425784
c 4 4779776 6425728 4779840 6425728 4779840 6425792 4779776 6425792
end
";
    let expected = "\
case 1
closed 0
open 2
4 4779840 6425775 4779838 6425776 4779835 6425784 4779832 6425792
2 4779837 6425792 4779835 6425784
";
    assert_eq!(run(input), expected);
}

/// Two neighbouring countries from tests/ne_110m_admin_0_countries as open lines,
/// clipped by one of them, in world coordinates.
#[test]
fn country_borders_z32() {
    let input = "\
case 1 2
o 16 2537988994 1699186101 2538832395 1699106178 2540556192 1695581738 2549149312 1695779103 2559997039 1691405260 2551935994 1697642559 2552804675 1700374821 2551526003 1699863309 2549246712 1700967054 2547460650 1700663630 2546865701 1701222221 2546628345 1699749149 2545768290 1698855080 2543467428 1698703108 2540229440 1699947323 2537988994 1699186101
o 15 2552804675 1700374821 2553177669 1701546193 2540948310 1707446508 2535107978 1705564582 2532320675 1699722768 2537988994 1699186101 2540229440 1699947323 2543467428 1698703108 2545768290 1698855080 2546628345 1699749149 2546865701 1701222221 2547460650 1700663630 2549246712 1700967054 2551526003 1699863309 2552804675 1700374821
c 16 2537988994 1699186101 2538832395 1699106178 2540556192 1695581738 2549149312 1695779103 2559997039 1691405260 2551935994 1697642559 2552804675 1700374821 2551526003 1699863309 2549246712 1700967054 2547460650 1700663630 2546865701 1701222221 2546628345 1699749149 2545768290 1698855080 2543467428 1698703108 2540229440 1699947323 2537988994 1699186101
end
";
    let expected = "\
case 1
closed 0
open 8
5 2543467428 1698703108 2545768290 1698855080 2546628345 1699749149 2546865701 1701222221 2547460650 1700663630
3 2547460650 1700663630 2549246712 1700967054 2551526003 1699863309
3 2551526003 1699863309 2549246712 1700967054 2547460650 1700663630
3 2537988994 1699186101 2540229440 1699947323 2543467428 1698703108
3 2543467428 1698703108 2540229440 1699947323 2537988994 1699186101
4 2559997039 1691405260 2551935994 1697642559 2552804675 1700374821 2551526003 1699863309
3 2537988994 1699186101 2538832395 1699106178 2540556192 1695581738
5 2547460650 1700663630 2546865701 1701222221 2546628345 1699749149 2545768290 1698855080 2543467428 1698703108
";
    assert_eq!(run(input), expected);
}
