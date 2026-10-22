//! Driver for differential tests against the C++ Clipper2 1.4.0.
//!
//! Reads cases from stdin and prints each solution, in the format the C++ harness uses:
//!
//! ```text
//! case <cliptype> <fillrule>
//! opt <reverse> <preserve_collinear>   (optional, default 0 1)
//! s <n> x y ...   closed subject
//! o <n> x y ...   open subject
//! c <n> x y ...   clip
//! end
//! ```
//!
//! Each case calls `add_subject` (only if there are closed subjects), `add_open_subject`
//! and `add_clip` with all paths of that kind, then `execute`.

use std::io::{self, BufWriter, Read, Write};

use clipper2::{ClipType, Clipper64, FillRule, Path64, Paths64, Point64};

fn print_paths(out: &mut impl Write, name: &str, p: &Paths64) {
    writeln!(out, "{} {}", name, p.len()).unwrap();
    for path in p {
        write!(out, "{}", path.len()).unwrap();
        for pt in path {
            write!(out, " {} {}", pt.x, pt.y).unwrap();
        }
        writeln!(out).unwrap();
    }
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut tokens = input.split_ascii_whitespace();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    while let Some(tok) = tokens.next() {
        assert_eq!(tok, "case", "bad token {tok}");
        let mut num = || tokens.next().unwrap().parse::<i64>().unwrap();
        let ct = match num() {
            0 => ClipType::NoClip,
            1 => ClipType::Intersection,
            2 => ClipType::Union,
            3 => ClipType::Difference,
            _ => ClipType::Xor,
        };
        let fr = match num() {
            0 => FillRule::EvenOdd,
            1 => FillRule::NonZero,
            2 => FillRule::Positive,
            _ => FillRule::Negative,
        };
        let (mut s, mut o, mut c) = (Paths64::new(), Paths64::new(), Paths64::new());
        let mut any_s = false;
        let (mut reverse, mut preserve) = (false, true);
        loop {
            let kind = tokens.next().unwrap();
            if kind == "end" {
                break;
            }
            if kind == "opt" {
                reverse = tokens.next().unwrap() != "0";
                preserve = tokens.next().unwrap() != "0";
                continue;
            }
            let n: usize = tokens.next().unwrap().parse().unwrap();
            let mut path = Path64::with_capacity(n);
            for _ in 0..n {
                let x: i64 = tokens.next().unwrap().parse().unwrap();
                let y: i64 = tokens.next().unwrap().parse().unwrap();
                path.push(Point64::new(x, y));
            }
            match kind {
                "s" => {
                    s.push(path);
                    any_s = true;
                }
                "o" => o.push(path),
                _ => c.push(path),
            }
        }
        let mut clipper = Clipper64::new();
        clipper.set_reverse_solution(reverse);
        clipper.set_preserve_collinear(preserve);
        if any_s {
            clipper.add_subject(&s);
        }
        clipper.add_open_subject(&o);
        clipper.add_clip(&c);
        let mut solution = Paths64::new();
        let mut open_solution = Paths64::new();
        let ok = clipper.execute(ct, fr, &mut solution, &mut open_solution);
        writeln!(out, "case {}", if ok { 1 } else { 0 }).unwrap();
        print_paths(&mut out, "closed", &solution);
        print_paths(&mut out, "open", &open_solution);
    }
}
